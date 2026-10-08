//! The headless application: every input is a `Msg`, every side effect is an `Effect`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use coder_sdk::{ChatStatus, StreamEvent, StreamEventType, types};
use uuid::Uuid;

use crate::attachments::{self, Chip, ChipState};
use crate::chat_list::{ChatList, ListQuery};
use crate::commands::{self, Command};
use crate::compaction::{self, Shown};
use crate::config::BusyBehavior;
use crate::density::DisplayPrefs;
use crate::files::{self, ConflictChoice, FileAction, OnConflict, SaveConflict, SaveTo};
use crate::line_edit::{Edit, EditOutcome, LineEdit};
use crate::live::{Applied, LiveBlock};
use crate::panels::{self, Fetched, GitPanel, LocalGit, McpPanel};
use crate::question::{self, Answer, Choice};
use crate::skills::{self, MenuEntry, Skill};
use crate::transcript::Transcript;
use crate::usage::{Limit, LimitState, Refusal};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkspaceRef {
    pub id: Uuid,
    pub name: String,
    /// The template's display name, else its name.
    pub template: String,
    /// The latest build's status, such as `running`.
    pub status: String,
    /// When the workspace was last used, in Unix seconds.
    pub last_used: Option<i64>,
}

/// Where the workspace list for the current organization stands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum WorkspacesState {
    #[default]
    Loading,
    Loaded,
    Failed(String),
}

/// One of the user's organizations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgRef {
    pub id: Uuid,
    pub name: String,
    pub display_name: String,
    pub is_default: bool,
    /// Whether the server lets the user create chats here. `true` when that could not be
    /// checked, so a failed check never hides an organization.
    pub can_create_chats: bool,
}

impl OrgRef {
    /// The display name, or the name when there is none.
    pub fn label(&self) -> &str {
        if self.display_name.trim().is_empty() {
            &self.name
        } else {
            &self.display_name
        }
    }
}

/// The signed-in user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRef {
    pub id: Uuid,
    pub username: String,
}

/// A `/chats` row action on one chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatAction {
    /// Archives a chat that is not archived, and unarchives one that is.
    ToggleArchive(Uuid),
    /// Archives the chat; a chat already archived stays as it is.
    Archive(Uuid),
    /// Archives the chat, then deletes its workspace, as the web UI's "Archive & delete
    /// workspace" does. `workspace` is the one the user was shown, and the delete refuses if
    /// the chat has moved to another by the time it is confirmed.
    ArchiveAndDeleteWorkspace {
        chat: Uuid,
        workspace: Uuid,
    },
    TogglePin(Uuid),
    ToggleRead(Uuid),
    Rename(Uuid),
}

/// What became of the workspace after `Effect::ArchiveAndDeleteWorkspace` archived its chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceDeletion {
    /// The delete build was queued; `no_provisioner` when no provisioner matched it yet.
    Started { no_provisioner: bool },
    /// The server answered 404 or 410: the workspace was already gone.
    AlreadyGone,
    /// The delete failed for another reason, with the server's message.
    Failed(String),
}

/// One field of `PATCH /chats/{chat}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatChange {
    Archived(bool),
    Title(String),
    /// 0 unpins; a positive value pins, and the server picks the position.
    PinOrder(i64),
    /// `true` marks every message read; `false` makes the chat read as unread.
    Read(bool),
}

impl ChatChange {
    fn verb(&self) -> &'static str {
        match self {
            ChatChange::Archived(true) => "archive the chat",
            ChatChange::Archived(false) => "unarchive the chat",
            ChatChange::Title(_) => "rename the chat",
            ChatChange::PinOrder(0) => "unpin the chat",
            ChatChange::PinOrder(_) => "pin the chat",
            ChatChange::Read(true) => "mark the chat read",
            ChatChange::Read(false) => "mark the chat unread",
        }
    }
}

/// A `/queue` action on one queued message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueAction {
    /// "Send now": runs the message next, interrupting a running turn.
    Promote(i64),
    /// "Send now" for the first queued message, from the send key on an empty composer.
    PromoteFirst,
    Remove(i64),
}

/// The cost row of `/info`.
#[derive(Debug, Clone)]
pub enum CostState {
    Loading,
    Loaded(types::CodersdkChatCost),
    /// The server refused (`403` or `404`), as it does without AI Gateway data access.
    Hidden,
    Failed(String),
}

/// An action in the `/workspace` panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceAction {
    CopySsh,
    OpenWeb,
    Detach,
    Switch,
}

/// An action in the `/git` panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitAction {
    OpenPr,
    ViewDiff,
}

/// What the one-line editor is editing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditTarget {
    Rename(Uuid),
    /// Confirming or editing a server-proposed title, from `/title` with no text.
    Title(Uuid),
    /// The free-text "Other" answer to a plan-mode question.
    Other,
    /// Where to save a chat file, from `s` in `/files`.
    SaveAs(Uuid),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editor {
    pub target: EditTarget,
    pub line: LineEdit,
    /// Set while the starting text is still on its way, such as a proposed title.
    pub loading: bool,
}

/// A read-only second stream on one subagent, shown live in the `/subagents` popup.
#[derive(Debug, Default)]
pub struct Preview {
    pub chat: Uuid,
    pub transcript: Transcript,
    /// Why the preview stream last ended, shown until it is live again.
    pub error: Option<String>,
    generation: u64,
    attempt: u32,
}

impl Preview {
    /// The stream generation this preview's events must carry.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// Progress through the pending question set.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Answering {
    call_id: String,
    /// The question being answered.
    index: usize,
    /// The highlighted option; `options.len()` is "Other".
    selected: usize,
    /// One answer per question answered so far; Back keeps later ones, as the web UI does.
    answers: Vec<Answer>,
    /// Set by Esc, and after the answers are sent, so the menu stays closed for this set.
    closed: bool,
    /// The message the answers were sent as, so its failed send can reopen the menu.
    sent: Option<String>,
}

impl Answering {
    fn new(call_id: String) -> Answering {
        Answering {
            call_id,
            index: 0,
            selected: 0,
            answers: vec![],
            closed: false,
            sent: None,
        }
    }

    /// Moves to question `index` of `questions`, highlighting its earlier answer if any.
    fn show(&mut self, questions: &[question::Question], index: usize) {
        self.index = index;
        self.selected = match (questions.get(index), self.answers.get(index)) {
            (Some(q), Some(Answer::Choice(label))) => q
                .options
                .iter()
                .position(|o| &o.label == label)
                .unwrap_or(0),
            (Some(q), Some(Answer::Other(_))) => q.options.len(),
            _ => 0,
        };
    }
}

/// A key for the question menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionKey {
    Up,
    Down,
    Enter,
    /// Returns to the previous question, like the web UI's Back button.
    Back,
    /// Hides the menu for this question set until `Show`.
    Dismiss,
    /// Shows the menu again after `Dismiss`.
    Show,
}

/// The question menu as the TUI draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionMenu {
    pub number: usize,
    pub count: usize,
    pub header: String,
    pub question: String,
    pub options: Vec<Choice>,
    /// The highlighted row; `options.len()` is "Other".
    pub selected: usize,
}

/// One provider's models in the `/model` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelGroup {
    pub provider_id: Option<Uuid>,
    pub provider: String,
    /// Why the provider cannot be used, when it cannot.
    pub reason: Option<String>,
    pub models: Vec<ModelRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRow {
    pub id: Uuid,
    pub name: String,
    pub current: bool,
    pub default: bool,
    /// The context window as `/usage` writes a token count, such as `200.0k tokens`.
    pub context: Option<String>,
    pub usable: bool,
    /// The compaction threshold in effect: the user's override, else the model's default.
    pub compaction: Shown,
    /// The reasoning efforts the model offers, lowest first.
    pub efforts: Vec<String>,
}

/// The organization servers a new chat starts with, as the web UI picks them: every enabled
/// server that is required or on by default (`getDefaultMCPSelection` in
/// `site/src/pages/AgentsPage/utils/mcpSelection.ts`).
pub(crate) fn default_mcp(servers: &[types::CodersdkMcpServerConfig]) -> Vec<Uuid> {
    servers
        .iter()
        .filter(|s| s.enabled != Some(false))
        .filter(|s| is_required(s) || s.availability.as_deref() == Some("default_on"))
        .filter_map(|s| s.id)
        .collect()
}

/// Whether the organization requires `server`: it is enabled and always on, whatever a
/// selection says.
pub(crate) fn is_required(server: &types::CodersdkMcpServerConfig) -> bool {
    server.enabled != Some(false) && server.availability.as_deref() == Some("force_on")
}

/// The MCP servers the workspace brings to `chat`.
pub(crate) fn workspace_mcp(
    chat: &types::CodersdkChat,
) -> impl Iterator<Item = &types::CodersdkChatContextResource> {
    chat.context
        .iter()
        .flat_map(|c| c.resources.iter())
        .filter(|r| r.kind.as_ref().is_some_and(|k| k.as_str() == "mcp_server"))
}

/// What a jump to a file's message says when something ends it before it lands.
const STOPPED_LOOKING: &str = "Stopped looking for the file's message.";

/// What `/mcp` says while a chat is being created or loaded, when no selection can land yet.
const MCP_WAIT: &str = "Wait for this chat to finish starting, then use /mcp.";

/// What `/mcp` says after the chat failed to load, when nothing is starting and only a message
/// retries the load.
const MCP_FAILED_LOAD: &str = "The chat did not load. Send a message to retry, then use /mcp.";

fn provider_reason(reason: Option<&str>) -> String {
    match reason {
        Some("missing_api_key") => "no API key is configured".into(),
        Some("user_api_key_required") => "needs your API key".into(),
        Some("fetch_failed") => "its models could not be loaded".into(),
        Some(other) if !other.is_empty() => other.replace('_', " "),
        _ => "unavailable".into(),
    }
}

/// The organization new chats go to: the saved one while the user is still a member, else
/// the default one, else the first, skipping organizations where the user cannot create chats
/// unless that is all of them. The server returns memberships in no stable order, so the first
/// alone is not a choice. The web UI's Agents page picks in the same order among the
/// organizations it permits (`AgentCreateForm.tsx:257-265`). Unlike the web UI, which clears
/// a stored choice that is no longer permitted, a saved organization without permission is
/// only skipped here, so it applies again once the permission returns.
pub fn pick_organization(saved: Option<Uuid>, orgs: &[OrgRef]) -> Option<Uuid> {
    let allowed: Vec<&OrgRef> = orgs.iter().filter(|o| o.can_create_chats).collect();
    let pool = if allowed.is_empty() {
        orgs.iter().collect()
    } else {
        allowed
    };
    saved
        .filter(|id| pool.iter().any(|o| o.id == *id))
        .or_else(|| pool.iter().find(|o| o.is_default).map(|o| o.id))
        .or_else(|| pool.first().map(|o| o.id))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    Info(String),
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Connection {
    #[default]
    Idle,
    Connecting,
    Live,
    Reconnecting {
        attempt: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picker {
    Model,
    Workspace,
    Effort,
    Organization,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModelsState {
    #[default]
    Loading,
    Loaded,
    Failed,
}

/// The model the open chat would send with when the server would refuse it: deleted,
/// disabled, or behind a provider that is off or lacks credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnavailableModel {
    /// The model's name while the model list still names it, as it does a disabled model; a
    /// deleted model has none.
    pub name: Option<String>,
}

impl UnavailableModel {
    /// The sentence that says the chat's model is not available, for notices.
    pub fn sentence(&self) -> String {
        match &self.name {
            Some(name) => format!("This chat's model, {name}, is not available."),
            None => "This chat's model is not available.".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyTarget {
    LastMessage,
    CodeBlock(usize),
}

/// Options that ride along with a new chat or a sent message.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TurnOptions {
    /// The reasoning effort to send, from `App::effort`; `None` only when the model offers none.
    pub effort: Option<String>,
    /// Switches the chat's plan mode with this request: `Some(true)` on, `Some(false)` off,
    /// `None` no change.
    pub plan_mode: Option<bool>,
    /// The plan-mode request generation a message to an existing chat carries with
    /// `plan_mode`, echoed in `Msg::ForPlan`; 0 when it carries no change.
    pub plan_generation: u64,
    /// Uploaded files the message carries.
    pub files: Vec<Uuid>,
    /// The organization MCP servers the chat uses from this message on; `None` leaves them.
    /// On `Effect::CreateChat` it is the first message's selection, sent as
    /// `mcp_server_ids`, and `None` leaves the choice to the server, which turns on only the
    /// required servers.
    pub mcp_servers: Option<Vec<Uuid>>,
}

/// What the agent is doing, for the animated activity line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    /// A message was sent and the agent has not picked it up yet.
    Waiting,
    Thinking,
    /// A tool call is running; holds the tool name, which may still be empty while it streams.
    Tool(String),
    Writing,
    Interrupting,
    /// Running, with nothing streamed for the current step yet.
    Working,
}

#[derive(Debug)]
#[expect(clippy::large_enum_variant)]
pub enum Msg {
    Started {
        org_id: Uuid,
        open_chat: Option<Uuid>,
    },
    /// Sent once by the main loop after startup; starts the fetches that belong to the user
    /// rather than to an organization or a chat.
    SessionStarted,
    UserLoaded(UserRef),
    /// The signed-in user's personal skills, from the experimental API.
    SkillsLoaded(Vec<Skill>),
    /// The personal skills failed to load; the slash menu says so in one line.
    SkillsFailed(String),
    /// Opens an existing chat in place of the open one, from `/chats`, `/subagents`, or
    /// `/parent`.
    OpenChat(Uuid),
    /// The user's organizations, sent once at startup, before `Started`.
    OrganizationsLoaded(Vec<OrgRef>),
    /// Sent at startup instead of `Started` when the organizations could not be loaded or
    /// there are none. A requested chat still loads, and its own organization scopes the lists.
    OrganizationsFailed {
        message: String,
        open_chat: Option<Uuid>,
    },
    /// An organization picked for new chats, from `/organization <name>` or the picker.
    OrganizationChosen(Uuid),
    /// A reply about the model or workspace list of `org`, applied only while those lists still
    /// belong to it.
    ForOrg {
        org: Uuid,
        msg: Box<Msg>,
    },
    /// A reply about `chat`, applied only while it is the open chat, so one already queued
    /// when the user leaves the chat cannot reach the next one. For a chat that is not open,
    /// only failures are reported, worded for the previous chat: `SendFailed`, which also
    /// restores its text unless it was an `/implement`, `ApiFailed`, and `PlanModeFailed`.
    /// Stream events use `ForStream`.
    ForChat {
        chat: Uuid,
        msg: Box<Msg>,
    },
    /// A message from the chat stream opened with `generation`, applied only while that chat
    /// is open and that stream is current. REST replies keep `ForChat`, because a late reply
    /// about the same chat is still true on a later visit.
    ForStream {
        chat: Uuid,
        generation: u64,
        msg: Box<Msg>,
    },
    /// The reply to the plan-mode request `generation` (an `Effect::SetPlanMode`, or an
    /// `Effect::SendMessage` that carried a change), which settles the queue only while it is
    /// the request in flight. A reply from an earlier visit to the chat is reported, as for a
    /// chat left, and the server's value is read back.
    ForPlan {
        chat: Uuid,
        generation: u64,
        msg: Box<Msg>,
    },
    /// Previews `Some(chat)` in place of any other preview, or closes the preview.
    PreviewChat(Option<Uuid>),
    /// A message from the preview stream opened with `generation`, applied only to the
    /// preview of `chat` while that stream is current, and never to the open chat. The
    /// runtime never sends stream messages unwrapped: the main stream's go in `ForStream`
    /// and the preview's in `ForPreview`.
    ForPreview {
        chat: Uuid,
        generation: u64,
        msg: Box<Msg>,
    },
    /// A message from the `/git` local-changes socket opened with `generation`, applied only
    /// while `chat` is open and that socket is current, so one still queued from a closed
    /// panel cannot end or fill the panel that replaced it.
    ForGit {
        chat: Uuid,
        generation: u64,
        msg: Box<Msg>,
    },
    /// The chat stream stayed open long enough to count as healthy; the backoff restarts.
    StreamHealthy,
    /// The runtime sends this when `Effect::LoadChat` loaded the chat and its newest page.
    ChatLoaded {
        chat: Box<types::CodersdkChat>,
        messages: Vec<types::CodersdkChatMessage>,
        /// The server's `has_more` for that page, when it sent one.
        has_more: Option<bool>,
    },
    /// The runtime sends this when `Effect::LoadChat` fails to load the chat or its messages.
    ChatLoadFailed {
        chat_id: Uuid,
        message: String,
    },
    ChatCreated(Box<types::CodersdkChat>),
    ChatsLoaded {
        query: ListQuery,
        offset: i64,
        chats: Vec<types::CodersdkChat>,
    },
    ChatsFailed {
        query: ListQuery,
        message: String,
    },
    /// A message from the watch connection opened with `generation`, applied only while that
    /// connection is current, so one still queued from a replaced connection cannot end,
    /// reset, or update the one that replaced it. The runtime never sends watch messages
    /// unwrapped.
    ForWatch {
        generation: u64,
        msg: Box<Msg>,
    },
    /// The watch socket connected; the first page is refetched, since the socket sends no
    /// snapshot.
    WatchConnected,
    Watch(coder_sdk::WatchEvent),
    WatchEnded {
        error: Option<String>,
    },
    /// The watch socket stayed open long enough to count as healthy; its backoff restarts.
    WatchHealthy,
    /// A fresh copy of the open chat, after `Effect::RefreshChat`.
    ChatRefreshed(Box<types::CodersdkChat>),
    /// The reply to the `Effect::RefreshChat` numbered `generation`: `Msg::ChatRefreshed`, or
    /// `Msg::ApiFailed` with `REFRESH_CHAT`. The runtime sends it inside `Msg::ForChat`.
    ForRefresh {
        generation: u64,
        msg: Box<Msg>,
    },
    /// Loads the first page of `query`, or with `more` the next one.
    LoadChats {
        query: ListQuery,
        more: bool,
    },
    /// Runs the server's full-text search from the /chats search row.
    SearchChats(String),
    ChatAction(ChatAction),
    QueueAction(QueueAction),
    /// A key for the one-line editor.
    Edit(Edit),
    /// A key for the plan-mode question menu.
    QuestionKey(QuestionKey),
    ChatUpdated {
        chat: Uuid,
        change: ChatChange,
    },
    ChatUpdateFailed {
        chat: Uuid,
        change: ChatChange,
        message: String,
    },
    /// The reply to `Effect::ArchiveAndDeleteWorkspace` once the archive succeeded, with what
    /// became of the workspace.
    ArchivedWithWorkspace {
        chat: Uuid,
        outcome: WorkspaceDeletion,
    },
    /// The server's proposed title for the open chat, not yet saved. `generation` is the
    /// `/title` that asked for it; a reply whose generation is no longer current (superseded
    /// by a later `/title`) is dropped.
    TitleProposed {
        title: String,
        generation: u64,
    },
    TitleProposeFailed {
        message: String,
        generation: u64,
    },
    /// The runtime sends this when `Effect::CreateChat` fails.
    CreateFailed {
        message: String,
        /// The `seq` of the `Effect::CreateChat` that failed.
        seq: u64,
    },
    /// The runtime sends this when `Effect::SendMessage` fails, with the text it tried to send
    /// and the plan mode change it carried, which a refetch of the chat then confirms.
    SendFailed {
        text: String,
        message: String,
        plan_mode: Option<bool>,
        /// The `seq` of the `Effect::SendMessage` that failed.
        seq: u64,
        /// Set when the send carried an MCP selection and the server refused the request as
        /// invalid (HTTP 400), as it does for a server disabled since /mcp loaded.
        mcp_rejected: bool,
    },
    /// The runtime sends this in place of `Msg::SendFailed` when the server refused the
    /// `Effect::SendMessage` numbered `seq` for its model (HTTP 400 naming
    /// `model_config_id`), with the uploaded files it carried, so the core can hold the text
    /// and those files while the user picks another model.
    ModelUnavailable {
        text: String,
        files: Vec<Uuid>,
        message: String,
        plan_mode: Option<bool>,
        seq: u64,
    },
    /// The `/model` table closed without a pick, which gives a held draft back.
    ModelPickerClosed,
    /// The UI did not open the `/model` table for a held draft, because another overlay is
    /// open or the user is typing. The draft stays held until `/model` picks a model.
    ModelPickDeferred,
    /// The runtime sends this when the `Effect::SendMessage` numbered `seq` was accepted,
    /// wrapping what the success means beyond that.
    Sent {
        seq: u64,
        then: Box<Msg>,
    },
    Stream(StreamEvent),
    StreamEnded {
        error: Option<String>,
    },
    PrefsLoaded(DisplayPrefs),
    ModelsLoaded(Vec<types::CodersdkChatModel>),
    /// The whole model list response: models, providers, and unsupported providers.
    CatalogLoaded(Box<types::CodersdkOrganizationChatModelsResponse>),
    /// The runtime sends this when `Effect::FetchModels` fails.
    ModelsFailed {
        message: String,
    },
    WorkspacesLoaded(Vec<WorkspaceRef>),
    /// The runtime sends this when `Effect::FetchWorkspaces` fails.
    WorkspacesFailed {
        message: String,
    },
    /// The composer began completing `command`'s argument, by the command's name. A list it
    /// completes from that failed to load is fetched again.
    ArgumentsWanted {
        command: &'static str,
    },
    ModelChosen(Uuid),
    /// Left or Right on a `/model` row: moves `model`'s compaction threshold one step, as an
    /// edit that is sent once the row is left or the table closes.
    ThresholdStep {
        model: Uuid,
        up: bool,
    },
    /// Delete on a `/model` row: goes back to `model`'s default threshold, sent as a step is.
    ThresholdReset {
        model: Uuid,
    },
    /// The `/model` selection moved to another row, which sends the edit on the row it left.
    ThresholdCommit,
    /// The runtime's reply to `Effect::FetchThresholds`: each override as a model config id
    /// and a percent.
    ThresholdsLoaded {
        thresholds: Vec<(Uuid, i64)>,
        generation: u64,
    },
    ThresholdsFailed {
        message: String,
        generation: u64,
    },
    /// The runtime's reply to `Effect::SaveThreshold`: the override the server now holds, or
    /// `None` after a reset.
    ThresholdSaved {
        model: Uuid,
        percent: Option<i64>,
        generation: u64,
    },
    ThresholdFailed {
        model: Uuid,
        message: String,
        generation: u64,
    },
    /// A reasoning effort picked by name, from `/effort <level>` or the effort picker.
    EffortChosen(String),
    WorkspaceChosen(Option<Uuid>),
    ApiFailed {
        action: &'static str,
        message: String,
    },
    /// The runtime sends this when `Effect::SetPlanMode` fails, with the state it asked for.
    PlanModeFailed {
        on: bool,
        message: String,
    },
    /// The runtime sends this when `Effect::SetPlanMode`, or an `Effect::SendMessage` that
    /// carried a plan mode change, succeeds.
    PlanModeApplied {
        on: bool,
    },
    /// The runtime sends this after `Effect::OpenWeb`, with the chat's URL and why no browser
    /// opened, if none did.
    WebOpened {
        url: String,
        outcome: Result<(), String>,
    },
    /// The runtime sends this after `Effect::OpenLink`, with the link and why no browser opened,
    /// if none did.
    LinkOpened {
        url: String,
        outcome: Result<(), String>,
    },
    FileUploaded {
        local: u64,
        file_id: Uuid,
        size: u64,
    },
    UploadFailed {
        local: u64,
        message: String,
    },
    /// Backspace on an empty composer.
    RemoveLastChip,
    /// A file an `@path` mention in the draft names. It shows as a chip, but uploads only
    /// when the message is sent.
    AttachMention(String),
    /// A large paste the composer showed as a token, sent with the message as the text file
    /// `name`. It shows as a chip and uploads with the send, as an `@path` mention does.
    AttachPaste {
        name: String,
        text: String,
    },
    /// The runtime sends these after `Effect::FetchCost`, wrapped in `ForChat`, with the
    /// request's `generation`.
    CostLoaded {
        cost: types::CodersdkChatCost,
        generation: u64,
    },
    /// The server refused the cost (`403` or `404`), so `/info`, `/usage`, and the footer's
    /// `cost` field hide it.
    CostHidden {
        generation: u64,
    },
    /// The server rejected the session token (`401`) on the cost, which stops the cost, spend,
    /// and quota refreshes as a `401` on either limit does.
    CostUnauthorized {
        generation: u64,
    },
    CostFailed {
        message: String,
        generation: u64,
    },
    /// The `/info` panel closed.
    InfoClosed,
    /// Asks for the AI spend and the workspace quota again: from the UI's minute timer, after
    /// a turn ends, and when the organization in view changes.
    RefreshLimits,
    /// Asks for the open chat's cost, while the footer lists `cost` or `/usage` is open.
    RefreshCost,
    /// The runtime's reply to `Effect::FetchSpend`, with the refresh's `generation`.
    SpendLoaded {
        spend: Box<types::CodersdkUserAiSpendStatus>,
        generation: u64,
    },
    /// The runtime's reply to `Effect::FetchQuota` for `org`, with the refresh's `generation`.
    QuotaLoaded {
        org: Uuid,
        quota: types::CodersdkWorkspaceQuota,
        generation: u64,
    },
    /// A spend or quota request failed, with why.
    LimitFailed {
        limit: Limit,
        refusal: Refusal,
        generation: u64,
    },
    /// The `/usage` panel closed.
    UsageClosed,
    /// The runtime sends these after `Effect::FetchWorkspaceDetails`, wrapped in `ForChat`. A
    /// reply about a workspace other than the attached one is dropped.
    WorkspaceDetailsLoaded(Box<types::CodersdkWorkspace>),
    WorkspaceDetailsFailed {
        workspace: Uuid,
        message: String,
    },
    /// The deployment's SSH hostname suffix, or `None` when it has none or the request failed.
    SshSuffixLoaded(Option<String>),
    /// The SSH hostname suffix failed to load, so the next `/workspace` asks again.
    SshSuffixFailed,
    /// An action chosen in the `/workspace` panel, which closes it.
    WorkspaceAction(WorkspaceAction),
    /// The `/workspace` panel closed.
    WorkspaceClosed,
    /// The runtime sends these after `Effect::FetchDiff`, wrapped in `ForChat`, with the
    /// request's `generation`.
    DiffLoaded {
        diff: Box<types::CodersdkChatDiffContents>,
        generation: u64,
    },
    DiffFailed {
        message: String,
        generation: u64,
    },
    /// The view reached the top of the transcript; loads the page before it when the server
    /// has more and no page is on its way.
    LoadOlder,
    /// The wheel reached the top of the transcript. Loads the page before it like
    /// `Msg::LoadOlder`, except after a failed page, which waits for a deliberate `LoadOlder`.
    ScrolledToTop,
    /// The user scrolled the transcript: PageUp, PageDown, End, or the wheel. It ends a jump
    /// still looking for a file's message, so the jump never fights the user's own scrolling.
    UserScrolled,
    /// The runtime sends these after `Effect::LoadOlder`, wrapped in `ForChat`, with the
    /// request's `generation`.
    OlderLoaded {
        messages: Vec<types::CodersdkChatMessage>,
        has_more: bool,
        generation: u64,
    },
    OlderFailed {
        message: String,
        generation: u64,
    },
    /// The runtime sends these from the `/git` socket, wrapped in `ForGit`.
    GitChanges(Box<types::CodersdkWorkspaceAgentGitServerMessage>),
    /// The `/git` socket ended or was refused, with why; it is not reopened until `/git` is.
    GitWatchEnded(String),
    /// The runtime reopened the `/git` socket after a terminal handoff, ahead of its first
    /// frame. That frame lists every repository the agent knows, but none it lost during the
    /// handoff, so the panel's rows are cleared first.
    GitReopened,
    /// An action chosen in the `/git` panel.
    GitAction(GitAction),
    /// The `/git` panel closed.
    GitClosed,
    /// The runtime sends these after `Effect::FetchMcpServers` and `Effect::FetchMcpHealth`,
    /// wrapped in `ForChat`, with the `/mcp` request's `generation`.
    McpServersLoaded {
        servers: Vec<types::CodersdkMcpServerConfig>,
        generation: u64,
    },
    McpServersFailed {
        message: String,
        generation: u64,
    },
    /// The newest debug run's connect outcomes, or `None` when the server reports none,
    /// including when debug logging is off or the debug runs are not visible.
    McpHealthLoaded {
        outcomes: Option<Vec<coder_sdk::McpConnectOutcome>>,
        generation: u64,
    },
    McpHealthFailed {
        message: String,
        generation: u64,
    },
    /// The `/mcp` panel closed.
    McpClosed,
    /// Turns the organization MCP server on or off for the next message.
    ToggleMcp(Uuid),
    /// The MCP servers of the organization the lists belong to, answering
    /// `Effect::FetchOrgMcpServers` in `Msg::ForOrg`.
    OrgMcpLoaded(Vec<types::CodersdkMcpServerConfig>),
    OrgMcpFailed(String),
    /// A `/files` key, or a click on an attached file's line.
    FileAction(FileAction),
    /// The answer to the question a save asks when its name is taken.
    ConflictAnswer(ConflictChoice),
    /// The runtime saved `file` at `path`.
    FileSaved {
        file: Uuid,
        path: PathBuf,
    },
    /// A save of `file` found `path` taken and wrote nothing.
    FileConflict {
        file: Uuid,
        name: String,
        path: PathBuf,
    },
    /// The text of `file`, for the pager.
    FileText {
        file: Uuid,
        text: String,
    },
    /// A save or view of `file` failed, with a sentence that says why.
    FileFailed {
        file: Uuid,
        message: String,
    },
    Submit(String),
    Command(Command),
    Interrupt,
    /// A background action finished with nothing to update; the next stream event carries the result.
    Refresh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    LoadChat(Uuid),
    /// Loads the page of `chat`'s messages before `before_id`, answered in `Msg::ForChat`
    /// with `Msg::OlderLoaded` or `Msg::OlderFailed` carrying `generation`.
    LoadOlder {
        chat: Uuid,
        before_id: i64,
        generation: u64,
    },
    FetchChats {
        query: ListQuery,
        offset: i64,
    },
    /// Opens the watch socket after `delay`, replacing any open one. Its messages come back
    /// in `Msg::ForWatch` with `generation`.
    OpenWatch {
        delay: Duration,
        generation: u64,
    },
    /// Refetches the open chat's record without touching its transcript. The reply comes
    /// back in `Msg::ForRefresh` with `generation`.
    RefreshChat {
        chat: Uuid,
        generation: u64,
    },
    OpenStream {
        chat: Uuid,
        after_id: Option<i64>,
        generation: u64,
    },
    ReconnectAfter {
        chat: Uuid,
        after_id: Option<i64>,
        delay: Duration,
        generation: u64,
    },
    /// Stops the chat stream without touching the chat on the server.
    CloseStream,
    /// Opens the preview stream on `chat` after `delay`, replacing any open one. Its messages
    /// come back in `Msg::ForPreview` with `generation`.
    OpenPreview {
        chat: Uuid,
        after_id: Option<i64>,
        delay: Duration,
        generation: u64,
    },
    /// Stops the preview stream.
    ClosePreview,
    CreateChat {
        org: Uuid,
        text: String,
        model: Option<Uuid>,
        workspace: Option<Uuid>,
        turn: TurnOptions,
        /// Echoed in `Msg::CreateFailed`, so a failure names this request's files.
        seq: u64,
    },
    SendMessage {
        chat: Uuid,
        text: String,
        model: Option<Uuid>,
        busy: BusyBehavior,
        turn: TurnOptions,
        /// Echoed in `Msg::Sent` or `Msg::SendFailed`, so the reply settles this send.
        seq: u64,
    },
    Interrupt(Uuid),
    Compact(Uuid),
    Clear(Uuid),
    SetWorkspace {
        chat: Uuid,
        workspace: Option<Uuid>,
    },
    SetPlanMode {
        chat: Uuid,
        on: bool,
        /// Echoed in the `Msg::ForPlan` reply.
        generation: u64,
    },
    UpdateChat {
        chat: Uuid,
        change: ChatChange,
    },
    /// Archives `chat`, then deletes `workspace` with a delete build, in that order as the web
    /// UI does: the server refuses the archive while the chat's family runs, so nothing is
    /// deleted then. Answered with `Msg::ChatUpdateFailed` when the archive fails, else with
    /// `Msg::ArchivedWithWorkspace`.
    ArchiveAndDeleteWorkspace {
        chat: Uuid,
        workspace: Uuid,
    },
    /// Asks the server to generate a title for `chat` without saving it. The reply comes back
    /// wrapped in `Msg::ForChat`, tagged with the `/title` generation that asked for it.
    ProposeTitle {
        chat: Uuid,
        generation: u64,
    },
    /// Promotes a queued message: "Send now", which interrupts a running turn.
    PromoteQueued {
        chat: Uuid,
        id: i64,
    },
    DeleteQueued {
        chat: Uuid,
        id: i64,
    },
    OpenWeb(Uuid),
    /// Copies the chat's URL after `Effect::OpenWeb` opened no browser, and reports the copy.
    CopyWebUrl(String),
    /// Opens a link from the transcript in the browser. The UI sends it on a click.
    OpenLink(String),
    /// Copies a link after `Effect::OpenLink` opened no browser, and reports the copy.
    CopyLink(String),
    FetchPrefs,
    FetchMe,
    /// Loads the signed-in user's personal skills.
    FetchSkills,
    /// Retries the organization lookup that failed at startup.
    FetchOrganizations,
    FetchModels(Uuid),
    FetchWorkspaces(Uuid),
    ShowPicker(Picker),
    ShowHelp,
    /// Opens the `/info` panel.
    ShowInfo,
    /// Opens the `/workspace` details panel.
    ShowWorkspace,
    /// Fetches the attached workspace's details, answered in `Msg::ForChat`.
    FetchWorkspaceDetails {
        chat: Uuid,
        workspace: Uuid,
    },
    /// Fetches the deployment's SSH hostname suffix, answered by `Msg::SshSuffixLoaded`, or by
    /// `Msg::SshSuffixFailed` when the request fails.
    FetchSshSuffix,
    /// Opens the `/git` panel.
    ShowGit,
    /// Opens the `/mcp` panel.
    ShowMcp,
    /// Opens the `/statusline` editor, which the UI owns, since the settings are local.
    ShowStatusline,
    /// Opens the `/usage` panel.
    ShowUsage,
    /// Lists `org`'s MCP server configs, answered in `Msg::ForChat` with `generation`.
    FetchMcpServers {
        chat: Uuid,
        org: Uuid,
        generation: u64,
    },
    /// Reads the chat's newest MCP connect outcomes while debug logging is on, answered in
    /// `Msg::ForChat` with `generation`.
    FetchMcpHealth {
        chat: Uuid,
        generation: u64,
    },
    /// Lists the organization's MCP server configs for new chats, answered in `Msg::ForOrg`.
    FetchOrgMcpServers(Uuid),
    /// Fetches the chat's diff contents, answered in `Msg::ForChat`.
    /// Its reply carries `generation`, so only the latest request's reply applies.
    FetchDiff {
        chat: Uuid,
        generation: u64,
    },
    /// Shows text in the user's pager, which takes over the terminal until it exits.
    Page(String),
    /// Opens the workspace's local-changes socket for `chat`, replacing any open one. Its
    /// messages come back in `Msg::ForGit` with `generation`.
    OpenGitWatch {
        chat: Uuid,
        generation: u64,
    },
    /// Closes the local-changes socket.
    CloseGitWatch,
    /// Copies `text` and says it copied `what`.
    CopyText {
        text: String,
        what: &'static str,
    },
    /// Opens `<deployment>/@<owner>/<workspace>`, which the runtime builds from its base URL.
    OpenWorkspaceWeb {
        owner: String,
        workspace: String,
    },
    /// Fetches the cost of the chat's whole tree, answered in `Msg::ForChat`.
    /// Its reply carries `generation`, so only the latest request's reply applies.
    FetchCost {
        chat: Uuid,
        generation: u64,
    },
    /// Fetches the signed-in user's AI spend, answered by `Msg::SpendLoaded` or
    /// `Msg::LimitFailed` with `generation`.
    FetchSpend {
        generation: u64,
    },
    /// Fetches the signed-in user's workspace quota in `org`, answered by `Msg::QuotaLoaded`
    /// or `Msg::LimitFailed` with `generation`.
    FetchQuota {
        org: Uuid,
        generation: u64,
    },
    /// Reads the user's compaction threshold overrides, answered by `Msg::ThresholdsLoaded` or
    /// `Msg::ThresholdsFailed` with `generation`.
    FetchThresholds {
        generation: u64,
    },
    /// Stores or removes one model's compaction threshold override, answered by
    /// `Msg::ThresholdSaved` or `Msg::ThresholdFailed` with the save's generation.
    SaveThreshold(compaction::Save),
    /// Opens /chats with its filter already typed.
    ShowChats(String),
    /// Opens the `/subagents` popup.
    ShowSubagents,
    /// Opens the `/queue` overlay.
    ShowQueue,
    Copy(CopyTarget),
    SetMouse(bool),
    /// Opens the local config file in `$EDITOR` and applies it when the editor exits. The main
    /// loop runs it, since the editor takes over the terminal.
    EditSettings,
    /// Saves the organization new chats go to in the local config.
    SaveOrganization(Uuid),
    /// Saves the reasoning effort chosen for `model` for a blank chat's new-chat form.
    SaveEffort {
        model: Uuid,
        effort: String,
    },
    /// Puts text back in the composer: after a failed chat creation or send, a send refused
    /// for an archived chat, a failed attachment, a held `/implement`, or a failed upload, and
    /// text that waited on a chat the user left, or a draft held for a model pick and given back.
    RestoreComposer(String),
    /// Uploads the file at `path` to `org`, answered by `Msg::FileUploaded` or
    /// `Msg::UploadFailed` for the chip numbered `local`.
    UploadFile {
        local: u64,
        path: String,
        org: Uuid,
    },
    /// Uploads `text` to `org` as the file `name`, answered as `Effect::UploadFile` is.
    UploadText {
        local: u64,
        name: String,
        text: String,
        org: Uuid,
    },
    /// Stops the upload for the chip numbered `local`, whose chip was removed or cleared.
    CancelUpload(u64),
    /// Clears what the UI keeps about the chat on screen: expanded blocks, the selection,
    /// the scroll position, and the composer's place in its history.
    ClearView,
    /// Opens the `/files` overlay.
    ShowFiles,
    /// Scrolls the transcript so message `id` is at the top, once it is drawn.
    ScrollToMessage(i64),
    /// Scrolls the transcript to its end, where the live turn is.
    ScrollToLatest,
    /// Downloads `file` and writes it to `to` under `name`, a `files::safe_name`, answered by
    /// `Msg::FileSaved`, `Msg::FileConflict`, or `Msg::FileFailed`.
    SaveFile {
        file: Uuid,
        name: String,
        to: SaveTo,
        conflict: OnConflict,
    },
    /// Downloads text file `file` for the pager, answered by `Msg::FileText` or
    /// `Msg::FileFailed`.
    ReadFile {
        file: Uuid,
        name: String,
    },
    Quit,
}

impl Effect {
    /// Whether the main loop runs this effect itself, rather than the UI or the runtime:
    /// `Quit` ends the loop, and `Page` and `EditSettings` need the terminal, which only the
    /// loop owns.
    pub fn runs_in_main(&self) -> bool {
        matches!(self, Effect::Quit | Effect::Page(_) | Effect::EditSettings)
    }
}

/// Messages loaded per history page, the most the server returns at once.
pub const HISTORY_PAGE: i64 = 200;

/// What a save says while the "name is taken" question is open.
const OPEN_SAVE_QUESTION: &str = "Answer the open save question first.";

/// What `g` in `/files` says when the whole history holds no message for the file.
const NOT_IN_HISTORY: &str = "Its message is not in this chat's history any more.";

/// What the top of a chat's transcript says about the history above it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryEdge {
    /// The server may hold older messages, which PageUp at the top loads.
    More,
    /// A page of older messages is on its way.
    Loading,
    /// Older pages were loaded until none were left.
    Start,
}

/// Reconnect delay: 500 ms doubling per attempt, capped at 10 s. The runtime adds jitter.
pub fn backoff(attempt: u32) -> Duration {
    let exp = attempt.saturating_sub(1).min(10);
    Duration::from_millis((500u64 << exp).min(10_000))
}

/// How long to wait before reconnect `attempt` of a chat stream. A gap in the stream's
/// sequence reconnects at once on its first attempt, since the socket itself was fine; a
/// closed stream, and every later attempt, waits out the backoff.
fn reconnect_delay(attempt: u32, gap: bool) -> Duration {
    if gap && attempt == 1 {
        Duration::ZERO
    } else {
        backoff(attempt)
    }
}

/// Whether the server may hold messages older than a chat's first page of `loaded` messages:
/// its own `has_more` when the load reply carries it, else whether the page came back full.
fn first_page_more(loaded: usize, has_more: Option<bool>) -> bool {
    has_more.unwrap_or(loaded as i64 >= HISTORY_PAGE)
}

/// The number of the send a `Msg::Sent` settles, looking inside the chat-routing wrappers.
fn sent_seq(msg: &Msg) -> Option<u64> {
    match msg {
        Msg::Sent { seq, .. } => Some(*seq),
        Msg::ForChat { msg, .. } | Msg::ForPlan { msg, .. } => sent_seq(msg),
        _ => None,
    }
}

/// The notice that warns, before anything is typed, that the chat's model is unavailable.
fn unavailable_warning(gone: &UnavailableModel) -> String {
    format!(
        "{} Pick another with /model before you send.",
        gone.sentence()
    )
}

/// `notice`, followed by the names of the files a failed message carried, if any.
fn lost_files_notice(notice: String, names: &[String]) -> String {
    if names.is_empty() {
        return notice;
    }
    format!(
        "{}. These files were not sent: {}. Attach them again.",
        notice.trim_end_matches('.'),
        names.join(", ")
    )
}

/// The `Msg::ApiFailed` action the runtime reports when `Effect::RefreshChat` fails.
pub const REFRESH_CHAT: &str = "refresh the chat";

/// The `Msg::ApiFailed` action the runtime reports when `Effect::PromoteQueued` fails.
pub const PROMOTE_QUEUED: &str = "run the queued message next";

/// The notice a rejected session token gives once, when it stops the spend, quota, and chat
/// cost refreshes.
pub const LIMITS_STOPPED: &str = "Spend, quota, and chat cost stopped updating: the session token was rejected. Run `coder login`, then restart scuttle.";

/// What `App::quota` reads as while the quota on hand belongs to another organization.
static NO_QUOTA: LimitState<types::CodersdkWorkspaceQuota> = LimitState::Unknown;

/// Whether the server reports plan mode on for `chat`.
fn is_plan(chat: &types::CodersdkChat) -> bool {
    chat.plan_mode.as_ref().is_some_and(|p| p.0 == "plan")
}

/// The state of the personal skills request.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SkillsLoad {
    #[default]
    Loading,
    Loaded(Vec<Skill>),
    Failed(String),
}

#[derive(Debug, Default)]
pub struct App {
    pub org_id: Option<Uuid>,
    pub chat_id: Option<Uuid>,
    pub chat: Option<Box<types::CodersdkChat>>,
    pub transcript: Transcript,
    pub chats: ChatList,
    pub prefs: DisplayPrefs,
    pub models: Vec<types::CodersdkChatModel>,
    pub providers: Vec<types::CodersdkChatModelProviderDescriptor>,
    pub unsupported_providers: Vec<types::CodersdkChatUnsupportedProvider>,
    pub workspaces: Vec<WorkspaceRef>,
    pub workspaces_state: WorkspacesState,
    pub organizations: Vec<OrgRef>,
    /// The signed-in user, once loaded.
    pub me: Option<UserRef>,
    /// The signed-in user's personal skills, listed in the slash menu.
    pub personal_skills: SkillsLoad,
    /// The organization saved in the local config, used when a retried lookup picks one.
    pub saved_org: Option<Uuid>,
    /// The reasoning effort saved in the local config for each model, by model config ID.
    /// Used by `effort()` on a blank chat, for the model it currently names.
    pub saved_efforts: BTreeMap<Uuid, String>,
    /// Set while a retried organization lookup is in flight.
    orgs_retry: bool,
    /// The organization `models` and `workspaces` were requested for.
    lists_org: Option<Uuid>,
    pub selected_model: Option<Uuid>,
    pub selected_workspace: Option<Uuid>,
    /// The reasoning effort chosen with `/effort`. It wins over the open chat's last effort
    /// while the current model offers it.
    pub selected_effort: Option<String>,
    /// Whether plan mode is on for the chat, or for the chat the next message creates.
    pub plan_mode: bool,
    /// The plan mode state of the change in flight, if any: an `Effect::SetPlanMode`, or an
    /// `Effect::SendMessage` that carries one. Only one is in flight at a time, because the
    /// server publishes no event for a plan mode change and two could land in either order.
    plan_request: Option<bool>,
    /// Bumped by every plan-mode request, so a reply from an earlier one is told apart.
    plan_generation: u64,
    /// The latest state asked for while a change was in flight, sent after it.
    plan_wanted: Option<bool>,
    /// Set by `/implement` while a change is in flight; its message goes once that settles.
    implement_held: bool,
    /// Set when a plan mode change failed, so the refetch that follows says which state holds.
    plan_failed: bool,
    /// Set when a change from an earlier visit settled while the chat was loading, so the
    /// loaded chat is read back once more.
    plan_refresh_on_load: bool,
    /// The one-line editor's state: the `/chats` rename, the `/title` proposal, or the "Other"
    /// answer to a plan-mode question.
    pub editor: Option<Editor>,
    /// Progress through the plan-mode questions the agent is waiting on.
    answering: Option<Answering>,
    pub notices: Vec<Notice>,
    pub connection: Connection,
    pub busy: BusyBehavior,
    pub mouse: bool,
    pub models_state: ModelsState,
    /// The user's compaction threshold for each model, as `/model` shows and edits it.
    pub compaction: compaction::Thresholds,
    /// The error from the most recent `Msg::StreamEnded`, cleared once the stream is live again.
    pub last_stream_error: Option<String>,
    /// The text of the message sent with the in-flight `Effect::CreateChat`, if any.
    creating: Option<String>,
    /// The existing chat whose `Effect::LoadChat` is in flight, if any.
    loading: Option<Uuid>,
    /// The existing chat that was requested but failed to load. While set, a submit retries the
    /// load instead of creating a new chat.
    failed_load: Option<Uuid>,
    /// Text submitted while a chat is being created or loaded, sent once it exists.
    pending_text: Option<String>,
    /// The attachments above the composer, which the next message carries.
    pub chips: Vec<Chip>,
    next_chip: u64,
    /// Text submitted while an attachment was still uploading, sent once all are ready.
    waiting_send: Option<String>,
    /// The names of the models the list holds but disables, by id, so a chat that used one
    /// can still name it.
    disabled_models: BTreeMap<Uuid, String>,
    /// The chat and model the unavailable-model notice was last given for, so it is given
    /// once per pair.
    model_warned: Option<(Uuid, Uuid)>,
    /// Text whose send waits on a model pick, because the chat's model is unavailable. A pick
    /// sends it, and closing the picker gives it back.
    model_held: Option<String>,
    /// The index in `notices` of the notice that offered the `/model` table for the held
    /// draft, so `Msg::ModelPickDeferred` can say instead how to pick later.
    hold_notice: Option<usize>,
    /// The number the next `Effect::SendMessage` or `Effect::CreateChat` takes.
    next_send: u64,
    /// The names of the files each message in flight carried, by its number, so a failed send
    /// can name them. An entry leaves once its send succeeds or fails.
    sent_files: BTreeMap<u64, Vec<String>>,
    /// The name and text of each paste a message in flight carried, by its number, so a failed
    /// send can put them back. An entry leaves with the send's `sent_files` entry.
    sent_pastes: BTreeMap<u64, Vec<(String, String)>>,
    /// The number of the in-flight `Effect::CreateChat`, whose success is `Msg::ChatCreated`.
    creating_seq: Option<u64>,
    /// Set by a submit that sends or queues a message, by "Send now", and by `/implement`,
    /// held or not. Cleared once the chat reports a status other than `waiting`, an error, a
    /// failed send, or a failed "Send now".
    awaiting_reply: bool,
    /// The id of the echoed user message the wait is for, once the stream has sent it; an
    /// assistant message with a higher id is the reply.
    sent_id: Option<i64>,
    reconnect_attempt: u32,
    /// Bumped by every `OpenStream`, `ReconnectAfter`, and `CloseStream`.
    stream_generation: u64,
    /// The subagent shown in the `/subagents` popup, streamed apart from the open chat.
    pub preview: Option<Preview>,
    /// The cost row of `/info`, set while the panel is open.
    pub info_panel: Option<CostState>,
    /// The attached workspace's details while `/workspace` shows them.
    pub workspace_panel: Option<Fetched<Box<types::CodersdkWorkspace>>>,
    /// The `/git` panel while it is open.
    pub git_panel: Option<GitPanel>,
    /// Bumped by every `Effect::OpenGitWatch`.
    git_generation: u64,
    /// The `/mcp` panel while it is open.
    pub mcp_panel: Option<McpPanel>,
    /// Bumped by every `/mcp`, so a reply to an earlier open of the panel is dropped.
    mcp_generation: u64,
    /// The MCP selection /mcp changed, sent with the next message.
    pub mcp_next: Option<Vec<Uuid>>,
    /// The MCP selection each send in flight carried, by its number. The selection applies
    /// once its send succeeds, and an entry leaves once its send settles.
    mcp_sends: BTreeMap<u64, Vec<Uuid>>,
    /// The number of the newest send that carried an MCP selection, the only one whose
    /// acceptance updates the chat's selection locally.
    mcp_newest: u64,
    /// The MCP servers of the organization the lists belong to, which a blank chat's `/mcp`
    /// lists and its first message selects from.
    pub org_mcp: Fetched<Vec<types::CodersdkMcpServerConfig>>,
    /// Whether the in-flight `Effect::CreateChat` carried an MCP selection, so a refusal
    /// fetches the list again in case a server was disabled since it loaded.
    creating_mcp: bool,
    /// Bumped by every `Effect::FetchDiff`, so a reply to an earlier request is dropped.
    diff_generation: u64,
    /// Whether the server may hold messages older than the loaded ones.
    history_more: bool,
    /// Whether an `Effect::LoadOlder` is on its way; only one page loads at a time.
    history_loading: bool,
    /// Whether the open chat's history was ever longer than its first page.
    history_paged: bool,
    /// Bumped by every `Effect::LoadOlder`, every load of a chat, and every history reset, so
    /// a page asked for on an earlier visit or before the history was replaced is dropped.
    history_generation: u64,
    /// Set by `/diff`, so the next current diff reply goes to the pager.
    page_when_loaded: bool,
    /// The deployment's SSH hostname suffix: `None` until loaded, `Some(None)` when it has none.
    pub ssh_suffix: Option<Option<String>>,
    /// Bumped by every `Effect::FetchCost`, so a reply to an earlier request is dropped.
    cost_generation: u64,
    /// The open chat's cost for the footer and `/usage`, fetched while either shows it.
    pub chat_cost: Option<CostState>,
    /// Whether the footer lists the `cost` field. The UI sets it from `statusline.fields`.
    pub cost_in_footer: bool,
    /// Set while `/usage` is open.
    pub usage_open: bool,
    /// The user's AI spend this period.
    spend: LimitState<Box<types::CodersdkUserAiSpendStatus>>,
    /// The user's workspace quota in `quota_org`.
    quota: LimitState<types::CodersdkWorkspaceQuota>,
    /// The organization `quota` was asked for.
    quota_org: Option<Uuid>,
    /// Bumped by every refresh of the limits, so a reply to an earlier one is dropped.
    limits_generation: u64,
    /// Set by a `401` on a limit request; the limits refresh no more until restart.
    limits_stopped: bool,
    /// The open chat's turns that ended: each time its status left running.
    turns_ended: u64,
    /// The parent `/parent` or Esc is opening, with its title, so a failed load names it.
    returning_to: Option<(Uuid, Option<String>)>,
    /// Bumped by every `OpenPreview` and `ClosePreview`.
    preview_generation: u64,
    /// Watch connections that ended without staying open long enough to count as healthy.
    watch_attempt: u32,
    /// Bumped by every `OpenWatch`.
    watch_generation: u64,
    /// Set when the watch connected while a load of the main list was in flight; the first
    /// page is refetched once that load lands, since it may predate the outage's events.
    refetch_after_load: bool,
    /// Bumped by every `/title` with no text, so a stale proposal (one superseded by a later
    /// `/title`) is dropped even when it is still about the open chat.
    title_generation: u64,
    /// Bumped by every `Effect::RefreshChat`.
    refresh_generation: u64,
    /// The newest refresh whose snapshot applied, or the last refresh issued before the open
    /// chat loaded. A reply to it or an earlier one is older than what the chat shows.
    refresh_applied: u64,
    /// The last refresh issued before plan mode or the MCP selection last changed here. A
    /// snapshot from it or an earlier one was read before that change, so it keeps the plan
    /// mode, the MCP selection, and any failure the next refresh reports.
    refresh_floor: u64,
    /// The workspace set for the open chat here, until the server reports it, so a snapshot
    /// read before the change lands does not undo it.
    workspace_pending: Option<Option<Uuid>>,
    /// The chats with an archive request in flight, so a second one waits for the reply.
    archiving: BTreeSet<Uuid>,
    /// The numbers of the `/implement` sends in flight, whose text the composer never held.
    implement_seqs: std::collections::BTreeSet<u64>,
    /// Set when an older page failed to load. The wheel then waits for a deliberate
    /// `Msg::LoadOlder`, so it never retries on every tick.
    history_failed: bool,
    /// Where Enter in `/files` and a click on an attached file save, from `files.save_dir`.
    /// The UI sets it.
    pub save_dir: PathBuf,
    /// The home directory, for `~` in a typed save path and in the paths notices show. The UI
    /// sets it.
    pub home: Option<PathBuf>,
    /// Whether scuttle runs over SSH, so a saved file lands on that machine, as the saved
    /// notice says. The UI sets it.
    pub over_ssh: bool,
    /// The files with a save or a view in flight, so a second press waits for the first.
    pub files_busy: BTreeSet<Uuid>,
    /// The question a save asks when its name is taken.
    pub save_conflict: Option<SaveConflict>,
    /// The chat and the file whose message older pages are loading to find.
    jump: Option<(Uuid, Uuid)>,
    /// The chat each view in flight was asked in, so a reply after a chat switch is dropped.
    viewing: BTreeMap<Uuid, Uuid>,
}

impl App {
    pub fn new(busy: BusyBehavior, mouse: bool) -> App {
        App {
            busy,
            mouse,
            ..App::default()
        }
    }

    /// The open chat's subagents, or for a subagent its siblings, with the parent's title when
    /// the open chat is a subagent. The watch keeps the list cache current, so it wins over
    /// the open chat's own copy.
    pub fn subagents(&self) -> (Option<String>, Vec<types::CodersdkChat>) {
        let Some(open) = self.chat.as_deref() else {
            return (None, vec![]);
        };
        match open.parent_chat_id {
            Some(parent) => {
                let root = self.chats.find(parent);
                let title = root
                    .and_then(|r| r.title.clone())
                    .unwrap_or_else(|| "the parent chat".into());
                // Without the parent in the list, the siblings are unknown, but the open
                // subagent itself is still one of them.
                let children = match root {
                    Some(r) => r.children.clone(),
                    None => vec![open.clone()],
                };
                (Some(title), children)
            }
            None => {
                let listed = open.id.and_then(|id| self.chats.find(id));
                let children = match listed {
                    Some(root) if !root.children.is_empty() => root.children.clone(),
                    _ => open.children.clone(),
                };
                (None, children)
            }
        }
    }

    /// The subagent `/subagents` starts on: the first one other than the open chat, which its
    /// own stream already shows.
    pub fn first_subagent(&self) -> Option<Uuid> {
        self.subagents()
            .1
            .iter()
            .filter_map(|c| c.id)
            .find(|id| self.chat_id != Some(*id))
    }

    /// Whether the open subagent's parent is in the list cache, which is where its siblings
    /// come from. A root chat counts as listed.
    pub fn parent_listed(&self) -> bool {
        match self.chat.as_ref().and_then(|c| c.parent_chat_id) {
            Some(parent) => self.chats.find(parent).is_some(),
            None => true,
        }
    }

    /// Whether Esc on an empty composer returns to the parent: the open chat is an idle subagent.
    pub fn can_return_to_parent(&self) -> bool {
        self.chat
            .as_ref()
            .is_some_and(|c| c.parent_chat_id.is_some())
            && !self.is_running()
    }

    pub fn is_running(&self) -> bool {
        matches!(
            self.transcript.status,
            Some(ChatStatus::Running | ChatStatus::Interrupting | ChatStatus::RequiresAction)
        )
    }

    /// Whether `Msg::Interrupt` should send `Effect::Interrupt`. Unlike `is_running`, this
    /// excludes `Interrupting` since an interrupt is already in flight.
    pub fn can_interrupt(&self) -> bool {
        matches!(
            self.transcript.status,
            Some(ChatStatus::Running | ChatStatus::RequiresAction)
        )
    }

    /// The generation of the chat stream the core last opened, reconnected, or closed.
    pub fn stream_generation(&self) -> u64 {
        self.stream_generation
    }

    fn open_stream(&mut self, chat: Uuid, after_id: Option<i64>) -> Effect {
        self.stream_generation += 1;
        Effect::OpenStream {
            chat,
            after_id,
            generation: self.stream_generation,
        }
    }

    fn reconnect(&mut self, chat: Uuid, delay: Duration) -> Effect {
        self.stream_generation += 1;
        Effect::ReconnectAfter {
            chat,
            after_id: self.transcript.last_message_id(),
            delay,
            generation: self.stream_generation,
        }
    }

    fn open_watch(&mut self, delay: Duration) -> Effect {
        self.watch_generation += 1;
        Effect::OpenWatch {
            delay,
            generation: self.watch_generation,
        }
    }

    fn close_stream(&mut self) -> Effect {
        self.stream_generation += 1;
        Effect::CloseStream
    }

    /// The model the next message will use: the chosen one, else the deployment default.
    pub fn current_model(&self) -> Option<&types::CodersdkChatModel> {
        let id = self.selected_model.or_else(|| {
            self.models
                .iter()
                .find(|m| m.is_default == Some(true))
                .and_then(|m| m.id)
        })?;
        self.models.iter().find(|m| m.id == Some(id))
    }

    /// The display name of the model the next message will use.
    pub fn model_name(&self) -> Option<String> {
        let model = self.current_model()?;
        model.display_name.clone().or_else(|| model.model.clone())
    }

    /// The model the next message to the open chat names, when the server would refuse it:
    /// the model list has loaded, and the model is missing from the enabled models, or its
    /// provider is in the provider list and unavailable. Like the web UI's
    /// `isUnavailableHistoricalModelID` (`site/src/pages/AgentsPage/utils/modelOptions.ts`),
    /// a chat that names no model is never unavailable.
    pub fn unavailable_model(&self) -> Option<UnavailableModel> {
        if self.models_state != ModelsState::Loaded || self.chat.is_none() {
            return None;
        }
        let id = self.selected_model?;
        let Some(model) = self.models.iter().find(|m| m.id == Some(id)) else {
            return Some(UnavailableModel {
                name: self.disabled_models.get(&id).cloned(),
            });
        };
        self.provider_off(model).then(|| UnavailableModel {
            name: model
                .display_name
                .clone()
                .filter(|n| !n.is_empty())
                .or_else(|| model.model.clone()),
        })
    }

    /// Whether the provider list says `model` cannot be used: its provider is missing from
    /// the list or marked unavailable. With no provider list, nothing says a provider is off.
    fn provider_off(&self, model: &types::CodersdkChatModel) -> bool {
        if self.providers.is_empty() {
            return false;
        }
        !model
            .ai_provider_id
            .and_then(|p| self.providers.iter().find(|d| d.id == Some(p)))
            .is_some_and(|d| d.available != Some(false))
    }

    /// Warns once per chat and model when the open chat's model is unavailable, so the user
    /// learns it before typing; the footer keeps saying so after the notice goes.
    fn warn_unavailable_model(&mut self) {
        let (Some(chat), Some(model)) = (self.chat_id, self.selected_model) else {
            return;
        };
        if self.model_warned == Some((chat, model)) {
            return;
        }
        let Some(gone) = self.unavailable_model() else {
            return;
        };
        self.model_warned = Some((chat, model));
        self.error(unavailable_warning(&gone));
    }

    /// Whether a draft waits on a model pick, so the UI knows the `/model` table is for it.
    pub fn holds_draft_for_model(&self) -> bool {
        self.model_held.is_some()
    }

    /// The `/model` table, grouped by provider, matching the web UI's order
    /// (`getModelOptionsFromModels`, `site/src/pages/AgentsPage/utils/modelOptions.ts:238-303`).
    /// With an empty `query`, groups sort alphabetically by provider display name and models
    /// sort alphabetically by their own name, both case-insensitive. A non-empty `query` ranks
    /// models by the match instead, and groups follow the order each provider's best match
    /// appears in. A model with no name, or whose `ai_provider_id` names no provider, is
    /// skipped: the former would show as a blank row, and the web UI drops the latter rather
    /// than inventing a catch-all group for it.
    pub fn model_groups(&self, query: &str) -> Vec<ModelGroup> {
        let current = self.current_model().and_then(|m| m.id);
        let provider_of = |m: &types::CodersdkChatModel| {
            m.ai_provider_id
                .and_then(|id| self.providers.iter().find(|p| p.id == Some(id)))
        };
        let models: Vec<&types::CodersdkChatModel> = self
            .models
            .iter()
            .filter(|m| {
                m.id.is_some()
                    && m.enabled != Some(false)
                    && (m.display_name.as_deref().is_some_and(|s| !s.is_empty())
                        || m.model.as_deref().is_some_and(|s| !s.is_empty()))
                    && provider_of(m).is_some()
            })
            .collect();
        let ranked = crate::fuzzy::rank(query, models, |m| {
            format!(
                "{} {} {}",
                m.display_name.as_deref().unwrap_or_default(),
                m.model.as_deref().unwrap_or_default(),
                provider_of(m)
                    .and_then(|p| p.display_name.as_deref())
                    .unwrap_or_default()
            )
        });
        let mut groups: Vec<ModelGroup> = Vec::new();
        for m in ranked {
            // Every model here has a resolvable provider; the filter above dropped the rest.
            let provider = provider_of(m).expect("models without a provider are filtered out");
            let reason = (provider.available == Some(false)).then(|| {
                provider_reason(provider.unavailable_reason.as_deref().map(|r| r.as_str()))
            });
            let row = ModelRow {
                id: m.id.unwrap_or_default(),
                name: files::display_name(
                    m.display_name
                        .as_deref()
                        .or(m.model.as_deref())
                        .unwrap_or_default(),
                ),
                current: m.id == current,
                default: m.is_default == Some(true),
                context: m
                    .context_limit
                    .map(|n| format!("{} tokens", crate::usage::format_tokens(n))),
                usable: reason.is_none(),
                compaction: self
                    .compaction
                    .shown(m.id.unwrap_or_default(), m.compression_threshold),
                efforts: m
                    .reasoning_efforts
                    .iter()
                    .map(|e| files::display_name(e))
                    .collect(),
            };
            let key = provider.id;
            match groups.iter_mut().find(|g| g.provider_id == key) {
                Some(group) => group.models.push(row),
                None => groups.push(ModelGroup {
                    provider_id: key,
                    provider: files::display_name(
                        provider.display_name.as_deref().unwrap_or_default(),
                    ),
                    reason,
                    models: vec![row],
                }),
            }
        }
        if query.trim().is_empty() {
            groups.sort_by_key(|g| g.provider.to_lowercase());
            for group in &mut groups {
                group.models.sort_by_key(|m| m.name.to_lowercase());
            }
        }
        groups
    }

    /// The compaction threshold `model` uses when the user has set none.
    fn default_threshold(&self, model: Uuid) -> Option<i64> {
        self.models
            .iter()
            .find(|m| m.id == Some(model))
            .and_then(|m| m.compression_threshold)
    }

    /// Left, Right, or Delete on the `/model` row for `model`: `Some(up)` steps its threshold,
    /// `None` goes back to its default. Before the overrides load nothing changes, and after a
    /// failed load the key loads them again.
    fn edit_threshold(&mut self, model: Uuid, step: Option<bool>) -> Vec<Effect> {
        match self.compaction.state().clone() {
            compaction::State::Loading => {
                self.info("Compaction thresholds are still loading.");
                return vec![];
            }
            compaction::State::Failed(_) => {
                let generation = self.compaction.start_load();
                self.info("Loading compaction thresholds again.");
                return vec![Effect::FetchThresholds { generation }];
            }
            compaction::State::Loaded => {}
        }
        let default = self.default_threshold(model);
        // With no override and no default of the model's, there is no value to step from.
        if step.is_some() && self.compaction.shown(model, default) == Shown::Unknown {
            let name = self.threshold_model_name(model);
            self.info(format!(
                "There is no compaction threshold to change for {name}."
            ));
            return self.commit_threshold();
        }
        let save = match step {
            Some(up) => self.compaction.step(model, up, default),
            None => self.compaction.reset(model),
        };
        save.map(Effect::SaveThreshold).into_iter().collect()
    }

    /// `model`'s name for a threshold notice, safe to show.
    fn threshold_model_name(&self, model: Uuid) -> String {
        self.models
            .iter()
            .find(|m| m.id == Some(model))
            .and_then(|m| {
                m.display_name
                    .clone()
                    .filter(|n| !n.is_empty())
                    .or_else(|| m.model.clone())
            })
            .map(|n| files::display_name(&n))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "this model".to_owned())
    }

    /// Sends the threshold edited on the highlighted `/model` row, if it changed.
    fn commit_threshold(&mut self) -> Vec<Effect> {
        self.compaction
            .commit()
            .map(Effect::SaveThreshold)
            .into_iter()
            .collect()
    }

    /// The reasoning efforts the current model offers, lowest first.
    pub fn efforts(&self) -> &[String] {
        self.current_model()
            .map(|m| m.reasoning_efforts.as_slice())
            .unwrap_or(&[])
    }

    /// The effort sent with the next message, picked the way the web UI picks it
    /// (`pickReasoningEffort` in `site/src/pages/AgentsPage/utils/reasoningEffort.ts:32-50`):
    /// the one chosen with `/effort`, else, for the open chat, its last effort
    /// (`AgentChatPage.tsx:464-471`), or, on a blank chat, the one saved in the local config
    /// for the current model (`AgentCreateForm.tsx:420,556` save and read it per model), if
    /// the current model offers it; else the model's default, if offered; else the highest.
    /// `None` when the model offers no efforts, and also while the model list is still
    /// unloaded (no model is known to offer any). For an existing chat, sending `None` is not
    /// a loss: the server keeps the chat's last effort when none is sent
    /// (`COALESCE(batch.last_reasoning_effort, chats.last_reasoning_effort)` in
    /// `coderd/database/queries/chats.sql`).
    pub fn effort(&self) -> Option<String> {
        let efforts = self.efforts();
        let offered = |e: &String| efforts.contains(e);
        let remembered = match self.chat.as_ref() {
            Some(chat) => chat.last_reasoning_effort.clone(),
            None => self.stored_effort(),
        };
        let wanted = self.selected_effort.clone().or(remembered);
        wanted
            .filter(offered)
            .or_else(|| {
                self.current_model()
                    .and_then(|m| m.model_config.as_ref())
                    .and_then(|c| c.reasoning_effort.as_ref())
                    .and_then(|r| r.default.clone())
                    .filter(offered)
            })
            .or_else(|| efforts.last().cloned())
    }

    /// The effort saved in the local config for the current model, if any. Only meaningful
    /// on a blank chat; an existing chat uses its own last effort instead.
    fn stored_effort(&self) -> Option<String> {
        let id = self.current_model()?.id?;
        self.saved_efforts.get(&id).cloned()
    }

    /// Options for the next message sent to an existing chat.
    fn turn(&self) -> TurnOptions {
        TurnOptions {
            effort: self.effort(),
            mcp_servers: self.mcp_next.clone(),
            ..Default::default()
        }
    }

    /// What the agent is doing, or `None` while it is idle or waiting on the user.
    pub fn activity(&self) -> Option<Activity> {
        match self.transcript.status {
            Some(ChatStatus::Interrupting) => Some(Activity::Interrupting),
            Some(ChatStatus::Running) => Some(match self.transcript.live.blocks.last() {
                Some(LiveBlock::Reasoning(_)) => Activity::Thinking,
                Some(LiveBlock::Text(_)) => Activity::Writing,
                Some(LiveBlock::ToolCall { name, .. })
                | Some(LiveBlock::ToolResult {
                    name, done: false, ..
                }) => Activity::Tool(name.clone()),
                _ if self.awaiting_reply => Activity::Waiting,
                _ => match self.transcript.unresolved_tool_calls().as_slice() {
                    [] => Activity::Working,
                    [one] => Activity::Tool(one.clone()),
                    [a, b] => Activity::Tool(format!("{a}, {b}")),
                    [first, rest @ ..] => {
                        Activity::Tool(format!("{first} and {} more", rest.len()))
                    }
                },
            }),
            _ if self.awaiting_reply => Some(Activity::Waiting),
            _ => None,
        }
    }

    /// Starts waiting for the agent to pick up a message that was just sent or queued.
    fn start_wait(&mut self) {
        self.awaiting_reply = true;
        self.sent_id = None;
    }

    /// Whether `ev`, already applied, means the agent picked up the sent message or gave up.
    /// A `waiting` or `interrupting` status does not count: a snapshot, the end of an earlier
    /// turn, or a busy interrupt can report it before the new message starts. The reply itself
    /// ends the wait whatever the status order, which covers a turn that ran while the stream
    /// was down. Records the echo of the sent message on the way.
    fn ends_wait(&mut self, ev: &StreamEvent) -> bool {
        match ev.kind {
            StreamEventType::Status => matches!(
                self.transcript.status,
                Some(ChatStatus::Running | ChatStatus::RequiresAction | ChatStatus::Error)
            ),
            StreamEventType::Error => true,
            StreamEventType::Message => {
                let Some(m) = ev.event.as_ref().and_then(|e| e.message.as_ref()) else {
                    return false;
                };
                let (Some(id), Some(role)) = (m.id, m.role.as_ref()) else {
                    return false;
                };
                match (role.as_str(), self.sent_id) {
                    ("user", None) => {
                        self.sent_id = Some(id);
                        false
                    }
                    ("assistant", Some(sent)) => id > sent,
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// The open chat's organization, else the loading chat's once the list names it, else
    /// the one new chats go to.
    pub fn current_org(&self) -> Option<Uuid> {
        self.chat
            .as_deref()
            .or_else(|| self.loading.and_then(|id| self.chats.find(id)))
            .and_then(|c| c.organization_id)
            .or(self.org_id)
    }

    /// Whether an existing chat is loading in place of the open one.
    pub fn is_loading_chat(&self) -> bool {
        self.loading.is_some()
    }

    /// The name of organization `id`, or a generic phrase when it is unknown or unnamed.
    pub fn org_label(&self, id: Option<Uuid>) -> String {
        id.and_then(|id| self.organizations.iter().find(|o| o.id == id))
            .map(|o| o.label().trim())
            .filter(|label| !label.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| "this organization".into())
    }

    /// The attached workspace's name, once the workspace list names it. `None` with no
    /// workspace attached, or while the list has not loaded.
    pub fn workspace_name(&self) -> Option<String> {
        let id = self.selected_workspace?;
        self.workspaces
            .iter()
            .find(|w| w.id == id)
            .map(|w| w.name.clone())
    }

    /// Whether the model list loaded and came back empty.
    fn no_models(&self) -> bool {
        self.models_state == ModelsState::Loaded && self.models.is_empty()
    }

    fn no_models_message(&self) -> String {
        format!(
            "No chat models are available in {}. Try /organization.",
            self.org_label(self.lists_org)
        )
    }

    /// Starts loading the model and workspace lists for `org`, unless they already belong to
    /// it. Model IDs are per organization, so a chosen model does not carry over.
    fn load_lists_for(&mut self, org: Uuid) -> Vec<Effect> {
        if self.lists_org == Some(org) {
            return vec![];
        }
        if self.lists_org.is_some() {
            self.selected_model = None;
            self.selected_effort = None;
        }
        self.lists_org = Some(org);
        self.models.clear();
        self.disabled_models.clear();
        self.providers.clear();
        self.unsupported_providers.clear();
        self.workspaces.clear();
        self.workspaces_state = WorkspacesState::Loading;
        self.models_state = ModelsState::Loading;
        self.org_mcp = Fetched::Loading;
        if self.chat.is_none() {
            // A blank chat's selection names the previous organization's servers.
            self.mcp_next = None;
            if let Some(panel) = self.mcp_panel.as_mut() {
                panel.servers = Fetched::Loading;
            }
        }
        vec![
            Effect::FetchModels(org),
            Effect::FetchWorkspaces(org),
            Effect::FetchOrgMcpServers(org),
        ]
    }

    fn info(&mut self, text: impl Into<String>) {
        self.notices.push(Notice::Info(text.into()));
    }

    /// Reports a browser open for `url`. A URL that opened no browser goes back to the UI as
    /// `copy(url)`, and the UI copies it and says whether that worked.
    fn opened(
        &mut self,
        url: String,
        outcome: Result<(), String>,
        copy: fn(String) -> Effect,
    ) -> Vec<Effect> {
        match outcome {
            Ok(()) => {
                self.info(format!("Opened {url}"));
                vec![]
            }
            Err(_) => vec![copy(url)],
        }
    }

    fn error(&mut self, text: impl Into<String>) {
        self.notices.push(Notice::Error(text.into()));
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        // A send that succeeded is settled wherever its reply is routed, even to a chat left.
        if let Some(seq) = sent_seq(&msg) {
            self.sent_files.remove(&seq);
            self.sent_pastes.remove(&seq);
            self.implement_seqs.remove(&seq);
            self.mcp_sent(seq);
        }
        match msg {
            Msg::Started { org_id, open_chat } => {
                self.org_id = Some(org_id);
                let mut effects = vec![Effect::FetchPrefs];
                effects.extend(self.load_lists_for(org_id));
                if let Some(id) = open_chat {
                    self.connection = Connection::Connecting;
                    self.loading = Some(id);
                    effects.push(Effect::LoadChat(id));
                }
                effects
            }
            Msg::SessionStarted => {
                let mut effects = vec![
                    Effect::FetchMe,
                    Effect::FetchSkills,
                    self.open_watch(Duration::ZERO),
                ];
                effects.extend(self.load_chats(ListQuery::Default, false));
                effects
            }
            Msg::UserLoaded(user) => {
                self.me = Some(user);
                vec![]
            }
            Msg::SkillsLoaded(list) => {
                self.personal_skills = SkillsLoad::Loaded(list);
                vec![]
            }
            Msg::SkillsFailed(message) => {
                // A failed refetch keeps the list that already loaded.
                if !matches!(self.personal_skills, SkillsLoad::Loaded(_)) {
                    self.personal_skills = SkillsLoad::Failed(message);
                }
                vec![]
            }
            Msg::OpenChat(id) => self.open_chat(id),
            Msg::ChatsLoaded {
                query,
                offset,
                chats,
            } => {
                // A refetch, such as the one after the watch reconnects, keeps the open chat.
                self.chats
                    .apply_page_keeping(&query, offset, chats, self.chat_id);
                // The open chat's stream keeps it read on the server.
                if let Some(open) = self.chat_id {
                    self.chats.set_read(open, true);
                }
                self.refetch_if_waiting(&query)
            }
            Msg::ChatsFailed { query, message } => {
                self.chats.fail(&query, message);
                self.refetch_if_waiting(&query)
            }
            Msg::LoadChats { query, more } => self.load_chats(query, more),
            Msg::SearchChats(text) => self.load_chats(ListQuery::Search(text), false),
            Msg::ChatAction(action) => self.chat_action(action),
            Msg::QueueAction(action) => match (self.chat_id, action) {
                (Some(chat), QueueAction::Promote(id)) => {
                    self.start_wait();
                    vec![Effect::PromoteQueued { chat, id }]
                }
                (Some(chat), QueueAction::PromoteFirst) => {
                    match self.transcript.queued.first().and_then(|q| q.id) {
                        Some(id) => {
                            self.start_wait();
                            vec![Effect::PromoteQueued { chat, id }]
                        }
                        None => vec![],
                    }
                }
                (Some(chat), QueueAction::Remove(id)) => vec![Effect::DeleteQueued { chat, id }],
                (None, _) => vec![],
            },
            Msg::Edit(edit) => self.edit(edit),
            Msg::QuestionKey(key) => self.question_key(key),
            Msg::FileAction(action) => self.file_action(action),
            Msg::ConflictAnswer(choice) => self.conflict_answer(choice),
            Msg::FileSaved { file, path } => {
                self.files_busy.remove(&file);
                let shown = files::display_path(&path, self.home.as_deref());
                if self.over_ssh {
                    self.info(format!("Saved {shown} on the machine scuttle runs on."));
                } else {
                    self.info(format!("Saved {shown}."));
                }
                vec![]
            }
            Msg::FileConflict { file, name, .. } if self.save_conflict.is_some() => {
                self.files_busy.remove(&file);
                self.info(format!(
                    "{name} was not saved: answer the open save question first."
                ));
                vec![]
            }
            Msg::FileConflict { file, name, path } => {
                self.files_busy.remove(&file);
                self.save_conflict = Some(SaveConflict { file, name, path });
                vec![]
            }
            Msg::FileText { file, text } => {
                self.files_busy.remove(&file);
                if self.viewing.remove(&file) != self.chat_id || self.chat_id.is_none() {
                    return vec![];
                }
                vec![Effect::Page(files::printable(&text))]
            }
            Msg::FileFailed { file, message } => {
                self.files_busy.remove(&file);
                self.viewing.remove(&file);
                self.error(message);
                vec![]
            }
            Msg::ChatUpdated { chat, change } => {
                let open = self.chat_id == Some(chat);
                match &change {
                    ChatChange::Archived(archived) => {
                        self.archiving.remove(&chat);
                        self.mark_archived(chat, *archived);
                        self.info(if *archived {
                            "Archived."
                        } else {
                            "Unarchived."
                        });
                    }
                    ChatChange::Title(title) => {
                        self.chats
                            .update_copies(chat, |c| c.title = Some(title.clone()));
                        if open && let Some(c) = self.chat.as_mut() {
                            c.title = Some(title.clone());
                        }
                        self.info(format!("Renamed to {title}."));
                    }
                    ChatChange::PinOrder(order) => {
                        self.chats
                            .update_copies(chat, |c| c.pin_order = Some(*order));
                        self.chats.resort();
                    }
                    ChatChange::Read(read) => {
                        self.chats.set_read(chat, *read);
                        if open {
                            if let Some(c) = self.chat.as_mut() {
                                c.has_unread = Some(!*read);
                            }
                            if !*read {
                                self.info("The open chat reads as read again while it stays open.");
                            }
                        }
                    }
                }
                vec![]
            }
            Msg::ChatUpdateFailed {
                chat,
                change,
                message,
            } => {
                if matches!(change, ChatChange::Archived(true)) {
                    self.archiving.remove(&chat);
                }
                self.error(format!("Could not {}: {message}", change.verb()));
                vec![]
            }
            Msg::ArchivedWithWorkspace { chat, outcome } => {
                self.archiving.remove(&chat);
                self.mark_archived(chat, true);
                match outcome {
                    WorkspaceDeletion::Started {
                        no_provisioner: false,
                    } => self.info("Archived, and its workspace is being deleted."),
                    WorkspaceDeletion::Started {
                        no_provisioner: true,
                    } => self.info(
                        "Archived. The workspace delete is queued, but no provisioner is available, so it runs once one comes online.",
                    ),
                    WorkspaceDeletion::AlreadyGone => {
                        self.info("Archived. Its workspace was already deleted.")
                    }
                    // As in the web UI, the chat stays archived: the delete may still have
                    // started, so unarchiving could revive a chat whose workspace is going.
                    WorkspaceDeletion::Failed(message) => self.error(format!(
                        "Archived, but the workspace delete failed, so the chat stays archived; unarchive it from the Archived tab in /chats, or delete the workspace in the web UI: {message}"
                    )),
                }
                vec![]
            }
            Msg::TitleProposed { title, generation } => {
                if generation != self.title_generation {
                    return vec![];
                }
                if let Some(editor) = self.editor.as_mut()
                    && matches!(editor.target, EditTarget::Title(_))
                    && editor.loading
                {
                    editor.line = LineEdit::new(&title);
                    editor.loading = false;
                }
                vec![]
            }
            Msg::TitleProposeFailed {
                message,
                generation,
            } => {
                if generation != self.title_generation {
                    return vec![];
                }
                // The controller's ruling overrides the brief: a failed proposal keeps the
                // editor open on the chat's current title instead of closing it, so the user
                // can type one without reopening `/title`.
                if let Some(editor) = self.editor.as_mut()
                    && matches!(editor.target, EditTarget::Title(_))
                {
                    let current = self.chat.as_ref().and_then(|c| c.title.as_deref());
                    editor.line = LineEdit::new(current.unwrap_or_default());
                    editor.loading = false;
                }
                self.error(format!("Could not propose a title: {message}"));
                vec![]
            }
            Msg::ForWatch { generation, msg } => {
                if generation == self.watch_generation {
                    self.update(*msg)
                } else {
                    vec![]
                }
            }
            Msg::WatchConnected => {
                self.chats.watch_live = true;
                let effects = self.load_chats(ListQuery::Default, false);
                self.refetch_after_load = effects.is_empty();
                effects
            }
            Msg::Watch(ev) => self.apply_watch(ev),
            Msg::WatchEnded { .. } => {
                self.chats.watch_live = false;
                self.watch_attempt += 1;
                vec![self.open_watch(backoff(self.watch_attempt))]
            }
            Msg::WatchHealthy => {
                self.watch_attempt = 0;
                vec![]
            }
            Msg::ForRefresh { generation, msg } => {
                if generation <= self.refresh_applied {
                    return vec![];
                }
                let before_change = generation <= self.refresh_floor;
                match *msg {
                    Msg::ChatRefreshed(chat) => {
                        self.refresh_applied = generation;
                        self.apply_refresh(chat, before_change);
                        vec![]
                    }
                    // Only the refetch after a failure says which state holds.
                    Msg::ApiFailed { action, message } if before_change => {
                        self.error(format!("Could not {action}: {message}"));
                        vec![]
                    }
                    msg => self.update(msg),
                }
            }
            Msg::ChatRefreshed(chat) => {
                self.apply_refresh(chat, false);
                vec![]
            }
            Msg::OrganizationsLoaded(organizations) => {
                self.organizations = organizations;
                if !std::mem::take(&mut self.orgs_retry) || self.org_id.is_some() {
                    return vec![];
                }
                let Some(org) = pick_organization(self.saved_org, &self.organizations) else {
                    self.error("You are not a member of any organization.");
                    return vec![];
                };
                self.org_id = Some(org);
                let mut effects = vec![Effect::FetchPrefs];
                // An open chat keeps the lists of its own organization.
                if self.chat_id.is_none() {
                    effects.extend(self.load_lists_for(org));
                }
                self.info(format!("New chats will use {}.", self.org_label(Some(org))));
                effects
            }
            Msg::OrganizationsFailed { message, open_chat } => {
                self.orgs_retry = false;
                self.error(format!("Could not load your organization: {message}"));
                let Some(id) = open_chat else {
                    return vec![];
                };
                self.connection = Connection::Connecting;
                self.loading = Some(id);
                vec![Effect::FetchPrefs, Effect::LoadChat(id)]
            }
            Msg::OrganizationChosen(id) => {
                let Some(org) = self.organizations.iter().find(|o| o.id == id) else {
                    return vec![];
                };
                let label = org.label().to_owned();
                if !org.can_create_chats {
                    self.error(format!(
                        "You do not have permission to create chats in {label}."
                    ));
                    return vec![];
                }
                self.org_id = Some(id);
                let mut effects = vec![Effect::SaveOrganization(id)];
                // An open chat, or one being created or loaded, keeps its own organization and
                // lists.
                if self.chat_id.is_some()
                    || self.creating.is_some()
                    || self.loading.is_some()
                    || self.failed_load.is_some()
                {
                    self.info(format!(
                        "New chats will use {label}; this chat stays in its organization. Use /new to start one."
                    ));
                } else {
                    let chosen = (
                        self.selected_model,
                        self.selected_effort.clone(),
                        self.selected_workspace,
                    );
                    let lists = self.load_lists_for(id);
                    if !lists.is_empty() {
                        // Workspaces belong to one organization too.
                        self.selected_workspace = None;
                    }
                    effects.extend(lists);
                    let reset = chosen
                        != (
                            self.selected_model,
                            self.selected_effort.clone(),
                            self.selected_workspace,
                        );
                    let note = if reset {
                        " Model, effort, and workspace reset to its defaults."
                    } else {
                        ""
                    };
                    self.info(format!("New chats will use {label}.{note}"));
                }
                effects
            }
            Msg::ForOrg { org, msg } => {
                if self.lists_org == Some(org) {
                    self.update(*msg)
                } else {
                    vec![]
                }
            }
            Msg::ForChat { chat, msg } => {
                if self.chat_id == Some(chat) {
                    return self.update(*msg);
                }
                // A chat left with /new: report its failures, but leave the wait and plan mode
                // of the chat now open alone.
                match *msg {
                    Msg::SendFailed {
                        text,
                        message,
                        seq,
                        mcp_rejected,
                        ..
                    } => self.report_failed_send(seq, mcp_rejected, text, &message, true),
                    // Nothing is held for a chat the user left.
                    Msg::ModelUnavailable {
                        text, message, seq, ..
                    } => self.report_failed_send(seq, false, text, &message, true),
                    Msg::ApiFailed { action, message } => {
                        self.error(format!(
                            "In the previous chat, could not {action}: {message}"
                        ));
                        vec![]
                    }
                    Msg::PlanModeFailed { on, message } => {
                        let word = if on { "on" } else { "off" };
                        self.error(format!(
                            "In the previous chat, could not turn plan mode {word}: {message}"
                        ));
                        vec![]
                    }
                    _ => vec![],
                }
            }
            Msg::ForStream {
                chat,
                generation,
                msg,
            } => {
                if self.chat_id == Some(chat) && self.stream_generation == generation {
                    self.update(*msg)
                } else {
                    vec![]
                }
            }
            Msg::ForPlan {
                chat,
                generation,
                msg,
            } => {
                let open = self.chat_id == Some(chat);
                if open && self.plan_request.is_some() && self.plan_generation == generation {
                    return self.update(*msg);
                }
                if !open {
                    // A late load may predate the change, so the reopened chat reads it back.
                    if self.loading == Some(chat) {
                        self.plan_refresh_on_load = true;
                    }
                    return self.update(Msg::ForChat { chat, msg });
                }
                // A reply from an earlier visit to the open chat: report it, and read the
                // server's value back unless the change in flight will.
                let mut effects = match *msg {
                    Msg::SendFailed {
                        text,
                        message,
                        seq,
                        mcp_rejected,
                        ..
                    } => self.report_failed_send(seq, mcp_rejected, text, &message, false),
                    Msg::ModelUnavailable {
                        text, message, seq, ..
                    } => self.report_failed_send(seq, false, text, &message, false),
                    // The change was made on a visit the user has since left.
                    Msg::PlanModeFailed { on, message } => {
                        let word = if on { "on" } else { "off" };
                        self.error(format!(
                            "In the previous chat, could not turn plan mode {word}: {message}"
                        ));
                        vec![]
                    }
                    _ => vec![],
                };
                if self.plan_request.is_none() {
                    effects.push(self.refresh_chat(chat));
                }
                effects
            }
            Msg::PreviewChat(target) => self.preview_chat(target),
            Msg::ForPreview {
                chat,
                generation,
                msg,
            } => {
                let current = self
                    .preview
                    .as_ref()
                    .is_some_and(|p| p.chat == chat && p.generation == generation);
                if current {
                    self.apply_preview(*msg)
                } else {
                    vec![]
                }
            }
            Msg::StreamHealthy => {
                self.reconnect_attempt = 0;
                vec![]
            }
            Msg::ChatLoaded {
                chat,
                messages,
                has_more,
            } => {
                let Some(id) = chat.id else {
                    self.error("The server returned a chat without an id.");
                    self.connection = Connection::Idle;
                    self.failed_load = self.loading.take().or(self.failed_load);
                    return self.restore_pending();
                };
                if !self.load_reply_applies(id) {
                    return vec![];
                }
                self.loading = None;
                self.failed_load = None;
                self.returning_to = None;
                // The load is newer than any refresh asked for before it.
                self.refresh_applied = self.refresh_generation;
                self.workspace_pending = None;
                let lists = chat
                    .organization_id
                    .map(|org| self.load_lists_for(org))
                    .unwrap_or_default();
                self.selected_model = self.selected_model.or(chat.last_model_config_id);
                self.selected_workspace = chat.workspace_id;
                self.plan_mode = is_plan(&chat);
                self.chat_id = Some(id);
                self.chat = Some(chat);
                self.history_more = first_page_more(messages.len(), has_more);
                self.history_paged = self.history_more;
                self.end_older_load();
                self.transcript.load(messages);
                // Every load warns again, so reopening the chat says so too.
                self.model_warned = None;
                self.warn_unavailable_model();
                self.connection = Connection::Connecting;
                let after_id = self.transcript.last_message_id();
                let mut effects = vec![self.open_stream(id, after_id)];
                effects.extend(lists);
                // Skills added since startup show in a workspace-bound chat's menu.
                if self.chat.as_ref().is_some_and(|c| c.workspace_id.is_some()) {
                    effects.push(Effect::FetchSkills);
                }
                if std::mem::take(&mut self.plan_refresh_on_load) {
                    effects.push(self.refresh_chat(id));
                }
                if let Some(text) = self.pending_text.take() {
                    match self.hold_for_chips(text) {
                        Ok(text) => {
                            let turn = self.turn();
                            effects.push(self.message_with_files(id, text, turn));
                        }
                        Err(held) => {
                            self.awaiting_reply = false;
                            effects.extend(held);
                        }
                    }
                }
                effects
            }
            Msg::ChatLoadFailed { chat_id, message } => {
                if !self.load_reply_applies(chat_id) {
                    return vec![];
                }
                self.loading = None;
                self.failed_load = Some(chat_id);
                self.connection = Connection::Idle;
                match self.returning_to.take().filter(|(id, _)| *id == chat_id) {
                    Some((_, Some(title))) => self.error(format!(
                        "Could not open the parent chat \u{201c}{title}\u{201d}: {message}. Find it with /chats."
                    )),
                    Some((_, None)) => self.error(format!(
                        "Could not open the parent chat {chat_id}: {message}. Find it with /chats."
                    )),
                    None => self.error(format!("Could not load chat {chat_id}: {message}")),
                }
                self.restore_pending()
            }
            Msg::ChatCreated(chat) => {
                let Some(id) = chat.id else {
                    let seq = self.creating_seq.unwrap_or_default();
                    return self
                        .fail_create("the server returned a chat without an id".into(), seq);
                };
                let workspace_mismatch = self.selected_workspace != chat.workspace_id;
                let plan_mismatch = is_plan(&chat) != self.plan_mode;
                self.creating = None;
                if let Some(seq) = self.creating_seq.take() {
                    self.sent_files.remove(&seq);
                    self.sent_pastes.remove(&seq);
                }
                self.chat_id = Some(id);
                self.chat = Some(chat);
                // The create carried the /mcp selection, so the chat already has it.
                self.mcp_next = None;
                self.creating_mcp = false;
                self.connection = Connection::Connecting;
                let mut effects = vec![self.open_stream(id, None)];
                // A `/mcp` panel opened on the blank chat now shows the new chat's servers.
                if self.mcp_panel.is_some() {
                    effects.extend(self.fetch_chat_mcp(id).unwrap_or_default());
                }
                if workspace_mismatch {
                    effects.push(Effect::SetWorkspace {
                        chat: id,
                        workspace: self.selected_workspace,
                    });
                }
                let pending = self.pending_text.take().map(|t| self.hold_for_chips(t));
                match pending {
                    // The queued message carries the plan mode change, so the two cannot race.
                    Some(Ok(text)) => {
                        let plan_mode = plan_mismatch.then_some(self.plan_mode);
                        let plan_generation = plan_mode
                            .map(|on| self.begin_plan_request(on))
                            .unwrap_or_default();
                        let turn = TurnOptions {
                            plan_mode,
                            plan_generation,
                            ..self.turn()
                        };
                        effects.push(self.message_with_files(id, text, turn));
                    }
                    // The message waits on its attachments, so the plan mode change goes alone.
                    Some(Err(held)) => {
                        self.awaiting_reply = false;
                        effects.extend(held);
                        if plan_mismatch {
                            effects.extend(self.request_plan_mode(id, self.plan_mode));
                        }
                    }
                    None if plan_mismatch => {
                        effects.extend(self.request_plan_mode(id, self.plan_mode))
                    }
                    None => {}
                }
                effects
            }
            Msg::CreateFailed { message, seq } => self.fail_create(message, seq),
            Msg::Sent { then, .. } => self.update(*then),
            Msg::SendFailed {
                text,
                message,
                plan_mode,
                seq,
                mcp_rejected,
            } => {
                self.awaiting_reply = false;
                // The answers did not arrive, so the menu starts over once the composer that
                // now holds them is cleared.
                if self
                    .answering
                    .as_ref()
                    .is_some_and(|a| a.sent.as_deref() == Some(text.as_str()))
                {
                    self.answering = None;
                }
                let mut effects = self.report_failed_send(seq, mcp_rejected, text, &message, false);
                if let Some(on) = plan_mode {
                    effects.extend(self.plan_settled(on, false));
                }
                effects
            }
            Msg::ModelUnavailable {
                text,
                files,
                message,
                plan_mode,
                seq,
            } => {
                // An /implement says to run it again, and with no model list there is nothing
                // to pick from, so those fail as any send does. A refusal while a draft is
                // held joins it, files and all, in `hold_for_model`.
                if self.implement_seqs.contains(&seq) || self.models_state != ModelsState::Loaded {
                    return self.update(Msg::SendFailed {
                        text,
                        message,
                        plan_mode,
                        seq,
                        mcp_rejected: false,
                    });
                }
                self.awaiting_reply = false;
                if self
                    .answering
                    .as_ref()
                    .is_some_and(|a| a.sent.as_deref() == Some(text.as_str()))
                {
                    self.answering = None;
                }
                // The MCP selection stays pending for the resend, as after any failed send.
                self.mcp_sends.remove(&seq);
                self.restore_chips(seq, files);
                let why = format!(
                    "The server refused this chat's model: {}.",
                    message.trim().trim_end_matches('.')
                );
                let mut effects = self.hold_for_model(text, &why);
                // The list may still offer the model the server refused, so it catches up.
                if let Some(org) = self.lists_org {
                    effects.push(Effect::FetchModels(org));
                }
                if let Some(on) = plan_mode {
                    effects.extend(self.plan_settled(on, false));
                }
                effects
            }
            Msg::ModelPickerClosed => {
                // A threshold edited in the table saves when it closes.
                let mut effects = self.commit_threshold();
                if let Some(text) = self.model_held.take() {
                    self.hold_notice = None;
                    self.info("Your message is back in the composer.");
                    effects.push(Effect::RestoreComposer(text));
                }
                effects
            }
            Msg::ModelPickDeferred => {
                if self.model_held.is_none() {
                    return vec![];
                }
                let later = Notice::Error(
                    "This chat's model is unavailable. Pick one with /model to send your held message."
                        .to_owned(),
                );
                // The offer of the table no longer holds, so the notice that made it says how
                // to pick later instead.
                match self.hold_notice.take().filter(|&i| i < self.notices.len()) {
                    Some(i) => self.notices[i] = later,
                    None => self.notices.push(later),
                }
                vec![]
            }
            Msg::Stream(ev) => {
                let was_running = self.is_running();
                let resets = self.transcript.history_resets();
                let applied = self.transcript.apply(&ev);
                // A turn ends when the status leaves running, interrupting, or requires
                // action, whatever it lands on; the UI refreshes the limits then.
                if was_running && !self.is_running() {
                    self.turns_ended += 1;
                }
                // A replaced history can drop or renumber what an older page would join, so
                // the page on its way is dropped. `history_more` stays: at worst the next
                // page asked for comes back empty.
                if ev.kind == coder_sdk::StreamEventType::HistoryReset
                    || self.transcript.history_resets() != resets
                {
                    self.end_older_load();
                }
                self.applied_stream(ev, applied)
            }
            Msg::StreamEnded { error } => {
                let Some(chat) = self.chat_id else {
                    return vec![];
                };
                self.last_stream_error = error;
                self.reconnect_attempt += 1;
                self.transcript.live.clear();
                self.connection = Connection::Reconnecting {
                    attempt: self.reconnect_attempt,
                };
                vec![self.reconnect(chat, reconnect_delay(self.reconnect_attempt, false))]
            }
            Msg::PrefsLoaded(prefs) => {
                self.prefs = prefs;
                vec![]
            }
            Msg::ModelsLoaded(models) => {
                let (enabled, disabled): (Vec<_>, Vec<_>) =
                    models.into_iter().partition(|m| m.enabled != Some(false));
                // A disabled model is never offered, but a chat that used it still names it.
                self.disabled_models = disabled
                    .into_iter()
                    .filter_map(|m| {
                        let name = m
                            .display_name
                            .filter(|n| !n.is_empty())
                            .or(m.model.filter(|n| !n.is_empty()))?;
                        Some((m.id?, name))
                    })
                    .collect();
                self.models = enabled;
                self.models_state = ModelsState::Loaded;
                self.warn_unavailable_model();
                vec![]
            }
            Msg::CatalogLoaded(catalog) => {
                let catalog = *catalog;
                self.providers = catalog.providers;
                self.unsupported_providers = catalog.unsupported_providers;
                let mut effects = self.update(Msg::ModelsLoaded(catalog.models));
                // The user's overrides are read with every model list, so `/model` shows them.
                effects.push(Effect::FetchThresholds {
                    generation: self.compaction.start_load(),
                });
                effects
            }
            Msg::ModelsFailed { message } => {
                self.models_state = ModelsState::Failed;
                self.error(format!("Could not load models: {message}"));
                vec![]
            }
            Msg::WorkspacesLoaded(mut workspaces) => {
                workspaces.sort_by_key(|w| std::cmp::Reverse(w.last_used));
                self.workspaces = workspaces;
                self.workspaces_state = WorkspacesState::Loaded;
                vec![]
            }
            Msg::WorkspacesFailed { message } => {
                self.workspaces_state = WorkspacesState::Failed(message);
                vec![]
            }
            Msg::ArgumentsWanted { command } => match command {
                "/workspace" => self.retry_workspaces(),
                _ => vec![],
            },
            Msg::ModelChosen(id) => {
                // A threshold edited in the table saves before anything the pick sends.
                let mut effects = self.commit_threshold();
                self.selected_model = Some(id);
                if let Some(name) = self.model_name() {
                    self.info(format!("Model set to {name}"));
                }
                if let Some(effort) = self
                    .selected_effort
                    .clone()
                    .filter(|e| !self.efforts().contains(e))
                {
                    // The selection stays (the web UI never clears it on a model change), so a
                    // later switch back to a model that offers it uses it again. `effort()`
                    // already falls back to this model's default, filtered by what it offers.
                    let note = match self.effort() {
                        Some(now) => format!(
                            "This model does not offer {effort} reasoning effort; using {now}."
                        ),
                        None => format!("This model does not offer {effort} reasoning effort."),
                    };
                    self.info(note);
                }
                // A draft held for this pick goes now, with the model just chosen.
                if let Some(text) = self.model_held.take() {
                    effects.extend(self.send_held(text));
                }
                effects
            }
            Msg::ThresholdStep { model, up } => self.edit_threshold(model, Some(up)),
            Msg::ThresholdReset { model } => self.edit_threshold(model, None),
            Msg::ThresholdCommit => self.commit_threshold(),
            Msg::ThresholdsLoaded {
                thresholds,
                generation,
            } => {
                self.compaction.loaded(thresholds, generation);
                vec![]
            }
            // The `/model` status line says so; a notice would interrupt every start on a
            // server without the route.
            Msg::ThresholdsFailed {
                message,
                generation,
            } => {
                self.compaction.load_failed(message, generation);
                vec![]
            }
            Msg::ThresholdSaved {
                model,
                percent,
                generation,
            } => self
                .compaction
                .saved(model, percent, generation)
                .map(Effect::SaveThreshold)
                .into_iter()
                .collect(),
            Msg::ThresholdFailed {
                model,
                message,
                generation,
            } => {
                if self.compaction.failed(model, generation) {
                    let name = self.threshold_model_name(model);
                    let message = files::display_name(&message);
                    self.error(format!(
                        "Could not save the compaction threshold for {name}: {message}"
                    ));
                }
                vec![]
            }
            Msg::EffortChosen(level) => {
                let wanted = level.to_lowercase();
                let found = self
                    .efforts()
                    .iter()
                    .find(|e| e.to_lowercase() == wanted)
                    .cloned();
                let Some(effort) = found else {
                    let offered = self.efforts().join(", ");
                    self.error(format!(
                        "No reasoning effort named {level:?}; choose one of {offered}."
                    ));
                    return vec![];
                };
                self.info(format!("Reasoning effort set to {effort}"));
                self.selected_effort = Some(effort.clone());
                // A blank chat's choice is saved per model, like the web UI's new-chat form;
                // an existing chat's choice is the chat's own and is never saved.
                let blank =
                    self.chat_id.is_none() && self.loading.is_none() && self.failed_load.is_none();
                match blank
                    .then(|| self.current_model().and_then(|m| m.id))
                    .flatten()
                {
                    Some(model) => vec![Effect::SaveEffort { model, effort }],
                    None => vec![],
                }
            }
            Msg::WorkspaceChosen(ws) => self.set_workspace(ws),
            Msg::ApiFailed { action, message } => {
                // The refetch that would say which state holds after a failure did not come.
                if action == REFRESH_CHAT {
                    self.plan_failed = false;
                }
                // No status event follows a failed "Send now" on an idle chat.
                if action == PROMOTE_QUEUED {
                    self.awaiting_reply = false;
                }
                self.error(format!("Could not {action}: {message}"));
                vec![]
            }
            Msg::PlanModeFailed { on, message } => {
                let word = if on { "on" } else { "off" };
                self.error(format!("Could not turn plan mode {word}: {message}"));
                self.plan_settled(on, false)
            }
            Msg::WebOpened { url, outcome } => self.opened(url, outcome, Effect::CopyWebUrl),
            Msg::LinkOpened { url, outcome } => self.opened(url, outcome, Effect::CopyLink),
            Msg::Submit(text) => self.submit(text, true),
            Msg::Command(cmd) => self.command(cmd),
            Msg::Interrupt => match self.chat_id {
                Some(chat) if self.can_interrupt() => vec![Effect::Interrupt(chat)],
                _ => vec![],
            },
            Msg::Refresh => vec![],
            Msg::PlanModeApplied { on } => self.plan_settled(on, true),
            Msg::FileUploaded {
                local,
                file_id,
                size,
            } => {
                // An upload whose chip is gone, as after a switch, belongs to no message here.
                let Some(chip) = self.chips.iter_mut().find(|c| c.local == local) else {
                    return vec![];
                };
                chip.state = ChipState::Ready(file_id);
                chip.size = Some(size);
                self.send_waiting()
            }
            Msg::UploadFailed { local, message } => {
                let Some(chip) = self.chips.iter_mut().find(|c| c.local == local) else {
                    return vec![];
                };
                chip.state = ChipState::Failed(message);
                match self.waiting_send.take() {
                    Some(text) => {
                        self.error("An attachment failed to upload. Remove it with Backspace on an empty composer, then send again.");
                        vec![Effect::RestoreComposer(text)]
                    }
                    None => vec![],
                }
            }
            Msg::CostLoaded { cost, generation } => {
                if generation == self.cost_generation {
                    if self.info_panel.is_some() {
                        self.info_panel = Some(CostState::Loaded(cost.clone()));
                    }
                    self.chat_cost = Some(CostState::Loaded(cost));
                }
                vec![]
            }
            Msg::CostHidden { generation } => {
                if generation == self.cost_generation {
                    if self.info_panel.is_some() {
                        self.info_panel = Some(CostState::Hidden);
                    }
                    self.chat_cost = Some(CostState::Hidden);
                }
                vec![]
            }
            Msg::CostUnauthorized { generation } => {
                if generation == self.cost_generation {
                    let rejected = || CostState::Failed(crate::usage::UNAUTHORIZED.into());
                    if self
                        .info_panel
                        .as_ref()
                        .is_some_and(|c| !matches!(c, CostState::Loaded(_)))
                    {
                        self.info_panel = Some(rejected());
                    }
                    if !matches!(self.chat_cost, Some(CostState::Loaded(_))) {
                        self.chat_cost = Some(rejected());
                    }
                    self.stop_limits();
                }
                vec![]
            }
            Msg::CostFailed {
                message,
                generation,
            } => {
                // A failed refetch keeps the total already shown.
                if generation == self.cost_generation {
                    if self
                        .info_panel
                        .as_ref()
                        .is_some_and(|c| !matches!(c, CostState::Loaded(_)))
                    {
                        self.info_panel = Some(CostState::Failed(message.clone()));
                    }
                    if !matches!(self.chat_cost, Some(CostState::Loaded(_))) {
                        self.chat_cost = Some(CostState::Failed(message));
                    }
                }
                vec![]
            }
            Msg::InfoClosed => {
                self.info_panel = None;
                vec![]
            }
            Msg::RefreshLimits => self.refresh_limits(),
            Msg::RefreshCost => {
                // A rejected token fails the cost request too, so it stops with the limits.
                if (self.cost_in_footer || self.usage_open) && !self.limits_stopped {
                    self.fetch_chat_cost()
                } else {
                    vec![]
                }
            }
            Msg::SpendLoaded { spend, generation } => {
                if generation == self.limits_generation {
                    self.spend = LimitState::Loaded(spend);
                }
                vec![]
            }
            Msg::QuotaLoaded {
                org,
                quota,
                generation,
            } => {
                if generation == self.limits_generation && self.quota_org == Some(org) {
                    self.quota = LimitState::Loaded(quota);
                }
                vec![]
            }
            Msg::LimitFailed {
                limit,
                refusal,
                generation,
            } => {
                if generation != self.limits_generation {
                    return vec![];
                }
                let unauthorized = match limit {
                    Limit::Spend => self.spend.refuse(refusal),
                    Limit::Quota => self.quota.refuse(refusal),
                };
                if unauthorized {
                    self.stop_limits();
                }
                vec![]
            }
            Msg::UsageClosed => {
                self.usage_open = false;
                vec![]
            }
            // Details for a panel that already closed are dropped.
            Msg::WorkspaceDetailsLoaded(ws) => {
                // A reply for an earlier workspace, after a switch, is dropped too.
                if self.workspace_panel.is_some() && ws.id == self.selected_workspace {
                    self.workspace_panel = Some(Fetched::Loaded(ws));
                }
                vec![]
            }
            Msg::WorkspaceDetailsFailed { workspace, message } => {
                if self.workspace_panel.is_some() && Some(workspace) == self.selected_workspace {
                    self.workspace_panel = Some(Fetched::Failed(message));
                }
                vec![]
            }
            Msg::SshSuffixLoaded(suffix) => {
                self.ssh_suffix = Some(suffix);
                vec![]
            }
            // Only a reply is cached, so a failure is asked again on the next `/workspace`.
            Msg::SshSuffixFailed => vec![],
            Msg::WorkspaceAction(action) => self.workspace_action(action),
            Msg::WorkspaceClosed => {
                self.workspace_panel = None;
                vec![]
            }
            Msg::ForGit {
                chat,
                generation,
                msg,
            } => {
                if self.chat_id == Some(chat)
                    && self.git_panel.is_some()
                    && self.git_generation == generation
                {
                    self.update(*msg)
                } else {
                    vec![]
                }
            }
            Msg::UserScrolled => {
                if self.jump.take().is_some() {
                    self.info(STOPPED_LOOKING);
                }
                vec![]
            }
            Msg::LoadOlder | Msg::ScrolledToTop => {
                let (Some(chat), Some(before_id)) =
                    (self.chat_id, self.transcript.first_message_id())
                else {
                    return vec![];
                };
                if matches!(msg, Msg::ScrolledToTop) && self.history_failed {
                    return vec![];
                }
                self.history_failed = false;
                if !self.history_more || self.history_loading {
                    return vec![];
                }
                self.history_loading = true;
                self.history_generation += 1;
                vec![Effect::LoadOlder {
                    chat,
                    before_id,
                    generation: self.history_generation,
                }]
            }
            Msg::OlderLoaded {
                messages,
                has_more,
                generation,
            } => {
                if !self.older_reply_applies(generation) {
                    return vec![];
                }
                self.history_loading = false;
                // An empty page means nothing older, whatever `has_more` says.
                self.history_more = has_more && !messages.is_empty();
                self.transcript.prepend(messages);
                self.resume_jump()
            }
            Msg::OlderFailed {
                message,
                generation,
            } => {
                if !self.older_reply_applies(generation) {
                    return vec![];
                }
                self.history_loading = false;
                self.history_failed = true;
                self.jump = None;
                self.error(format!("Could not load older messages: {message}"));
                vec![]
            }
            Msg::DiffLoaded { diff, generation } => {
                if generation != self.diff_generation {
                    return vec![];
                }
                let wanted = std::mem::take(&mut self.page_when_loaded);
                let text = diff.diff.clone().filter(|d| !d.trim().is_empty());
                if let Some(panel) = self.git_panel.as_mut() {
                    panel.diff = Fetched::Loaded(diff);
                }
                if !wanted {
                    return vec![];
                }
                let text =
                    text.or_else(|| self.git_panel.as_ref().and_then(crate::panels::diff_text));
                self.page(text)
            }
            Msg::DiffFailed {
                message,
                generation,
            } => {
                if generation != self.diff_generation {
                    return vec![];
                }
                if std::mem::take(&mut self.page_when_loaded) {
                    self.error(format!("Could not load the diff: {message}"));
                }
                if let Some(panel) = self.git_panel.as_mut() {
                    panel.diff = Fetched::Failed(message);
                }
                vec![]
            }
            Msg::GitChanges(message) => {
                let Some(panel) = self.git_panel.as_mut() else {
                    return vec![];
                };
                match message.type_.as_ref().map(|t| t.0.as_str()) {
                    // The workspace reports a failure over a socket that stays open.
                    Some("error") => {
                        panel.local = LocalGit::Live;
                        let text = message.message.clone().unwrap_or_default();
                        self.error(format!("The workspace reported a git error: {text}"));
                    }
                    _ => {
                        panel.local = LocalGit::Live;
                        for repo in message.repositories {
                            let Some(root) = repo.repo_root.clone() else {
                                continue;
                            };
                            if repo.removed == Some(true) {
                                panel.repos.remove(&root);
                            } else {
                                panel.repos.insert(root, repo);
                            }
                        }
                    }
                }
                vec![]
            }
            Msg::GitReopened => {
                if let Some(panel) = self.git_panel.as_mut() {
                    panel.repos.clear();
                }
                vec![]
            }
            Msg::GitWatchEnded(message) => {
                if let Some(panel) = self.git_panel.as_mut() {
                    panel.local = LocalGit::Ended(message);
                }
                vec![]
            }
            Msg::GitAction(action) => self.git_action(action),
            Msg::GitClosed => match self.git_panel.take() {
                Some(_) => vec![Effect::CloseGitWatch],
                None => vec![],
            },
            Msg::McpServersLoaded {
                servers,
                generation,
            } => {
                if let Some(panel) = self.current_mcp(generation) {
                    panel.servers = Fetched::Loaded(servers);
                }
                vec![]
            }
            Msg::McpServersFailed {
                message,
                generation,
            } => {
                if let Some(panel) = self.current_mcp(generation) {
                    panel.servers = Fetched::Failed(message);
                }
                vec![]
            }
            Msg::McpHealthLoaded {
                outcomes,
                generation,
            } => {
                if let Some(panel) = self.current_mcp(generation) {
                    panel.health = Fetched::Loaded(outcomes);
                }
                vec![]
            }
            Msg::McpHealthFailed {
                message,
                generation,
            } => {
                if let Some(panel) = self.current_mcp(generation) {
                    panel.health = Fetched::Failed(message);
                }
                vec![]
            }
            Msg::ToggleMcp(server) => self.toggle_mcp(server),
            Msg::McpClosed => {
                self.mcp_panel = None;
                vec![]
            }
            Msg::OrgMcpLoaded(servers) => {
                if self.chat.is_none() {
                    // The first message sends only servers still enabled, so a server disabled
                    // since the list last loaded cannot make the create fail again.
                    let enabled: Vec<Uuid> = servers
                        .iter()
                        .filter(|s| s.enabled != Some(false))
                        .filter_map(|s| s.id)
                        .collect();
                    if let Some(next) = self.mcp_next.as_mut() {
                        next.retain(|id| enabled.contains(id));
                    }
                    if let Some(panel) = self.mcp_panel.as_mut() {
                        panel.servers = Fetched::Loaded(servers.clone());
                    }
                }
                self.org_mcp = Fetched::Loaded(servers);
                vec![]
            }
            Msg::OrgMcpFailed(message) => {
                if self.chat.is_none()
                    && let Some(panel) = self.mcp_panel.as_mut()
                {
                    panel.servers = Fetched::Failed(message.clone());
                }
                self.org_mcp = Fetched::Failed(message);
                vec![]
            }
            Msg::RemoveLastChip => {
                let mut effects = match self.chips.pop() {
                    Some(chip) if chip.state == ChipState::Uploading => {
                        vec![Effect::CancelUpload(chip.local)]
                    }
                    _ => vec![],
                };
                effects.extend(self.send_waiting());
                effects
            }
            Msg::AttachMention(path) => self.add_chip(path, true),
            Msg::AttachPaste { name, text } => {
                // A draft recalled after a failed send names a paste already back as a chip.
                if !self
                    .chips
                    .iter()
                    .any(|c| c.pasted.is_some() && c.name == name)
                {
                    self.push_paste(name, text);
                }
                vec![]
            }
        }
    }

    /// Restores whatever text was typed for the failed `Effect::CreateChat` (and anything
    /// queued behind it) to the composer, and records the failure.
    fn fail_create(&mut self, message: String, seq: u64) -> Vec<Effect> {
        self.awaiting_reply = false;
        let in_flight = self.creating.take().unwrap_or_default();
        let restored = match self.pending_text.take() {
            Some(pending) => format!("{in_flight}\n\n{pending}"),
            None => in_flight,
        };
        self.creating_seq = None;
        let notice = self.with_lost_files(seq, format!("Could not create the chat: {message}"));
        self.error(notice);
        let mut effects = if restored.is_empty() {
            vec![]
        } else {
            vec![Effect::RestoreComposer(restored)]
        };
        // A server disabled since the list loaded makes the server refuse the create, so the
        // list is fetched again, and its reply drops that server from the selection.
        if std::mem::take(&mut self.creating_mcp)
            && let Some(org) = self.lists_org
        {
            effects.push(Effect::FetchOrgMcpServers(org));
        }
        effects
    }

    /// Puts text queued behind a chat load back in the composer once that load has failed.
    fn restore_pending(&mut self) -> Vec<Effect> {
        match self.pending_text.take() {
            Some(text) => {
                self.awaiting_reply = false;
                vec![Effect::RestoreComposer(text)]
            }
            None => vec![],
        }
    }

    fn queue_pending(&mut self, text: String) {
        self.pending_text = Some(match self.pending_text.take() {
            Some(prev) => format!("{prev}\n\n{text}"),
            None => text,
        });
    }

    /// Sends `text`, or runs it as a command. With `release`, the user's own send, the files
    /// `@path` mentions held start uploading, and the message waits for them; text that
    /// waited on uploads goes out without `release`, so a mention added since keeps waiting
    /// for its own send.
    fn submit(&mut self, text: String, release: bool) -> Vec<Effect> {
        let text = text.trim().to_owned();
        // A large paste is content of its own, so it sends with no text, as in the web UI.
        if text.is_empty() && !self.chips.iter().any(|c| c.pasted.is_some()) {
            return vec![];
        }
        let text = if text.starts_with('/') {
            match commands::parse(&text) {
                // Built-in commands win over skills of the same name.
                Ok(cmd) => return self.command(cmd),
                Err(e) => match skills::rewrite(&text, &self.slash_menu()) {
                    Some(message) => message,
                    None => {
                        self.error(e);
                        return vec![];
                    }
                },
            }
        } else {
            text
        };
        if self.chat_id.is_some() && self.implement_held {
            self.info("Wait for the plan to start implementing, then send your message.");
            return vec![Effect::RestoreComposer(text)];
        }
        let mut uploads = if release { self.upload_held() } else { vec![] };
        let text = match self.hold_for_chips(text) {
            Ok(text) => text,
            Err(held) => {
                uploads.extend(held);
                return uploads;
            }
        };
        if let Some(chat) = self.chat_id {
            // A send the server would refuse waits for a model pick instead.
            if let Some(gone) = self.unavailable_model() {
                return self.hold_for_model(text, &gone.sentence());
            }
            let turn = self.turn();
            let mut effects = self.send(chat, text, turn);
            self.carry_files(&mut effects);
            return effects;
        }
        if self.creating.is_some() {
            self.queue_pending(text);
            self.start_wait();
            self.info("Waiting for the chat to be created; your message will follow.");
            return vec![];
        }
        if self.loading.is_some() {
            self.queue_pending(text);
            self.start_wait();
            self.info("Waiting for the chat to load; your message will follow.");
            return vec![];
        }
        if let Some(id) = self.failed_load.take() {
            self.queue_pending(text);
            self.start_wait();
            self.loading = Some(id);
            self.connection = Connection::Connecting;
            self.info("Retrying the chat load.");
            return vec![Effect::LoadChat(id)];
        }
        let Some(org) = self.org_id else {
            self.error("Not connected to Coder yet.");
            return vec![];
        };
        if self.no_models() {
            let message = self.no_models_message();
            self.error(message);
            return vec![Effect::RestoreComposer(text)];
        }
        self.creating = Some(text.clone());
        self.start_wait();
        let (files, names, pastes) = self.take_files();
        let seq = self.next_seq();
        self.note_send(seq, names, pastes);
        self.creating_seq = Some(seq);
        let mcp_servers = self.first_mcp();
        self.creating_mcp = mcp_servers.as_ref().is_some_and(|ids| !ids.is_empty());
        vec![Effect::CreateChat {
            org,
            text,
            model: self.selected_model,
            workspace: self.selected_workspace,
            turn: TurnOptions {
                plan_mode: self.plan_mode.then_some(true),
                files,
                mcp_servers,
                ..self.turn()
            },
            seq,
        }]
    }

    /// Sends the text that waited on uploads once none is still uploading. `submit` checks the
    /// chips again, so a failed upload hands the text back instead.
    fn send_waiting(&mut self) -> Vec<Effect> {
        if self.chips.iter().any(|c| c.state == ChipState::Uploading) {
            return vec![];
        }
        match self.waiting_send.take() {
            Some(text) => self.submit(text, false),
            None => vec![],
        }
    }

    /// Adds a chip for the file at `path` and starts its upload, unless its type is one the
    /// server never accepts.
    fn attach(&mut self, path: String) -> Vec<Effect> {
        self.add_chip(path, false)
    }

    /// Adds a chip for the file at `path`: refused when its type is one the server never
    /// accepts, else held for the send when `held`, else uploading now.
    fn add_chip(&mut self, path: String, held: bool) -> Vec<Effect> {
        let name = std::path::Path::new(&path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&path)
            .to_owned();
        let Some(org) = self.current_org() else {
            self.error("Not connected to Coder yet.");
            return vec![];
        };
        self.next_chip += 1;
        let local = self.next_chip;
        match attachments::check_type(&name) {
            Err(reason) => {
                self.chips.push(Chip {
                    local,
                    name,
                    size: None,
                    state: ChipState::Failed(reason),
                    pasted: None,
                });
                vec![]
            }
            Ok(()) if held => {
                self.chips.push(Chip {
                    local,
                    name,
                    size: None,
                    state: ChipState::Held(path),
                    pasted: None,
                });
                vec![]
            }
            Ok(()) => {
                self.chips.push(Chip {
                    local,
                    name,
                    size: None,
                    state: ChipState::Uploading,
                    pasted: None,
                });
                vec![Effect::UploadFile { local, path, org }]
            }
        }
    }

    /// Adds a chip for the large paste `text`, uploaded as `name` when the message is sent. A
    /// paste over the upload limit is refused as a failed chip.
    fn push_paste(&mut self, name: String, text: String) {
        self.next_chip += 1;
        let size = text.len() as u64;
        let state = if size > attachments::MAX_FILE_BYTES {
            ChipState::Failed(format!(
                "the pasted text is {size} bytes; the limit is {} bytes.",
                attachments::MAX_FILE_BYTES
            ))
        } else {
            ChipState::Pasted
        };
        self.chips.push(Chip {
            local: self.next_chip,
            name,
            size: Some(size),
            state,
            pasted: Some(text),
        });
    }

    /// Starts the uploads of the chips held for the send: the files `@path` mentions name and
    /// the large pastes.
    fn upload_held(&mut self) -> Vec<Effect> {
        if !self
            .chips
            .iter()
            .any(|c| matches!(c.state, ChipState::Held(_) | ChipState::Pasted))
        {
            return vec![];
        }
        let Some(org) = self.current_org() else {
            return vec![];
        };
        self.chips
            .iter_mut()
            .filter_map(|chip| {
                let effect = match &chip.state {
                    ChipState::Held(path) => Effect::UploadFile {
                        local: chip.local,
                        path: path.clone(),
                        org,
                    },
                    ChipState::Pasted => Effect::UploadText {
                        local: chip.local,
                        name: chip.name.clone(),
                        text: chip.pasted.clone().unwrap_or_default(),
                        org,
                    },
                    _ => return None,
                };
                chip.state = ChipState::Uploading;
                Some(effect)
            })
            .collect()
    }

    /// The uploaded files for a message about to go out, with their names and the name and
    /// text of each paste among them; the chips clear with it, except those held for a later
    /// send.
    fn take_files(&mut self) -> (Vec<Uuid>, Vec<String>, Vec<(String, String)>) {
        let (mut files, mut names, mut pastes) = (Vec::new(), Vec::new(), Vec::new());
        for chip in &self.chips {
            if let ChipState::Ready(id) = chip.state {
                files.push(id);
                names.push(chip.name.clone());
                if let Some(text) = &chip.pasted {
                    pastes.push((chip.name.clone(), text.clone()));
                }
            }
        }
        self.chips
            .retain(|c| matches!(c.state, ChipState::Held(_) | ChipState::Pasted));
        (files, names, pastes)
    }

    /// Holds `text` back while an attachment is uploading, to send once all are ready, or
    /// hands it back while one has failed. Otherwise returns it to send now.
    fn hold_for_chips(&mut self, text: String) -> Result<String, Vec<Effect>> {
        if self.chips.iter().any(|c| c.state == ChipState::Uploading) {
            self.waiting_send = Some(match self.waiting_send.take() {
                Some(prev) => format!("{prev}\n\n{text}"),
                None => text,
            });
            self.info("Sending when uploads finish.");
            return Err(vec![]);
        }
        if self
            .chips
            .iter()
            .any(|c| matches!(c.state, ChipState::Failed(_)))
        {
            self.error(
                "Remove the attachment that failed (Backspace on an empty composer), then send.",
            );
            return Err(vec![Effect::RestoreComposer(text)]);
        }
        Ok(text)
    }

    /// Holds `text` while the user picks a model, because the chat's model is unavailable
    /// (`why` says so), and opens the `/model` table: `Msg::ModelChosen` sends it with the
    /// pick, and `Msg::ModelPickerClosed` gives it back. The chips stay above the composer for
    /// the resend. With no model to pick, the text goes back at once.
    fn hold_for_model(&mut self, text: String, why: &str) -> Vec<Effect> {
        // A draft already held is never replaced: the new text joins it, as its chips do.
        let text = match self.model_held.take() {
            Some(held) => format!("{held}\n\n{text}"),
            None => text,
        };
        if self.no_models() {
            self.hold_notice = None;
            let none = self.no_models_message();
            self.error(format!("{why} {none}"));
            return vec![Effect::RestoreComposer(text)];
        }
        self.model_held = Some(text);
        self.error(format!(
            "{why} Pick a model to send your message, or press Esc to keep editing it."
        ));
        self.hold_notice = Some(self.notices.len() - 1);
        vec![Effect::ShowPicker(Picker::Model)]
    }

    /// Puts the files of the send numbered `seq`, which the server refused for its model, back
    /// above the composer as uploaded chips, under the names the send recorded and with the
    /// text of any paste among them, so the resend carries them without a second upload. They
    /// go ahead of the chips held for a later send, in the order they were attached.
    fn restore_chips(&mut self, seq: u64, files: Vec<Uuid>) {
        let names = self.sent_files.remove(&seq).unwrap_or_default();
        let pastes = self.sent_pastes.remove(&seq).unwrap_or_default();
        let mut restored = Vec::with_capacity(files.len());
        for (i, id) in files.into_iter().enumerate() {
            self.next_chip += 1;
            let name = names
                .get(i)
                .cloned()
                .unwrap_or_else(|| "attachment".to_owned());
            let pasted = pastes
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, text)| text.clone());
            restored.push(Chip {
                local: self.next_chip,
                name,
                size: pasted.as_ref().map(|t| t.len() as u64),
                state: ChipState::Ready(id),
                pasted,
            });
        }
        self.chips.splice(0..0, restored);
    }

    /// Sends the text held for a model pick to the open chat with the model now chosen,
    /// carrying the chips that waited with it, through the same path as a typed message.
    fn send_held(&mut self, text: String) -> Vec<Effect> {
        let Some(chat) = self.chat_id else {
            return vec![Effect::RestoreComposer(text)];
        };
        // A pick the server would refuse too, such as a model whose provider is off, holds
        // the draft again.
        if let Some(gone) = self.unavailable_model() {
            return self.hold_for_model(text, &gone.sentence());
        }
        let text = match self.hold_for_chips(text) {
            Ok(text) => text,
            Err(held) => return held,
        };
        let turn = self.turn();
        let mut effects = self.send(chat, text, turn);
        self.carry_files(&mut effects);
        effects
    }

    /// A message queued behind a load or a create, carrying the attachments.
    fn message_with_files(&mut self, chat: Uuid, text: String, turn: TurnOptions) -> Effect {
        let (files, names, pastes) = self.take_files();
        let seq = self.next_seq();
        self.note_send(seq, names, pastes);
        self.note_mcp_send(seq, &turn);
        Effect::SendMessage {
            chat,
            text,
            model: self.selected_model,
            busy: self.busy,
            turn: TurnOptions { files, ..turn },
            seq,
        }
    }

    /// Puts the uploaded files on the message `send` returned. A refused send leaves the chips
    /// for the next try.
    fn carry_files(&mut self, effects: &mut [Effect]) {
        let Some(Effect::SendMessage { turn, seq, .. }) = effects
            .iter_mut()
            .find(|e| matches!(e, Effect::SendMessage { .. }))
        else {
            return;
        };
        let (files, names, pastes) = self.take_files();
        turn.files = files;
        let seq = *seq;
        self.note_send(seq, names, pastes);
    }

    /// Numbers the next outgoing send.
    fn next_seq(&mut self) -> u64 {
        self.next_send += 1;
        self.next_send
    }

    /// Records the names of the files the send numbered `seq` carries, and its pastes, if any.
    fn note_send(&mut self, seq: u64, names: Vec<String>, pastes: Vec<(String, String)>) {
        if !names.is_empty() {
            self.sent_files.insert(seq, names);
        }
        if !pastes.is_empty() {
            self.sent_pastes.insert(seq, pastes);
        }
    }

    /// Records the MCP selection the send numbered `seq` carries, if any.
    fn note_mcp_send(&mut self, seq: u64, turn: &TurnOptions) {
        if let Some(ids) = &turn.mcp_servers {
            self.mcp_sends.insert(seq, ids.clone());
            self.mcp_newest = seq;
        }
    }

    /// Applies the MCP selection the accepted send numbered `seq` carried. It is no longer
    /// pending unless /mcp changed it since, and the chat shows it until a refetch confirms it
    /// when it is the newest send's, so a reply arriving out of order never shows an older
    /// selection. A send from a chat left was dropped with that chat's state, so it applies
    /// nothing.
    fn mcp_sent(&mut self, seq: u64) {
        let Some(ids) = self.mcp_sends.remove(&seq) else {
            return;
        };
        if self.mcp_next.as_ref() == Some(&ids) {
            self.mcp_next = None;
        }
        if seq == self.mcp_newest
            && let Some(chat) = self.chat.as_mut()
        {
            chat.mcp_server_ids = ids;
            self.refresh_floor = self.refresh_generation;
        }
    }

    /// The organization MCP servers the chat uses once the sends in flight land: the newest
    /// in-flight send's selection, else the chat's own. On a blank chat, the organization's
    /// defaults once its list loads, else none. A pending /mcp change is relative to it.
    pub(crate) fn mcp_selection(&self) -> Vec<Uuid> {
        if let Some((_, ids)) = self.mcp_sends.last_key_value() {
            return ids.clone();
        }
        match (self.chat.as_deref(), &self.org_mcp) {
            (Some(chat), _) => chat.mcp_server_ids.clone(),
            (None, Fetched::Loaded(servers)) => default_mcp(servers),
            (None, _) => Vec::new(),
        }
    }

    /// How many MCP servers the next message uses, counted as `/mcp` shows them: the enabled
    /// organization servers its selection turns on or the organization requires, and the open
    /// chat's inline and workspace servers. `None` until the organization's list loads, since
    /// only the list says which servers exist and which are required, and `None` while no chat
    /// is open but one is loading or failed to load, whose selection is not known.
    pub fn mcp_on_count(&self) -> Option<usize> {
        let Fetched::Loaded(servers) = &self.org_mcp else {
            return None;
        };
        if self.chat.is_none() && (self.loading.is_some() || self.failed_load.is_some()) {
            return None;
        }
        let selected = self
            .mcp_next
            .clone()
            .unwrap_or_else(|| self.mcp_selection());
        let org = servers
            .iter()
            .filter(|s| s.enabled != Some(false))
            .filter(|s| is_required(s) || s.id.is_some_and(|id| selected.contains(&id)))
            .count();
        let chat = self.chat.as_deref().map_or(0, |chat| {
            chat.inline_mcp_servers.len() + workspace_mcp(chat).count()
        });
        Some(org + chat)
    }

    /// The MCP selection a new chat is created with: what `/mcp` changed, else the
    /// organization's defaults once its list loads. `None` leaves the choice to the server,
    /// which turns on only the required servers.
    fn first_mcp(&self) -> Option<Vec<Uuid>> {
        self.mcp_next.clone().or_else(|| match &self.org_mcp {
            Fetched::Loaded(servers) => Some(default_mcp(servers)),
            _ => None,
        })
    }

    /// Opens `/mcp` on a blank chat: the organization's servers, as the first message turns
    /// them on. A list that failed to load is fetched again.
    fn blank_mcp(&mut self) -> Vec<Effect> {
        let Some(org) = self.lists_org.or(self.org_id) else {
            self.error("Not connected to Coder yet.");
            return vec![];
        };
        let mut effects = vec![Effect::ShowMcp];
        if matches!(self.org_mcp, Fetched::Failed(_)) {
            self.org_mcp = Fetched::Loading;
            effects.push(Effect::FetchOrgMcpServers(org));
        }
        // A reply to an earlier chat's panel finds this one's generation and is dropped.
        self.mcp_generation += 1;
        self.mcp_panel = Some(McpPanel {
            servers: self.org_mcp.clone(),
            // Connect outcomes come from a chat's debug runs, and a blank chat has none.
            health: Fetched::Loaded(None),
        });
        effects
    }

    /// Fills `/mcp` from chat `chat`'s servers and connect outcomes, returning the fetches, or
    /// `None` before the organization is known.
    fn fetch_chat_mcp(&mut self, chat: Uuid) -> Option<Vec<Effect>> {
        let org = self.current_org()?;
        self.mcp_panel = Some(McpPanel {
            servers: Fetched::Loading,
            health: Fetched::Loading,
        });
        self.mcp_generation += 1;
        let generation = self.mcp_generation;
        Some(vec![
            Effect::FetchMcpServers {
                chat,
                org,
                generation,
            },
            Effect::FetchMcpHealth { chat, generation },
        ])
    }

    /// Whether a chat being created, loaded, or left unloaded by a failed load keeps a
    /// selection from landing, saying why when it does.
    fn mcp_unready(&mut self) -> bool {
        if self.creating.is_some() || self.loading.is_some() {
            self.info(MCP_WAIT);
            true
        } else if self.failed_load.is_some() {
            self.info(MCP_FAILED_LOAD);
            true
        } else {
            false
        }
    }

    /// Turns the organization MCP server `server` on or off for the next message. A server
    /// the organization requires stays on.
    fn toggle_mcp(&mut self, server: Uuid) -> Vec<Effect> {
        // The create or load in flight decides which chat a selection belongs to.
        if self.chat.is_none() && self.mcp_unready() {
            return vec![];
        }
        let current = self.mcp_selection();
        let config = match self.mcp_panel.as_ref().map(|p| &p.servers) {
            Some(Fetched::Loaded(servers)) => servers
                .iter()
                .find(|s| s.id == Some(server) && s.enabled != Some(false))
                .cloned(),
            _ => None,
        };
        let Some(config) = config else {
            return vec![];
        };
        let name = config
            .display_name
            .clone()
            .or(config.slug.clone())
            .unwrap_or_default();
        if is_required(&config) {
            self.info(format!(
                "{name} is required by your organization and stays on."
            ));
            return vec![];
        }
        let mut next = self.mcp_next.clone().unwrap_or_else(|| current.clone());
        let on = match next.iter().position(|id| *id == server) {
            Some(i) => {
                next.remove(i);
                false
            }
            None => {
                next.push(server);
                true
            }
        };
        // Toggling back to the selection the chat will have leaves nothing to send.
        let same = next.len() == current.len() && next.iter().all(|id| current.contains(id));
        self.mcp_next = (!same).then_some(next);
        let word = if on { "on" } else { "off" };
        let when = if self.chat.is_some() { "next" } else { "first" };
        self.info(format!("{name} turns {word} with your {when} message."));
        vec![]
    }

    /// Reports the failed send numbered `seq` of `text`, to the previous chat when `previous`,
    /// and puts the text back in the composer. A failed `/implement` says to run it again
    /// instead, since its text was never typed and would no longer carry the plan-mode change.
    fn report_failed_send(
        &mut self,
        seq: u64,
        mcp_rejected: bool,
        text: String,
        message: &str,
        previous: bool,
    ) -> Vec<Effect> {
        let implement = self.implement_seqs.remove(&seq);
        let notice = match (implement, previous) {
            (true, false) => format!("The plan was not sent: {message}. Run /implement again."),
            (true, true) => format!(
                "The plan was not sent to the previous chat: {message}. Open it and run /implement again."
            ),
            (false, false) => format!("Could not send the message: {message}"),
            (false, true) => format!("Could not send the message to the previous chat: {message}"),
        };
        let notice = self.failed_send_notice(seq, mcp_rejected, notice);
        self.error(notice);
        if implement {
            vec![]
        } else {
            vec![Effect::RestoreComposer(text)]
        }
    }

    /// Settles the failed send numbered `seq`, adding the names of its files to `notice`. The
    /// MCP selection it carried stays pending for the next send, unless the server refused it
    /// (`mcp_rejected`), since every later send would fail the same way.
    fn failed_send_notice(&mut self, seq: u64, mcp_rejected: bool, notice: String) -> String {
        let carried = self.mcp_sends.remove(&seq).is_some();
        let notice = if carried && mcp_rejected {
            self.mcp_next = None;
            format!(
                "{notice} The MCP server selection was not accepted and was dropped; change it in /mcp."
            )
        } else {
            notice
        };
        self.with_lost_files(seq, notice)
    }

    /// Settles the failed create or send numbered `seq`, adding the names of its files to
    /// `notice`. Its pastes come back above the composer as chips that upload again, since
    /// nothing else holds their text.
    fn with_lost_files(&mut self, seq: u64, notice: String) -> String {
        let pastes = self.sent_pastes.remove(&seq).unwrap_or_default();
        let names = self.sent_files.remove(&seq).unwrap_or_default();
        let lost: Vec<String> = names
            .into_iter()
            .filter(|n| !pastes.iter().any(|(p, _)| p == n))
            .collect();
        let notice = lost_files_notice(notice, &lost);
        if pastes.is_empty() {
            return notice;
        }
        let back: Vec<String> = pastes.iter().map(|(n, _)| n.clone()).collect();
        for (name, text) in pastes {
            self.push_paste(name, text);
        }
        format!(
            "{}. Your pasted text is back above the composer as {}; send again to attach it.",
            notice.trim_end_matches('.'),
            back.join(", ")
        )
    }

    /// Sends `text` to the open chat as it is, never as a slash command, and starts the wait.
    fn send(&mut self, chat: Uuid, text: String, turn: TurnOptions) -> Vec<Effect> {
        if self.is_archived() {
            self.error("This chat is archived. Ctrl+A in /chats unarchives it.");
            return vec![Effect::RestoreComposer(text)];
        }
        self.start_wait();
        let seq = self.next_seq();
        self.note_mcp_send(seq, &turn);
        vec![Effect::SendMessage {
            chat,
            text,
            model: self.selected_model,
            busy: self.busy,
            turn,
            seq,
        }]
    }

    /// Whether a jump to a file's message is still waiting on older pages.
    pub fn looking_for_file(&self) -> bool {
        self.jump.is_some()
    }

    /// Whether the agent proposed a plan that `/implement` would start. Like the web UI, the
    /// action waits for the turn to finish.
    pub fn plan_ready(&self) -> bool {
        !self.is_running() && question::plan_ready(&self.transcript)
    }

    /// What the top of the open chat's transcript says about older history; `None` for a
    /// chat whose first page held all of it.
    pub fn history_edge(&self) -> Option<HistoryEdge> {
        if self.history_loading {
            Some(HistoryEdge::Loading)
        } else if self.history_more {
            Some(HistoryEdge::More)
        } else if self.history_paged {
            Some(HistoryEdge::Start)
        } else {
            None
        }
    }

    /// The questions the agent is waiting on, once its turn has finished, as the web UI
    /// offers them only then.
    fn pending_question(&self) -> Option<question::PendingQuestion> {
        if self.is_running() {
            return None;
        }
        question::pending(&self.transcript)
    }

    /// Whether Esc hid the menu of questions the agent is still waiting on.
    pub fn questions_hidden(&self) -> bool {
        let Some(pending) = self.pending_question() else {
            return false;
        };
        self.answering
            .as_ref()
            .is_some_and(|a| a.call_id == pending.call_id && a.closed && a.sent.is_none())
    }

    /// The question menu to show, while a question set is pending and not closed.
    pub fn question_menu(&self) -> Option<QuestionMenu> {
        let pending = self.pending_question()?;
        let state = self
            .answering
            .as_ref()
            .filter(|a| a.call_id == pending.call_id);
        if state.is_some_and(|a| a.closed) {
            return None;
        }
        let index = state.map_or(0, |a| a.index);
        let q = pending.questions.get(index)?;
        Some(QuestionMenu {
            number: index + 1,
            count: pending.questions.len(),
            header: q.header.clone(),
            question: q.question.clone(),
            options: q.options.clone(),
            selected: state.map_or(0, |a| a.selected),
        })
    }

    fn question_key(&mut self, key: QuestionKey) -> Vec<Effect> {
        let Some(pending) = self.pending_question() else {
            self.answering = None;
            return vec![];
        };
        if self
            .answering
            .as_ref()
            .is_none_or(|a| a.call_id != pending.call_id)
        {
            self.answering = Some(Answering::new(pending.call_id.clone()));
        }
        let Some(state) = self.answering.as_mut() else {
            return vec![];
        };
        if state.closed {
            // Answers already sent stay closed; only a set hidden with Esc shows again.
            if key == QuestionKey::Show && state.sent.is_none() {
                state.closed = false;
            }
            return vec![];
        }
        let Some(q) = pending.questions.get(state.index) else {
            return vec![];
        };
        let other = q.options.len();
        match key {
            QuestionKey::Up => state.selected = state.selected.saturating_sub(1),
            QuestionKey::Down => state.selected = (state.selected + 1).min(other),
            QuestionKey::Dismiss => state.closed = true,
            QuestionKey::Show => {}
            QuestionKey::Back => {
                if let Some(previous) = state.index.checked_sub(1) {
                    state.show(&pending.questions, previous);
                }
            }
            QuestionKey::Enter if state.selected == other => {
                // Going back to an "Other" answer keeps its text, as the web UI does.
                let earlier = match state.answers.get(state.index) {
                    Some(Answer::Other(text)) => text.as_str(),
                    _ => "",
                };
                self.editor = Some(Editor {
                    target: EditTarget::Other,
                    line: LineEdit::new(earlier),
                    loading: false,
                });
            }
            QuestionKey::Enter => {
                let label = q.options[state.selected].label.clone();
                return self.record_answer(Answer::Choice(label));
            }
        }
        vec![]
    }

    /// Records the answer to the current question, and sends every answer after the last.
    fn record_answer(&mut self, answer: Answer) -> Vec<Effect> {
        let Some(pending) = self.pending_question() else {
            return vec![];
        };
        let Some(chat) = self.chat_id else {
            return vec![];
        };
        let Some(state) = self
            .answering
            .as_mut()
            .filter(|a| a.call_id == pending.call_id)
        else {
            return vec![];
        };
        match state.answers.get_mut(state.index) {
            Some(earlier) => *earlier = answer,
            None => state.answers.push(answer),
        }
        if state.index + 1 < pending.questions.len() {
            state.show(&pending.questions, state.index + 1);
            return vec![];
        }
        let text = question::answer_text(&pending.questions, &state.answers);
        state.closed = true;
        state.sent = Some(text.clone());
        let turn = self.turn();
        let effects = self.send(chat, text, turn);
        // A refused send (an archived chat) restores the text, so the menu starts over too.
        if !matches!(effects.as_slice(), [Effect::SendMessage { .. }]) {
            self.answering = None;
        }
        effects
    }

    /// Runs an action from the `/git` panel, which stays open: opening the pull request, or
    /// paging the diff once it has loaded.
    fn git_action(&mut self, action: GitAction) -> Vec<Effect> {
        match action {
            GitAction::OpenPr => {
                let status = self.chat.as_ref().and_then(|c| c.diff_status.as_ref());
                let from_diff = match self.git_panel.as_ref().map(|p| &p.diff) {
                    Some(Fetched::Loaded(d)) => d.pull_request_url.clone(),
                    _ => None,
                };
                match from_diff
                    .or_else(|| status.and_then(|s| s.url.clone()))
                    .filter(|u| !u.is_empty())
                {
                    // M1.6's link opener: a browser, or a copy when none opens.
                    Some(url) => vec![Effect::OpenLink(url)],
                    None => {
                        self.info("This chat has no pull request yet.");
                        vec![]
                    }
                }
            }
            GitAction::ViewDiff => {
                let Some(panel) = self.git_panel.as_ref() else {
                    return vec![];
                };
                if matches!(panel.diff, Fetched::Loading) {
                    self.info("The diff is still loading.");
                    return vec![];
                }
                let text = crate::panels::diff_text(panel);
                self.page(text)
            }
        }
    }

    /// Fetches the chat's diff as the newest request, so earlier replies are dropped.
    fn fetch_diff(&mut self, chat: Uuid) -> Effect {
        self.diff_generation += 1;
        Effect::FetchDiff {
            chat,
            generation: self.diff_generation,
        }
    }

    /// Hands `text` to the pager, or says there is nothing to page. An empty diff never
    /// starts the pager.
    fn page(&mut self, text: Option<String>) -> Vec<Effect> {
        match text {
            Some(text) => vec![Effect::Page(text)],
            None => {
                self.info("No git changes for this chat yet.");
                vec![]
            }
        }
    }

    /// Runs an action from the `/workspace` panel. Every action closes the panel, so details
    /// that arrive afterward are dropped.
    fn workspace_action(&mut self, action: WorkspaceAction) -> Vec<Effect> {
        let loaded = self.workspace_panel.take();
        match action {
            WorkspaceAction::CopySsh | WorkspaceAction::OpenWeb => {
                let ws = match loaded {
                    Some(Fetched::Loaded(ws)) => ws,
                    Some(Fetched::Failed(message)) => {
                        self.error(format!(
                            "The workspace details could not load: {message}. /workspace retries."
                        ));
                        return vec![];
                    }
                    Some(Fetched::Loading) | None => {
                        self.info(
                            "The workspace details have not loaded yet. Run /workspace again.",
                        );
                        return vec![];
                    }
                };
                let name = ws.name.clone().unwrap_or_default();
                let owner = ws.owner_name.clone().unwrap_or_default();
                if action == WorkspaceAction::OpenWeb {
                    return vec![Effect::OpenWorkspaceWeb {
                        owner,
                        workspace: name,
                    }];
                }
                let agent =
                    panels::workspace_agent(&ws, self.chat.as_ref().and_then(|c| c.agent_id));
                let agents = panels::workspace_agent_names(&ws);
                // `coder ssh owner/workspace` is refused when the workspace has several agents.
                if agent.is_none() && agents.len() > 1 {
                    self.info(format!(
                        "This workspace has several agents ({}) and the chat names none. Run coder ssh {owner}/{name}.<agent> with one of them.",
                        agents.join(", ")
                    ));
                    return vec![];
                }
                let suffix = self.ssh_suffix.clone().flatten();
                vec![Effect::CopyText {
                    text: panels::ssh_command(agent.as_deref(), &name, &owner, suffix.as_deref()),
                    what: "the SSH command",
                }]
            }
            WorkspaceAction::Detach => self.set_workspace(None),
            WorkspaceAction::Switch => self.workspace_table(),
        }
    }

    /// Opens the workspace table, retrying a list that failed to load.
    fn workspace_table(&mut self) -> Vec<Effect> {
        let mut effects = vec![Effect::ShowPicker(Picker::Workspace)];
        effects.extend(self.retry_workspaces());
        effects
    }

    /// Fetches the workspace list again when it failed to load; a list loading or loaded
    /// stays as it is.
    fn retry_workspaces(&mut self) -> Vec<Effect> {
        let failed = matches!(self.workspaces_state, WorkspacesState::Failed(_));
        let Some(org) = self.lists_org.filter(|_| failed) else {
            return vec![];
        };
        self.workspaces_state = WorkspacesState::Loading;
        vec![Effect::FetchWorkspaces(org)]
    }

    fn set_workspace(&mut self, ws: Option<Uuid>) -> Vec<Effect> {
        self.selected_workspace = ws;
        match self.chat_id {
            Some(chat) => {
                self.workspace_pending = Some(ws);
                vec![Effect::SetWorkspace {
                    chat,
                    workspace: ws,
                }]
            }
            None => vec![],
        }
    }

    fn effort_command(&mut self, level: Option<String>) -> Vec<Effect> {
        if self.efforts().is_empty() {
            let name = self
                .model_name()
                .unwrap_or_else(|| "The current model".into());
            self.info(format!("{name} has no reasoning effort levels."));
            return vec![];
        }
        match level {
            None => vec![Effect::ShowPicker(Picker::Effort)],
            Some(level) => self.update(Msg::EffortChosen(level)),
        }
    }

    /// Sends "Implement the plan." carrying `plan_mode`, which then holds the plan-mode queue.
    /// Returns `None` for an archived chat, after saying why. The composer never held this
    /// text, so neither this refusal nor a failed send restores it.
    fn send_implement(&mut self, chat: Uuid, plan_mode: Option<bool>) -> Option<Vec<Effect>> {
        if self.is_archived() {
            self.error("This chat is archived. Ctrl+A in /chats unarchives it.");
            return None;
        }
        let plan_generation = plan_mode
            .map(|on| self.begin_plan_request(on))
            .unwrap_or_default();
        let turn = TurnOptions {
            plan_mode,
            plan_generation,
            ..self.turn()
        };
        let effects = self.send(chat, "Implement the plan.".into(), turn);
        for effect in &effects {
            if let Effect::SendMessage { seq, .. } = effect {
                self.implement_seqs.insert(*seq);
            }
        }
        Some(effects)
    }

    /// Refetches `chat`'s record as the newest refresh.
    fn refresh_chat(&mut self, chat: Uuid) -> Effect {
        self.refresh_generation += 1;
        Effect::RefreshChat {
            chat,
            generation: self.refresh_generation,
        }
    }

    /// Marks a change to `on` as the one in flight and returns its generation.
    fn begin_plan_request(&mut self, on: bool) -> u64 {
        self.plan_generation += 1;
        self.plan_request = Some(on);
        self.plan_generation
    }

    /// Sends plan mode `on`, or, while a change is in flight, remembers it for afterwards.
    fn request_plan_mode(&mut self, chat: Uuid, on: bool) -> Vec<Effect> {
        if self.plan_request.is_some() {
            self.plan_wanted = Some(on);
            return vec![];
        }
        let generation = self.begin_plan_request(on);
        vec![Effect::SetPlanMode {
            chat,
            on,
            generation,
        }]
    }

    /// Ends the plan mode change to `on` in flight and starts what waited on it: a held
    /// `/implement`, then the latest state asked for, else a refetch, since the server
    /// publishes nothing and only its copy of the chat says which state holds. A failed change
    /// may still have landed, so a later different choice is sent either way.
    fn plan_settled(&mut self, on: bool, applied: bool) -> Vec<Effect> {
        self.plan_request = None;
        self.refresh_floor = self.refresh_generation;
        self.plan_failed |= !applied;
        let wanted = self.plan_wanted.take();
        let Some(chat) = self.chat_id else {
            return vec![];
        };
        let mut wanted = wanted;
        if std::mem::take(&mut self.implement_held) {
            let on = wanted.unwrap_or(false);
            if let Some(effects) = self.send_implement(chat, Some(on)) {
                return effects;
            }
            // The chat was archived meanwhile: the message cannot go, but the change can.
            self.awaiting_reply = false;
            wanted = Some(on);
        }
        match wanted {
            Some(wanted) if wanted != on => self.request_plan_mode(chat, wanted),
            _ => vec![self.refresh_chat(chat)],
        }
    }

    fn plan_mode_command(&mut self, wanted: Option<bool>) -> Vec<Effect> {
        // Loading replaces `plan_mode` with the chat's own, which would drop this change.
        if self.loading.is_some() {
            self.info("Wait for the chat to load, then set plan mode.");
            return vec![];
        }
        if self.chat_id.is_none() && self.failed_load.is_some() {
            self.info("The chat did not load. Send a message to retry, then set plan mode.");
            return vec![];
        }
        // The held message turns plan mode off, so a change now would contradict it.
        if self.implement_held {
            self.info("Wait for the plan to start implementing, then set plan mode.");
            return vec![];
        }
        let on = wanted.unwrap_or(!self.plan_mode);
        let word = if on { "on" } else { "off" };
        if on == self.plan_mode {
            self.info(format!("Plan mode is already {word}."));
            return vec![];
        }
        self.plan_mode = on;
        match self.chat_id {
            Some(chat) => {
                self.info(format!("Plan mode {word}."));
                self.request_plan_mode(chat, on)
            }
            None if self.creating.is_some() => {
                self.info(format!(
                    "Plan mode {word}; it applies once the chat is created."
                ));
                vec![]
            }
            None => {
                self.info(format!("Plan mode {word} for the new chat."));
                vec![]
            }
        }
    }

    /// Retries the organization lookup once at a time.
    fn retry_organizations(&mut self) -> Vec<Effect> {
        if self.orgs_retry {
            self.info("Still loading your organizations.");
            return vec![];
        }
        self.orgs_retry = true;
        self.info("Retrying your organizations.");
        vec![Effect::FetchOrganizations]
    }

    fn organization_command(&mut self, name: Option<String>) -> Vec<Effect> {
        if self.organizations.is_empty() {
            return self.retry_organizations();
        }
        if let [only] = self.organizations.as_slice() {
            let label = only.label().to_owned();
            self.info(format!("You belong to one organization, {label}."));
            return vec![];
        }
        if self.creating.is_some() {
            self.info("The chat is still being created.");
            return vec![];
        }
        let Some(name) = name else {
            return vec![Effect::ShowPicker(Picker::Organization)];
        };
        let wanted = name.to_lowercase();
        let found = self
            .organizations
            .iter()
            .find(|o| o.name.to_lowercase() == wanted || o.display_name.to_lowercase() == wanted)
            .map(|o| o.id);
        match found {
            Some(id) => self.update(Msg::OrganizationChosen(id)),
            None => {
                self.error(format!("No organization named {name:?}"));
                vec![]
            }
        }
    }

    /// Drops the older page on its way, if any, so its reply no longer applies, and the wait
    /// after a failed one.
    fn end_older_load(&mut self) {
        self.history_loading = false;
        self.history_failed = false;
        self.history_generation += 1;
        if self.jump.take().is_some() {
            self.info(STOPPED_LOOKING);
        }
    }

    /// The open chat's reply to a stream event the transcript applied as `applied`.
    fn applied_stream(&mut self, ev: StreamEvent, applied: Applied) -> Vec<Effect> {
        match applied {
            Applied::Reconnect(_) => match self.chat_id {
                Some(chat) => {
                    self.reconnect_attempt += 1;
                    self.transcript.live.clear();
                    self.connection = Connection::Reconnecting {
                        attempt: self.reconnect_attempt,
                    };
                    let delay = reconnect_delay(self.reconnect_attempt, true);
                    vec![self.reconnect(chat, delay)]
                }
                None => vec![],
            },
            _ => {
                self.connection = Connection::Live;
                self.last_stream_error = None;
                if self.awaiting_reply && self.ends_wait(&ev) {
                    self.awaiting_reply = false;
                }
                vec![]
            }
        }
    }

    /// Whether an older page's reply answers the page on its way now.
    fn older_reply_applies(&self, generation: u64) -> bool {
        self.history_loading && generation == self.history_generation
    }

    /// Whether a load reply for `id` still answers something: the load in flight, or, with none
    /// in flight, a blank screen.
    fn load_reply_applies(&self, id: Uuid) -> bool {
        match self.loading {
            Some(loading) => loading == id,
            None => self.chat_id.is_none(),
        }
    }

    /// Clears what belongs to the open chat and closes its stream, as a launch with no chat ID
    /// starts. The chat itself, running or not, is left alone on the server, and text waiting
    /// on a superseded load goes back to the composer. Every per-chat field is cleared here, so
    /// `/new` and opening a chat cannot drift apart. The one exception is the one-line editor,
    /// which stays open: a `/chats` rename edits a list row, and a `/title` proposal comes back
    /// in `Msg::ForChat`, so one for the chat left never fills it.
    fn reset_chat_state(&mut self) -> Vec<Effect> {
        let mut effects = self.close_preview();
        effects.extend([self.close_stream(), Effect::ClearView]);
        if self.git_panel.take().is_some() {
            effects.push(Effect::CloseGitWatch);
        }
        // Text waiting on uploads belongs to this chat too, so it never goes to the next one.
        let restored = [
            self.pending_text.take(),
            self.waiting_send.take(),
            self.model_held.take(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        if !restored.is_empty() {
            effects.push(Effect::RestoreComposer(restored.join("\n\n")));
        }
        // An upload still in flight stops, and a reply already on its way finds no chip.
        effects.extend(
            self.chips
                .iter()
                .filter(|c| c.state == ChipState::Uploading)
                .map(|c| Effect::CancelUpload(c.local)),
        );
        if !self.chips.is_empty() {
            let pastes: Vec<String> = self
                .chips
                .iter()
                .filter(|c| c.pasted.is_some())
                .map(|c| c.name.clone())
                .collect();
            let names: Vec<String> = self.chips.drain(..).map(|c| c.name).collect();
            let mut notice = format!("The attachments were cleared: {}.", names.join(", "));
            // A paste's text lives only in its chip, so the notice says it is gone.
            if !pastes.is_empty() {
                notice.push_str(&format!(
                    " The pasted text in {} was not sent; paste it again to send it.",
                    pastes.join(", ")
                ));
            }
            self.info(notice);
        }
        self.returning_to = None;
        self.workspace_pending = None;
        self.hold_notice = None;
        self.model_warned = None;
        self.chat_id = None;
        self.chat = None;
        self.loading = None;
        self.failed_load = None;
        self.transcript = Transcript::default();
        self.history_more = false;
        self.jump = None;
        self.history_paged = false;
        self.end_older_load();
        self.connection = Connection::Idle;
        self.last_stream_error = None;
        self.reconnect_attempt = 0;
        self.awaiting_reply = false;
        self.sent_id = None;
        self.answering = None;
        // A chat brings its own workspace and plan mode; a new one has none until chosen.
        self.selected_workspace = None;
        self.plan_mode = false;
        self.plan_request = None;
        self.plan_wanted = None;
        self.implement_held = false;
        self.plan_failed = false;
        self.plan_refresh_on_load = false;
        self.info_panel = None;
        self.chat_cost = None;
        self.workspace_panel = None;
        self.mcp_panel = None;
        self.mcp_next = None;
        self.mcp_sends.clear();
        self.mcp_newest = 0;
        self.page_when_loaded = false;
        effects
    }

    /// Previews `target` in place of any other preview, or closes the preview with `None`.
    /// Reselecting the subagent already previewed keeps its stream, and the open chat is
    /// never previewed, since its own stream already shows it. The server marks a chat read
    /// whenever its stream connects, so the list marks the previewed subagent read too.
    fn preview_chat(&mut self, target: Option<Uuid>) -> Vec<Effect> {
        if target.is_some() && self.preview.as_ref().map(|p| p.chat) == target {
            return vec![];
        }
        let mut effects = self.close_preview();
        if let Some(chat) = target.filter(|id| self.chat_id != Some(*id)) {
            self.chats.set_read(chat, true);
            self.preview_generation += 1;
            self.preview = Some(Preview {
                chat,
                generation: self.preview_generation,
                ..Preview::default()
            });
            effects.push(Effect::OpenPreview {
                chat,
                after_id: None,
                delay: Duration::ZERO,
                generation: self.preview_generation,
            });
        }
        effects
    }

    /// Drops the preview and stops its stream; the bump makes anything it queued stale.
    fn close_preview(&mut self) -> Vec<Effect> {
        match self.preview.take() {
            Some(_) => {
                self.preview_generation += 1;
                vec![Effect::ClosePreview]
            }
            None => vec![],
        }
    }

    /// Applies a preview stream message, reconnecting the preview with the chat stream's
    /// backoff when its stream ends or skips. Only the preview changes: the open chat's
    /// transcript, wait, connection, and read state belong to the main stream.
    fn apply_preview(&mut self, msg: Msg) -> Vec<Effect> {
        let Some(preview) = self.preview.as_mut() else {
            return vec![];
        };
        let delay = match msg {
            Msg::Stream(ev) => match preview.transcript.apply(&ev) {
                Applied::Reconnect(_) => {
                    preview.attempt += 1;
                    reconnect_delay(preview.attempt, true)
                }
                _ => {
                    preview.error = None;
                    return vec![];
                }
            },
            Msg::StreamEnded { error } => {
                preview.error = Some(error.unwrap_or_else(|| "the stream closed".into()));
                preview.attempt += 1;
                reconnect_delay(preview.attempt, false)
            }
            Msg::StreamHealthy => {
                preview.attempt = 0;
                return vec![];
            }
            _ => return vec![],
        };
        preview.transcript.live.clear();
        self.preview_generation += 1;
        preview.generation = self.preview_generation;
        vec![Effect::OpenPreview {
            chat: preview.chat,
            after_id: preview.transcript.last_message_id(),
            delay,
            generation: self.preview_generation,
        }]
    }

    /// The `/mcp` panel, when a reply with `generation` answers its latest open.
    fn current_mcp(&mut self, generation: u64) -> Option<&mut McpPanel> {
        if generation != self.mcp_generation {
            return None;
        }
        self.mcp_panel.as_mut()
    }

    /// The user's AI spend this period.
    pub fn spend(&self) -> &LimitState<Box<types::CodersdkUserAiSpendStatus>> {
        &self.spend
    }

    /// The user's workspace quota in the organization in view; `Unknown` while the quota on
    /// hand was asked for another organization.
    pub fn quota(&self) -> &LimitState<types::CodersdkWorkspaceQuota> {
        if self.quota_org == self.current_org() {
            &self.quota
        } else {
            &NO_QUOTA
        }
    }

    /// Whether a `401` stopped the limits and the cost from refreshing until restart.
    pub fn limits_stopped(&self) -> bool {
        self.limits_stopped
    }

    /// Whether a refresh of the limits would ask for either: not after a `401`, nor once the
    /// deployment lacks or refuses both, since neither changes while scuttle runs.
    pub fn limits_refresh(&self) -> bool {
        !self.limits_stopped && (self.spend.refreshes() || self.quota.refreshes())
    }

    /// Stops the cost, spend, and quota refreshes after a rejected session token, saying so
    /// once.
    fn stop_limits(&mut self) {
        if !self.limits_stopped {
            self.limits_stopped = true;
            self.error(LIMITS_STOPPED);
        }
    }

    /// How many turns of the open chat ended, so the UI can refresh the limits after each.
    pub fn turns_ended(&self) -> u64 {
        self.turns_ended
    }

    /// Asks for the AI spend and the quota of the organization in view under a new
    /// generation, so a reply to an earlier refresh is dropped. A limit the deployment lacks
    /// or does not license is not asked for again, and nothing is after a `401`.
    fn refresh_limits(&mut self) -> Vec<Effect> {
        if self.limits_stopped {
            return vec![];
        }
        self.limits_generation += 1;
        let generation = self.limits_generation;
        let mut effects = Vec::new();
        if self.spend.refreshes() {
            effects.push(Effect::FetchSpend { generation });
        }
        let org = self.current_org();
        if org != self.quota_org {
            // Another organization's credits or failure say nothing about this one, but a
            // missing route or license holds for the whole deployment. A 404 for an
            // organization the user cannot read arrives as `Failed`, so it is asked again.
            if matches!(self.quota, LimitState::Loaded(_) | LimitState::Failed(_)) {
                self.quota = LimitState::Unknown;
            }
            self.quota_org = org;
        }
        if let Some(org) = org
            && self.quota.refreshes()
        {
            effects.push(Effect::FetchQuota { org, generation });
        }
        effects
    }

    /// Asks for the open chat's cost under a new generation, unless the server refused it.
    fn fetch_chat_cost(&mut self) -> Vec<Effect> {
        let Some(chat) = self.chat_id else {
            return vec![];
        };
        if matches!(self.chat_cost, Some(CostState::Hidden)) {
            return vec![];
        }
        if self.chat_cost.is_none() {
            self.chat_cost = Some(CostState::Loading);
        }
        self.cost_generation += 1;
        vec![Effect::FetchCost {
            chat,
            generation: self.cost_generation,
        }]
    }

    /// Replaces the open chat's record with a refetched `chat`. A snapshot read `before_change`
    /// to plan mode or the MCP selection keeps both as they are here, and leaves a failed
    /// change for the refetch that follows it to report.
    fn apply_refresh(&mut self, mut chat: Box<types::CodersdkChat>, before_change: bool) {
        if chat.id.is_none() || chat.id != self.chat_id {
            return;
        }
        if before_change {
            if let Some(local) = self.chat.as_deref() {
                chat.plan_mode = local.plan_mode.clone();
                chat.mcp_server_ids = local.mcp_server_ids.clone();
            }
        } else if self.plan_request.is_none() {
            // A refetch that a change in flight could still overtake says nothing about plan
            // mode.
            self.plan_mode = is_plan(&chat);
            if std::mem::take(&mut self.plan_failed) {
                let word = if self.plan_mode { "on" } else { "off" };
                self.info(format!("Plan mode is {word}."));
            }
        }
        self.adopt_workspace(chat.workspace_id);
        self.chat = Some(chat);
    }

    /// Follows the server's `workspace` for the open chat, as when the agent binds one with
    /// `create_workspace`, unless a change made here has not reached the server yet.
    fn adopt_workspace(&mut self, workspace: Option<Uuid>) {
        match self.workspace_pending {
            Some(pending) if pending != workspace => {}
            _ => {
                self.workspace_pending = None;
                self.selected_workspace = workspace;
            }
        }
    }

    /// Whether the open chat is archived, which makes it read-only.
    pub fn is_archived(&self) -> bool {
        self.chat.as_ref().is_some_and(|c| c.archived == Some(true))
    }

    /// Merges a watch event into the list and, for the open chat, into its record. The chat
    /// stream owns the open chat's transcript and status, so the watch never touches them.
    fn apply_watch(&mut self, ev: coder_sdk::WatchEvent) -> Vec<Effect> {
        let Some(chat) = ev.event.and_then(|e| e.chat) else {
            return vec![];
        };
        self.chats.apply_watch(&ev.kind, &chat, self.chat_id);
        let Some(open) = self.chat_id else {
            return vec![];
        };
        let mut effects = Vec::new();
        // Cost covers the whole chat tree, so any family member's turn changes it, for `/info`,
        // `/usage`, and the footer. The root is resolved as the web UI and the server do. As
        // with `RefreshCost`, a refused cost or a rejected token is not asked for again.
        let root = |c: &types::CodersdkChat| c.root_chat_id.or(c.parent_chat_id).or(c.id);
        let open_root = self.chat.as_deref().and_then(root);
        if ev.kind == "status_change"
            && (self.info_panel.is_some() || self.cost_in_footer || self.usage_open)
            && !self.limits_stopped
            && open_root.is_some()
            && root(&chat) == open_root
        {
            effects.extend(self.fetch_chat_cost());
        }
        if chat.id != Some(open) {
            return effects;
        }
        let Some(record) = self.chat.as_mut() else {
            return effects;
        };
        // As in the list, `updated_at` guards the binding, so a late event cannot undo a newer
        // one, and an event without a workspace leaves it.
        if chat.updated_at >= record.updated_at {
            record.workspace_id = chat.workspace_id.or(record.workspace_id);
            record.updated_at = chat.updated_at;
            let workspace = record.workspace_id;
            self.adopt_workspace(workspace);
        }
        let Some(record) = self.chat.as_mut() else {
            return effects;
        };
        match ev.kind.as_str() {
            "title_change" => record.title = chat.title.clone(),
            "diff_status_change" => {
                record.diff_status = chat.diff_status.clone();
                if self.git_panel.is_some() {
                    let fetch = self.fetch_diff(open);
                    effects.push(fetch);
                }
            }
            "deleted" => record.archived = Some(true),
            "created" => record.archived = Some(false),
            "context_dirty" => effects.push(self.refresh_chat(open)),
            _ => {}
        }
        effects
    }

    /// Refetches the main list's first page after a load of it lands, when the watch
    /// connected while that load was in flight.
    fn refetch_if_waiting(&mut self, query: &ListQuery) -> Vec<Effect> {
        if *query != ListQuery::Default || !std::mem::take(&mut self.refetch_after_load) {
            return vec![];
        }
        self.load_chats(ListQuery::Default, false)
    }

    /// Starts loading the first page of `query`, or with `more` the next one, unless a load of
    /// it is in flight or nothing is left.
    fn load_chats(&mut self, query: ListQuery, more: bool) -> Vec<Effect> {
        match self.chats.begin_load(&query, more) {
            Some(offset) => vec![Effect::FetchChats { query, offset }],
            None => vec![],
        }
    }

    /// Opens the existing chat `id` in place of the open one and of any load in flight.
    fn open_chat(&mut self, id: Uuid) -> Vec<Effect> {
        // The create reply would otherwise open the chat the user just left.
        if self.creating.is_some() {
            self.info("Wait for this chat to finish starting, then open another.");
            return vec![];
        }
        // Reopening the chat being loaded would drop the message queued for it.
        if self.chat_id == Some(id) || self.loading == Some(id) {
            return vec![];
        }
        let mut effects = self.reset_chat_state();
        // The opened chat's last model and effort apply, not a choice made for another chat.
        self.selected_model = None;
        self.selected_effort = None;
        self.chats.set_read(id, true);
        self.loading = Some(id);
        self.connection = Connection::Connecting;
        effects.push(Effect::LoadChat(id));
        effects
    }

    /// The open chat's workspace skills that loaded cleanly, first of each name.
    fn workspace_skills(&self) -> Vec<Skill> {
        let mut out: Vec<Skill> = Vec::new();
        let resources = self
            .chat
            .as_ref()
            .and_then(|c| c.context.as_ref())
            .map(|c| c.resources.as_slice())
            .unwrap_or_default();
        for r in resources {
            let skill = r.kind.as_ref().is_some_and(|k| k.as_str() == "skill");
            let ok = r.status.as_ref().is_some_and(|s| s.as_str() == "ok");
            let Some(name) = r.skill_name.clone().filter(|_| skill && ok) else {
                continue;
            };
            if !out.iter().any(|s| s.name == name) {
                out.push(Skill {
                    name,
                    description: r.skill_description.clone().unwrap_or_default(),
                });
            }
        }
        out
    }

    /// The slash menu for the composer.
    pub fn slash_menu(&self) -> Vec<MenuEntry> {
        let (personal, note) = match &self.personal_skills {
            SkillsLoad::Loading => (Vec::new(), Some("Loading skills…".to_owned())),
            SkillsLoad::Loaded(list) => (list.clone(), None),
            SkillsLoad::Failed(message) => (
                Vec::new(),
                Some(format!("Skills are unavailable: {message}")),
            ),
        };
        skills::menu(
            &personal,
            &self.workspace_skills(),
            self.me.as_ref().map(|m| m.username.as_str()),
            note.as_deref(),
        )
    }

    /// The slash menu's entries for `command`'s argument, by the command's name, for each
    /// command in `skills::ARGUMENT_COMMANDS`; empty for any other command.
    pub fn argument_menu(&self, command: &str) -> Vec<MenuEntry> {
        match command {
            "/workspace" => self.workspace_arguments(),
            _ => Vec::new(),
        }
    }

    /// The names in the workspace list, most recently used first, each described by its
    /// status and template, then `none`, then a note while the list loads or after it failed.
    fn workspace_arguments(&self) -> Vec<MenuEntry> {
        let mut entries: Vec<MenuEntry> = self
            .workspaces
            .iter()
            .map(|w| {
                let about: Vec<&str> = [w.status.as_str(), w.template.as_str()]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect();
                skills::argument(&w.name, &about.join(" · "))
            })
            .collect();
        entries.push(skills::argument("none", "Detach the workspace"));
        match &self.workspaces_state {
            WorkspacesState::Loading => entries.push(skills::note_entry("Loading workspaces…")),
            WorkspacesState::Failed(message) => entries.push(skills::note_entry(&format!(
                "Workspaces failed to load: {}",
                files::display_name(message)
            ))),
            WorkspacesState::Loaded => {}
        }
        entries
    }

    /// Returns to the state of a launch with no chat ID. The old chat's stream closes, but the
    /// chat itself, running or not, is left alone on the server. The composer text, the chosen
    /// model, and the chosen effort stay, unless the organization for new chats changed.
    fn new_chat(&mut self) -> Vec<Effect> {
        // The reply to the create or load would reopen that chat.
        if self.creating.is_some() || self.loading.is_some() {
            self.info("Wait for this chat to finish starting, then use /new.");
            return vec![];
        }
        if self.chat_id.is_none() && self.failed_load.is_none() {
            if self.org_id.is_none() {
                return self.retry_organizations();
            }
            self.info("This is already a new chat.");
            return vec![];
        }
        // The chat's own model does not carry over when the server would refuse it. The new
        // chat names no model, so the server applies the user's own default or the
        // deployment's.
        if let Some(gone) = self.unavailable_model() {
            self.selected_model = None;
            // Only the default the model list names can be checked; the user's own default
            // is known to the server alone, so the notice promises nothing about it.
            let next = match self
                .current_model()
                .filter(|m| self.provider_off(m))
                .map(|m| m.display_name.clone().or_else(|| m.model.clone()))
            {
                Some(Some(name)) => format!(
                    "The default model, {name}, is not available either. Pick one with /model."
                ),
                Some(None) => {
                    "The default model is not available either. Pick one with /model.".to_owned()
                }
                None => "The server picks the new chat's model.".to_owned(),
            };
            self.error(format!("{} {next}", gone.sentence()));
        }
        let mut effects = self.reset_chat_state();
        if let Some(org) = self.org_id {
            effects.extend(self.load_lists_for(org));
        }
        if self.org_id.is_none() {
            effects.extend(self.retry_organizations());
        }
        // Skills added since startup show in the new chat's menu.
        effects.push(Effect::FetchSkills);
        self.info("New chat. Type a message to start it.");
        effects
    }

    /// Marks `chat` archived or not in the list and the open chat. Archiving or unarchiving a
    /// root cascades to its whole family on the server, so its children are marked the same
    /// way locally before the list rules run, and a fresh snapshot (not one taken when the
    /// action started) goes through the same rules the watch socket uses, so a pin, a page
    /// membership, and the archived flag all land together.
    fn mark_archived(&mut self, chat: Uuid, archived: bool) {
        let open = self.chat_id == Some(chat);
        let child_ids: Vec<Uuid> = self
            .chats
            .find(chat)
            .map(|c| c.children.iter().filter_map(|ch| ch.id).collect())
            .unwrap_or_default();
        for child in child_ids {
            self.chats
                .update_copies(child, |c| c.archived = Some(archived));
        }
        let known = self
            .chats
            .find(chat)
            .cloned()
            .or_else(|| self.chat.as_deref().filter(|c| c.id == Some(chat)).cloned());
        if let Some(mut known) = known {
            known.archived = Some(archived);
            if archived {
                known.pin_order = Some(0);
            }
            let kind = if archived { "deleted" } else { "created" };
            self.chats.apply_watch(kind, &known, self.chat_id);
        }
        if open && let Some(c) = self.chat.as_mut() {
            c.archived = Some(archived);
        }
    }

    fn chat_action(&mut self, action: ChatAction) -> Vec<Effect> {
        let id = match &action {
            ChatAction::ToggleArchive(id)
            | ChatAction::Archive(id)
            | ChatAction::ArchiveAndDeleteWorkspace { chat: id, .. }
            | ChatAction::TogglePin(id)
            | ChatAction::ToggleRead(id)
            | ChatAction::Rename(id) => *id,
        };
        let open = self.chat.as_deref().filter(|c| c.id == Some(id)).cloned();
        let Some(chat) = self.chats.find(id).cloned().or(open) else {
            self.error("That chat is not loaded.");
            return vec![];
        };
        let child = chat.parent_chat_id.is_some();
        let running =
            self.chats.family_running(id) || (self.chat_id == Some(id) && self.is_running());
        let archived = chat.archived == Some(true);
        let archiving = matches!(
            action,
            ChatAction::ToggleArchive(_)
                | ChatAction::Archive(_)
                | ChatAction::ArchiveAndDeleteWorkspace { .. }
        );
        let change = match action {
            _ if archiving && child => {
                self.error("Only a root chat can be archived. Archive its parent.");
                return vec![];
            }
            _ if archiving && self.archiving.contains(&id) => {
                self.info("This chat is already being archived.");
                return vec![];
            }
            ChatAction::ToggleArchive(_) if archived => ChatChange::Archived(false),
            ChatAction::Archive(_) | ChatAction::ArchiveAndDeleteWorkspace { .. } if archived => {
                self.info("This chat is already archived.");
                return vec![];
            }
            _ if archiving && running => {
                self.error("Wait for this chat and its subagents to stop, then archive it.");
                return vec![];
            }
            ChatAction::ArchiveAndDeleteWorkspace { workspace, .. } => {
                match chat.workspace_id {
                    None => {
                        self.error("This chat has no workspace to delete.");
                        return vec![];
                    }
                    Some(current) if current != workspace => {
                        self.error("This chat's workspace changed. Open the archive box again.");
                        return vec![];
                    }
                    Some(_) => {}
                }
                self.archiving.insert(id);
                return vec![Effect::ArchiveAndDeleteWorkspace {
                    chat: id,
                    workspace,
                }];
            }
            ChatAction::ToggleArchive(_) | ChatAction::Archive(_) => {
                self.archiving.insert(id);
                ChatChange::Archived(true)
            }
            ChatAction::TogglePin(_) if child => {
                self.error("A subagent cannot be pinned.");
                return vec![];
            }
            ChatAction::TogglePin(_) => ChatChange::PinOrder(if chat.pin_order.unwrap_or(0) > 0 {
                0
            } else {
                // The server ignores this value and appends the chat to the end of the pinned
                // list, but the local sort needs a value that ranks it last among the pins
                // already loaded, the way the web UI guesses it
                // (`site/src/api/queries/chats.ts`, `getNextOptimisticPinOrder`).
                self.chats.max_pin_order() + 1
            }),
            ChatAction::ToggleRead(_) => ChatChange::Read(chat.has_unread == Some(true)),
            ChatAction::Rename(_) => {
                self.editor = Some(Editor {
                    target: EditTarget::Rename(id),
                    line: LineEdit::new(chat.title.as_deref().unwrap_or_default()),
                    loading: false,
                });
                return vec![];
            }
        };
        vec![Effect::UpdateChat { chat: id, change }]
    }

    fn edit(&mut self, edit: Edit) -> Vec<Effect> {
        let Some(editor) = self.editor.as_mut() else {
            return vec![];
        };
        if editor.loading && edit != Edit::Cancel {
            return vec![];
        }
        let outcome = editor.line.apply(edit);
        let target = editor.target.clone();
        match outcome {
            EditOutcome::Editing => vec![],
            EditOutcome::Cancelled => {
                self.editor = None;
                vec![]
            }
            // Validated before the editor closes, so a rejection (for example, the server's own
            // 200-rune title limit) never discards what the user typed: `LineEdit::apply` for
            // `Submit` only returns a trimmed copy and leaves the editor's own text untouched.
            EditOutcome::Submitted(text) => match Self::validate_edit(&target, &text)
                .map_err(Notice::Error)
                .and_then(|()| self.check_save_as(&target, &text))
            {
                Ok(()) => {
                    self.editor = None;
                    self.finish_edit(target, text)
                }
                Err(Notice::Info(message)) => {
                    self.info(message);
                    vec![]
                }
                Err(Notice::Error(message)) => {
                    self.error(message);
                    vec![]
                }
            },
        }
    }

    /// The server's own limits on a submitted edit (`coderd/exp_chats.go`,
    /// `applyChatTitleUpdate`): a title cannot be empty or hold more than 200 runes.
    fn validate_edit(target: &EditTarget, text: &str) -> Result<(), String> {
        match target {
            EditTarget::Rename(_) | EditTarget::Title(_) if text.is_empty() => {
                Err("A title cannot be empty.".into())
            }
            EditTarget::Rename(_) | EditTarget::Title(_) if text.chars().count() > 200 => {
                Err("A title must be at most 200 characters.".into())
            }
            EditTarget::Rename(_) | EditTarget::Title(_) => Ok(()),
            // The web UI accepts an "Other" answer only once it holds text.
            EditTarget::Other if text.is_empty() => {
                Err("Type an answer, or press Esc and pick an option.".into())
            }
            EditTarget::Other => Ok(()),
            EditTarget::SaveAs(_) if text.is_empty() => {
                Err("Type where to save the file, or press Esc.".into())
            }
            EditTarget::SaveAs(_) => Ok(()),
        }
    }

    fn finish_edit(&mut self, target: EditTarget, text: String) -> Vec<Effect> {
        match target {
            EditTarget::Rename(chat) | EditTarget::Title(chat) => vec![Effect::UpdateChat {
                chat,
                change: ChatChange::Title(text),
            }],
            EditTarget::Other => self.record_answer(Answer::Other(text)),
            EditTarget::SaveAs(file) => {
                let Some(row) = self.file_row(file) else {
                    self.error("That file is not in this chat.");
                    return vec![];
                };
                let Ok(path) = self.save_path(&text) else {
                    return vec![];
                };
                let name = files::safe_name(&row.name, &row.media_type);
                self.save_file(file, name, SaveTo::Typed(path))
            }
        }
    }

    /// Where a typed save path points: `~` and `~/` are the home directory, a relative path
    /// is inside the save directory, and `~user` is refused.
    fn save_path(&self, text: &str) -> Result<PathBuf, String> {
        if text.starts_with('~') && text != "~" && !text.starts_with("~/") {
            let user = text.split('/').next().unwrap_or(text);
            return Err(format!("Cannot expand {user}; type the full path."));
        }
        if text.starts_with('~') && self.home.is_none() {
            return Err("Cannot find your home directory; type the full path.".into());
        }
        let path = files::expand_home(text, self.home.as_deref());
        Ok(if path.is_relative() {
            self.save_dir.join(path)
        } else {
            path
        })
    }

    /// The checks a save-as submit repeats, since the file may have changed state while the
    /// editor was open.
    fn check_save_as(&self, target: &EditTarget, text: &str) -> Result<(), Notice> {
        let EditTarget::SaveAs(file) = target else {
            return Ok(());
        };
        let Some(row) = self.file_row(*file) else {
            return Err(Notice::Error("That file is not in this chat.".into()));
        };
        let shown = files::shown_name(&row.name);
        if row.expired {
            return Err(Notice::Error(Self::no_longer_available(shown)));
        }
        if self.files_busy.contains(file) {
            return Err(Notice::Info(format!("Still working on {shown}.")));
        }
        if self.save_conflict.is_some() {
            return Err(Notice::Error(OPEN_SAVE_QUESTION.into()));
        }
        self.save_path(text).map(|_| ()).map_err(Notice::Error)
    }

    fn no_longer_available(shown: &str) -> String {
        format!(
            "{shown} is no longer available: a chat keeps a limited number of files, and the oldest go first."
        )
    }

    /// The `/files` row of `file`.
    fn file_row(&self, file: Uuid) -> Option<files::FileRow> {
        files::file_rows(self.chat.as_deref(), &self.transcript)
            .into_iter()
            .find(|r| r.id == file)
    }

    /// Runs a `/files` key or a click on an attached file.
    fn file_action(&mut self, action: FileAction) -> Vec<Effect> {
        if let FileAction::Jump(file) = action {
            return self.jump_to_file(file);
        }
        let id = action.file();
        let Some(row) = self.file_row(id) else {
            self.error("That file is not in this chat.");
            return vec![];
        };
        let shown = files::shown_name(&row.name).to_owned();
        if row.expired {
            self.error(Self::no_longer_available(&shown));
            return vec![];
        }
        if self.files_busy.contains(&id) {
            self.info(format!("Still working on {shown}."));
            return vec![];
        }
        if self.save_conflict.is_some()
            && matches!(action, FileAction::Save(_) | FileAction::SaveAs(_))
        {
            self.error(OPEN_SAVE_QUESTION);
            return vec![];
        }
        let name = files::safe_name(&row.name, &row.media_type);
        match action {
            FileAction::Save(_) => self.save_file(id, name, SaveTo::Dir(self.save_dir.clone())),
            FileAction::SaveAs(_) => {
                let start = files::display_path(&self.save_dir.join(&name), self.home.as_deref());
                self.editor = Some(Editor {
                    target: EditTarget::SaveAs(id),
                    line: LineEdit::new(&start),
                    loading: false,
                });
                vec![]
            }
            FileAction::View(_) if !files::is_text(&row.media_type) => {
                self.info(format!(
                    "{shown} is not text, so it cannot be shown here. Press Enter to save it."
                ));
                vec![]
            }
            FileAction::View(_) => {
                self.files_busy.insert(id);
                if let Some(chat) = self.chat_id {
                    self.viewing.insert(id, chat);
                }
                vec![Effect::ReadFile { file: id, name }]
            }
            // Answered before the row was looked up.
            FileAction::Jump(_) => vec![],
        }
    }

    /// Saves `file` as `name` to `to`, asking first if the name is taken.
    fn save_file(&mut self, file: Uuid, name: String, to: SaveTo) -> Vec<Effect> {
        self.files_busy.insert(file);
        vec![Effect::SaveFile {
            file,
            name,
            to,
            conflict: OnConflict::Ask,
        }]
    }

    /// Answers the "name is taken" question. Only a letter answers it, never Enter, so a
    /// held key that asked the question cannot also answer it.
    fn conflict_answer(&mut self, choice: ConflictChoice) -> Vec<Effect> {
        let Some(c) = self.save_conflict.take() else {
            return vec![];
        };
        let conflict = match choice {
            ConflictChoice::KeepBoth => OnConflict::KeepBoth,
            ConflictChoice::Replace => OnConflict::Replace,
            ConflictChoice::Cancel => return vec![],
        };
        self.files_busy.insert(c.file);
        vec![Effect::SaveFile {
            file: c.file,
            name: c.name,
            to: SaveTo::File(c.path),
            conflict,
        }]
    }

    /// Scrolls to the message that carries `file`, loading older pages while the history may
    /// hold it.
    fn jump_to_file(&mut self, file: Uuid) -> Vec<Effect> {
        let Some(chat) = self.chat_id else {
            return vec![];
        };
        if let Some(id) = files::message_with(&self.transcript, file) {
            self.jump = None;
            return vec![Effect::ScrollToMessage(id)];
        }
        // A file only the live turn carries has no message yet; the turn is at the end of the
        // transcript, so no older page can hold it.
        if self
            .file_row(file)
            .is_some_and(|r| r.place == files::Place::Live)
        {
            self.jump = None;
            return vec![Effect::ScrollToLatest];
        }
        if self.history_more {
            self.jump = Some((chat, file));
            self.info("Loading older messages to find it.");
            return self.update(Msg::LoadOlder);
        }
        self.jump = None;
        self.info(NOT_IN_HISTORY);
        vec![]
    }

    /// Looks again for the file a jump is after, once an older page landed.
    fn resume_jump(&mut self) -> Vec<Effect> {
        let Some((chat, file)) = self.jump else {
            return vec![];
        };
        if self.chat_id != Some(chat) {
            self.jump = None;
            return vec![];
        }
        if let Some(id) = files::message_with(&self.transcript, file) {
            self.jump = None;
            return vec![Effect::ScrollToMessage(id)];
        }
        if self.history_more {
            return self.update(Msg::LoadOlder);
        }
        self.jump = None;
        self.info(NOT_IN_HISTORY);
        vec![]
    }

    fn command(&mut self, cmd: Command) -> Vec<Effect> {
        match cmd {
            Command::Model(_) | Command::Effort(_) if self.models_state == ModelsState::Loading => {
                self.info("Models are still loading.");
                vec![]
            }
            Command::Model(_) | Command::Effort(_) if self.models_state == ModelsState::Failed => {
                // Retry for the organization the lists belong to, so the tagged reply is applied.
                let Some(org) = self.lists_org.or(self.org_id) else {
                    self.info("Models are still loading.");
                    return vec![];
                };
                self.models_state = ModelsState::Loading;
                self.info("Retrying the model list.");
                vec![Effect::FetchModels(org)]
            }
            Command::Model(_) | Command::Effort(_) if self.no_models() => {
                let message = self.no_models_message();
                self.info(message);
                vec![]
            }
            Command::Model(None) => vec![Effect::ShowPicker(Picker::Model)],
            Command::Model(Some(name)) => {
                let wanted = name.to_lowercase();
                let found = self.models.iter().find(|m| {
                    m.display_name.as_deref().map(str::to_lowercase) == Some(wanted.clone())
                        || m.model.as_deref().map(str::to_lowercase) == Some(wanted.clone())
                });
                match found.and_then(|m| m.id) {
                    Some(id) => self.update(Msg::ModelChosen(id)),
                    None => {
                        self.error(format!("No enabled model named {name:?}"));
                        vec![]
                    }
                }
            }
            Command::Effort(level) => self.effort_command(level),
            Command::PlanMode(wanted) => self.plan_mode_command(wanted),
            // With a workspace attached, `/workspace` shows it; otherwise it lists them.
            Command::Workspace(None)
                if self.chat_id.is_some() && self.selected_workspace.is_some() =>
            {
                let (Some(chat), Some(workspace)) = (self.chat_id, self.selected_workspace) else {
                    return vec![];
                };
                self.workspace_panel = Some(Fetched::Loading);
                let mut effects = vec![
                    Effect::ShowWorkspace,
                    Effect::FetchWorkspaceDetails { chat, workspace },
                ];
                if self.ssh_suffix.is_none() {
                    effects.push(Effect::FetchSshSuffix);
                }
                effects
            }
            Command::Workspace(None) => self.workspace_table(),
            Command::Workspace(Some(name)) if name == "none" => self.set_workspace(None),
            Command::Workspace(Some(_)) if self.workspaces_state == WorkspacesState::Loading => {
                self.info("Workspaces are still loading.");
                vec![]
            }
            Command::Workspace(Some(_))
                if matches!(self.workspaces_state, WorkspacesState::Failed(_)) =>
            {
                if let WorkspacesState::Failed(message) = self.workspaces_state.clone() {
                    self.error(format!(
                        "Workspaces failed to load: {message}. /workspace retries."
                    ));
                }
                vec![]
            }
            Command::Workspace(Some(name)) => match self
                .workspaces
                .iter()
                .find(|w| w.name == name)
                .map(|w| w.id)
            {
                Some(id) => self.set_workspace(Some(id)),
                None => {
                    self.error(format!("No workspace named {name:?}"));
                    vec![]
                }
            },
            Command::Web | Command::Compact | Command::Clear if self.creating.is_some() => {
                self.info("The chat is still being created.");
                vec![]
            }
            // The chat being loaded, or the one that failed to load, already has a URL.
            Command::Web => match self.chat_id.or(self.loading).or(self.failed_load) {
                Some(chat) => vec![Effect::OpenWeb(chat)],
                None => {
                    self.error("Start a chat first.");
                    vec![]
                }
            },
            Command::Compact | Command::Clear => match self.chat_id {
                Some(chat) => vec![match cmd {
                    Command::Compact => Effect::Compact(chat),
                    _ => Effect::Clear(chat),
                }],
                None => {
                    self.error("Start a chat first.");
                    vec![]
                }
            },
            Command::Copy(None) => vec![Effect::Copy(CopyTarget::LastMessage)],
            Command::Copy(Some(n)) => vec![Effect::Copy(CopyTarget::CodeBlock(n))],
            Command::Mouse => {
                self.mouse = !self.mouse;
                vec![Effect::SetMouse(self.mouse)]
            }
            Command::Settings => vec![Effect::EditSettings],
            Command::Organization(name) => self.organization_command(name),
            Command::Chats(query) => {
                let mut effects = vec![Effect::ShowChats(query.unwrap_or_default())];
                // The watch keeps a loaded list current; without it, refetch the first page.
                let stale = !matches!(self.chats.main.load, crate::chat_list::Load::Loaded);
                if stale || !self.chats.watch_live {
                    effects.extend(self.load_chats(ListQuery::Default, false));
                }
                effects
            }
            Command::Subagents => {
                if self.chat_id.is_none() {
                    self.error("Start a chat first.");
                    return vec![];
                }
                let first = self.first_subagent();
                let mut effects = vec![Effect::ShowSubagents];
                effects.extend(self.preview_chat(first));
                effects
            }
            Command::Parent => match self.chat.as_ref().and_then(|c| c.parent_chat_id) {
                Some(parent) => {
                    let effects = self.open_chat(parent);
                    if effects.contains(&Effect::LoadChat(parent)) {
                        let title = self.chats.find(parent).and_then(|c| c.title.clone());
                        self.returning_to = Some((parent, title));
                    }
                    effects
                }
                None => {
                    self.info("This chat is not a subagent.");
                    vec![]
                }
            },
            Command::Title(text) => {
                let Some(chat) = self.chat_id else {
                    self.error("Start a chat first.");
                    return vec![];
                };
                match text {
                    Some(title) => match Self::validate_edit(&EditTarget::Title(chat), &title) {
                        Ok(()) => vec![Effect::UpdateChat {
                            chat,
                            change: ChatChange::Title(title),
                        }],
                        Err(message) => {
                            self.error(message);
                            vec![]
                        }
                    },
                    None => {
                        // Only one proposal is ever in flight per chat: while one is already
                        // loading, a repeated `/title` leaves it (and the request) alone
                        // instead of firing a second one.
                        let already_proposing = self.editor.as_ref().is_some_and(|e| {
                            e.loading && matches!(e.target, EditTarget::Title(c) if c == chat)
                        });
                        if already_proposing {
                            return vec![];
                        }
                        self.title_generation += 1;
                        let generation = self.title_generation;
                        self.editor = Some(Editor {
                            target: EditTarget::Title(chat),
                            line: LineEdit::default(),
                            loading: true,
                        });
                        vec![Effect::ProposeTitle { chat, generation }]
                    }
                }
            }
            Command::Queue => match self.chat_id {
                Some(_) => vec![Effect::ShowQueue],
                None => {
                    self.error("Start a chat first.");
                    vec![]
                }
            },
            Command::Implement => {
                let Some(chat) = self.chat_id else {
                    self.error("Start a chat first.");
                    return vec![];
                };
                if self.is_running() {
                    self.info("Wait for the turn to finish, then implement the plan.");
                    return vec![];
                }
                if !question::plan_proposed(&self.transcript) {
                    self.info("There is no proposed plan to implement.");
                    return vec![];
                }
                if self.is_archived() {
                    self.error("This chat is archived. Ctrl+A in /chats unarchives it.");
                    return vec![];
                }
                // As for a typed message, a model the server would refuse is changed first.
                if let Some(gone) = self.unavailable_model() {
                    self.error(format!(
                        "{} Pick another with /model, then run /implement again.",
                        gone.sentence()
                    ));
                    return vec![Effect::ShowPicker(Picker::Model)];
                }
                // The plan mode change rides on the message, as the web UI sends it. A change
                // in flight could land after it, so the message waits for that one to settle.
                if self.plan_request.is_some() {
                    self.plan_wanted = Some(false);
                    self.plan_mode = false;
                    if !std::mem::replace(&mut self.implement_held, true) {
                        self.start_wait();
                        self.info("Implementing once the plan mode change in flight is done.");
                    }
                    return vec![];
                }
                // With plan mode already off there is no change to carry, and a failed send
                // is settled by a refetch of the chat.
                let plan_mode = self.plan_mode.then_some(false);
                self.plan_mode = false;
                self.send_implement(chat, plan_mode).unwrap_or_default()
            }
            Command::Attach(path) => self.attach(path),
            Command::Files => match self.chat_id {
                // `chat.files` is a snapshot, so the list is read again as it opens.
                Some(chat) => vec![Effect::ShowFiles, self.refresh_chat(chat)],
                None => {
                    self.error("Start a chat first.");
                    vec![]
                }
            },
            Command::Info => match self.chat_id {
                Some(chat) => {
                    // A total the footer already shows stays until the refetch replaces it,
                    // so a failed refetch reads the same in both.
                    self.info_panel = Some(match &self.chat_cost {
                        Some(loaded @ CostState::Loaded(_)) => loaded.clone(),
                        _ => CostState::Loading,
                    });
                    self.cost_generation += 1;
                    vec![
                        Effect::ShowInfo,
                        self.refresh_chat(chat),
                        Effect::FetchCost {
                            chat,
                            generation: self.cost_generation,
                        },
                    ]
                }
                None => {
                    self.error("Start a chat first.");
                    vec![]
                }
            },
            Command::Git => {
                let Some(chat) = self.chat_id else {
                    self.error("Start a chat first.");
                    return vec![];
                };
                let has_workspace = self.selected_workspace.is_some();
                self.git_panel = Some(GitPanel {
                    diff: Fetched::Loading,
                    repos: Default::default(),
                    local: if has_workspace {
                        LocalGit::Connecting
                    } else {
                        LocalGit::NoWorkspace
                    },
                });
                let mut effects = vec![Effect::ShowGit, self.fetch_diff(chat)];
                if has_workspace {
                    self.git_generation += 1;
                    effects.push(Effect::OpenGitWatch {
                        chat,
                        generation: self.git_generation,
                    });
                }
                effects
            }
            Command::Diff => {
                let Some(chat) = self.chat_id else {
                    self.error("Start a chat first.");
                    return vec![];
                };
                self.page_when_loaded = true;
                vec![self.fetch_diff(chat)]
            }
            Command::Mcp => {
                if self.chat_id.is_none() && self.mcp_unready() {
                    return vec![];
                }
                let Some(chat) = self.chat_id else {
                    return self.blank_mcp();
                };
                let Some(fetches) = self.fetch_chat_mcp(chat) else {
                    self.error("Not connected to Coder yet.");
                    return vec![];
                };
                let mut effects = vec![Effect::ShowMcp, self.refresh_chat(chat)];
                effects.extend(fetches);
                effects
            }
            Command::Statusline => vec![Effect::ShowStatusline],
            Command::Usage => {
                self.usage_open = true;
                let mut effects = vec![Effect::ShowUsage];
                effects.extend(self.refresh_limits());
                // The rejected token would fail the cost request too.
                if !self.limits_stopped {
                    effects.extend(self.fetch_chat_cost());
                }
                effects
            }
            Command::New => self.new_chat(),
            Command::Help => vec![Effect::ShowHelp],
            Command::Quit => vec![Effect::Quit],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attachments::ChipState;
    use crate::chat_list::ListQuery;
    use crate::files::*;
    use crate::line_edit::Edit;
    use serde_json::json;
    use std::path::PathBuf;
    use std::time::Duration;

    fn chat(id: Uuid) -> Box<types::CodersdkChat> {
        Box::new(serde_json::from_value(json!({"id": id, "title": "t", "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap())
    }

    fn message(id: i64) -> types::CodersdkChatMessage {
        serde_json::from_value(json!({"id": id, "role": "user", "content": []})).unwrap()
    }

    fn started(app: &mut App) -> Uuid {
        let org = Uuid::new_v4();
        app.update(Msg::Started {
            org_id: org,
            open_chat: None,
        });
        org
    }

    #[test]
    fn statusline_opens_its_editor() {
        let mut app = App::new(BusyBehavior::Queue, true);
        assert_eq!(
            app.update(Msg::Command(Command::Statusline)),
            vec![Effect::ShowStatusline]
        );
    }

    #[test]
    fn chats_opens_the_overlay_and_loads_the_list_unless_it_is_live() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert_eq!(
            app.update(Msg::Command(Command::Chats(Some("fix".into())))),
            vec![
                Effect::ShowChats("fix".into()),
                Effect::FetchChats {
                    query: ListQuery::Default,
                    offset: 0
                }
            ]
        );
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats: vec![],
        });
        app.update(Msg::WatchConnected);
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Chats(None))),
            vec![Effect::ShowChats(String::new())]
        );
    }

    /// Loads a default model with three efforts and a second model with none.
    fn with_efforts(app: &mut App) -> (Uuid, Uuid) {
        let (thinker, plain) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ModelsLoaded(vec![
            serde_json::from_value(json!({"id": thinker, "display_name": "Thinker", "model": "thinker", "enabled": true, "is_default": true, "reasoning_efforts": ["low", "medium", "high"], "model_config": {"reasoning_effort": {"default": "medium"}}})).unwrap(),
            serde_json::from_value(json!({"id": plain, "display_name": "Plain", "model": "plain", "enabled": true, "reasoning_efforts": []})).unwrap(),
        ]));
        (thinker, plain)
    }

    #[test]
    fn effort_is_chosen_by_name_and_sent_with_messages() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let (thinker, _) = with_efforts(&mut app);
        assert_eq!(
            app.update(Msg::Command(Command::Effort(Some("HIGH".into())))),
            vec![Effect::SaveEffort {
                model: thinker,
                effort: "high".into(),
            }],
            "a blank chat saves the choice for its model"
        );
        assert_eq!(app.selected_effort.as_deref(), Some("high"));
        assert_eq!(
            app.update(Msg::Submit("hi".into())),
            vec![Effect::CreateChat {
                org,
                text: "hi".into(),
                model: None,
                workspace: None,
                turn: TurnOptions {
                    effort: Some("high".into()),
                    ..Default::default()
                },
                seq: 1,
            }]
        );
        assert_eq!(
            app.update(Msg::Command(Command::Effort(None))),
            vec![Effect::ShowPicker(Picker::Effort)]
        );
        app.update(Msg::Command(Command::Effort(Some("extreme".into()))));
        assert!(
            matches!(app.notices.last(), Some(Notice::Error(m)) if m.contains("low, medium, high"))
        );
        assert_eq!(app.selected_effort.as_deref(), Some("high"));
    }

    #[test]
    fn effort_on_a_model_without_efforts_says_so() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let (_, plain) = with_efforts(&mut app);
        app.update(Msg::ModelChosen(plain));
        assert!(app.update(Msg::Command(Command::Effort(None))).is_empty());
        assert!(
            matches!(app.notices.last(), Some(Notice::Info(m)) if m == "Plain has no reasoning effort levels.")
        );
        assert!(
            app.update(Msg::Command(Command::Effort(Some("high".into()))))
                .is_empty()
        );
        assert_eq!(app.selected_effort, None);
        assert_eq!(
            app.update(Msg::Submit("hi".into())),
            vec![Effect::CreateChat {
                org,
                text: "hi".into(),
                model: Some(plain),
                workspace: None,
                turn: TurnOptions::default(),
                seq: 1,
            }]
        );
    }

    #[test]
    fn switching_to_a_model_without_the_effort_keeps_it_for_later() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (thinker, _) = with_efforts(&mut app);
        let other = Uuid::new_v4();
        let mut models = app.models.clone();
        models.push(serde_json::from_value(json!({"id": other, "display_name": "Other", "enabled": true, "reasoning_efforts": ["medium", "high"], "model_config": {"reasoning_effort": {"default": "high"}}})).unwrap());
        app.update(Msg::ModelsLoaded(models));
        app.update(Msg::EffortChosen("low".into()));
        assert_eq!(app.selected_effort.as_deref(), Some("low"));

        app.update(Msg::ModelChosen(other));
        assert_eq!(
            app.selected_effort.as_deref(),
            Some("low"),
            "the web UI never clears the selection on a model change"
        );
        assert_eq!(
            app.effort().as_deref(),
            Some("high"),
            "the new model's default is sent, not the selection it lacks"
        );
        assert!(
            matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("does not offer low") && m.contains("using high"))
        );

        app.update(Msg::ModelChosen(thinker));
        assert_eq!(
            app.effort().as_deref(),
            Some("low"),
            "switching back to a model that offers the selection sends it again"
        );
    }

    #[test]
    fn a_selected_effort_the_new_model_lacks_beats_the_chats_last_effort_it_offers() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        with_efforts(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat_with_effort("low"),
            messages: vec![],
        });
        app.update(Msg::EffortChosen("high".into()));

        let other = Uuid::new_v4();
        let mut models = app.models.clone();
        models.push(serde_json::from_value(json!({"id": other, "display_name": "Other", "enabled": true, "reasoning_efforts": ["low", "medium"], "model_config": {"reasoning_effort": {"default": "medium"}}})).unwrap());
        app.update(Msg::ModelsLoaded(models));
        app.update(Msg::ModelChosen(other));

        assert_eq!(
            app.effort().as_deref(),
            Some("medium"),
            "the model default wins; the chat's last effort does not, even though the new model offers it"
        );
    }

    #[test]
    fn effort_waits_for_the_model_list() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert!(app.update(Msg::Command(Command::Effort(None))).is_empty());
        assert!(matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("still loading")));
    }

    fn chat_with_effort(effort: &str) -> Box<types::CodersdkChat> {
        Box::new(
            serde_json::from_value(json!({"id": Uuid::new_v4(), "title": "t", "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}, "last_reasoning_effort": effort}))
                .unwrap(),
        )
    }

    #[test]
    fn the_effort_prefers_the_choice_then_the_chat_then_the_default() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        with_efforts(&mut app);
        assert_eq!(
            app.effort().as_deref(),
            Some("medium"),
            "the model's default"
        );
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat_with_effort("low"),
            messages: vec![],
        });
        assert_eq!(
            app.effort().as_deref(),
            Some("low"),
            "the chat's last effort"
        );
        app.update(Msg::EffortChosen("high".into()));
        assert_eq!(app.effort().as_deref(), Some("high"), "the /effort choice");
    }

    #[test]
    fn an_opened_chat_sends_its_last_effort_with_every_message() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        with_efforts(&mut app);
        let chat = chat_with_effort("low");
        let id = chat.id.unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat,
            messages: vec![],
        });
        for (seq, text) in [(1, "one"), (2, "two")] {
            assert_eq!(
                app.update(Msg::Submit(text.into())),
                vec![Effect::SendMessage {
                    chat: id,
                    text: text.into(),
                    model: None,
                    busy: BusyBehavior::Queue,
                    turn: TurnOptions {
                        effort: Some("low".into()),
                        ..Default::default()
                    },
                    seq,
                }]
            );
        }
    }

    #[test]
    fn a_new_chat_sends_the_model_default_effort() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        with_efforts(&mut app);
        assert_eq!(
            app.update(Msg::Submit("hi".into())),
            vec![Effect::CreateChat {
                org,
                text: "hi".into(),
                model: None,
                workspace: None,
                turn: TurnOptions {
                    effort: Some("medium".into()),
                    ..Default::default()
                },
                seq: 1,
            }]
        );
    }

    #[test]
    fn an_effort_the_model_does_not_offer_falls_back_to_the_default_then_the_highest() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        with_efforts(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat_with_effort("xhigh"),
            messages: vec![],
        });
        assert_eq!(
            app.effort().as_deref(),
            Some("medium"),
            "the model's default"
        );
        app.update(Msg::ModelsLoaded(vec![
            serde_json::from_value(json!({"id": Uuid::new_v4(), "display_name": "Plain", "enabled": true, "is_default": true, "reasoning_efforts": ["low", "high"]})).unwrap(),
        ]));
        assert_eq!(
            app.effort().as_deref(),
            Some("high"),
            "without a default, the highest"
        );
    }

    #[test]
    fn switching_models_names_the_effort_now_used() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        with_efforts(&mut app);
        let other = Uuid::new_v4();
        let mut models = app.models.clone();
        models.push(serde_json::from_value(json!({"id": other, "display_name": "Other", "enabled": true, "reasoning_efforts": ["low", "high"]})).unwrap());
        app.update(Msg::ModelsLoaded(models));
        app.update(Msg::EffortChosen("medium".into()));
        app.update(Msg::ModelChosen(other));
        assert_eq!(
            app.selected_effort.as_deref(),
            Some("medium"),
            "the selection stays for a later model that offers it"
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info(
                "This model does not offer medium reasoning effort; using high.".into()
            ))
        );
    }

    #[test]
    fn a_blank_chat_with_a_stored_effort_sends_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (thinker, _) = with_efforts(&mut app);
        app.saved_efforts.insert(thinker, "high".into());
        assert_eq!(
            app.effort().as_deref(),
            Some("high"),
            "the model's default is medium; the stored choice wins on a blank chat"
        );
    }

    #[test]
    fn a_stored_effort_the_model_does_not_offer_falls_to_the_default() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (thinker, _) = with_efforts(&mut app);
        app.saved_efforts.insert(thinker, "extreme".into());
        assert_eq!(
            app.effort().as_deref(),
            Some("medium"),
            "an effort the model does not offer is ignored, like the lenient config load"
        );
    }

    #[test]
    fn effort_chosen_saves_on_a_blank_chat_but_not_an_existing_one() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (thinker, _) = with_efforts(&mut app);
        assert_eq!(
            app.update(Msg::EffortChosen("high".into())),
            vec![Effect::SaveEffort {
                model: thinker,
                effort: "high".into(),
            }],
            "a blank chat saves the choice for the selected model"
        );

        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat_with_effort("low"),
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::EffortChosen("medium".into())),
            vec![],
            "an existing chat's choice is its own and is never saved"
        );
    }

    #[test]
    fn changing_model_on_a_blank_chat_picks_up_its_stored_effort() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (thinker, _) = with_efforts(&mut app);
        let other = Uuid::new_v4();
        let mut models = app.models.clone();
        models.push(serde_json::from_value(json!({"id": other, "display_name": "Other", "enabled": true, "reasoning_efforts": ["low", "medium", "high"], "model_config": {"reasoning_effort": {"default": "high"}}})).unwrap());
        app.update(Msg::ModelsLoaded(models));
        app.saved_efforts.insert(thinker, "high".into());
        app.saved_efforts.insert(other, "medium".into());
        assert_eq!(
            app.effort().as_deref(),
            Some("high"),
            "thinker's own stored effort applies first"
        );

        app.update(Msg::ModelChosen(other));
        assert_eq!(
            app.effort().as_deref(),
            Some("medium"),
            "switching models on a blank chat picks up the new model's stored effort, \
             not its default (high)"
        );
    }

    #[test]
    fn models_group_by_provider_with_reasons_tags_and_context() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let (anthropic, openai) = (Uuid::new_v4(), Uuid::new_v4());
        let (sonnet, long, gpt) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let catalog = serde_json::from_value(json!({
            "models": [
                {"id": sonnet, "display_name": "Claude Sonnet", "ai_provider_id": anthropic, "enabled": true, "context_limit": 200000, "reasoning_efforts": []},
                {"id": long, "display_name": "Claude Sonnet (extended)", "ai_provider_id": anthropic, "enabled": true, "is_default": true, "context_limit": 1000000, "reasoning_efforts": []},
                {"id": gpt, "display_name": "GPT-5", "ai_provider_id": openai, "enabled": true, "context_limit": 400000, "reasoning_efforts": []}
            ],
            "providers": [
                {"id": anthropic, "display_name": "Anthropic", "available": true},
                {"id": openai, "display_name": "OpenAI", "available": false, "unavailable_reason": "user_api_key_required"}
            ],
            "unsupported_providers": [{"display_name": "Copilot", "provider": "copilot"}]
        }))
        .unwrap();
        app.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::CatalogLoaded(Box::new(catalog))),
        });
        app.update(Msg::ModelChosen(sonnet));
        let groups = app.model_groups("");
        let names: Vec<&str> = groups.iter().map(|g| g.provider.as_str()).collect();
        assert_eq!(names, ["Anthropic", "OpenAI"]);
        assert_eq!(groups[0].models[0].name, "Claude Sonnet");
        assert!(groups[0].models[0].current);
        assert!(groups[0].models[1].default);
        assert_eq!(
            groups[0].models[1].context.as_deref(),
            Some("1.0M tokens"),
            "as /usage writes a token count"
        );
        assert_eq!(groups[1].reason.as_deref(), Some("needs your API key"));
        assert!(!groups[1].models[0].usable);
        assert_eq!(app.unsupported_providers.len(), 1);
        let filtered = app.model_groups("openai");
        assert_eq!(filtered.len(), 1, "the provider name is searchable");
        assert_eq!(filtered[0].models[0].name, "GPT-5");
    }

    #[test]
    fn an_empty_query_sorts_alphabetically_but_filtering_keeps_the_match_order() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        // "Zebra" sorts before "apple" by raw byte order (uppercase 'Z' < lowercase 'a'), so
        // this only passes if the provider sort is case-insensitive.
        let (zebra, apple) = (Uuid::new_v4(), Uuid::new_v4());
        let (zulu, alpha) = (Uuid::new_v4(), Uuid::new_v4());
        let catalog = serde_json::from_value(json!({
            "models": [
                {"id": zulu, "display_name": "Zulu Model", "ai_provider_id": zebra, "enabled": true, "reasoning_efforts": []},
                {"id": alpha, "display_name": "alpha model", "ai_provider_id": apple, "enabled": true, "reasoning_efforts": []}
            ],
            "providers": [
                {"id": zebra, "display_name": "Zebra", "available": true},
                {"id": apple, "display_name": "apple", "available": true}
            ],
            "unsupported_providers": []
        }))
        .unwrap();
        app.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::CatalogLoaded(Box::new(catalog))),
        });
        let groups = app.model_groups("");
        let names: Vec<&str> = groups.iter().map(|g| g.provider.as_str()).collect();
        assert_eq!(
            names,
            ["apple", "Zebra"],
            "an empty query sorts groups alphabetically by provider, case-insensitive"
        );
        assert_eq!(
            groups[1]
                .models
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>(),
            ["Zulu Model"]
        );

        // "model" matches both, but only "Zulu Model" is an exact contiguous match, so it
        // ranks first even though "apple" sorts first alphabetically.
        let filtered = app.model_groups("model");
        let filtered_names: Vec<&str> = filtered.iter().map(|g| g.provider.as_str()).collect();
        assert_eq!(
            filtered_names,
            ["Zebra", "apple"],
            "filtering keeps the best-match order instead of sorting alphabetically"
        );
    }

    #[test]
    fn a_model_whose_provider_does_not_resolve_is_not_offered() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let known = Uuid::new_v4();
        let (orphan_ref, orphaned, named) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let catalog = serde_json::from_value(json!({
            "models": [
                {"id": orphaned, "display_name": "Orphan", "ai_provider_id": orphan_ref, "enabled": true, "reasoning_efforts": []},
                {"id": named, "display_name": "Named", "ai_provider_id": known, "enabled": true, "reasoning_efforts": []}
            ],
            "providers": [
                {"id": known, "display_name": "Known", "available": true}
            ],
            "unsupported_providers": []
        }))
        .unwrap();
        app.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::CatalogLoaded(Box::new(catalog))),
        });
        let groups = app.model_groups("");
        assert_eq!(
            groups.len(),
            1,
            "a model whose provider id names no provider is dropped, not grouped under \"Other\""
        );
        assert_eq!(groups[0].provider, "Known");
        assert_eq!(groups[0].models[0].name, "Named");
    }

    fn ev(v: serde_json::Value) -> Msg {
        Msg::Stream(StreamEvent {
            kind: coder_sdk::StreamEventType::parse(v["type"].as_str().unwrap_or_default()),
            event: serde_json::from_value(v.clone()).ok(),
            raw: v,
        })
    }

    #[test]
    fn start_fetches_settings_and_opens_a_requested_chat() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = Uuid::new_v4();
        let id = Uuid::new_v4();
        let effects = app.update(Msg::Started {
            org_id: org,
            open_chat: Some(id),
        });
        assert!(effects.contains(&Effect::FetchPrefs));
        assert!(effects.contains(&Effect::FetchModels(org)));
        assert!(effects.contains(&Effect::FetchWorkspaces(org)));
        assert!(effects.contains(&Effect::LoadChat(id)));
        assert_eq!(app.connection, Connection::Connecting);
    }

    fn org(label: &str, is_default: bool) -> OrgRef {
        OrgRef {
            id: Uuid::new_v4(),
            name: label.to_lowercase(),
            display_name: label.into(),
            is_default,
            can_create_chats: true,
        }
    }

    /// Loads a non-default "Product" and a default "Coder", and starts in Coder.
    fn two_orgs(app: &mut App) -> (OrgRef, OrgRef) {
        let (product, coder) = (org("Product", false), org("Coder", true));
        app.update(Msg::OrganizationsLoaded(vec![
            product.clone(),
            coder.clone(),
        ]));
        app.update(Msg::Started {
            org_id: coder.id,
            open_chat: None,
        });
        (product, coder)
    }

    #[test]
    fn organization_with_one_membership_says_so() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::OrganizationsLoaded(vec![org("Coder", true)]));
        assert!(
            app.update(Msg::Command(Command::Organization(None)))
                .is_empty()
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info(
                "You belong to one organization, Coder.".into()
            ))
        );
    }

    #[test]
    fn choosing_an_organization_on_a_blank_chat_saves_it_and_reloads_its_lists() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (product, _) = two_orgs(&mut app);
        assert_eq!(
            app.update(Msg::Command(Command::Organization(None))),
            vec![Effect::ShowPicker(Picker::Organization)]
        );
        let effects = app.update(Msg::Command(Command::Organization(Some("PRODUCT".into()))));
        assert_eq!(
            effects,
            vec![
                Effect::SaveOrganization(product.id),
                Effect::FetchModels(product.id),
                Effect::FetchWorkspaces(product.id),
                Effect::FetchOrgMcpServers(product.id)
            ]
        );
        assert_eq!(app.org_id, Some(product.id));
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info("New chats will use Product.".into()))
        );
    }

    #[test]
    fn choosing_an_organization_with_a_chat_open_waits_for_new() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (product, _) = two_orgs(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages: vec![],
        });
        let effects = app.update(Msg::OrganizationChosen(product.id));
        assert_eq!(effects, vec![Effect::SaveOrganization(product.id)]);
        assert!(matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("/new")));
    }

    #[test]
    fn an_unknown_organization_name_is_an_error() {
        let mut app = App::new(BusyBehavior::Queue, true);
        two_orgs(&mut app);
        assert!(
            app.update(Msg::Command(Command::Organization(Some("nope".into()))))
                .is_empty()
        );
        assert!(matches!(app.notices.last(), Some(Notice::Error(m)) if m.contains("nope")));
    }

    #[test]
    fn switching_organizations_drops_the_old_model_effort_and_workspace() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (product, coder) = two_orgs(&mut app);
        let (thinker, _) = with_efforts(&mut app);
        app.update(Msg::ModelChosen(thinker));
        app.update(Msg::EffortChosen("high".into()));
        app.update(Msg::WorkspaceChosen(Some(Uuid::new_v4())));
        app.update(Msg::OrganizationChosen(product.id));
        assert_eq!(app.selected_model, None, "model IDs are per organization");
        assert_eq!(app.selected_effort, None);
        assert_eq!(app.selected_workspace, None);
        assert!(app.models.is_empty());
        assert_eq!(app.models_state, ModelsState::Loading);
        app.update(Msg::ForOrg {
            org: coder.id,
            msg: Box::new(Msg::ModelsLoaded(vec![])),
        });
        assert_eq!(
            app.models_state,
            ModelsState::Loading,
            "a late reply for the old organization is dropped"
        );
        assert_eq!(
            app.update(Msg::ForOrg {
                org: product.id,
                msg: Box::new(Msg::ModelsFailed {
                    message: "HTTP 500".into()
                }),
            }),
            vec![]
        );
        assert_eq!(
            app.update(Msg::Command(Command::Model(None))),
            vec![Effect::FetchModels(product.id)],
            "a retry fetches for the new organization"
        );
    }

    #[test]
    fn choosing_an_organization_while_a_chat_loads_waits_for_new() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (product, coder) = two_orgs(&mut app);
        let effects = app.update(Msg::Started {
            org_id: coder.id,
            open_chat: Some(Uuid::new_v4()),
        });
        assert!(effects.iter().any(|e| matches!(e, Effect::LoadChat(_))));
        assert_eq!(
            app.update(Msg::OrganizationChosen(product.id)),
            vec![Effect::SaveOrganization(product.id)],
            "the chat being loaded brings its own lists"
        );
        assert!(matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("/new")));
        assert_eq!(app.org_id, Some(product.id));
    }

    #[test]
    fn choosing_the_current_organization_again_keeps_the_lists() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (_, coder) = two_orgs(&mut app);
        let (thinker, _) = with_efforts(&mut app);
        app.update(Msg::ModelChosen(thinker));
        assert_eq!(
            app.update(Msg::OrganizationChosen(coder.id)),
            vec![Effect::SaveOrganization(coder.id)]
        );
        assert_eq!(app.selected_model, Some(thinker));
        assert_eq!(app.models_state, ModelsState::Loaded);
    }

    #[test]
    fn the_saved_organization_wins_then_the_default_then_the_first() {
        let (product, coder) = (org("Product", false), org("Coder", true));
        let orgs = vec![product.clone(), coder.clone()];
        assert_eq!(
            pick_organization(None, &orgs),
            Some(coder.id),
            "list order must not matter"
        );
        assert_eq!(pick_organization(Some(product.id), &orgs), Some(product.id));
        assert_eq!(
            pick_organization(Some(Uuid::new_v4()), &orgs),
            Some(coder.id),
            "a saved organization the user left is ignored"
        );
        let no_default = vec![org("A", false), org("B", false)];
        assert_eq!(pick_organization(None, &no_default), Some(no_default[0].id));
        assert_eq!(pick_organization(None, &[]), None);
    }

    #[test]
    fn an_organization_without_chat_permission_is_never_picked() {
        let (mut product, mut coder, other) = (
            org("Product", false),
            org("Coder", true),
            org("Other", false),
        );
        product.can_create_chats = false;
        let orgs = vec![product.clone(), coder.clone(), other.clone()];
        assert_eq!(
            pick_organization(Some(product.id), &orgs),
            Some(coder.id),
            "a saved organization without permission is skipped"
        );
        coder.can_create_chats = false;
        let orgs = vec![product.clone(), coder.clone(), other.clone()];
        assert_eq!(
            pick_organization(None, &orgs),
            Some(other.id),
            "a default without permission is skipped too"
        );
        let mut alone = other.clone();
        alone.can_create_chats = false;
        assert_eq!(
            pick_organization(None, &[product, coder.clone(), alone]),
            Some(coder.id),
            "with none allowed, the usual order applies and the server explains"
        );
    }

    #[test]
    fn an_organization_without_chat_permission_is_refused_with_the_reason() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let mut product = org("Product", false);
        product.can_create_chats = false;
        let coder = org("Coder", true);
        app.update(Msg::OrganizationsLoaded(vec![
            product.clone(),
            coder.clone(),
        ]));
        app.update(Msg::Started {
            org_id: coder.id,
            open_chat: None,
        });
        assert!(app.update(Msg::OrganizationChosen(product.id)).is_empty());
        assert_eq!(app.org_id, Some(coder.id));
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "You do not have permission to create chats in Product.".into()
            ))
        );
        assert!(
            app.update(Msg::Command(Command::Organization(Some("product".into()))))
                .is_empty(),
            "choosing it by name is refused the same way"
        );
        assert_eq!(app.org_id, Some(coder.id));
    }

    #[test]
    fn a_loaded_chat_uses_its_own_organization_for_models_and_workspaces() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let home = started(&mut app);
        let other = Uuid::new_v4();
        let loaded: Box<types::CodersdkChat> = Box::new(
            serde_json::from_value(json!({"id": Uuid::new_v4(), "title": "t", "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}, "organization_id": other}))
                .unwrap(),
        );
        let effects = app.update(Msg::ChatLoaded {
            has_more: None,
            chat: loaded,
            messages: vec![],
        });
        assert!(effects.contains(&Effect::FetchModels(other)), "{effects:?}");
        assert!(effects.contains(&Effect::FetchWorkspaces(other)));
        assert_eq!(app.current_org(), Some(other));
        let model = |name: &str| -> types::CodersdkChatModel {
            serde_json::from_value(json!({"id": Uuid::new_v4(), "display_name": name, "enabled": true, "is_default": true, "reasoning_efforts": []})).unwrap()
        };
        app.update(Msg::ForOrg {
            org: home,
            msg: Box::new(Msg::ModelsLoaded(vec![model("Stale")])),
        });
        assert!(
            app.models.is_empty(),
            "a late reply for the other organization is dropped"
        );
        app.update(Msg::ForOrg {
            org: other,
            msg: Box::new(Msg::ModelsLoaded(vec![model("Fresh")])),
        });
        assert_eq!(app.model_name().as_deref(), Some("Fresh"));
        assert_eq!(
            app.org_id,
            Some(home),
            "new chats still go to the picked organization"
        );
    }

    #[test]
    fn a_failed_model_list_retries_for_the_organization_the_lists_belong_to() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let home = started(&mut app);
        let other = Uuid::new_v4();
        let loaded: Box<types::CodersdkChat> = Box::new(
            serde_json::from_value(json!({"id": Uuid::new_v4(), "title": "t", "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}, "organization_id": other}))
                .unwrap(),
        );
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: loaded,
            messages: vec![],
        });
        app.update(Msg::ForOrg {
            org: other,
            msg: Box::new(Msg::ModelsFailed {
                message: "HTTP 500".into(),
            }),
        });
        assert_eq!(app.models_state, ModelsState::Failed);
        let effects = app.update(Msg::Command(Command::Model(None)));
        assert_eq!(effects, vec![Effect::FetchModels(other)], "not {home}");
        let model: types::CodersdkChatModel = serde_json::from_value(json!({"id": Uuid::new_v4(), "display_name": "M", "enabled": true, "is_default": true, "reasoning_efforts": []})).unwrap();
        app.update(Msg::ForOrg {
            org: other,
            msg: Box::new(Msg::ModelsLoaded(vec![model])),
        });
        assert_eq!(
            app.models_state,
            ModelsState::Loaded,
            "the retried reply is applied"
        );
        assert_eq!(app.model_name().as_deref(), Some("M"));
    }

    #[test]
    fn an_organization_without_any_name_is_called_this_organization() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let mut unnamed = org("", false);
        unnamed.name = String::new();
        app.update(Msg::OrganizationsLoaded(vec![unnamed.clone()]));
        app.update(Msg::Started {
            org_id: unnamed.id,
            open_chat: None,
        });
        app.update(Msg::ForOrg {
            org: unnamed.id,
            msg: Box::new(Msg::ModelsLoaded(vec![])),
        });
        app.update(Msg::Command(Command::Model(None)));
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info(
                "No chat models are available in this organization. Try /organization.".into()
            ))
        );
    }

    #[test]
    fn an_empty_model_list_points_to_organization_instead_of_failing() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let product = org("Product", false);
        app.update(Msg::OrganizationsLoaded(vec![product.clone()]));
        app.update(Msg::Started {
            org_id: product.id,
            open_chat: None,
        });
        app.update(Msg::ForOrg {
            org: product.id,
            msg: Box::new(Msg::ModelsLoaded(vec![])),
        });
        let guidance = "No chat models are available in Product. Try /organization.";
        assert!(app.update(Msg::Command(Command::Model(None))).is_empty());
        assert_eq!(app.notices.last(), Some(&Notice::Info(guidance.into())));
        assert_eq!(
            app.update(Msg::Submit("hello".into())),
            vec![Effect::RestoreComposer("hello".into())]
        );
        assert_eq!(app.notices.last(), Some(&Notice::Error(guidance.into())));
    }

    #[test]
    fn loaded_chat_opens_the_stream_after_the_last_message() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        let effects = app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![message(3), message(9)],
        });
        assert_eq!(
            effects,
            vec![Effect::OpenStream {
                chat: id,
                after_id: Some(9),
                generation: app.stream_generation(),
            }]
        );
    }

    #[test]
    fn chat_loaded_without_id_shows_an_error() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = Uuid::new_v4();
        let requested = Uuid::new_v4();
        app.update(Msg::Started {
            org_id: org,
            open_chat: Some(requested),
        });
        assert_eq!(app.connection, Connection::Connecting);
        let chat_without_id: Box<types::CodersdkChat> = Box::new(
            serde_json::from_value(json!({"title": "t", "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}))
                .unwrap(),
        );
        let effects = app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat_without_id,
            messages: vec![],
        });
        assert!(effects.is_empty());
        assert!(matches!(app.notices.last(), Some(Notice::Error(_))));
        assert_eq!(app.connection, Connection::Idle);
    }

    #[test]
    fn submit_on_blank_chat_creates_one_chat() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let effects = app.update(Msg::Submit("hello".into()));
        assert_eq!(
            effects,
            vec![Effect::CreateChat {
                org,
                text: "hello".into(),
                model: None,
                workspace: None,
                turn: TurnOptions::default(),
                seq: 1,
            }]
        );
    }

    #[test]
    fn second_submit_while_creating_does_not_create_twice() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("one".into()));
        let effects = app.update(Msg::Submit("two".into()));
        assert!(effects.is_empty());
        assert!(matches!(app.notices.last(), Some(Notice::Info(_))));
        let id = Uuid::new_v4();
        let effects = app.update(Msg::ChatCreated(chat(id)));
        assert!(effects.contains(&Effect::OpenStream {
            chat: id,
            after_id: None,
            generation: app.stream_generation(),
        }));
        assert!(effects.contains(&Effect::SendMessage {
            chat: id,
            text: "two".into(),
            model: None,
            busy: BusyBehavior::Queue,
            turn: TurnOptions::default(),
            seq: 2,
        }));
    }

    #[test]
    fn unrelated_api_failure_does_not_reset_creation() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("one".into()));
        app.update(Msg::Submit("two".into()));
        app.update(Msg::ApiFailed {
            action: "load models",
            message: "HTTP 500".into(),
        });
        let effects = app.update(Msg::Submit("three".into()));
        assert!(effects.is_empty(), "no second CreateChat: {effects:?}");
    }

    #[test]
    fn failed_create_restores_all_typed_text() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("one".into()));
        app.update(Msg::Submit("two".into()));
        let effects = app.update(Msg::CreateFailed {
            message: "HTTP 500".into(),
            seq: 0,
        });
        assert_eq!(effects, vec![Effect::RestoreComposer("one\n\ntwo".into())]);
        let org = app.org_id.unwrap();
        let effects = app.update(Msg::Submit("three".into()));
        assert_eq!(
            effects,
            vec![Effect::CreateChat {
                org,
                text: "three".into(),
                model: None,
                workspace: None,
                turn: TurnOptions::default(),
                seq: 2,
            }]
        );
        let id = Uuid::new_v4();
        let effects = app.update(Msg::ChatCreated(chat(id)));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::SendMessage { text, .. } if text == "two"))
        );
    }

    #[test]
    fn chat_created_without_id_is_a_failed_create() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("hi".into()));
        let chat_without_id: Box<types::CodersdkChat> = Box::new(
            serde_json::from_value(json!({"title": "t", "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}))
                .unwrap(),
        );
        let effects = app.update(Msg::ChatCreated(chat_without_id));
        assert_eq!(effects, vec![Effect::RestoreComposer("hi".into())]);
        assert!(
            matches!(app.notices.last(), Some(Notice::Error(m)) if m.contains("without an id"))
        );
    }

    #[test]
    fn workspace_chosen_during_create_is_applied_after_creation() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("hello".into()));
        let ws = Uuid::new_v4();
        let effects = app.update(Msg::WorkspaceChosen(Some(ws)));
        assert!(effects.is_empty());
        let id = Uuid::new_v4();
        let effects = app.update(Msg::ChatCreated(chat(id)));
        assert!(effects.contains(&Effect::SetWorkspace {
            chat: id,
            workspace: Some(ws)
        }));
    }

    #[test]
    fn set_workspace_precedes_the_queued_message() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("one".into()));
        app.update(Msg::Submit("two".into()));
        let ws = Uuid::new_v4();
        app.update(Msg::WorkspaceChosen(Some(ws)));
        let id = Uuid::new_v4();
        let effects = app.update(Msg::ChatCreated(chat(id)));
        let set_workspace = effects
            .iter()
            .position(|e| matches!(e, Effect::SetWorkspace { .. }))
            .expect("SetWorkspace effect");
        let send_message = effects
            .iter()
            .position(|e| matches!(e, Effect::SendMessage { .. }))
            .expect("SendMessage effect");
        assert!(set_workspace < send_message);
    }

    #[test]
    fn stray_create_failure_does_not_clear_the_composer() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let effects = app.update(Msg::CreateFailed {
            message: "HTTP 500".into(),
            seq: 0,
        });
        assert!(effects.is_empty());
        assert!(matches!(app.notices.last(), Some(Notice::Error(_))));
    }

    #[test]
    fn submit_with_a_chat_sends_with_the_configured_busy_behavior() {
        let mut app = App::new(BusyBehavior::Interrupt, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        let effects = app.update(Msg::Submit("  hi  ".into()));
        assert_eq!(
            effects,
            vec![Effect::SendMessage {
                chat: id,
                text: "hi".into(),
                model: None,
                busy: BusyBehavior::Interrupt,
                turn: TurnOptions::default(),
                seq: 1,
            }]
        );
        assert!(app.update(Msg::Submit("   ".into())).is_empty());
    }

    #[test]
    fn stream_end_schedules_backoff_reconnect_with_after_id() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![message(5)],
        });
        let first = app.update(Msg::StreamEnded { error: None });
        assert_eq!(
            first,
            vec![Effect::ReconnectAfter {
                chat: id,
                after_id: Some(5),
                delay: Duration::from_millis(500),
                generation: app.stream_generation(),
            }]
        );
        let second = app.update(Msg::StreamEnded {
            error: Some("reset".into()),
        });
        assert_eq!(
            second,
            vec![Effect::ReconnectAfter {
                chat: id,
                after_id: Some(5),
                delay: Duration::from_millis(1000),
                generation: app.stream_generation(),
            }]
        );
        assert_eq!(app.connection, Connection::Reconnecting { attempt: 2 });
    }

    #[test]
    fn only_a_healthy_stream_resets_backoff() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.update(Msg::StreamEnded { error: None });
        app.update(ev(
            json!({"type": "status", "status": {"status": "waiting"}}),
        ));
        assert_eq!(app.connection, Connection::Live);
        let effects = app.update(Msg::StreamEnded { error: None });
        assert_eq!(
            effects,
            vec![Effect::ReconnectAfter {
                chat: id,
                after_id: None,
                delay: Duration::from_millis(1000),
                generation: app.stream_generation(),
            }],
            "the snapshot after a reopen does not prove the connection holds"
        );
        app.update(Msg::StreamHealthy);
        let effects = app.update(Msg::StreamEnded { error: None });
        assert_eq!(
            effects,
            vec![Effect::ReconnectAfter {
                chat: id,
                after_id: None,
                delay: Duration::from_millis(500),
                generation: app.stream_generation(),
            }]
        );
    }

    #[test]
    fn a_stream_message_applies_only_while_its_generation_is_current() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        let first = app.stream_generation();
        app.update(Msg::StreamEnded { error: None });
        let second = app.stream_generation();
        assert!(second > first);
        let running = |generation| Msg::ForStream {
            chat: id,
            generation,
            msg: Box::new(ev(
                json!({"type": "status", "status": {"status": "running"}}),
            )),
        };
        assert!(app.update(running(first)).is_empty());
        assert_eq!(
            app.transcript.status, None,
            "the replaced stream is ignored"
        );
        app.update(running(second));
        assert_eq!(app.transcript.status, Some(ChatStatus::Running));
    }

    #[test]
    fn stream_gap_reconnects_immediately() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        let part = |seq| json!({"type": "message_part", "message_part": {"history_version": 1, "generation_attempt": 1, "seq": seq, "part": {"type": "text", "text": "x"}}});
        app.update(ev(part(1)));
        let effects = app.update(ev(part(4)));
        assert_eq!(
            effects,
            vec![Effect::ReconnectAfter {
                chat: id,
                after_id: None,
                delay: Duration::ZERO,
                generation: app.stream_generation(),
            }]
        );
        assert_eq!(app.connection, Connection::Reconnecting { attempt: 1 });
    }

    #[test]
    fn repeated_stream_gaps_back_off() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        let part = |seq| json!({"type": "message_part", "message_part": {"history_version": 1, "generation_attempt": 1, "seq": seq, "part": {"type": "text", "text": "x"}}});
        app.update(ev(part(1)));
        let first = app.update(ev(part(4)));
        assert_eq!(
            first,
            vec![Effect::ReconnectAfter {
                chat: id,
                after_id: None,
                delay: Duration::ZERO,
                generation: app.stream_generation(),
            }]
        );
        assert_eq!(app.connection, Connection::Reconnecting { attempt: 1 });
        let second = app.update(ev(part(9)));
        assert_eq!(
            second,
            vec![Effect::ReconnectAfter {
                chat: id,
                after_id: None,
                delay: backoff(2),
                generation: app.stream_generation(),
            }]
        );
        assert_eq!(app.connection, Connection::Reconnecting { attempt: 2 });
    }

    #[test]
    fn a_good_event_between_two_gaps_keeps_the_backoff() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
            has_more: None,
        });
        let part = |seq| json!({"type": "message_part", "message_part": {"history_version": 1, "generation_attempt": 1, "seq": seq, "part": {"type": "text", "text": "x"}}});
        app.update(ev(part(1)));
        let first = app.update(ev(part(4)));
        assert!(
            matches!(first.as_slice(), [Effect::ReconnectAfter { delay, .. }] if delay.is_zero()),
            "{first:?}"
        );
        app.update(ev(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        assert_eq!(app.connection, Connection::Live);
        app.update(ev(part(1)));
        let second = app.update(ev(part(4)));
        assert_eq!(
            second,
            vec![Effect::ReconnectAfter {
                chat: id,
                after_id: None,
                delay: backoff(2),
                generation: app.stream_generation(),
            }]
        );
    }

    #[test]
    fn a_gap_reconnects_at_once_only_on_its_first_attempt() {
        assert_eq!(reconnect_delay(1, true), Duration::ZERO);
        assert_eq!(reconnect_delay(2, true), backoff(2));
        assert_eq!(reconnect_delay(1, false), backoff(1));
        assert_eq!(reconnect_delay(3, false), backoff(3));
    }

    #[test]
    fn stream_gap_clears_the_live_turn() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        let part = |seq| json!({"type": "message_part", "message_part": {"history_version": 1, "generation_attempt": 1, "seq": seq, "part": {"type": "text", "text": "x"}}});
        app.update(ev(part(1)));
        assert!(!app.transcript.live.is_empty());
        app.update(ev(part(4)));
        assert!(app.transcript.live.is_empty());
    }

    #[test]
    fn commands_need_a_chat_and_resolve_names() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert!(app.update(Msg::Command(Command::Compact)).is_empty());
        assert!(matches!(app.notices.last(), Some(Notice::Error(_))));
        let model_id = Uuid::new_v4();
        app.update(Msg::ModelsLoaded(vec![serde_json::from_value(json!({"id": model_id, "display_name": "Big Model", "model": "big", "enabled": true, "reasoning_efforts": []})).unwrap()]));
        app.update(Msg::Command(Command::Model(Some("big model".into()))));
        assert_eq!(app.selected_model, Some(model_id));
        assert_eq!(
            app.update(Msg::Command(Command::Model(None))),
            vec![Effect::ShowPicker(Picker::Model)]
        );
        let ws = Uuid::new_v4();
        app.update(Msg::WorkspacesLoaded(vec![WorkspaceRef {
            id: ws,
            name: "dev".into(),
            ..Default::default()
        }]));
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Workspace(Some("dev".into())))),
            vec![Effect::SetWorkspace {
                chat: id,
                workspace: Some(ws)
            }]
        );
        assert_eq!(
            app.update(Msg::Command(Command::Workspace(Some("none".into())))),
            vec![Effect::SetWorkspace {
                chat: id,
                workspace: None
            }]
        );
        assert_eq!(
            app.update(Msg::Command(Command::Compact)),
            vec![Effect::Compact(id)]
        );
        assert_eq!(
            app.update(Msg::Command(Command::Clear)),
            vec![Effect::Clear(id)]
        );
        assert_eq!(
            app.update(Msg::Command(Command::Copy(Some(2)))),
            vec![Effect::Copy(CopyTarget::CodeBlock(2))]
        );
        assert_eq!(
            app.update(Msg::Command(Command::Mouse)),
            vec![Effect::SetMouse(false)]
        );
        assert!(!app.mouse);
        assert_eq!(app.update(Msg::Command(Command::Quit)), vec![Effect::Quit]);
    }

    #[test]
    fn commands_while_creating_explain_the_wait() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("hello".into()));
        assert!(app.update(Msg::Command(Command::Compact)).is_empty());
        assert!(
            matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("still being created"))
        );
        assert!(app.update(Msg::Command(Command::Clear)).is_empty());
        assert!(
            matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("still being created"))
        );
    }

    #[test]
    fn web_opens_the_current_chat() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert!(app.update(Msg::Command(Command::Web)).is_empty());
        assert!(matches!(app.notices.last(), Some(Notice::Error(m)) if m.contains("Start a chat")));
        app.update(Msg::Submit("hi".into()));
        assert!(app.update(Msg::Command(Command::Web)).is_empty());
        assert!(
            matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("still being created"))
        );
        let id = Uuid::new_v4();
        app.update(Msg::ChatCreated(chat(id)));
        assert_eq!(
            app.update(Msg::Command(Command::Web)),
            vec![Effect::OpenWeb(id)]
        );
        let url = format!("https://coder.example.com/agents/{id}");
        app.update(Msg::WebOpened {
            url: url.clone(),
            outcome: Ok(()),
        });
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info(format!("Opened {url}")))
        );
        assert_eq!(
            app.update(Msg::WebOpened {
                url: url.clone(),
                outcome: Err("over SSH".into()),
            }),
            vec![Effect::CopyWebUrl(url)]
        );
    }

    #[test]
    fn a_link_that_opened_no_browser_is_handed_back_to_copy() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let url = "https://coder.com/docs".to_owned();
        assert!(
            app.update(Msg::LinkOpened {
                url: url.clone(),
                outcome: Ok(()),
            })
            .is_empty()
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info(format!("Opened {url}")))
        );
        assert_eq!(
            app.update(Msg::LinkOpened {
                url: url.clone(),
                outcome: Err("over SSH".into()),
            }),
            vec![Effect::CopyLink(url)]
        );
    }

    #[test]
    fn model_commands_before_models_load_explain_the_wait() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert!(app.update(Msg::Command(Command::Model(None))).is_empty());
        assert!(matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("still loading")));
        assert!(
            app.update(Msg::Command(Command::Model(Some("big model".into()))))
                .is_empty()
        );
        assert!(matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("still loading")));
    }

    #[test]
    fn failed_model_load_retries_on_model_command() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        app.update(Msg::ModelsFailed {
            message: "HTTP 500".into(),
        });
        assert_eq!(app.models_state, ModelsState::Failed);
        let effects = app.update(Msg::Command(Command::Model(None)));
        assert_eq!(effects, vec![Effect::FetchModels(org)]);
        assert_eq!(app.models_state, ModelsState::Loading);
        assert!(matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("Retrying")));
    }

    #[test]
    fn submit_parses_slash_commands() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert_eq!(
            app.update(Msg::Submit("/help".into())),
            vec![Effect::ShowHelp]
        );
        assert!(app.update(Msg::Submit("/bogus".into())).is_empty());
        assert!(matches!(app.notices.last(), Some(Notice::Error(m)) if m.contains("/help")));
    }

    #[test]
    fn interrupt_only_while_running() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        assert!(app.update(Msg::Interrupt).is_empty());
        app.update(ev(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        assert_eq!(app.update(Msg::Interrupt), vec![Effect::Interrupt(id)]);
    }

    #[test]
    fn interrupt_is_not_sent_while_interrupting() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.update(ev(
            json!({"type": "status", "status": {"status": "interrupting"}}),
        ));
        assert!(app.update(Msg::Interrupt).is_empty());
    }

    #[test]
    fn api_failure_becomes_an_error_notice() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::ApiFailed {
            action: "send message",
            message: "HTTP 409".into(),
        });
        assert!(
            matches!(app.notices.last(), Some(Notice::Error(m)) if m.contains("send message") && m.contains("409"))
        );
    }

    #[test]
    fn refresh_changes_nothing() {
        let mut app = App::new(BusyBehavior::Queue, true);
        assert!(app.update(Msg::Refresh).is_empty());
    }

    #[test]
    fn a_failed_chat_load_goes_idle_and_the_next_submit_retries_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = Uuid::new_v4();
        let requested = Uuid::new_v4();
        app.update(Msg::Started {
            org_id: org,
            open_chat: Some(requested),
        });
        let effects = app.update(Msg::ChatLoadFailed {
            chat_id: requested,
            message: "HTTP 404".into(),
        });
        assert!(effects.is_empty());
        assert_eq!(app.connection, Connection::Idle);
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(format!(
                "Could not load chat {requested}: HTTP 404"
            )))
        );
        let effects = app.update(Msg::Submit("reply".into()));
        assert_eq!(effects, vec![Effect::LoadChat(requested)]);
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info("Retrying the chat load.".into()))
        );
        assert_eq!(app.connection, Connection::Connecting);
        let effects = app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(requested),
            messages: vec![message(4)],
        });
        assert_eq!(
            effects,
            vec![
                Effect::OpenStream {
                    chat: requested,
                    after_id: Some(4),
                    generation: app.stream_generation(),
                },
                Effect::SendMessage {
                    chat: requested,
                    text: "reply".into(),
                    model: None,
                    busy: BusyBehavior::Queue,
                    turn: TurnOptions::default(),
                    seq: 1,
                }
            ]
        );
    }

    #[test]
    fn submit_while_the_requested_chat_loads_waits_for_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let requested = Uuid::new_v4();
        app.update(Msg::Started {
            org_id: Uuid::new_v4(),
            open_chat: Some(requested),
        });
        assert!(app.update(Msg::Submit("early".into())).is_empty());
        assert!(matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("load")));
        let effects = app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(requested),
            messages: vec![],
        });
        assert!(effects.contains(&Effect::SendMessage {
            chat: requested,
            text: "early".into(),
            model: None,
            busy: BusyBehavior::Queue,
            turn: TurnOptions::default(),
            seq: 1,
        }));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::CreateChat { .. }))
        );
    }

    #[test]
    fn a_failed_retry_puts_the_waiting_text_back() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let requested = Uuid::new_v4();
        app.update(Msg::Started {
            org_id: Uuid::new_v4(),
            open_chat: Some(requested),
        });
        app.update(Msg::ChatLoadFailed {
            chat_id: requested,
            message: "timeout".into(),
        });
        app.update(Msg::Submit("reply".into()));
        let effects = app.update(Msg::ChatLoadFailed {
            chat_id: requested,
            message: "timeout".into(),
        });
        assert_eq!(effects, vec![Effect::RestoreComposer("reply".into())]);
        assert_eq!(
            app.update(Msg::Submit("again".into())),
            vec![Effect::LoadChat(requested)]
        );
    }

    #[test]
    fn a_failed_send_restores_the_text() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let effects = app.update(Msg::SendFailed {
            text: "hello".into(),
            message: "HTTP 409".into(),
            plan_mode: None,
            seq: 0,
            mcp_rejected: false,
        });
        assert_eq!(effects, vec![Effect::RestoreComposer("hello".into())]);
        assert!(
            matches!(app.notices.last(), Some(Notice::Error(m)) if m.contains("send") && m.contains("HTTP 409"))
        );
    }

    #[test]
    fn the_last_stream_error_is_kept_until_the_stream_is_live() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.update(Msg::StreamEnded {
            error: Some("HTTP 404".into()),
        });
        assert_eq!(app.last_stream_error.as_deref(), Some("HTTP 404"));
        app.update(ev(
            json!({"type": "status", "status": {"status": "waiting"}}),
        ));
        assert_eq!(app.last_stream_error, None);
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff(1), Duration::from_millis(500));
        assert_eq!(backoff(3), Duration::from_millis(2000));
        assert_eq!(backoff(20), Duration::from_secs(10));
    }

    fn text_part(seq: i64, kind: &str, text: &str) -> Msg {
        ev(
            json!({"type": "message_part", "message_part": {"history_version": 1, "generation_attempt": 1, "seq": seq, "part": {"type": kind, "text": text}}}),
        )
    }

    fn status(s: &str) -> Msg {
        ev(json!({"type": "status", "status": {"status": s}}))
    }

    #[test]
    fn activity_follows_the_whole_turn() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        assert_eq!(app.activity(), None);
        app.update(Msg::Submit("hi".into()));
        assert_eq!(app.activity(), Some(Activity::Waiting));
        app.update(ev(json!({"type": "message", "message": {"id": 1, "role": "user", "content": [{"type": "text", "text": "hi"}]}})));
        assert_eq!(
            app.activity(),
            Some(Activity::Waiting),
            "the echo of our own message"
        );
        app.update(status("waiting"));
        assert_eq!(
            app.activity(),
            Some(Activity::Waiting),
            "a stale waiting status"
        );
        app.update(status("running"));
        assert_eq!(app.activity(), Some(Activity::Working));
        app.update(text_part(1, "reasoning", "hmm"));
        assert_eq!(app.activity(), Some(Activity::Thinking));
        app.update(ev(json!({"type": "message_part", "message_part": {"history_version": 1, "generation_attempt": 1, "seq": 2, "part": {"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args_delta": "{"}}})));
        assert_eq!(app.activity(), Some(Activity::Tool("execute".into())));
        app.update(text_part(3, "text", "Done"));
        assert_eq!(app.activity(), Some(Activity::Writing));
        app.update(ev(json!({"type": "message", "message": {"id": 2, "role": "assistant", "content": [{"type": "text", "text": "Done"}]}})));
        assert_eq!(
            app.activity(),
            Some(Activity::Working),
            "running until the status changes"
        );
        app.update(status("interrupting"));
        assert_eq!(app.activity(), Some(Activity::Interrupting));
        app.update(status("waiting"));
        assert_eq!(app.activity(), None);
    }

    #[test]
    fn a_running_tool_is_named_once_its_message_lands() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let messages = serde_json::from_value(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "list files"}]},
            {"id": 2, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "c1", "tool_name": "execute", "args": {"command": "ls"}},
                {"type": "tool-call", "tool_call_id": "c2", "tool_name": "read_file", "args": {}}
            ]}
        ]))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages,
        });
        app.update(running());
        assert_eq!(
            app.activity(),
            Some(Activity::Tool("execute, read_file".into()))
        );
        app.update(ev(json!({"type": "message", "message": {"id": 3, "role": "tool", "content": [
            {"type": "tool-result", "tool_call_id": "c1", "tool_name": "execute", "result": {"output": ""}}
        ]}})));
        assert_eq!(app.activity(), Some(Activity::Tool("read_file".into())));
        app.update(ev(json!({"type": "message", "message": {"id": 4, "role": "tool", "content": [
            {"type": "tool-result", "tool_call_id": "c2", "tool_name": "read_file", "result": {}}
        ]}})));
        assert_eq!(app.activity(), Some(Activity::Working));
    }

    #[test]
    fn three_or_more_running_tools_are_named_first_plus_a_count() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let messages = serde_json::from_value(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "do three things"}]},
            {"id": 2, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "c1", "tool_name": "execute", "args": {}},
                {"type": "tool-call", "tool_call_id": "c2", "tool_name": "read_file", "args": {}},
                {"type": "tool-call", "tool_call_id": "c3", "tool_name": "write_file", "args": {}}
            ]}
        ]))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages,
        });
        app.update(running());
        assert_eq!(
            app.activity(),
            Some(Activity::Tool("execute and 2 more".into())),
            "several tools running in parallel name the first one plus a count"
        );
    }

    #[test]
    fn unresolved_calls_from_an_earlier_turn_are_never_named() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let messages = serde_json::from_value(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "first"}]},
            {"id": 2, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "old", "tool_name": "abandoned", "args": {}}
            ]},
            {"id": 3, "role": "user", "content": [{"type": "text", "text": "second"}]},
            {"id": 4, "role": "assistant", "content": [{"type": "text", "text": "ok"}]}
        ]))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages,
        });
        app.update(running());
        assert_eq!(
            app.activity(),
            Some(Activity::Working),
            "the interrupted call from the earlier turn is never named"
        );
    }

    #[test]
    fn interrupting_hides_the_tool_name() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let messages = serde_json::from_value(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "list files"}]},
            {"id": 2, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "c1", "tool_name": "execute", "args": {}}
            ]}
        ]))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages,
        });
        app.update(running());
        assert_eq!(app.activity(), Some(Activity::Tool("execute".into())));
        app.update(ev(
            json!({"type": "status", "status": {"status": "interrupting"}}),
        ));
        assert_eq!(
            app.activity(),
            Some(Activity::Interrupting),
            "an interrupted tool call is never named"
        );
    }

    #[test]
    fn a_failed_send_or_create_stops_the_wait() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("hi".into()));
        assert_eq!(app.activity(), Some(Activity::Waiting));
        app.update(Msg::CreateFailed {
            message: "HTTP 500".into(),
            seq: 0,
        });
        assert_eq!(app.activity(), None);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.update(Msg::Submit("again".into()));
        app.update(Msg::SendFailed {
            text: "again".into(),
            message: "HTTP 409".into(),
            plan_mode: None,
            seq: 0,
            mcp_rejected: false,
        });
        assert_eq!(app.activity(), None);
        app.update(Msg::Submit("third".into()));
        app.update(ev(
            json!({"type": "error", "error": {"message": "provider down"}}),
        ));
        assert_eq!(app.activity(), None);
    }

    #[test]
    fn requires_action_is_not_working() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages: vec![],
        });
        app.update(Msg::Submit("hi".into()));
        assert_eq!(app.activity(), Some(Activity::Waiting));
        app.update(status("requires_action"));
        assert_eq!(app.activity(), None, "requires_action ends the wait");
    }

    fn user_message(id: i64) -> Msg {
        ev(
            json!({"type": "message", "message": {"id": id, "role": "user", "content": [{"type": "text", "text": "hi"}]}}),
        )
    }

    fn assistant_message(id: i64) -> Msg {
        ev(
            json!({"type": "message", "message": {"id": id, "role": "assistant", "content": [{"type": "text", "text": "Done"}]}}),
        )
    }

    #[test]
    fn the_reply_ends_the_wait_without_a_running_status() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages: vec![],
        });
        app.update(Msg::Submit("hi".into()));
        app.update(user_message(1));
        assert_eq!(
            app.activity(),
            Some(Activity::Waiting),
            "the echo of our own message"
        );
        app.update(assistant_message(2));
        assert_eq!(app.activity(), None);
    }

    #[test]
    fn a_reply_from_the_earlier_turn_does_not_end_a_queued_wait() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages: vec![message(1)],
        });
        app.update(status("running"));
        app.update(Msg::Submit("next".into()));
        assert_eq!(app.activity(), Some(Activity::Waiting));
        app.update(assistant_message(2));
        assert_eq!(
            app.activity(),
            Some(Activity::Waiting),
            "the earlier turn replied before our message was echoed"
        );
        app.update(status("waiting"));
        app.update(user_message(3));
        app.update(assistant_message(2));
        assert_eq!(
            app.activity(),
            Some(Activity::Waiting),
            "an older assistant message is not the reply"
        );
        app.update(assistant_message(4));
        assert_eq!(app.activity(), None);
    }

    #[test]
    fn an_interrupting_hand_off_keeps_the_activity_row() {
        let mut app = App::new(BusyBehavior::Interrupt, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages: vec![],
        });
        app.update(status("running"));
        app.update(Msg::Submit("stop and do this".into()));
        assert!(app.activity().is_some());
        for s in ["interrupting", "waiting", "running"] {
            app.update(status(s));
            assert!(app.activity().is_some(), "no activity after {s}");
        }
        assert_eq!(app.activity(), Some(Activity::Working));
    }

    #[test]
    fn a_failed_load_stops_the_wait() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let id = Uuid::new_v4();
        app.update(Msg::Started {
            org_id: Uuid::new_v4(),
            open_chat: Some(id),
        });
        app.update(Msg::Submit("hi".into()));
        assert_eq!(app.activity(), Some(Activity::Waiting));
        app.update(Msg::ChatLoadFailed {
            chat_id: id,
            message: "HTTP 404".into(),
        });
        assert_eq!(app.activity(), None);
    }

    fn chat_with_plan(id: Uuid, plan: &str) -> Box<types::CodersdkChat> {
        Box::new(serde_json::from_value(json!({"id": id, "title": "t", "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}, "plan_mode": plan})).unwrap())
    }

    #[test]
    fn plan_mode_toggles_and_sets_on_an_existing_chat() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        assert!(!app.plan_mode);
        assert_eq!(
            app.update(Msg::Command(Command::PlanMode(None))),
            vec![Effect::SetPlanMode {
                chat: id,
                on: true,
                generation: 1
            }]
        );
        assert!(app.plan_mode);
        assert!(
            app.update(Msg::Command(Command::PlanMode(Some(true))))
                .is_empty()
        );
        assert!(matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("already on")));
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::PlanModeApplied { on: true }),
        });
        assert_eq!(
            app.update(Msg::Command(Command::PlanMode(Some(false)))),
            vec![Effect::SetPlanMode {
                chat: id,
                on: false,
                generation: 2
            }]
        );
        assert!(!app.plan_mode);
    }

    #[test]
    fn a_chat_loaded_in_plan_mode_shows_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat_with_plan(Uuid::new_v4(), "plan"),
            messages: vec![],
        });
        assert!(app.plan_mode);
    }

    #[test]
    fn plan_mode_on_a_blank_chat_rides_on_the_create() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        assert!(
            app.update(Msg::Command(Command::PlanMode(Some(true))))
                .is_empty()
        );
        assert_eq!(
            app.update(Msg::Submit("hi".into())),
            vec![Effect::CreateChat {
                org,
                text: "hi".into(),
                model: None,
                workspace: None,
                turn: TurnOptions {
                    plan_mode: Some(true),
                    ..Default::default()
                },
                seq: 1,
            }]
        );
    }

    #[test]
    fn plan_mode_set_while_the_chat_is_being_created_applies_after_creation() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("hi".into()));
        assert!(
            app.update(Msg::Command(Command::PlanMode(Some(true))))
                .is_empty()
        );
        assert!(
            matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("once the chat is created"))
        );
        let id = Uuid::new_v4();
        let effects = app.update(Msg::ChatCreated(chat_with_plan(id, "")));
        assert!(
            effects.contains(&Effect::SetPlanMode {
                chat: id,
                on: true,
                generation: 1
            }),
            "{effects:?}"
        );
        assert!(app.plan_mode);
    }

    #[test]
    fn plan_mode_set_while_creating_rides_on_the_queued_message() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("one".into()));
        app.update(Msg::Submit("two".into()));
        app.update(Msg::Command(Command::PlanMode(Some(true))));
        let id = Uuid::new_v4();
        let effects = app.update(Msg::ChatCreated(chat(id)));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::SetPlanMode { .. })),
            "a separate update could race the message: {effects:?}"
        );
        assert!(effects.contains(&Effect::SendMessage {
            chat: id,
            text: "two".into(),
            model: None,
            busy: BusyBehavior::Queue,
            turn: TurnOptions {
                plan_mode: Some(true),
                plan_generation: 1,
                ..Default::default()
            },
            seq: 2,
        }));
    }

    #[test]
    fn a_failed_send_that_carried_plan_mode_takes_the_servers_value() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("one".into()));
        app.update(Msg::Submit("two".into()));
        app.update(Msg::Command(Command::PlanMode(Some(true))));
        let id = Uuid::new_v4();
        app.update(Msg::ChatCreated(chat(id)));
        assert!(app.plan_mode);
        let effects = app.update(Msg::SendFailed {
            text: "two".into(),
            message: "HTTP 500".into(),
            plan_mode: Some(true),
            seq: 0,
            mcp_rejected: false,
        });
        assert_eq!(
            effects,
            vec![
                Effect::RestoreComposer("two".into()),
                Effect::RefreshChat {
                    chat: id,
                    generation: 1
                }
            ]
        );
        assert!(
            matches!(app.notices.last(), Some(Notice::Error(m)) if m.contains("Could not send the message") && m.contains("HTTP 500")),
            "{:?}",
            app.notices.last()
        );
        app.update(Msg::ChatRefreshed(chat_with_plan(id, "")));
        assert!(!app.plan_mode);
        assert!(
            matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("Plan mode is off")),
            "{:?}",
            app.notices.last()
        );
    }

    #[test]
    fn a_later_failed_send_with_the_same_text_leaves_plan_mode_alone() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("one".into()));
        app.update(Msg::Submit("continue".into()));
        app.update(Msg::Command(Command::PlanMode(Some(true))));
        app.update(Msg::ChatCreated(chat(Uuid::new_v4())));
        // The carrying send succeeded.
        app.update(Msg::PlanModeApplied { on: true });
        app.update(Msg::Submit("continue".into()));
        app.update(Msg::SendFailed {
            text: "continue".into(),
            message: "HTTP 500".into(),
            plan_mode: None,
            seq: 0,
            mcp_rejected: false,
        });
        assert!(app.plan_mode);
    }

    #[test]
    fn a_failed_send_without_plan_mode_leaves_it_alone() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat_with_plan(id, "plan"),
            messages: vec![],
        });
        app.update(Msg::Submit("two".into()));
        app.update(Msg::SendFailed {
            text: "two".into(),
            message: "HTTP 500".into(),
            plan_mode: None,
            seq: 0,
            mcp_rejected: false,
        });
        assert!(app.plan_mode);
    }

    #[test]
    fn a_created_chat_that_already_matches_needs_no_update() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Command(Command::PlanMode(Some(true))));
        app.update(Msg::Submit("hi".into()));
        let effects = app.update(Msg::ChatCreated(chat_with_plan(Uuid::new_v4(), "plan")));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::SetPlanMode { .. }))
        );
    }

    #[test]
    fn plan_mode_waits_while_the_chat_loads() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::Started {
            org_id: Uuid::new_v4(),
            open_chat: Some(Uuid::new_v4()),
        });
        assert!(app.update(Msg::Command(Command::PlanMode(None))).is_empty());
        assert!(!app.plan_mode);
        assert!(
            matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("Wait for the chat to load"))
        );
    }

    #[test]
    fn a_failed_plan_mode_update_takes_the_servers_value() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.update(Msg::Command(Command::PlanMode(Some(true))));
        assert_eq!(
            app.update(Msg::PlanModeFailed {
                on: true,
                message: "HTTP 500".into(),
            }),
            vec![Effect::RefreshChat {
                chat: id,
                generation: 1
            }]
        );
        assert!(matches!(app.notices.last(), Some(Notice::Error(m)) if m.contains("plan mode on")));
        app.update(Msg::ChatRefreshed(chat(id)));
        assert!(!app.plan_mode);
    }

    #[test]
    fn plan_mode_changes_go_one_at_a_time_and_the_servers_value_wins() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        let applied = |on| Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::PlanModeApplied { on }),
        };
        let set = |on| Msg::Command(Command::PlanMode(Some(on)));
        assert_eq!(
            app.update(set(true)),
            vec![Effect::SetPlanMode {
                chat: id,
                on: true,
                generation: 1
            }]
        );
        assert!(
            app.update(set(false)).is_empty(),
            "a second change waits for the first"
        );
        assert!(app.update(set(true)).is_empty());
        assert_eq!(
            app.update(applied(true)),
            vec![Effect::RefreshChat {
                chat: id,
                generation: 1
            }],
            "only the latest wanted state goes next, and the server already has it"
        );
        assert_eq!(
            app.update(set(false)),
            vec![Effect::SetPlanMode {
                chat: id,
                on: false,
                generation: 2
            }]
        );
        assert!(app.update(set(true)).is_empty());
        assert_eq!(
            app.update(applied(false)),
            vec![Effect::SetPlanMode {
                chat: id,
                on: true,
                generation: 3
            }]
        );
        assert_eq!(
            app.update(applied(true)),
            vec![Effect::RefreshChat {
                chat: id,
                generation: 2
            }]
        );
        assert!(app.plan_mode);
        // The refetched chat says plan mode is off, as another client set it, and scuttle agrees.
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::ChatRefreshed(chat_with_plan(id, ""))),
        });
        assert!(!app.plan_mode);
    }

    #[test]
    fn two_failed_plan_mode_changes_end_on_the_servers_value() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        let failed = |on| Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::PlanModeFailed {
                on,
                message: "HTTP 500".into(),
            }),
        };
        assert_eq!(
            app.update(Msg::Command(Command::PlanMode(Some(true)))),
            vec![Effect::SetPlanMode {
                chat: id,
                on: true,
                generation: 1
            }]
        );
        assert!(
            app.update(Msg::Command(Command::PlanMode(Some(false))))
                .is_empty()
        );
        // A failed request may still have landed, so the last choice goes next.
        assert_eq!(
            app.update(failed(true)),
            vec![Effect::SetPlanMode {
                chat: id,
                on: false,
                generation: 2
            }]
        );
        assert!(
            matches!(app.notices.last(), Some(Notice::Error(m)) if m.contains("plan mode on") && m.contains("HTTP 500"))
        );
        assert_eq!(
            app.update(failed(false)),
            vec![Effect::RefreshChat {
                chat: id,
                generation: 1
            }]
        );
        assert!(
            matches!(app.notices.last(), Some(Notice::Error(m)) if m.contains("plan mode off") && m.contains("HTTP 500"))
        );
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::ChatRefreshed(chat_with_plan(id, ""))),
        });
        assert!(!app.plan_mode, "the footer shows what the server holds");
    }

    #[test]
    fn a_plan_mode_change_waits_for_the_message_carrying_one() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("one".into()));
        app.update(Msg::Submit("two".into()));
        app.update(Msg::Command(Command::PlanMode(Some(true))));
        let id = Uuid::new_v4();
        let effects = app.update(Msg::ChatCreated(chat(id)));
        assert!(
            effects.iter().any(
                |e| matches!(e, Effect::SendMessage { turn, .. } if turn.plan_mode == Some(true))
            ),
            "{effects:?}"
        );
        assert!(
            app.update(Msg::Command(Command::PlanMode(Some(false))))
                .is_empty(),
            "a PATCH now could land before the message"
        );
        assert!(!app.plan_mode);
        // A refetch answering something else while the change is in flight leaves the footer.
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::ChatRefreshed(chat_with_plan(id, "plan"))),
        });
        assert!(!app.plan_mode);
        assert_eq!(
            app.update(Msg::ForChat {
                chat: id,
                msg: Box::new(Msg::PlanModeApplied { on: true }),
            }),
            vec![Effect::SetPlanMode {
                chat: id,
                on: false,
                generation: 2
            }]
        );
        assert_eq!(
            app.update(Msg::ForChat {
                chat: id,
                msg: Box::new(Msg::PlanModeApplied { on: false }),
            }),
            vec![Effect::RefreshChat {
                chat: id,
                generation: 1
            }]
        );
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::ChatRefreshed(chat_with_plan(id, ""))),
        });
        assert!(!app.plan_mode);
    }

    #[test]
    fn plan_mode_replies_for_a_chat_left_are_dropped() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let old = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat_with_plan(old, "plan"),
            messages: vec![],
        });
        app.update(Msg::Command(Command::PlanMode(Some(false))));
        app.update(Msg::Command(Command::New));
        assert!(
            app.update(Msg::ForChat {
                chat: old,
                msg: Box::new(Msg::PlanModeApplied { on: false }),
            })
            .is_empty()
        );
        app.update(Msg::ForChat {
            chat: old,
            msg: Box::new(Msg::ChatRefreshed(chat_with_plan(old, "plan"))),
        });
        assert!(!app.plan_mode);
        let next = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(next),
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::PlanMode(Some(true)))),
            vec![Effect::SetPlanMode {
                chat: next,
                on: true,
                generation: 2
            }],
            "the old chat's request no longer holds the queue"
        );
    }

    #[test]
    fn stream_events_from_the_old_chat_never_reach_a_new_one() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let old = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(old),
            messages: vec![message(1)],
        });
        app.update(ev(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        let old_stream = app.stream_generation();
        let effects = app.update(Msg::Command(Command::New));
        assert!(effects.contains(&Effect::CloseStream), "{effects:?}");
        assert!(effects.contains(&Effect::ClearView), "{effects:?}");
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Interrupt(_))),
            "/new leaves the old chat running"
        );
        assert_eq!(app.chat_id, None);
        assert_eq!(app.transcript.messages().count(), 0);
        let late = |msg: Msg| Msg::ForStream {
            chat: old,
            generation: old_stream,
            msg: Box::new(msg),
        };
        assert!(
            app.update(late(ev(json!({"type": "message", "message": {"id": 2, "role": "assistant", "content": [{"type": "text", "text": "late"}]}}))))
                .is_empty()
        );
        assert!(
            app.update(late(Msg::StreamEnded { error: None }))
                .is_empty(),
            "no reconnect to the old chat"
        );
        assert_eq!(app.transcript.messages().count(), 0);
        assert_eq!(app.connection, Connection::Idle);
        assert_eq!(app.activity(), None);
        let effects = app.update(Msg::Submit("fresh".into()));
        assert!(
            matches!(effects.as_slice(), [Effect::CreateChat { org: o, text, .. }] if *o == org && text == "fresh"),
            "{effects:?}"
        );
    }

    #[test]
    fn events_for_the_open_chat_still_apply() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.update(Msg::ForStream {
            chat: id,
            generation: app.stream_generation(),
            msg: Box::new(ev(
                json!({"type": "status", "status": {"status": "running"}}),
            )),
        });
        assert_eq!(app.activity(), Some(Activity::Working));
    }

    #[test]
    fn a_failed_workspace_load_says_so_and_workspace_retries_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        app.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::WorkspacesFailed {
                message: "HTTP 502".into(),
            }),
        });
        assert!(
            app.update(Msg::Command(Command::Workspace(Some("dev".into()))))
                .is_empty()
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "Workspaces failed to load: HTTP 502. /workspace retries.".into()
            ))
        );
        assert_eq!(
            app.update(Msg::Command(Command::Workspace(None))),
            vec![
                Effect::ShowPicker(Picker::Workspace),
                Effect::FetchWorkspaces(org)
            ]
        );
        assert_eq!(app.workspaces_state, WorkspacesState::Loading);
    }

    #[test]
    fn workspaces_sort_by_last_used() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let ws = |name: &str, last_used: Option<i64>| WorkspaceRef {
            id: Uuid::new_v4(),
            name: name.into(),
            last_used,
            ..Default::default()
        };
        app.update(Msg::WorkspacesLoaded(vec![
            ws("old", Some(1)),
            ws("never", None),
            ws("new", Some(9)),
        ]));
        let names: Vec<&str> = app.workspaces.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["new", "old", "never"]);
        assert_eq!(app.workspaces_state, WorkspacesState::Loaded);
    }

    #[test]
    fn new_keeps_the_model_and_effort_and_resets_the_rest() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (thinker, _) = with_efforts(&mut app);
        app.update(Msg::ModelChosen(thinker));
        app.update(Msg::EffortChosen("high".into()));
        let ws = Uuid::new_v4();
        app.update(Msg::WorkspacesLoaded(vec![WorkspaceRef {
            id: ws,
            name: "dev".into(),
            ..Default::default()
        }]));
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat_with_plan(Uuid::new_v4(), "plan"),
            messages: vec![],
        });
        app.update(Msg::WorkspaceChosen(Some(ws)));
        assert!(app.plan_mode);
        app.update(Msg::Command(Command::New));
        assert_eq!(app.selected_model, Some(thinker));
        assert_eq!(app.selected_effort.as_deref(), Some("high"));
        assert_eq!(
            app.selected_workspace, None,
            "a new chat has no workspace until chosen"
        );
        assert!(!app.plan_mode);
    }

    #[test]
    fn new_waits_while_a_chat_starts_and_says_when_it_is_already_new() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert!(app.update(Msg::Command(Command::New)).is_empty());
        assert!(
            matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("already a new chat"))
        );
        app.update(Msg::Submit("hi".into()));
        assert!(app.update(Msg::Command(Command::New)).is_empty());
        assert!(matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("Wait")));
    }

    #[test]
    fn new_after_choosing_another_organization_loads_its_lists() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (product, _) = two_orgs(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages: vec![],
        });
        app.update(Msg::OrganizationChosen(product.id));
        let effects = app.update(Msg::Command(Command::New));
        assert!(
            effects.contains(&Effect::FetchModels(product.id)),
            "{effects:?}"
        );
        assert!(effects.contains(&Effect::FetchWorkspaces(product.id)));
    }

    #[test]
    fn new_stops_waiting_for_the_old_chats_reply() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let old = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(old),
            messages: vec![],
        });
        app.update(Msg::Submit("hi".into()));
        app.update(ev(
            json!({"type": "message", "message": {"id": 5, "role": "user", "content": []}}),
        ));
        assert_eq!(app.activity(), Some(Activity::Waiting));
        assert_eq!(app.sent_id, Some(5));
        app.update(Msg::Command(Command::New));
        assert_eq!(
            app.activity(),
            None,
            "the spinner does not carry into the new chat"
        );
        assert!(!app.awaiting_reply);
        assert_eq!(app.sent_id, None);
    }

    #[test]
    fn a_late_plan_mode_failure_from_the_old_chat_leaves_the_new_one_alone() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let old = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat_with_plan(old, "plan"),
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::PlanMode(Some(false)))),
            vec![Effect::SetPlanMode {
                chat: old,
                on: false,
                generation: 1
            }]
        );
        app.update(Msg::Command(Command::New));
        assert!(!app.plan_mode);
        // Taking back the old chat's "off" would turn plan mode on for the new chat.
        assert!(
            app.update(Msg::ForChat {
                chat: old,
                msg: Box::new(Msg::PlanModeFailed {
                    on: false,
                    message: "nope".into(),
                }),
            })
            .is_empty()
        );
        assert!(!app.plan_mode);
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "In the previous chat, could not turn plan mode off: nope".into()
            ))
        );
    }

    #[test]
    fn new_in_another_organization_drops_the_old_model_and_effort() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (product, coder) = two_orgs(&mut app);
        let (thinker, _) = with_efforts(&mut app);
        app.update(Msg::ModelChosen(thinker));
        app.update(Msg::EffortChosen("high".into()));
        let mut open = chat(Uuid::new_v4());
        open.organization_id = Some(coder.id);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: open,
            messages: vec![],
        });
        app.update(Msg::OrganizationChosen(product.id));
        assert_eq!(
            app.selected_model,
            Some(thinker),
            "the open chat keeps its model"
        );
        app.update(Msg::Command(Command::New));
        assert_eq!(app.selected_model, None);
        assert_eq!(app.selected_effort, None);
        assert_eq!(app.current_org(), Some(product.id));
        app.update(Msg::ForOrg {
            org: product.id,
            msg: Box::new(Msg::ModelsLoaded(vec![])),
        });
        let effects = app.update(Msg::Submit("hi".into()));
        assert_eq!(effects, vec![Effect::RestoreComposer("hi".into())]);
        assert!(
            matches!(app.notices.last(), Some(Notice::Error(m)) if m.contains("No chat models are available in Product")),
            "{:?}",
            app.notices.last()
        );
    }

    #[test]
    fn new_in_the_same_organization_keeps_its_lists() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let mut open = chat(Uuid::new_v4());
        open.organization_id = Some(org);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: open,
            messages: vec![],
        });
        let effects = app.update(Msg::Command(Command::New));
        assert_eq!(
            effects,
            vec![Effect::CloseStream, Effect::ClearView, Effect::FetchSkills]
        );
    }

    #[test]
    fn a_shadowing_skill_is_sent_by_its_trigger_and_the_command_still_runs() {
        use crate::skills::Skill;
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert!(
            app.update(Msg::SessionStarted)
                .contains(&Effect::FetchSkills)
        );
        app.update(Msg::UserLoaded(UserRef {
            id: Uuid::new_v4(),
            username: "nick".into(),
        }));
        app.update(Msg::SkillsLoaded(vec![Skill {
            name: "new".into(),
            description: "Scaffold a crate".into(),
        }]));
        let id = Uuid::new_v4();
        let mut open = chat(id);
        open.context = Some(types::CodersdkChatContext {
            resources: vec![types::CodersdkChatContextResource {
                kind: Some(types::CodersdkChatContextResourceKind("skill".into())),
                status: Some(types::CodersdkChatContextResourceStatus("ok".into())),
                skill_name: Some("lint".into()),
                skill_description: Some("Run the linters".into()),
                ..Default::default()
            }],
            ..Default::default()
        });
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: open,
            messages: vec![],
        });
        let labels: Vec<String> = app.slash_menu().iter().map(|e| e.label.clone()).collect();
        assert!(labels.contains(&"/nick:new".to_owned()), "{labels:?}");
        assert!(labels.contains(&"/lint".to_owned()), "{labels:?}");
        let effects = app.update(Msg::Submit("/nick:new scuttle-mcp".into()));
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { text, .. }] if text == "/personal/new scuttle-mcp"),
            "{effects:?}"
        );
        let effects = app.update(Msg::Submit("/new".into()));
        assert!(
            effects.contains(&Effect::ClearView),
            "the built-in /new still runs: {effects:?}"
        );
    }

    #[test]
    fn a_shadowing_skill_before_the_user_loads_still_sends_its_personal_trigger() {
        use crate::skills::Skill;
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::SkillsLoaded(vec![Skill {
            name: "model".into(),
            description: "Pick a model for me".into(),
        }]));
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        let menu = app.slash_menu();
        let skill = menu
            .iter()
            .find(|e| e.description == "Pick a model for me")
            .expect("the skill is listed");
        assert_eq!(
            skill.label, "/personal/model",
            "no owner to name, so a neutral label"
        );
        assert_eq!(skill.insert, "/personal/model ");
        let effects = app.update(Msg::Submit("/personal/model fast".into()));
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { text, .. }] if text == "/personal/model fast"),
            "{effects:?}"
        );
    }

    #[test]
    fn opening_a_workspace_chat_refetches_the_skills() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let mut bound = chat(Uuid::new_v4());
        bound.workspace_id = Some(Uuid::new_v4());
        let effects = app.update(Msg::ChatLoaded {
            has_more: None,
            chat: bound,
            messages: vec![],
        });
        assert!(effects.contains(&Effect::FetchSkills), "{effects:?}");
        app.update(Msg::Command(Command::New));
        let effects = app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages: vec![],
        });
        assert!(!effects.contains(&Effect::FetchSkills), "{effects:?}");
    }

    #[test]
    fn a_failed_refetch_keeps_the_skills_already_loaded() {
        use crate::skills::Skill;
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::SkillsLoaded(vec![Skill {
            name: "deploy".into(),
            description: "Ship it".into(),
        }]));
        app.update(Msg::SkillsFailed("HTTP 503".into()));
        let menu = app.slash_menu();
        assert!(menu.iter().any(|e| e.label == "/deploy"), "{menu:?}");
        assert!(
            !menu
                .iter()
                .any(|e| e.label.starts_with("Skills are unavailable")),
            "{menu:?}"
        );
    }

    #[test]
    fn a_refreshed_chat_lists_the_workspace_skills_its_context_gained() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        assert!(!app.slash_menu().iter().any(|e| e.label == "/lint"));
        let mut refreshed = chat(id);
        refreshed.context = Some(types::CodersdkChatContext {
            resources: vec![types::CodersdkChatContextResource {
                kind: Some(types::CodersdkChatContextResourceKind("skill".into())),
                status: Some(types::CodersdkChatContextResourceStatus("ok".into())),
                skill_name: Some("lint".into()),
                ..Default::default()
            }],
            ..Default::default()
        });
        app.update(Msg::ChatRefreshed(refreshed));
        assert!(app.slash_menu().iter().any(|e| e.label == "/lint"));
    }

    #[test]
    fn a_skill_load_failure_leaves_the_commands_and_one_line() {
        let mut app = App::new(BusyBehavior::Queue, true);
        assert!(
            app.slash_menu()
                .iter()
                .any(|e| e.label == "Loading skills…")
        );
        app.update(Msg::SkillsFailed("HTTP 404".into()));
        let menu = app.slash_menu();
        assert!(menu.iter().any(|e| e.label == "/new"));
        assert!(
            menu.iter()
                .any(|e| e.label == "Skills are unavailable: HTTP 404")
        );
    }

    #[test]
    fn new_after_a_failed_load_starts_fresh_instead_of_retrying() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = Uuid::new_v4();
        let missing = Uuid::new_v4();
        app.update(Msg::Started {
            org_id: org,
            open_chat: Some(missing),
        });
        app.update(Msg::ChatLoadFailed {
            chat_id: missing,
            message: "gone".into(),
        });
        let effects = app.update(Msg::Command(Command::New));
        assert!(effects.contains(&Effect::ClearView), "{effects:?}");
        let effects = app.update(Msg::Submit("hi".into()));
        assert!(
            matches!(effects.as_slice(), [Effect::CreateChat { org: o, .. }] if *o == org),
            "{effects:?}"
        );
    }

    /// Opens a chat, then starts a new one, and returns the old chat's id.
    fn left_a_chat(app: &mut App) -> Uuid {
        started(app);
        let old = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(old),
            messages: vec![],
        });
        app.update(Msg::Command(Command::New));
        old
    }

    #[test]
    fn a_late_send_failure_from_the_previous_chat_restores_its_text_only() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let old = left_a_chat(&mut app);
        app.update(Msg::Command(Command::PlanMode(Some(true))));
        app.update(Msg::Submit("new question".into()));
        assert_eq!(app.activity(), Some(Activity::Waiting));
        let effects = app.update(Msg::ForChat {
            chat: old,
            msg: Box::new(Msg::SendFailed {
                text: "old question".into(),
                message: "busy".into(),
                plan_mode: Some(true),
                seq: 0,
                mcp_rejected: false,
            }),
        });
        assert_eq!(
            effects,
            vec![Effect::RestoreComposer("old question".into())]
        );
        assert!(
            app.plan_mode,
            "the old chat's plan mode change is not taken back here"
        );
        assert_eq!(app.activity(), Some(Activity::Waiting));
        assert!(
            matches!(app.notices.last(), Some(Notice::Error(m)) if m.contains("previous chat") && m.contains("busy") && !m.contains("Plan mode")),
            "{:?}",
            app.notices.last()
        );
    }

    #[test]
    fn a_late_api_failure_from_the_previous_chat_says_so() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let old = left_a_chat(&mut app);
        let effects = app.update(Msg::ForChat {
            chat: old,
            msg: Box::new(Msg::ApiFailed {
                action: "compact the chat",
                message: "gone".into(),
            }),
        });
        assert!(effects.is_empty());
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "In the previous chat, could not compact the chat: gone".into()
            ))
        );
    }

    #[test]
    fn plan_mode_after_a_failed_load_says_a_message_retries_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = Uuid::new_v4();
        let id = Uuid::new_v4();
        app.update(Msg::Started {
            org_id: org,
            open_chat: Some(id),
        });
        app.update(Msg::Command(Command::PlanMode(Some(true))));
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info(
                "Wait for the chat to load, then set plan mode.".into()
            ))
        );
        app.update(Msg::ChatLoadFailed {
            chat_id: id,
            message: "HTTP 500".into(),
        });
        assert!(
            app.update(Msg::Command(Command::PlanMode(Some(true))))
                .is_empty()
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info(
                "The chat did not load. Send a message to retry, then set plan mode.".into()
            ))
        );
        assert!(!app.plan_mode);
    }

    #[test]
    fn web_opens_a_chat_that_is_loading_or_failed_to_load() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let id = Uuid::new_v4();
        app.update(Msg::Started {
            org_id: Uuid::new_v4(),
            open_chat: Some(id),
        });
        assert_eq!(
            app.update(Msg::Command(Command::Web)),
            vec![Effect::OpenWeb(id)]
        );
        assert!(app.update(Msg::Command(Command::Compact)).is_empty());
        assert!(matches!(app.notices.last(), Some(Notice::Error(m)) if m == "Start a chat first."));
        app.update(Msg::ChatLoadFailed {
            chat_id: id,
            message: "HTTP 500".into(),
        });
        assert_eq!(
            app.update(Msg::Command(Command::Web)),
            vec![Effect::OpenWeb(id)]
        );
        assert!(app.update(Msg::Command(Command::Clear)).is_empty());
        assert!(matches!(app.notices.last(), Some(Notice::Error(m)) if m == "Start a chat first."));
    }

    #[test]
    fn web_without_a_browser_asks_the_ui_to_copy_the_url() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let url = "https://coder.example.com/agents/x".to_owned();
        let before = app.notices.len();
        assert_eq!(
            app.update(Msg::WebOpened {
                url: url.clone(),
                outcome: Err("over SSH".into()),
            }),
            vec![Effect::CopyWebUrl(url)]
        );
        assert_eq!(app.notices.len(), before, "the UI reports the copy");
    }

    #[test]
    fn organization_on_a_blank_chat_says_when_it_reset_choices() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (product, coder) = two_orgs(&mut app);
        let (thinker, _) = with_efforts(&mut app);
        app.update(Msg::ModelChosen(thinker));
        app.update(Msg::OrganizationChosen(product.id));
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info(
                "New chats will use Product. Model, effort, and workspace reset to its defaults."
                    .into()
            ))
        );
        app.update(Msg::WorkspaceChosen(Some(Uuid::new_v4())));
        app.update(Msg::OrganizationChosen(coder.id));
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info(
                "New chats will use Coder. Model, effort, and workspace reset to its defaults."
                    .into()
            ))
        );
        app.update(Msg::OrganizationChosen(product.id));
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info("New chats will use Product.".into())),
            "nothing was set, so nothing reset"
        );
    }

    #[test]
    fn a_late_plan_mode_failure_from_the_old_chat_is_reported() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let old = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(old),
            messages: vec![],
        });
        app.update(Msg::Command(Command::PlanMode(Some(true))));
        app.update(Msg::Command(Command::New));
        app.update(Msg::ForChat {
            chat: old,
            msg: Box::new(Msg::PlanModeFailed {
                on: true,
                message: "nope".into(),
            }),
        });
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "In the previous chat, could not turn plan mode on: nope".into()
            ))
        );
        assert!(!app.plan_mode);
    }

    #[test]
    fn a_failed_organization_load_still_loads_the_requested_chat() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let id = Uuid::new_v4();
        let effects = app.update(Msg::OrganizationsFailed {
            message: "HTTP 500".into(),
            open_chat: Some(id),
        });
        assert!(effects.contains(&Effect::LoadChat(id)), "{effects:?}");
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "Could not load your organization: HTTP 500".into()
            ))
        );
        let org = Uuid::new_v4();
        let mut open = chat(id);
        open.organization_id = Some(org);
        let effects = app.update(Msg::ChatLoaded {
            has_more: None,
            chat: open,
            messages: vec![],
        });
        assert!(effects.contains(&Effect::FetchModels(org)), "{effects:?}");
        assert!(effects.contains(&Effect::FetchWorkspaces(org)));
    }

    #[test]
    fn a_failed_organization_load_on_a_blank_chat_only_explains() {
        let mut app = App::new(BusyBehavior::Queue, true);
        assert!(
            app.update(Msg::OrganizationsFailed {
                message: "HTTP 500".into(),
                open_chat: None,
            })
            .is_empty()
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "Could not load your organization: HTTP 500".into()
            ))
        );
    }

    fn running() -> Msg {
        ev(json!({"type": "status", "status": {"status": "running"}}))
    }

    #[test]
    fn opening_a_chat_closes_the_old_one_and_loads_the_new_one() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(a),
            messages: vec![message(1)],
        });
        app.update(running());
        let effects = app.update(Msg::OpenChat(b));
        assert_eq!(
            effects,
            vec![Effect::CloseStream, Effect::ClearView, Effect::LoadChat(b)]
        );
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Interrupt(_))),
            "leaving a running chat never interrupts it"
        );
        assert_eq!(app.chat_id, None);
        assert_eq!(app.transcript.messages().count(), 0);
        assert_eq!(app.activity(), None);
        assert_eq!(app.connection, Connection::Connecting);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(b),
            messages: vec![message(4)],
        });
        assert_eq!(app.chat_id, Some(b));
        assert_eq!(app.transcript.last_message_id(), Some(4));
    }

    #[test]
    fn a_slow_reply_for_an_abandoned_chat_is_dropped() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::OpenChat(a));
        app.update(Msg::OpenChat(b));
        let notices = app.notices.len();
        assert!(
            app.update(Msg::ChatLoaded {
                has_more: None,
                chat: chat(a),
                messages: vec![message(1)],
            })
            .is_empty()
        );
        assert!(
            app.update(Msg::ChatLoadFailed {
                chat_id: a,
                message: "gone".into(),
            })
            .is_empty()
        );
        assert_eq!(app.chat_id, None);
        assert_eq!(
            app.notices.len(),
            notices,
            "an abandoned load fails silently"
        );
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(b),
            messages: vec![],
        });
        assert_eq!(app.chat_id, Some(b));
    }

    #[test]
    fn a_to_b_and_back_to_a_drops_the_first_visits_events() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let visit = |app: &mut App, id: Uuid| {
            app.update(Msg::OpenChat(id));
            app.update(Msg::ChatLoaded {
                has_more: None,
                chat: chat(id),
                messages: vec![message(1)],
            });
            app.stream_generation()
        };
        let first = visit(&mut app, a);
        visit(&mut app, b);
        let second = visit(&mut app, a);
        let reply = |generation| Msg::ForStream {
            chat: a,
            generation,
            msg: Box::new(ev(
                json!({"type": "message", "message": {"id": 9, "role": "assistant", "content": [{"type": "text", "text": "late"}]}}),
            )),
        };
        app.update(reply(first));
        assert_eq!(
            app.transcript.messages().count(),
            1,
            "the first visit's event is dropped"
        );
        app.update(reply(second));
        assert_eq!(app.transcript.messages().count(), 2);
    }

    #[test]
    fn an_opened_chat_uses_its_own_model() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (thinker, plain) = with_efforts(&mut app);
        app.update(Msg::ModelChosen(plain));
        let id = Uuid::new_v4();
        app.update(Msg::OpenChat(id));
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(types::CodersdkChat {
                id: Some(id),
                last_model_config_id: Some(thinker),
                ..Default::default()
            }),
            messages: vec![],
        });
        assert_eq!(app.selected_model, Some(thinker));
        assert_eq!(
            app.selected_effort, None,
            "the opened chat's own effort applies"
        );
    }

    #[test]
    fn an_effort_chosen_in_one_chat_does_not_carry_into_another() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (thinker, _) = with_efforts(&mut app);
        app.update(Msg::EffortChosen("high".into()));
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::OpenChat(a));
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(types::CodersdkChat {
                id: Some(a),
                last_model_config_id: Some(thinker),
                last_reasoning_effort: Some("low".into()),
                ..Default::default()
            }),
            messages: vec![],
        });
        assert_eq!(
            app.effort().as_deref(),
            Some("low"),
            "the chat's last effort"
        );
        app.update(Msg::OpenChat(b));
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(types::CodersdkChat {
                id: Some(b),
                last_model_config_id: Some(thinker),
                ..Default::default()
            }),
            messages: vec![],
        });
        assert_eq!(app.effort().as_deref(), Some("medium"), "the model default");
    }

    #[test]
    fn opening_waits_while_a_chat_is_being_created() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("hi".into()));
        assert!(app.update(Msg::OpenChat(Uuid::new_v4())).is_empty());
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info(
                "Wait for this chat to finish starting, then open another.".into()
            ))
        );
    }

    #[test]
    fn text_waiting_on_a_superseded_load_goes_back_to_the_composer() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::OpenChat(Uuid::new_v4()));
        app.update(Msg::Submit("hello".into()));
        let effects = app.update(Msg::OpenChat(Uuid::new_v4()));
        assert!(
            effects.contains(&Effect::RestoreComposer("hello".into())),
            "{effects:?}"
        );
    }

    #[test]
    fn opening_a_loading_chat_again_keeps_its_queued_message() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let x = Uuid::new_v4();
        app.update(Msg::OpenChat(x));
        app.update(Msg::Submit("hello".into()));
        assert!(app.update(Msg::OpenChat(x)).is_empty());
        assert_eq!(app.pending_text.as_deref(), Some("hello"));
        let effects = app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(x),
            messages: vec![],
        });
        assert!(
            effects.iter().any(|e| matches!(
                e,
                Effect::SendMessage { chat, text, .. } if *chat == x && text == "hello"
            )),
            "{effects:?}"
        );
    }

    #[test]
    fn the_first_page_loads_at_session_start_and_opening_a_chat_marks_it_read() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let effects = app.update(Msg::SessionStarted);
        assert!(
            effects.contains(&Effect::FetchChats {
                query: ListQuery::Default,
                offset: 0
            }),
            "{effects:?}"
        );
        let id = Uuid::new_v4();
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats: vec![types::CodersdkChat {
                id: Some(id),
                has_unread: Some(true),
                ..Default::default()
            }],
        });
        app.update(Msg::OpenChat(id));
        assert_eq!(app.chats.find(id).and_then(|c| c.has_unread), Some(false));
        assert_eq!(
            app.update(Msg::LoadChats {
                query: ListQuery::Default,
                more: true
            }),
            vec![],
            "a short page has nothing more"
        );
    }

    #[test]
    fn the_session_start_fetches_user_scoped_state() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let effects = app.update(Msg::SessionStarted);
        assert!(effects.contains(&Effect::FetchMe), "{effects:?}");
        let me = UserRef {
            id: Uuid::new_v4(),
            username: "nick".into(),
        };
        app.update(Msg::UserLoaded(me.clone()));
        assert_eq!(app.me, Some(me));
    }

    #[test]
    fn new_and_organization_retry_a_failed_organization_lookup() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::OrganizationsFailed {
            message: "HTTP 502".into(),
            open_chat: None,
        });
        assert_eq!(
            app.update(Msg::Command(Command::New)),
            vec![Effect::FetchOrganizations]
        );
        assert!(
            app.update(Msg::Command(Command::Organization(None)))
                .is_empty(),
            "a retry already in flight is not sent twice"
        );
        app.update(Msg::OrganizationsFailed {
            message: "HTTP 502".into(),
            open_chat: None,
        });
        assert_eq!(
            app.update(Msg::Command(Command::Organization(None))),
            vec![Effect::FetchOrganizations]
        );
    }

    #[test]
    fn a_retried_lookup_picks_the_saved_organization_and_loads_its_lists() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (product, coder) = (org("Product", false), org("Coder", true));
        app.saved_org = Some(product.id);
        app.update(Msg::OrganizationsFailed {
            message: "HTTP 502".into(),
            open_chat: None,
        });
        app.update(Msg::Command(Command::New));
        let effects = app.update(Msg::OrganizationsLoaded(vec![coder, product.clone()]));
        assert_eq!(app.org_id, Some(product.id));
        assert!(
            effects.contains(&Effect::FetchModels(product.id)),
            "{effects:?}"
        );
        assert!(effects.contains(&Effect::FetchPrefs));
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info("New chats will use Product.".into()))
        );
    }

    fn listed(id: Uuid, title: &str, updated: &str) -> types::CodersdkChat {
        types::CodersdkChat {
            id: Some(id),
            title: Some(title.into()),
            updated_at: Some(updated.parse().unwrap()),
            ..Default::default()
        }
    }

    fn watch(kind: &str, chat: types::CodersdkChat) -> Msg {
        Msg::Watch(coder_sdk::WatchEvent {
            kind: kind.into(),
            event: Some(types::CodersdkChatWatchEvent {
                chat: Some(chat),
                kind: Some(types::CodersdkChatWatchEventKind(kind.into())),
                tool_calls: vec![],
            }),
            raw: serde_json::Value::Null,
        })
    }

    #[test]
    fn info_fetches_cost_and_refetches_it_when_the_tree_changes() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(root),
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Info)),
            vec![
                Effect::ShowInfo,
                Effect::RefreshChat {
                    chat: root,
                    generation: 1
                },
                Effect::FetchCost {
                    chat: root,
                    generation: 1
                }
            ]
        );
        let mut sub = listed(child, "explore", "2026-09-30T10:00:00Z");
        sub.parent_chat_id = Some(root);
        assert_eq!(
            app.update(watch("status_change", sub.clone())),
            vec![Effect::FetchCost {
                chat: root,
                generation: 2
            }],
            "a subagent's turn changes the tree's cost"
        );
        app.update(Msg::ForChat {
            chat: root,
            msg: Box::new(Msg::CostHidden { generation: 2 }),
        });
        assert!(matches!(app.info_panel, Some(CostState::Hidden)));
        app.update(Msg::InfoClosed);
        assert!(app.update(watch("status_change", sub)).is_empty());
    }

    #[test]
    fn only_the_latest_cost_reply_applies_and_a_failed_refetch_keeps_the_total() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(root),
            messages: vec![],
        });
        let fetched = |effects: Vec<Effect>| {
            effects
                .into_iter()
                .find_map(|e| match e {
                    Effect::FetchCost { generation, .. } => Some(generation),
                    _ => None,
                })
                .expect("a cost fetch")
        };
        let cost = |micros: i64| {
            serde_json::from_value::<types::CodersdkChatCost>(
                json!({"total_cost_micros": micros, "request_count": 1}),
            )
            .unwrap()
        };
        let reply = |app: &mut App, msg: Msg| {
            app.update(Msg::ForChat {
                chat: root,
                msg: Box::new(msg),
            })
        };
        let total = |app: &App| match &app.info_panel {
            Some(CostState::Loaded(c)) => c.total_cost_micros,
            _ => None,
        };
        let first = fetched(app.update(Msg::Command(Command::Info)));
        // The server's tree root wins over the parent, as in the web UI.
        let mut sub = listed(child, "explore", "2026-09-30T10:00:00Z");
        sub.root_chat_id = Some(root);
        sub.parent_chat_id = Some(Uuid::new_v4());
        let second = fetched(app.update(watch("status_change", sub.clone())));
        assert_ne!(first, second);
        reply(
            &mut app,
            Msg::CostLoaded {
                cost: cost(5),
                generation: second,
            },
        );
        reply(
            &mut app,
            Msg::CostLoaded {
                cost: cost(3),
                generation: first,
            },
        );
        assert_eq!(
            total(&app),
            Some(5),
            "the earlier request's late reply is dropped"
        );
        let third = fetched(app.update(watch("status_change", sub)));
        reply(
            &mut app,
            Msg::CostFailed {
                message: "timed out".into(),
                generation: third,
            },
        );
        assert_eq!(
            total(&app),
            Some(5),
            "a failed refetch keeps the loaded total"
        );
    }

    fn for_watch(generation: u64, msg: Msg) -> Msg {
        Msg::ForWatch {
            generation,
            msg: Box::new(msg),
        }
    }

    #[test]
    fn the_watch_opens_at_session_start_and_backs_off_until_it_holds() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let effects = app.update(Msg::SessionStarted);
        assert!(effects.contains(&Effect::OpenWatch {
            delay: Duration::ZERO,
            generation: 1
        }));
        assert_eq!(
            app.update(Msg::WatchEnded { error: None }),
            vec![Effect::OpenWatch {
                delay: backoff(1),
                generation: 2
            }]
        );
        assert_eq!(
            app.update(Msg::WatchEnded { error: None }),
            vec![Effect::OpenWatch {
                delay: backoff(2),
                generation: 3
            }]
        );
        app.update(Msg::WatchHealthy);
        assert_eq!(
            app.update(Msg::WatchEnded { error: None }),
            vec![Effect::OpenWatch {
                delay: backoff(1),
                generation: 4
            }]
        );
    }

    #[test]
    fn a_stale_watch_connection_cannot_apply_anything_after_a_reconnect() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.update(Msg::SessionStarted);
        assert_eq!(
            app.update(for_watch(1, Msg::WatchEnded { error: None })),
            vec![Effect::OpenWatch {
                delay: backoff(1),
                generation: 2
            }]
        );
        assert_eq!(
            app.update(for_watch(1, Msg::WatchEnded { error: None })),
            vec![],
            "a second end from the old connection does not open a third"
        );
        app.update(for_watch(1, Msg::WatchConnected));
        assert!(!app.chats.watch_live);
        app.update(for_watch(
            1,
            watch("title_change", listed(id, "Stale", "2026-09-30T10:00:00Z")),
        ));
        assert_eq!(
            app.chat.as_ref().and_then(|c| c.title.as_deref()),
            Some("t")
        );
        app.update(for_watch(1, Msg::WatchHealthy));
        app.update(for_watch(2, Msg::WatchConnected));
        assert!(app.chats.watch_live);
        app.update(for_watch(
            2,
            watch(
                "title_change",
                listed(id, "Current", "2026-09-30T10:00:00Z"),
            ),
        ));
        assert_eq!(
            app.chat.as_ref().and_then(|c| c.title.as_deref()),
            Some("Current")
        );
        assert_eq!(
            app.update(for_watch(2, Msg::WatchEnded { error: None })),
            vec![Effect::OpenWatch {
                delay: backoff(2),
                generation: 3
            }],
            "the old connection's health report did not reset the backoff"
        );
    }

    #[test]
    fn a_watch_reconnect_during_a_switch_refetches_without_touching_the_open_chat() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::WatchConnected);
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats: vec![
                listed(a, "A", "2026-09-30T10:00:00Z"),
                listed(b, "B", "2026-09-30T09:00:00Z"),
            ],
        });
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(a),
            messages: vec![message(1)],
        });
        assert_eq!(
            app.update(Msg::WatchEnded {
                error: Some("reset".into())
            }),
            vec![Effect::OpenWatch {
                delay: backoff(1),
                generation: 1
            }]
        );
        assert!(!app.chats.watch_live);
        app.update(Msg::OpenChat(b));
        let mut open_b = chat(b);
        open_b.title = Some("B".into());
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: open_b,
            messages: vec![message(7)],
        });
        app.update(running());
        assert_eq!(
            app.update(Msg::WatchConnected),
            vec![Effect::FetchChats {
                query: ListQuery::Default,
                offset: 0
            }]
        );
        assert!(app.chats.watch_live);
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats: vec![
                listed(a, "A", "2026-09-30T10:00:00Z"),
                types::CodersdkChat {
                    has_unread: Some(true),
                    ..listed(b, "B", "2026-09-30T09:00:00Z")
                },
            ],
        });
        let mut done = listed(a, "A", "2026-09-30T10:05:00Z");
        done.status = Some(types::CodersdkChatStatus("waiting".into()));
        app.update(watch("status_change", done));
        app.update(watch("status_change", {
            let mut b_waiting = listed(b, "B", "2026-09-30T10:06:00Z");
            b_waiting.status = Some(types::CodersdkChatStatus("waiting".into()));
            b_waiting
        }));
        assert_eq!(app.chats.find(a).and_then(|c| c.has_unread), Some(true));
        assert_eq!(app.chats.find(b).and_then(|c| c.has_unread), Some(false));
        assert_eq!(app.chat_id, Some(b));
        assert_eq!(
            app.chat.as_ref().and_then(|c| c.title.as_deref()),
            Some("B")
        );
        assert_eq!(app.transcript.messages().count(), 1);
        assert_eq!(
            app.transcript.status,
            Some(ChatStatus::Running),
            "the chat stream owns the open chat's status"
        );
    }

    #[test]
    fn a_watch_reconnect_refetch_keeps_the_open_chats_row_when_it_ranks_past_the_page() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let open = Uuid::new_v4();
        let mut pinned = listed(open, "Open", "2026-09-30T08:00:00Z");
        pinned.pin_order = Some(1);
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats: vec![pinned],
        });
        app.update(Msg::OpenChat(open));
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(open),
            messages: vec![],
        });
        app.update(Msg::WatchEnded { error: None });
        assert_eq!(
            app.update(Msg::WatchConnected),
            vec![Effect::FetchChats {
                query: ListQuery::Default,
                offset: 0
            }]
        );
        // While the watch was down the chat was unpinned elsewhere and 50 newer chats arrived,
        // so the server ranks it 51st, while the stale pin sorts it inside the fresh page.
        let newer: Vec<_> = (0..50)
            .map(|n| {
                listed(
                    Uuid::new_v4(),
                    &format!("N{n}"),
                    &format!("2026-09-30T09:{n:02}:00Z"),
                )
            })
            .collect();
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats: newer,
        });
        assert!(
            app.chats.find(open).is_some(),
            "the open chat keeps its row in the list"
        );
        assert_eq!(app.chats.main.chats.len(), 51);
    }

    #[test]
    fn the_watch_updates_the_open_chats_record_and_refetches_it_when_its_context_changes() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.update(watch(
            "title_change",
            listed(id, "Renamed", "2026-09-30T10:00:00Z"),
        ));
        assert_eq!(
            app.chat.as_ref().and_then(|c| c.title.as_deref()),
            Some("Renamed")
        );
        assert_eq!(
            app.update(watch(
                "context_dirty",
                listed(id, "Renamed", "2026-09-30T10:00:00Z")
            )),
            vec![Effect::RefreshChat {
                chat: id,
                generation: 1
            }]
        );
        let mut fresh = chat(id);
        fresh.title = Some("Fresh".into());
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::ChatRefreshed(fresh)),
        });
        assert_eq!(
            app.chat.as_ref().and_then(|c| c.title.as_deref()),
            Some("Fresh")
        );
    }

    #[test]
    fn an_archived_chat_does_not_send() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.update(watch("deleted", listed(id, "t", "2026-09-30T10:00:00Z")));
        assert!(app.is_archived());
        assert_eq!(
            app.update(Msg::Submit("hello".into())),
            vec![Effect::RestoreComposer("hello".into())]
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "This chat is archived. Ctrl+A in /chats unarchives it.".into()
            ))
        );
    }

    #[test]
    fn a_watch_reconnect_during_an_in_flight_load_refetches_once_it_lands() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let page: Vec<_> = (0..50)
            .map(|n| {
                listed(
                    Uuid::new_v4(),
                    &format!("N{n}"),
                    &format!("2026-09-30T09:{n:02}:00Z"),
                )
            })
            .collect();
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats: page,
        });
        assert_eq!(
            app.update(Msg::LoadChats {
                query: ListQuery::Default,
                more: true
            }),
            vec![Effect::FetchChats {
                query: ListQuery::Default,
                offset: 50
            }]
        );
        assert_eq!(
            app.update(Msg::WatchConnected),
            vec![],
            "the load more is still in flight"
        );
        assert_eq!(
            app.update(Msg::ChatsLoaded {
                query: ListQuery::Default,
                offset: 50,
                chats: vec![],
            }),
            vec![Effect::FetchChats {
                query: ListQuery::Default,
                offset: 0
            }],
            "the outage is refetched once the load lands"
        );
        app.update(Msg::WatchEnded { error: None });
        assert_eq!(app.update(Msg::WatchConnected), vec![]);
        assert_eq!(
            app.update(Msg::ChatsFailed {
                query: ListQuery::Default,
                message: "HTTP 502".into(),
            }),
            vec![Effect::FetchChats {
                query: ListQuery::Default,
                offset: 0
            }],
            "a failed load also leaves room for the refetch"
        );
        assert_eq!(
            app.update(Msg::ChatsLoaded {
                query: ListQuery::Default,
                offset: 0,
                chats: vec![],
            }),
            vec![],
            "the refetch is asked for once"
        );
    }

    fn loaded_list(app: &mut App, chats: Vec<types::CodersdkChat>) {
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats,
        });
    }

    fn loaded_archived(app: &mut App, chats: Vec<types::CodersdkChat>) {
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Archived,
            offset: 0,
            chats,
        });
    }

    #[test]
    fn archiving_waits_for_a_running_subagent_and_a_refusal_leaves_the_list() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
        let mut family = listed(root, "root", "2026-09-30T10:00:00Z");
        let mut sub = listed(child, "explore", "2026-09-30T10:00:00Z");
        sub.parent_chat_id = Some(root);
        sub.status = Some(types::CodersdkChatStatus("running".into()));
        family.children = vec![sub];
        loaded_list(&mut app, vec![family]);
        assert!(
            app.update(Msg::ChatAction(ChatAction::ToggleArchive(root)))
                .is_empty()
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "Wait for this chat and its subagents to stop, then archive it.".into()
            ))
        );
        let mut done = listed(child, "explore", "2026-09-30T10:05:00Z");
        done.status = Some(types::CodersdkChatStatus("waiting".into()));
        app.update(watch("status_change", done));
        assert_eq!(
            app.update(Msg::ChatAction(ChatAction::ToggleArchive(root))),
            vec![Effect::UpdateChat {
                chat: root,
                change: ChatChange::Archived(true)
            }]
        );
        // A subagent that starts again before the request lands makes the server refuse it.
        app.update(Msg::ChatUpdateFailed {
            chat: root,
            change: ChatChange::Archived(true),
            message: "Chat has a running subagent.".into(),
        });
        assert_eq!(app.chats.find(root).and_then(|c| c.archived), None);
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "Could not archive the chat: Chat has a running subagent.".into()
            ))
        );
        app.update(Msg::ChatUpdated {
            chat: root,
            change: ChatChange::Archived(true),
        });
        assert_eq!(app.chats.find(root).and_then(|c| c.archived), Some(true));
    }

    #[test]
    fn archiving_with_the_workspace_reports_what_became_of_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (id, ws) = (Uuid::new_v4(), Uuid::new_v4());
        let mut ship = listed(id, "ship", "2026-09-30T10:00:00Z");
        ship.workspace_id = Some(ws);
        loaded_list(&mut app, vec![ship]);
        assert_eq!(
            app.update(Msg::ChatAction(ChatAction::ArchiveAndDeleteWorkspace {
                chat: id,
                workspace: ws
            })),
            vec![Effect::ArchiveAndDeleteWorkspace {
                chat: id,
                workspace: ws
            }]
        );
        for (outcome, notice) in [
            (
                WorkspaceDeletion::Started {
                    no_provisioner: false,
                },
                Notice::Info("Archived, and its workspace is being deleted.".into()),
            ),
            (
                WorkspaceDeletion::Started {
                    no_provisioner: true,
                },
                Notice::Info(
                    "Archived. The workspace delete is queued, but no provisioner is available, so it runs once one comes online."
                        .into(),
                ),
            ),
            (
                WorkspaceDeletion::AlreadyGone,
                Notice::Info("Archived. Its workspace was already deleted.".into()),
            ),
            (
                WorkspaceDeletion::Failed("HTTP 500: provisioner exploded".into()),
                Notice::Error(
                    "Archived, but the workspace delete failed, so the chat stays archived; unarchive it from the Archived tab in /chats, or delete the workspace in the web UI: HTTP 500: provisioner exploded"
                        .into(),
                ),
            ),
        ] {
            app.update(Msg::ArchivedWithWorkspace { chat: id, outcome });
            assert_eq!(app.chats.find(id).and_then(|c| c.archived), Some(true));
            assert_eq!(app.notices.last(), Some(&notice));
        }
    }

    #[test]
    fn archive_and_delete_refuses_when_the_workspace_is_not_the_one_shown() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (id, shown, now) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut ship = listed(id, "ship", "2026-09-30T10:00:00Z");
        ship.workspace_id = Some(now);
        loaded_list(&mut app, vec![ship]);
        assert!(
            app.update(Msg::ChatAction(ChatAction::ArchiveAndDeleteWorkspace {
                chat: id,
                workspace: shown
            }))
            .is_empty(),
            "nothing is archived or deleted"
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "This chat's workspace changed. Open the archive box again.".into()
            ))
        );
        assert_eq!(
            app.update(Msg::ChatAction(ChatAction::ArchiveAndDeleteWorkspace {
                chat: id,
                workspace: now
            })),
            vec![Effect::ArchiveAndDeleteWorkspace {
                chat: id,
                workspace: now
            }]
        );
    }

    #[test]
    fn a_running_chat_refuses_archive_and_delete() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (id, ws) = (Uuid::new_v4(), Uuid::new_v4());
        let mut busy = listed(id, "busy", "2026-09-30T10:00:00Z");
        busy.workspace_id = Some(ws);
        busy.status = Some(types::CodersdkChatStatus("running".into()));
        loaded_list(&mut app, vec![busy]);
        assert!(
            app.update(Msg::ChatAction(ChatAction::ArchiveAndDeleteWorkspace {
                chat: id,
                workspace: ws
            }))
            .is_empty(),
            "no PATCH and no delete"
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "Wait for this chat and its subagents to stop, then archive it.".into()
            ))
        );
    }

    #[test]
    fn a_second_archive_waits_for_the_first_to_finish_however_it_ends() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (id, ws) = (Uuid::new_v4(), Uuid::new_v4());
        let mut ship = listed(id, "ship", "2026-09-30T10:00:00Z");
        ship.workspace_id = Some(ws);
        loaded_list(&mut app, vec![ship]);
        let delete = || {
            Msg::ChatAction(ChatAction::ArchiveAndDeleteWorkspace {
                chat: id,
                workspace: ws,
            })
        };
        let finishes = [
            Msg::ChatUpdateFailed {
                chat: id,
                change: ChatChange::Archived(true),
                message: "no".into(),
            },
            Msg::ArchivedWithWorkspace {
                chat: id,
                outcome: WorkspaceDeletion::AlreadyGone,
            },
            Msg::ArchivedWithWorkspace {
                chat: id,
                outcome: WorkspaceDeletion::Failed("x".into()),
            },
        ];
        for finish in finishes {
            app.chats.update_copies(id, |c| c.archived = Some(false));
            assert_eq!(app.update(delete()).len(), 1, "the first goes out");
            assert!(app.update(delete()).is_empty(), "the second is held back");
            assert!(
                app.update(Msg::ChatAction(ChatAction::Archive(id)))
                    .is_empty()
            );
            assert_eq!(
                app.notices.last(),
                Some(&Notice::Info("This chat is already being archived.".into()))
            );
            app.update(finish);
            app.chats.update_copies(id, |c| c.archived = Some(false));
            assert_eq!(app.update(delete()).len(), 1, "it ended, so a new one goes");
            app.update(Msg::ChatUpdateFailed {
                chat: id,
                change: ChatChange::Archived(true),
                message: "reset".into(),
            });
        }
        assert_eq!(
            app.update(Msg::ChatAction(ChatAction::Archive(id))).len(),
            1
        );
        app.update(Msg::ChatUpdated {
            chat: id,
            change: ChatChange::Archived(true),
        });
        app.chats.update_copies(id, |c| c.archived = Some(false));
        assert_eq!(
            app.update(Msg::ChatAction(ChatAction::Archive(id))).len(),
            1,
            "a plain archive clears too"
        );
    }

    #[test]
    fn a_refresh_showing_the_chat_archived_does_not_free_a_delete_in_flight() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (id, ws) = (Uuid::new_v4(), Uuid::new_v4());
        let mut ship = listed(id, "ship", "2026-09-30T10:00:00Z");
        ship.workspace_id = Some(ws);
        loaded_list(&mut app, vec![ship]);
        let delete = || {
            Msg::ChatAction(ChatAction::ArchiveAndDeleteWorkspace {
                chat: id,
                workspace: ws,
            })
        };
        assert_eq!(app.update(delete()).len(), 1);
        app.chats.update_copies(id, |c| c.archived = Some(true));
        assert!(app.update(delete()).is_empty(), "no second delete");
        assert!(
            app.update(Msg::ChatAction(ChatAction::ToggleArchive(id)))
                .is_empty(),
            "and no unarchive racing the archive"
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info("This chat is already being archived.".into()))
        );
    }

    #[test]
    fn archive_and_delete_needs_a_workspace_and_a_root_that_is_not_archived() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (bare, old, root, child) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let mut archived = listed(old, "old", "2026-09-30T09:00:00Z");
        archived.archived = Some(true);
        archived.workspace_id = Some(Uuid::new_v4());
        let mut family = listed(root, "root", "2026-09-30T08:00:00Z");
        let mut sub = listed(child, "explore", "2026-09-30T08:00:00Z");
        sub.parent_chat_id = Some(root);
        sub.workspace_id = Some(Uuid::new_v4());
        family.children = vec![sub];
        loaded_list(
            &mut app,
            vec![
                listed(bare, "bare", "2026-09-30T10:00:00Z"),
                archived,
                family,
            ],
        );
        assert!(
            app.update(Msg::ChatAction(ChatAction::ArchiveAndDeleteWorkspace {
                chat: bare,
                workspace: Uuid::new_v4()
            }))
            .is_empty()
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "This chat has no workspace to delete.".into()
            ))
        );
        for action in [
            ChatAction::Archive(old),
            ChatAction::ArchiveAndDeleteWorkspace {
                chat: old,
                workspace: Uuid::new_v4(),
            },
        ] {
            assert!(app.update(Msg::ChatAction(action)).is_empty());
            assert_eq!(
                app.notices.last(),
                Some(&Notice::Info("This chat is already archived.".into())),
                "an archive never unarchives"
            );
        }
        assert!(
            app.update(Msg::ChatAction(ChatAction::ArchiveAndDeleteWorkspace {
                chat: child,
                workspace: Uuid::new_v4()
            }))
            .is_empty()
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "Only a root chat can be archived. Archive its parent.".into()
            ))
        );
        assert_eq!(
            app.update(Msg::ChatAction(ChatAction::Archive(bare))),
            vec![Effect::UpdateChat {
                chat: bare,
                change: ChatChange::Archived(true)
            }]
        );
    }

    #[test]
    fn subagents_cannot_be_archived_or_pinned_and_pin_and_read_toggle() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
        let mut family = listed(root, "root", "2026-09-30T10:00:00Z");
        family.has_unread = Some(true);
        let mut sub = listed(child, "explore", "2026-09-30T10:00:00Z");
        sub.parent_chat_id = Some(root);
        family.children = vec![sub];
        loaded_list(&mut app, vec![family]);
        assert!(
            app.update(Msg::ChatAction(ChatAction::ToggleArchive(child)))
                .is_empty()
        );
        assert!(
            app.update(Msg::ChatAction(ChatAction::TogglePin(child)))
                .is_empty()
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error("A subagent cannot be pinned.".into()))
        );
        assert_eq!(
            app.update(Msg::ChatAction(ChatAction::TogglePin(root))),
            vec![Effect::UpdateChat {
                chat: root,
                change: ChatChange::PinOrder(1)
            }]
        );
        assert_eq!(
            app.update(Msg::ChatAction(ChatAction::ToggleRead(root))),
            vec![Effect::UpdateChat {
                chat: root,
                change: ChatChange::Read(true)
            }]
        );
        app.update(Msg::ChatUpdated {
            chat: root,
            change: ChatChange::Read(true),
        });
        assert_eq!(app.chats.find(root).and_then(|c| c.has_unread), Some(false));
    }

    #[test]
    fn rename_edits_the_title_in_the_core_and_saves_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        loaded_list(
            &mut app,
            vec![listed(id, "Untitled", "2026-09-30T10:00:00Z")],
        );
        app.update(Msg::ChatAction(ChatAction::Rename(id)));
        assert_eq!(app.editor.as_ref().map(|e| e.line.text()), Some("Untitled"));
        for _ in 0.."Untitled".len() {
            app.update(Msg::Edit(Edit::Backspace));
        }
        for c in "Watch fix".chars() {
            app.update(Msg::Edit(Edit::Char(c)));
        }
        assert_eq!(
            app.update(Msg::Edit(Edit::Submit)),
            vec![Effect::UpdateChat {
                chat: id,
                change: ChatChange::Title("Watch fix".into())
            }]
        );
        assert!(app.editor.is_none());
        app.update(Msg::ChatUpdated {
            chat: id,
            change: ChatChange::Title("Watch fix".into()),
        });
        assert_eq!(
            app.chats.find(id).and_then(|c| c.title.as_deref()),
            Some("Watch fix")
        );
        app.update(Msg::ChatAction(ChatAction::Rename(id)));
        app.update(Msg::Edit(Edit::Cancel));
        assert!(app.editor.is_none());
    }

    #[test]
    fn unarchiving_moves_the_chat_from_archived_to_the_main_list() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        let mut archived_chat = listed(id, "Old bug", "2026-09-30T10:00:00Z");
        archived_chat.archived = Some(true);
        loaded_archived(&mut app, vec![archived_chat]);
        assert_eq!(
            app.update(Msg::ChatAction(ChatAction::ToggleArchive(id))),
            vec![Effect::UpdateChat {
                chat: id,
                change: ChatChange::Archived(false)
            }]
        );
        app.update(Msg::ChatUpdated {
            chat: id,
            change: ChatChange::Archived(false),
        });
        assert!(
            app.chats
                .page(&ListQuery::Archived)
                .is_some_and(|p| p.chats.iter().all(|c| c.id != Some(id))),
            "removed from the archived page"
        );
        assert!(
            app.chats.page(&ListQuery::Default).is_some_and(|p| p
                .chats
                .iter()
                .any(|c| c.id == Some(id) && c.archived != Some(true))),
            "present in the main page, unarchived"
        );
    }

    #[test]
    fn archiving_pins_to_zero_marks_children_and_adds_an_archived_page_row() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
        let mut family = listed(root, "root", "2026-09-30T10:00:00Z");
        family.pin_order = Some(3);
        let sub = listed(child, "explore", "2026-09-30T10:00:00Z");
        family.children = vec![sub];
        loaded_list(&mut app, vec![family]);
        loaded_archived(&mut app, vec![]);
        assert_eq!(
            app.update(Msg::ChatAction(ChatAction::ToggleArchive(root))),
            vec![Effect::UpdateChat {
                chat: root,
                change: ChatChange::Archived(true)
            }]
        );
        app.update(Msg::ChatUpdated {
            chat: root,
            change: ChatChange::Archived(true),
        });
        assert_eq!(app.chats.find(root).and_then(|c| c.archived), Some(true));
        assert_eq!(app.chats.find(root).and_then(|c| c.pin_order), Some(0));
        assert_eq!(
            app.chats.find(child).and_then(|c| c.archived),
            Some(true),
            "the child follows the root into the archive"
        );
        let archived_page = app.chats.page(&ListQuery::Archived).expect("loaded");
        assert!(
            archived_page.chats.iter().any(|c| c.id == Some(root)),
            "the archived page gained the chat"
        );
    }

    #[test]
    fn a_subagent_that_arrives_mid_flight_is_swept_into_the_archive_reply() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let root = Uuid::new_v4();
        loaded_list(&mut app, vec![listed(root, "root", "2026-09-30T10:00:00Z")]);
        assert_eq!(
            app.update(Msg::ChatAction(ChatAction::ToggleArchive(root))),
            vec![Effect::UpdateChat {
                chat: root,
                change: ChatChange::Archived(true)
            }]
        );
        // A subagent starts and is published before the archive's reply lands.
        let child = Uuid::new_v4();
        let mut sub = listed(child, "explore", "2026-09-30T10:00:01Z");
        sub.parent_chat_id = Some(root);
        app.update(watch("created", sub));
        assert!(
            app.chats
                .find(root)
                .is_some_and(|c| c.children.iter().any(|ch| ch.id == Some(child))),
            "the watch event added the subagent before the reply landed"
        );
        app.update(Msg::ChatUpdated {
            chat: root,
            change: ChatChange::Archived(true),
        });
        assert_eq!(app.chats.find(root).and_then(|c| c.archived), Some(true));
        assert_eq!(
            app.chats.find(child).and_then(|c| c.archived),
            Some(true),
            "the cascade used the family as it stood at the reply, not when the action started"
        );
    }

    #[test]
    fn pinning_after_an_existing_pin_sorts_last_locally() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
        let mut pinned = listed(first, "Pinned", "2026-09-30T09:00:00Z");
        pinned.pin_order = Some(3);
        let unpinned = listed(second, "Unpinned", "2026-09-30T10:00:00Z");
        loaded_list(&mut app, vec![pinned, unpinned]);
        assert_eq!(
            app.update(Msg::ChatAction(ChatAction::TogglePin(second))),
            vec![Effect::UpdateChat {
                chat: second,
                change: ChatChange::PinOrder(4)
            }]
        );
    }

    #[test]
    fn renaming_the_open_chat_updates_its_own_record() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.update(Msg::ChatUpdated {
            chat: id,
            change: ChatChange::Title("New title".into()),
        });
        assert_eq!(
            app.chat.as_ref().and_then(|c| c.title.as_deref()),
            Some("New title")
        );
    }

    #[test]
    fn archiving_the_open_chat_makes_it_refuse_new_messages() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.update(Msg::ChatUpdated {
            chat: id,
            change: ChatChange::Archived(true),
        });
        assert!(app.is_archived());
        assert_eq!(
            app.update(Msg::Submit("hello".into())),
            vec![Effect::RestoreComposer("hello".into())]
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "This chat is archived. Ctrl+A in /chats unarchives it.".into()
            ))
        );
    }

    #[test]
    fn marking_the_open_chat_unread_updates_its_own_record() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.update(Msg::ChatUpdated {
            chat: id,
            change: ChatChange::Read(false),
        });
        assert_eq!(app.chat.as_ref().and_then(|c| c.has_unread), Some(true));
        app.update(Msg::ChatUpdated {
            chat: id,
            change: ChatChange::Read(true),
        });
        assert_eq!(app.chat.as_ref().and_then(|c| c.has_unread), Some(false));
    }

    #[test]
    fn an_empty_or_whitespace_rename_is_refused_and_keeps_the_editor_open() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        loaded_list(
            &mut app,
            vec![listed(id, "Has a title", "2026-09-30T10:00:00Z")],
        );
        app.update(Msg::ChatAction(ChatAction::Rename(id)));
        for _ in 0.."Has a title".len() {
            app.update(Msg::Edit(Edit::Backspace));
        }
        for c in "   ".chars() {
            app.update(Msg::Edit(Edit::Char(c)));
        }
        assert!(app.update(Msg::Edit(Edit::Submit)).is_empty());
        assert!(
            app.editor.is_some(),
            "the editor stays open so the user can try again"
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error("A title cannot be empty.".into()))
        );
        assert_eq!(
            app.chats.find(id).and_then(|c| c.title.as_deref()),
            Some("Has a title"),
            "unchanged"
        );
    }

    #[test]
    fn a_title_over_two_hundred_runes_is_refused_and_keeps_the_editor_open() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        loaded_list(
            &mut app,
            vec![listed(id, "Untitled", "2026-09-30T10:00:00Z")],
        );
        app.update(Msg::ChatAction(ChatAction::Rename(id)));
        for _ in 0.."Untitled".len() {
            app.update(Msg::Edit(Edit::Backspace));
        }
        for c in "x".repeat(201).chars() {
            app.update(Msg::Edit(Edit::Char(c)));
        }
        assert!(app.update(Msg::Edit(Edit::Submit)).is_empty());
        assert!(
            app.editor.is_some(),
            "the 201-rune title is not lost when the server would refuse it"
        );
        assert_eq!(
            app.editor.as_ref().map(|e| e.line.text().chars().count()),
            Some(201),
            "the typed text is preserved"
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "A title must be at most 200 characters.".into()
            ))
        );
    }

    fn opened_preview(effects: &[Effect]) -> u64 {
        match effects {
            [.., Effect::OpenPreview { generation, .. }] => *generation,
            other => panic!("expected OpenPreview last, got {other:?}"),
        }
    }

    #[test]
    fn the_preview_follows_the_selection_and_closes_with_the_popup() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let first = opened_preview(&app.update(Msg::PreviewChat(Some(a))));
        let reply = json!({"type": "message", "message": {"id": 3, "role": "assistant", "content": [{"type": "text", "text": "Found 3 callers"}]}});
        app.update(Msg::ForPreview {
            chat: a,
            generation: first,
            msg: Box::new(ev(reply.clone())),
        });
        assert_eq!(
            app.preview
                .as_ref()
                .map(|p| p.transcript.messages().count()),
            Some(1)
        );
        let effects = app.update(Msg::PreviewChat(Some(b)));
        assert_eq!(effects[0], Effect::ClosePreview);
        let second = opened_preview(&effects);
        assert!(
            app.update(Msg::ForPreview {
                chat: a,
                generation: first,
                msg: Box::new(ev(reply)),
            })
            .is_empty()
        );
        assert_eq!(
            app.preview
                .as_ref()
                .map(|p| p.transcript.messages().count()),
            Some(0)
        );
        assert!(second > first);
        assert_eq!(
            app.update(Msg::PreviewChat(None)),
            vec![Effect::ClosePreview]
        );
        assert!(app.preview.is_none());
    }

    #[test]
    fn a_failed_preview_shows_its_error_and_retries_with_backoff() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let child = Uuid::new_v4();
        let generation = opened_preview(&app.update(Msg::PreviewChat(Some(child))));
        let effects = app.update(Msg::ForPreview {
            chat: child,
            generation,
            msg: Box::new(Msg::StreamEnded {
                error: Some("connection reset".into()),
            }),
        });
        assert!(matches!(
            effects.as_slice(),
            [Effect::OpenPreview { chat, delay, generation: g, .. }] if *chat == child && *delay == backoff(1) && *g > generation
        ));
        assert_eq!(
            app.preview.as_ref().and_then(|p| p.error.as_deref()),
            Some("connection reset")
        );
    }

    #[test]
    fn switching_with_a_live_preview_drops_both_old_streams() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (a, child, b) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(a),
            messages: vec![message(1)],
        });
        let main = app.stream_generation();
        let preview = opened_preview(&app.update(Msg::PreviewChat(Some(child))));
        let effects = app.update(Msg::OpenChat(b));
        assert!(effects.contains(&Effect::ClosePreview), "{effects:?}");
        assert!(effects.contains(&Effect::CloseStream), "{effects:?}");
        assert!(app.preview.is_none());
        let late = || {
            ev(
                json!({"type": "message", "message": {"id": 9, "role": "assistant", "content": [{"type": "text", "text": "late"}]}}),
            )
        };
        let from_old_streams = |app: &mut App| {
            app.update(Msg::ForStream {
                chat: a,
                generation: main,
                msg: Box::new(late()),
            });
            app.update(Msg::ForPreview {
                chat: child,
                generation: preview,
                msg: Box::new(late()),
            });
        };
        from_old_streams(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(b),
            messages: vec![],
        });
        assert_eq!(app.transcript.messages().count(), 0);
        // Back on A with the same subagent previewed again, the first visit's events stay out.
        app.update(Msg::OpenChat(a));
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(a),
            messages: vec![message(1)],
        });
        app.update(Msg::PreviewChat(Some(child)));
        from_old_streams(&mut app);
        assert_eq!(app.transcript.messages().count(), 1);
        assert_eq!(
            app.preview
                .as_ref()
                .map(|p| p.transcript.messages().count()),
            Some(0)
        );
    }

    #[test]
    fn new_closes_a_live_preview() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (a, child) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(a),
            messages: vec![],
        });
        let generation = opened_preview(&app.update(Msg::PreviewChat(Some(child))));
        let effects = app.update(Msg::Command(Command::New));
        assert!(effects.contains(&Effect::ClosePreview), "{effects:?}");
        assert!(app.preview.is_none());
        let reply =
            json!({"type": "message", "message": {"id": 3, "role": "assistant", "content": []}});
        assert!(
            app.update(Msg::ForPreview {
                chat: child,
                generation,
                msg: Box::new(ev(reply)),
            })
            .is_empty()
        );
        assert!(app.preview.is_none());
    }

    #[test]
    fn the_preview_never_touches_the_open_chat() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (a, child) = (Uuid::new_v4(), Uuid::new_v4());
        let mut unread = *chat(child);
        unread.has_unread = Some(true);
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats: vec![unread],
        });
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(a),
            messages: vec![message(1)],
        });
        app.update(Msg::ForStream {
            chat: a,
            generation: app.stream_generation(),
            msg: Box::new(Msg::StreamEnded {
                error: Some("main dropped".into()),
            }),
        });
        let sent = app.update(Msg::Submit("hi".into()));
        assert!(
            sent.iter().any(|e| matches!(e, Effect::SendMessage { .. })),
            "{sent:?}"
        );
        app.plan_mode = true;
        app.selected_effort = Some("high".into());
        let (main_generation, attempt) = (app.stream_generation(), app.reconnect_attempt);
        let connection = app.connection;
        let generation = opened_preview(&app.update(Msg::PreviewChat(Some(child))));
        let preview_msgs = [
            ev(json!({"type": "message", "message": {"id": 2, "role": "user", "content": []}})),
            ev(
                json!({"type": "message", "message": {"id": 5, "role": "assistant", "content": [{"type": "text", "text": "done"}]}}),
            ),
            ev(json!({"type": "status", "status": {"status": "waiting"}})),
            Msg::StreamHealthy,
        ];
        for msg in preview_msgs {
            let effects = app.update(Msg::ForPreview {
                chat: child,
                generation,
                msg: Box::new(msg),
            });
            assert!(effects.is_empty(), "{effects:?}");
        }
        assert_eq!(
            app.preview
                .as_ref()
                .map(|p| p.transcript.messages().count()),
            Some(2)
        );
        assert_eq!(app.transcript.messages().count(), 1);
        assert_eq!(app.transcript.status, None);
        assert!(app.awaiting_reply, "the main chat's wait must not end");
        assert_eq!(app.reconnect_attempt, attempt);
        assert_eq!(app.stream_generation(), main_generation);
        assert_eq!(app.connection, connection);
        assert_eq!(app.last_stream_error.as_deref(), Some("main dropped"));
        assert!(app.plan_mode);
        assert_eq!(app.selected_effort.as_deref(), Some("high"));
        // The preview reads the subagent, as the server does on stream connect.
        assert_eq!(
            app.chats.find(child).and_then(|c| c.has_unread),
            Some(false)
        );
        // A main-stream event never reaches the preview.
        app.update(Msg::ForStream {
            chat: a,
            generation: app.stream_generation(),
            msg: Box::new(ev(json!({"type": "message", "message": {"id": 7, "role": "assistant", "content": []}}))),
        });
        assert_eq!(app.transcript.messages().count(), 2);
        assert_eq!(
            app.preview
                .as_ref()
                .map(|p| p.transcript.messages().count()),
            Some(2)
        );
    }

    fn family_of_two(app: &mut App) -> (Uuid, Uuid, Uuid) {
        let (root, a, b) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut family = listed(root, "Fix the watch test", "2026-09-30T10:00:00Z");
        family.children = [a, b]
            .into_iter()
            .map(|id| types::CodersdkChat {
                parent_chat_id: Some(root),
                ..listed(id, "explore", "2026-09-30T10:00:00Z")
            })
            .collect();
        loaded_list(app, vec![family]);
        (root, a, b)
    }

    #[test]
    fn subagents_lists_children_or_siblings_and_previews_the_first() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (root, a, b) = family_of_two(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(root),
            messages: vec![],
        });
        let effects = app.update(Msg::Command(Command::Subagents));
        assert_eq!(effects[0], Effect::ShowSubagents);
        assert!(matches!(effects[1], Effect::OpenPreview { chat, .. } if chat == a));
        let (parent, children) = app.subagents();
        assert_eq!(parent, None);
        assert_eq!(
            children.iter().map(|c| c.id).collect::<Vec<_>>(),
            [Some(a), Some(b)]
        );
        app.update(Msg::OpenChat(a));
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(types::CodersdkChat {
                parent_chat_id: Some(root),
                ..*chat(a)
            }),
            messages: vec![],
        });
        let (parent, children) = app.subagents();
        assert_eq!(parent.as_deref(), Some("Fix the watch test"));
        assert_eq!(children.len(), 2, "a subagent lists its siblings");
    }

    #[test]
    fn parent_returns_from_an_idle_subagent_and_esc_only_does_so_while_idle() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (root, a, _) = family_of_two(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(types::CodersdkChat {
                parent_chat_id: Some(root),
                ..*chat(a)
            }),
            messages: vec![],
        });
        assert!(app.can_return_to_parent());
        app.update(running());
        assert!(
            !app.can_return_to_parent(),
            "Esc interrupts a running subagent"
        );
        let effects = app.update(Msg::Command(Command::Parent));
        assert!(effects.contains(&Effect::LoadChat(root)), "{effects:?}");
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(root),
            messages: vec![],
        });
        assert!(app.update(Msg::Command(Command::Parent)).is_empty());
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info("This chat is not a subagent.".into()))
        );
    }

    #[test]
    fn previewing_a_subagent_marks_it_read_locally() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (root, a, _) = family_of_two(&mut app);
        app.chats.set_read(a, false);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(root),
            messages: vec![],
        });
        let effects = app.update(Msg::PreviewChat(Some(a)));
        assert!(
            matches!(effects.as_slice(), [Effect::OpenPreview { chat, .. }] if *chat == a),
            "{effects:?}"
        );
        assert_eq!(app.chats.find(a).and_then(|c| c.has_unread), Some(false));
    }

    #[test]
    fn the_open_chat_is_never_previewed() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (root, a, b) = family_of_two(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(types::CodersdkChat {
                parent_chat_id: Some(root),
                ..*chat(a)
            }),
            messages: vec![],
        });
        opened_preview(&app.update(Msg::PreviewChat(Some(b))));
        assert_eq!(
            app.update(Msg::PreviewChat(Some(a))),
            vec![Effect::ClosePreview],
            "selecting the open chat closes the preview instead of streaming it twice"
        );
        assert!(app.preview.is_none());
        assert!(app.update(Msg::PreviewChat(Some(a))).is_empty());
        // From the root, `/subagents` lists only the root's own children.
        app.update(Msg::OpenChat(root));
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(root),
            messages: vec![],
        });
        let (_, children) = app.subagents();
        assert!(children.iter().all(|c| c.parent_chat_id == Some(root)));
        assert!(children.iter().all(|c| c.id != Some(root)));
    }

    #[test]
    fn subagents_from_a_subagent_previews_the_first_sibling() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (root, a, b) = family_of_two(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(types::CodersdkChat {
                parent_chat_id: Some(root),
                ..*chat(a)
            }),
            messages: vec![],
        });
        assert_eq!(app.first_subagent(), Some(b));
        let effects = app.update(Msg::Command(Command::Subagents));
        assert!(
            matches!(effects.as_slice(), [Effect::ShowSubagents, Effect::OpenPreview { chat, .. }] if *chat == b),
            "{effects:?}"
        );
    }

    #[test]
    fn an_only_child_previews_nothing() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (root, only) = (Uuid::new_v4(), Uuid::new_v4());
        let mut family = listed(root, "Fix the watch test", "2026-09-30T10:00:00Z");
        family.children = vec![types::CodersdkChat {
            parent_chat_id: Some(root),
            ..listed(only, "explore", "2026-09-30T10:00:00Z")
        }];
        loaded_list(&mut app, vec![family]);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(types::CodersdkChat {
                parent_chat_id: Some(root),
                ..*chat(only)
            }),
            messages: vec![],
        });
        assert_eq!(app.first_subagent(), None);
        assert_eq!(
            app.update(Msg::Command(Command::Subagents)),
            vec![Effect::ShowSubagents]
        );
    }

    #[test]
    fn a_subagent_whose_parent_is_not_listed_still_lists_itself() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (root, a) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(types::CodersdkChat {
                parent_chat_id: Some(root),
                ..*chat(a)
            }),
            messages: vec![],
        });
        let (parent, children) = app.subagents();
        assert_eq!(parent.as_deref(), Some("the parent chat"));
        assert_eq!(children.iter().map(|c| c.id).collect::<Vec<_>>(), [Some(a)]);
        assert!(!app.parent_listed());
    }

    #[test]
    fn a_parent_that_fails_to_load_is_named_in_the_error() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (root, a, _) = family_of_two(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(types::CodersdkChat {
                parent_chat_id: Some(root),
                ..*chat(a)
            }),
            messages: vec![],
        });
        assert!(app.parent_listed());
        let effects = app.update(Msg::Command(Command::Parent));
        assert!(effects.contains(&Effect::LoadChat(root)), "{effects:?}");
        app.update(Msg::ChatLoadFailed {
            chat_id: root,
            message: "chat not found".into(),
        });
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "Could not open the parent chat \u{201c}Fix the watch test\u{201d}: chat not found. Find it with /chats.".into()
            ))
        );
    }

    #[test]
    fn title_renames_directly_or_proposes_one_to_confirm() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert!(app.update(Msg::Command(Command::Title(None))).is_empty());
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error("Start a chat first.".into()))
        );
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Title(Some("Watch fix".into())))),
            vec![Effect::UpdateChat {
                chat: id,
                change: ChatChange::Title("Watch fix".into())
            }]
        );
        assert_eq!(
            app.update(Msg::Command(Command::Title(None))),
            vec![Effect::ProposeTitle {
                chat: id,
                generation: 1
            }]
        );
        assert!(app.editor.as_ref().is_some_and(|e| e.loading));
        assert!(
            app.update(Msg::Edit(Edit::Char('x'))).is_empty(),
            "no typing while it loads"
        );
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::TitleProposed {
                title: "Fix the flaky watch test".into(),
                generation: 1,
            }),
        });
        assert_eq!(
            app.editor.as_ref().map(|e| (e.loading, e.line.text())),
            Some((false, "Fix the flaky watch test"))
        );
        assert_eq!(
            app.update(Msg::Edit(Edit::Submit)),
            vec![Effect::UpdateChat {
                chat: id,
                change: ChatChange::Title("Fix the flaky watch test".into())
            }]
        );
    }

    #[test]
    fn a_failed_proposal_opens_the_editor_on_the_current_title() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.update(Msg::Command(Command::Title(None)));
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::TitleProposeFailed {
                message: "Title generation timed out.".into(),
                generation: 1,
            }),
        });
        assert_eq!(
            app.editor.as_ref().map(|e| (e.loading, e.line.text())),
            Some((false, "t")),
            "the editor stays open on the chat's current title so the user can type one"
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "Could not propose a title: Title generation timed out.".into()
            ))
        );
    }

    /// A reply tagged with the chat it was generated for must never reach the editor once the
    /// user has left that chat: `Msg::ForChat` applies only while it is still the open chat.
    #[test]
    fn a_stale_title_proposal_for_a_left_chat_is_dropped() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.update(Msg::Command(Command::Title(None)));
        let other = Uuid::new_v4();
        app.update(Msg::OpenChat(other));
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(other),
            messages: vec![],
        });
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::TitleProposed {
                title: "Stale title".into(),
                generation: 1,
            }),
        });
        assert!(
            app.editor.as_ref().is_some_and(|e| e.loading),
            "a reply for a chat the user left must not fill the editor"
        );
    }

    /// Esc cancels a loading proposal without canceling the request already sent; a second
    /// `/title` starts a new one, and the first (now stale) reply must not fill it in.
    #[test]
    fn a_stale_proposal_is_dropped_after_a_newer_title_request_starts() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Title(None))),
            vec![Effect::ProposeTitle {
                chat: id,
                generation: 1
            }]
        );
        assert!(app.update(Msg::Edit(Edit::Cancel)).is_empty());
        assert!(app.editor.is_none(), "Esc closed the editor");
        assert_eq!(
            app.update(Msg::Command(Command::Title(None))),
            vec![Effect::ProposeTitle {
                chat: id,
                generation: 2
            }]
        );
        // The first request's reply arrives late, tagged with the superseded generation.
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::TitleProposed {
                title: "Stale title".into(),
                generation: 1,
            }),
        });
        assert!(
            app.editor.as_ref().is_some_and(|e| e.loading),
            "the stale reply must not fill the second request's editor"
        );
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::TitleProposed {
                title: "Fix the flaky watch test".into(),
                generation: 2,
            }),
        });
        assert_eq!(
            app.editor.as_ref().map(|e| (e.loading, e.line.text())),
            Some((false, "Fix the flaky watch test")),
            "the current request's reply fills the editor"
        );
    }

    /// Only one proposal is ever in flight per chat: `/title` again while one is already
    /// loading reopens the same editor instead of firing a second request.
    #[test]
    fn a_repeated_title_while_loading_does_not_fire_a_second_request() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Title(None))),
            vec![Effect::ProposeTitle {
                chat: id,
                generation: 1
            }]
        );
        assert!(
            app.update(Msg::Command(Command::Title(None))).is_empty(),
            "a second /title while loading fires no second request"
        );
        assert!(app.editor.as_ref().is_some_and(|e| e.loading));
    }

    #[test]
    fn title_with_text_over_the_limit_is_refused_like_the_editor() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        let long = "x".repeat(201);
        assert!(
            app.update(Msg::Command(Command::Title(Some(long))))
                .is_empty()
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "A title must be at most 200 characters.".into()
            ))
        );
    }

    #[test]
    fn queue_actions_go_to_the_server_for_the_open_chat() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert!(app.update(Msg::Command(Command::Queue)).is_empty());
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Queue)),
            vec![Effect::ShowQueue]
        );
        assert_eq!(
            app.update(Msg::QueueAction(QueueAction::Promote(7))),
            vec![Effect::PromoteQueued { chat: id, id: 7 }]
        );
        assert_eq!(
            app.update(Msg::QueueAction(QueueAction::Remove(7))),
            vec![Effect::DeleteQueued { chat: id, id: 7 }]
        );
    }

    #[test]
    fn send_now_on_an_empty_composer_promotes_the_first_queued_message() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        assert!(
            app.update(Msg::QueueAction(QueueAction::PromoteFirst))
                .is_empty(),
            "nothing is queued"
        );
        app.update(ev(json!({"type": "queue_update", "queued_messages": [
            {"id": 7, "content": [{"type": "text", "text": "then run the tests"}]},
            {"id": 8, "content": [{"type": "text", "text": "and open a PR"}]}
        ]})));
        assert_eq!(
            app.update(Msg::QueueAction(QueueAction::PromoteFirst)),
            vec![Effect::PromoteQueued { chat: id, id: 7 }]
        );
    }

    /// From idle, the server inserts the promoted message synchronously (no `running` status
    /// event follows the request), so the wait must start on the promote itself or the
    /// activity line shows nothing until the next status event.
    #[test]
    fn promoting_from_idle_starts_waiting() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.update(ev(
            json!({"type": "status", "status": {"status": "waiting"}}),
        ));
        app.update(ev(json!({"type": "queue_update", "queued_messages": [
            {"id": 7, "content": [{"type": "text", "text": "then run the tests"}]}
        ]})));
        app.update(Msg::QueueAction(QueueAction::Promote(7)));
        assert!(app.awaiting_reply);
        assert_eq!(app.activity(), Some(Activity::Waiting));
    }

    fn asked(app: &mut App) -> Uuid {
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: serde_json::from_value(crate::question::tests::asked()).unwrap(),
        });
        id
    }

    #[test]
    fn answering_every_question_sends_one_message_and_other_takes_free_text() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = asked(&mut app);
        let menu = app.question_menu().unwrap();
        assert_eq!(
            (menu.number, menu.count, menu.header.as_str()),
            (1, 2, "Scope")
        );
        app.update(Msg::QuestionKey(QuestionKey::Down));
        assert!(app.update(Msg::QuestionKey(QuestionKey::Enter)).is_empty());
        assert_eq!(app.question_menu().map(|m| m.number), Some(2));
        app.update(Msg::QuestionKey(QuestionKey::Down));
        app.update(Msg::QuestionKey(QuestionKey::Enter));
        assert!(matches!(
            app.editor.as_ref().map(|e| &e.target),
            Some(EditTarget::Other)
        ));
        for c in "unit tests only".chars() {
            app.update(Msg::Edit(Edit::Char(c)));
        }
        let effects = app.update(Msg::Edit(Edit::Submit));
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { chat, text, .. }]
                if *chat == id && text == "1. Scope: tui\n2. Question 2: Other: unit tests only"),
            "{effects:?}"
        );
        assert_eq!(
            app.question_menu(),
            None,
            "the menu stays closed until the echo arrives"
        );
    }

    #[test]
    fn esc_hides_the_menu_and_a_reply_from_elsewhere_closes_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        asked(&mut app);
        app.update(Msg::QuestionKey(QuestionKey::Dismiss));
        assert_eq!(app.question_menu(), None);
        let mut other = App::new(BusyBehavior::Queue, true);
        started(&mut other);
        asked(&mut other);
        assert!(other.question_menu().is_some());
        other.update(user_message(3));
        assert_eq!(other.question_menu(), None);
    }

    #[test]
    fn implement_leaves_plan_mode_on_the_same_request() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat_with_plan(id, "plan"),
            messages: serde_json::from_value(json!(crate::question::tests::proposed(1))).unwrap(),
        });
        assert!(app.plan_ready());
        assert_eq!(
            app.update(Msg::Command(Command::Implement)),
            vec![Effect::SendMessage {
                chat: id,
                text: "Implement the plan.".into(),
                model: None,
                busy: BusyBehavior::Queue,
                turn: TurnOptions {
                    plan_mode: Some(false),
                    plan_generation: 1,
                    ..app.turn()
                },
                seq: 1,
            }]
        );
        assert!(!app.plan_mode);
        app.update(Msg::PlanModeApplied { on: false });
        app.update(user_message(3));
        assert!(!app.plan_ready(), "the hint is for the latest turn only");
        let effects = app.update(Msg::Command(Command::Implement));
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { text, turn, .. }]
                if text == "Implement the plan." && turn.plan_mode.is_none()),
            "/implement takes an earlier plan, and plan mode is already off: {effects:?}"
        );
        let mut blank = App::new(BusyBehavior::Queue, true);
        started(&mut blank);
        blank.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages: vec![],
        });
        assert!(blank.update(Msg::Command(Command::Implement)).is_empty());
        assert_eq!(
            blank.notices.last(),
            Some(&Notice::Info(
                "There is no proposed plan to implement.".into()
            ))
        );
    }

    #[test]
    fn a_failed_implement_with_plan_mode_already_off_leaves_it_off() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages: serde_json::from_value(json!(crate::question::tests::proposed(1))).unwrap(),
        });
        assert!(!app.plan_mode);
        let effects = app.update(Msg::Command(Command::Implement));
        let [Effect::SendMessage { text, turn, .. }] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        app.update(Msg::SendFailed {
            text: text.clone(),
            message: "HTTP 500".into(),
            plan_mode: turn.plan_mode,
            seq: 0,
            mcp_rejected: false,
        });
        assert!(!app.plan_mode);
        assert!(
            matches!(app.notices.last(), Some(Notice::Error(m)) if !m.contains("Plan mode is still on")),
            "{:?}",
            app.notices.last()
        );
    }

    #[test]
    fn implement_waits_for_a_plan_mode_change_in_flight() {
        for patch_failed in [false, true] {
            let mut app = App::new(BusyBehavior::Queue, true);
            started(&mut app);
            let id = Uuid::new_v4();
            app.update(Msg::ChatLoaded {
                has_more: None,
                chat: chat(id),
                messages: serde_json::from_value(json!(crate::question::tests::proposed(1)))
                    .unwrap(),
            });
            assert_eq!(
                app.update(Msg::Command(Command::PlanMode(Some(true)))),
                vec![Effect::SetPlanMode {
                    chat: id,
                    on: true,
                    generation: 1
                }]
            );
            assert!(
                app.update(Msg::Command(Command::Implement)).is_empty(),
                "the message would race the PATCH"
            );
            assert!(!app.plan_mode);
            let settled = if patch_failed {
                Msg::PlanModeFailed {
                    on: true,
                    message: "HTTP 500".into(),
                }
            } else {
                Msg::PlanModeApplied { on: true }
            };
            let effects = app.update(Msg::ForChat {
                chat: id,
                msg: Box::new(settled),
            });
            assert_eq!(
                effects,
                vec![Effect::SendMessage {
                    chat: id,
                    text: "Implement the plan.".into(),
                    model: None,
                    busy: BusyBehavior::Queue,
                    turn: TurnOptions {
                        plan_mode: Some(false),
                        plan_generation: 2,
                        ..app.turn()
                    },
                    seq: 1,
                }],
                "patch_failed: {patch_failed}"
            );
            assert!(
                app.update(Msg::Command(Command::PlanMode(Some(true))))
                    .is_empty(),
                "the implement message now holds the queue"
            );
            assert!(
                app.update(Msg::Command(Command::PlanMode(Some(false))))
                    .is_empty()
            );
            assert_eq!(
                app.update(Msg::ForChat {
                    chat: id,
                    msg: Box::new(Msg::PlanModeApplied { on: false }),
                }),
                vec![Effect::RefreshChat {
                    chat: id,
                    generation: 1
                }]
            );
            app.update(Msg::ForChat {
                chat: id,
                msg: Box::new(Msg::ChatRefreshed(chat_with_plan(id, ""))),
            });
            assert!(!app.plan_mode, "plan mode ends where the user last set it");
        }
    }

    /// The runtime's reply to the plan-mode request `generation` for `chat`.
    fn plan_reply(chat: Uuid, generation: u64, msg: Msg) -> Msg {
        Msg::ForPlan {
            chat,
            generation,
            msg: Box::new(msg),
        }
    }

    #[test]
    fn a_reply_from_before_a_reopen_never_settles_the_new_change() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let visit = |app: &mut App, id: Uuid| {
            app.update(Msg::OpenChat(id));
            app.update(Msg::ChatLoaded {
                has_more: None,
                chat: chat(id),
                messages: vec![],
            })
        };
        visit(&mut app, a);
        assert_eq!(
            app.update(Msg::Command(Command::PlanMode(Some(true)))),
            vec![Effect::SetPlanMode {
                chat: a,
                on: true,
                generation: 1
            }]
        );
        visit(&mut app, b);
        visit(&mut app, a);
        assert_eq!(
            app.update(Msg::Command(Command::PlanMode(Some(true)))),
            vec![Effect::SetPlanMode {
                chat: a,
                on: true,
                generation: 2
            }]
        );
        assert!(
            app.update(plan_reply(a, 1, Msg::PlanModeApplied { on: true }))
                .is_empty(),
            "the first visit's reply leaves the change in flight alone"
        );
        assert!(
            app.update(Msg::Command(Command::PlanMode(Some(false))))
                .is_empty(),
            "the second visit's change is still in flight"
        );
        assert_eq!(
            app.update(plan_reply(a, 2, Msg::PlanModeApplied { on: true })),
            vec![Effect::SetPlanMode {
                chat: a,
                on: false,
                generation: 3
            }]
        );
    }

    #[test]
    fn a_change_from_an_earlier_visit_is_read_back_once_it_settles() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let visit = |app: &mut App, id: Uuid| {
            app.update(Msg::OpenChat(id));
            app.update(Msg::ChatLoaded {
                has_more: None,
                chat: chat(id),
                messages: vec![],
            })
        };
        visit(&mut app, a);
        app.update(Msg::Command(Command::PlanMode(Some(true))));
        visit(&mut app, b);
        visit(&mut app, a);
        // The load may have been served before the change landed.
        assert_eq!(
            app.update(plan_reply(a, 1, Msg::PlanModeApplied { on: true })),
            vec![Effect::RefreshChat {
                chat: a,
                generation: 1
            }]
        );
        app.update(Msg::ForChat {
            chat: a,
            msg: Box::new(Msg::ChatRefreshed(chat_with_plan(a, "plan"))),
        });
        assert!(app.plan_mode);
        // The same change settling while the chat reloads reads it back after the load.
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        visit(&mut app, a);
        app.update(Msg::Command(Command::PlanMode(Some(true))));
        visit(&mut app, b);
        app.update(Msg::OpenChat(a));
        assert!(
            app.update(plan_reply(a, 1, Msg::PlanModeApplied { on: true }))
                .is_empty()
        );
        let effects = app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(a),
            messages: vec![],
        });
        assert!(
            effects.contains(&Effect::RefreshChat {
                chat: a,
                generation: 1
            }),
            "{effects:?}"
        );
    }

    #[test]
    fn a_held_implement_into_an_archived_chat_still_reads_plan_mode_back() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: serde_json::from_value(json!(crate::question::tests::proposed(1))).unwrap(),
        });
        app.update(Msg::Command(Command::PlanMode(Some(true))));
        assert!(app.update(Msg::Command(Command::Implement)).is_empty());
        assert_eq!(
            app.activity(),
            Some(Activity::Waiting),
            "the held message waits"
        );
        assert_eq!(
            app.update(Msg::Submit("hi".into())),
            vec![Effect::RestoreComposer("hi".into())],
            "a plain message never overtakes the held one"
        );
        assert!(
            matches!(app.notices.last(), Some(Notice::Info(m)) if m.starts_with("Wait for")),
            "{:?}",
            app.notices.last()
        );
        let archived: Box<types::CodersdkChat> = Box::new(serde_json::from_value(json!({"id": id, "title": "t", "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}, "archived": true})).unwrap());
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::ChatRefreshed(archived)),
        });
        assert_eq!(
            app.update(plan_reply(id, 1, Msg::PlanModeApplied { on: true })),
            vec![Effect::SetPlanMode {
                chat: id,
                on: false,
                generation: 2
            }],
            "the user's last choice still goes, and no text they never typed is restored"
        );
        assert_eq!(app.activity(), None);
        assert_eq!(
            app.update(plan_reply(id, 2, Msg::PlanModeApplied { on: false })),
            vec![Effect::RefreshChat {
                chat: id,
                generation: 1
            }]
        );
    }

    #[test]
    fn a_failed_refetch_forgets_the_failure_it_would_report() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.update(Msg::Command(Command::PlanMode(Some(true))));
        assert_eq!(
            app.update(plan_reply(
                id,
                1,
                Msg::PlanModeFailed {
                    on: true,
                    message: "HTTP 500".into(),
                }
            )),
            vec![Effect::RefreshChat {
                chat: id,
                generation: 1
            }]
        );
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::ApiFailed {
                action: "refresh the chat",
                message: "HTTP 502".into(),
            }),
        });
        let before = app.notices.len();
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::ChatRefreshed(chat(id))),
        });
        assert_eq!(app.notices.len(), before, "{:?}", app.notices.last());
    }

    #[test]
    fn implement_during_a_running_turn_says_to_wait() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat_with_plan(Uuid::new_v4(), "plan"),
            messages: serde_json::from_value(json!(crate::question::tests::proposed(1))).unwrap(),
        });
        app.update(running());
        assert!(app.update(Msg::Command(Command::Implement)).is_empty());
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info(
                "Wait for the turn to finish, then implement the plan.".into()
            ))
        );
        assert!(app.plan_mode);
    }

    #[test]
    fn esc_hides_the_questions_until_they_are_shown_again() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        asked(&mut app);
        assert!(!app.questions_hidden());
        app.update(Msg::QuestionKey(QuestionKey::Down));
        app.update(Msg::QuestionKey(QuestionKey::Dismiss));
        assert_eq!(app.question_menu(), None);
        assert!(app.questions_hidden());
        app.update(Msg::QuestionKey(QuestionKey::Show));
        assert!(!app.questions_hidden());
        assert_eq!(
            app.question_menu().map(|m| (m.number, m.selected)),
            Some((1, 1)),
            "showing them again keeps the place"
        );
        app.update(Msg::QuestionKey(QuestionKey::Enter));
        app.update(Msg::QuestionKey(QuestionKey::Enter));
        assert!(
            !app.questions_hidden(),
            "sent answers are not hidden questions"
        );
        app.update(Msg::QuestionKey(QuestionKey::Show));
        assert_eq!(app.question_menu(), None, "sent answers stay closed");
    }

    #[test]
    fn an_answer_refused_for_an_archived_chat_brings_the_menu_back() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let mut archived = chat(Uuid::new_v4());
        archived.archived = Some(true);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: archived,
            messages: serde_json::from_value(crate::question::tests::asked()).unwrap(),
        });
        app.update(Msg::QuestionKey(QuestionKey::Enter));
        let effects = app.update(Msg::QuestionKey(QuestionKey::Enter));
        assert!(
            matches!(effects.as_slice(), [Effect::RestoreComposer(_)]),
            "{effects:?}"
        );
        assert_eq!(app.question_menu().map(|m| m.number), Some(1));
    }

    #[test]
    fn an_answer_starts_the_wait_and_a_failed_send_brings_the_menu_back() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        asked(&mut app);
        app.update(Msg::QuestionKey(QuestionKey::Enter));
        let effects = app.update(Msg::QuestionKey(QuestionKey::Enter));
        let [Effect::SendMessage { text, turn, .. }] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        assert_eq!(text, "1. Scope: core\n2. Question 2: yes");
        assert_eq!(turn.plan_mode, None, "an answer leaves plan mode alone");
        assert_eq!(app.activity(), Some(Activity::Waiting));
        assert_eq!(app.question_menu(), None);
        let restored = app.update(Msg::SendFailed {
            text: text.clone(),
            message: "HTTP 500".into(),
            plan_mode: None,
            seq: 0,
            mcp_rejected: false,
        });
        assert_eq!(restored, vec![Effect::RestoreComposer(text.clone())]);
        assert_eq!(
            app.question_menu().map(|m| (m.number, m.selected)),
            Some((1, 0)),
            "the menu starts over once the restored text is cleared"
        );
    }

    #[test]
    fn back_returns_to_the_previous_question_with_its_answer_selected() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        asked(&mut app);
        app.update(Msg::QuestionKey(QuestionKey::Back));
        assert_eq!(
            app.question_menu().map(|m| m.number),
            Some(1),
            "no question before the first"
        );
        app.update(Msg::QuestionKey(QuestionKey::Down));
        app.update(Msg::QuestionKey(QuestionKey::Enter));
        app.update(Msg::QuestionKey(QuestionKey::Back));
        let menu = app.question_menu().unwrap();
        assert_eq!((menu.number, menu.selected), (1, 1), "tui stays selected");
        app.update(Msg::QuestionKey(QuestionKey::Down));
        app.update(Msg::QuestionKey(QuestionKey::Enter));
        for c in "both".chars() {
            app.update(Msg::Edit(Edit::Char(c)));
        }
        app.update(Msg::Edit(Edit::Submit));
        app.update(Msg::QuestionKey(QuestionKey::Back));
        assert_eq!(
            app.question_menu().map(|m| (m.number, m.selected)),
            Some((1, 2))
        );
        app.update(Msg::QuestionKey(QuestionKey::Enter));
        assert_eq!(
            app.editor.as_ref().map(|e| e.line.text()),
            Some("both"),
            "the earlier Other text is kept"
        );
        app.update(Msg::Edit(Edit::Submit));
        let effects = app.update(Msg::QuestionKey(QuestionKey::Enter));
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { text, .. }]
                if text == "1. Scope: Other: both\n2. Question 2: yes"),
            "{effects:?}"
        );
    }

    #[test]
    fn an_empty_other_answer_is_refused_and_keeps_the_editor_open() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        asked(&mut app);
        app.update(Msg::QuestionKey(QuestionKey::Down));
        app.update(Msg::QuestionKey(QuestionKey::Down));
        app.update(Msg::QuestionKey(QuestionKey::Enter));
        app.update(Msg::Edit(Edit::Char(' ')));
        assert!(app.update(Msg::Edit(Edit::Submit)).is_empty());
        assert!(matches!(
            app.editor.as_ref().map(|e| &e.target),
            Some(EditTarget::Other)
        ));
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "Type an answer, or press Esc and pick an option.".into()
            ))
        );
        app.update(Msg::Edit(Edit::Cancel));
        assert_eq!(
            app.question_menu().map(|m| (m.number, m.selected)),
            Some((1, 2)),
            "Esc in the editor goes back to the menu"
        );
    }

    #[test]
    fn an_answer_that_starts_with_a_slash_is_sent_as_text() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages: serde_json::from_value(json!([{"id": 1, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "q1", "tool_name": "ask_user_question", "args": {"questions": [
                    {"header": "Dir", "question": "Where?", "options": [{"label": "/tmp", "description": "scratch"}]}
                ]}}
            ]}]))
            .unwrap(),
        });
        let effects = app.update(Msg::QuestionKey(QuestionKey::Enter));
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { text, .. }] if text == "/tmp"),
            "{effects:?}"
        );
    }

    #[test]
    fn no_menu_or_plan_action_while_the_turn_is_still_running() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        asked(&mut app);
        app.update(running());
        assert_eq!(app.question_menu(), None);
        let mut plan = App::new(BusyBehavior::Queue, true);
        started(&mut plan);
        plan.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat_with_plan(Uuid::new_v4(), "plan"),
            messages: serde_json::from_value(json!(crate::question::tests::proposed(1))).unwrap(),
        });
        plan.update(running());
        assert!(!plan.plan_ready());
    }

    #[test]
    fn a_failed_implement_takes_the_servers_plan_mode() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat_with_plan(id, "plan"),
            messages: serde_json::from_value(json!(crate::question::tests::proposed(1))).unwrap(),
        });
        app.update(Msg::Command(Command::Implement));
        assert_eq!(app.activity(), Some(Activity::Waiting));
        let effects = app.update(Msg::SendFailed {
            text: "Implement the plan.".into(),
            message: "HTTP 500".into(),
            plan_mode: Some(false),
            seq: 0,
            mcp_rejected: false,
        });
        assert!(
            effects.contains(&Effect::RefreshChat {
                chat: id,
                generation: 1
            }),
            "{effects:?}"
        );
        app.update(Msg::ChatRefreshed(chat_with_plan(id, "plan")));
        assert!(app.plan_mode);
        assert!(app.plan_ready(), "the plan is still offered");
    }

    #[test]
    fn an_attachment_uploads_and_the_next_message_carries_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Attach("/tmp/shot.png".into()))),
            vec![Effect::UploadFile {
                local: 1,
                path: "/tmp/shot.png".into(),
                org
            }]
        );
        let file = Uuid::new_v4();
        app.update(Msg::FileUploaded {
            local: 1,
            file_id: file,
            size: 2048,
        });
        assert_eq!(app.chips[0].state, ChipState::Ready(file));
        let effects = app.update(Msg::Submit("what is wrong here?".into()));
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { turn, .. }] if turn.files == vec![file]),
            "{effects:?}"
        );
        assert!(app.chips.is_empty(), "sent chips clear");
    }

    #[test]
    fn a_message_waits_for_uploads_and_a_failed_upload_hands_the_text_back() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages: vec![],
        });
        app.update(Msg::Command(Command::Attach("/tmp/a.md".into())));
        app.update(Msg::Command(Command::Attach("/tmp/b.md".into())));
        assert!(app.update(Msg::Submit("read both".into())).is_empty());
        let file = Uuid::new_v4();
        assert!(
            app.update(Msg::FileUploaded {
                local: 1,
                file_id: file,
                size: 10
            })
            .is_empty()
        );
        assert_eq!(
            app.update(Msg::UploadFailed {
                local: 2,
                message: "b.md is 12 MiB; the limit is 10 MiB.".into()
            }),
            vec![Effect::RestoreComposer("read both".into())]
        );
        assert_eq!(
            app.chips[1].label(),
            "b.md: b.md is 12 MiB; the limit is 10 MiB."
        );
        app.update(Msg::RemoveLastChip);
        assert_eq!(app.chips.len(), 1);
        let effects = app.update(Msg::Submit("read a".into()));
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { turn, .. }] if turn.files == vec![file])
        );
    }

    #[test]
    fn an_unsupported_type_is_rejected_before_upload() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert!(
            app.update(Msg::Command(Command::Attach("/tmp/logs.tar.gz".into())))
                .is_empty(),
            "nothing is uploaded"
        );
        assert!(matches!(&app.chips[0].state, ChipState::Failed(m) if m.contains(".gz file")));
    }

    #[test]
    fn a_message_waiting_on_uploads_goes_back_to_the_composer_on_a_switch() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(a),
            messages: vec![],
        });
        app.update(Msg::Command(Command::Attach("/tmp/a.md".into())));
        assert!(app.update(Msg::Submit("for chat a".into())).is_empty());
        let effects = app.update(Msg::OpenChat(b));
        assert!(
            effects.contains(&Effect::RestoreComposer("for chat a".into())),
            "{effects:?}"
        );
        assert!(app.chips.is_empty(), "the chips belong to chat a");
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(b),
            messages: vec![],
        });
        let effects = app.update(Msg::FileUploaded {
            local: 1,
            file_id: Uuid::new_v4(),
            size: 10,
        });
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::SendMessage { .. })),
            "chat a's message never goes to chat b: {effects:?}"
        );
        assert!(app.chips.is_empty(), "a stale upload adds no chip");
    }

    #[test]
    fn a_send_refused_for_an_archived_chat_keeps_the_chips() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        let mut archived = chat(id);
        archived.archived = Some(true);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: archived,
            messages: vec![],
        });
        app.update(Msg::Command(Command::Attach("/tmp/a.md".into())));
        app.update(Msg::FileUploaded {
            local: 1,
            file_id: Uuid::new_v4(),
            size: 10,
        });
        assert_eq!(
            app.update(Msg::Submit("hello".into())),
            vec![Effect::RestoreComposer("hello".into())]
        );
        assert_eq!(app.chips.len(), 1, "nothing was sent, so the chip stays");
    }

    #[test]
    fn a_held_implement_refuses_a_message_with_files_and_keeps_the_chips() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.implement_held = true;
        app.update(Msg::Command(Command::Attach("/tmp/a.md".into())));
        app.update(Msg::FileUploaded {
            local: 1,
            file_id: Uuid::new_v4(),
            size: 10,
        });
        assert_eq!(
            app.update(Msg::Submit("hello".into())),
            vec![Effect::RestoreComposer("hello".into())]
        );
        assert_eq!(app.chips.len(), 1);
    }

    /// Attaches `name` and lands its upload, returning the uploaded file's id.
    fn attach_ready(app: &mut App, name: &str) -> Uuid {
        app.update(Msg::Command(Command::Attach(format!("/tmp/{name}"))));
        let local = app.chips.last().unwrap().local;
        let file = Uuid::new_v4();
        app.update(Msg::FileUploaded {
            local,
            file_id: file,
            size: 10,
        });
        file
    }

    #[test]
    fn a_paste_uploads_as_a_text_file_with_its_message() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages: vec![],
        });
        let text = "line\n".repeat(12);
        assert!(
            app.update(Msg::AttachPaste {
                name: "paste-1.txt".into(),
                text: text.clone(),
            })
            .is_empty(),
            "nothing uploads before the send"
        );
        assert_eq!(app.chips[0].state, ChipState::Pasted);
        assert_eq!(app.chips[0].label(), "paste-1.txt 60 B uploads when sent");
        assert_eq!(
            app.update(Msg::Submit("what failed?".into())),
            vec![Effect::UploadText {
                local: 1,
                name: "paste-1.txt".into(),
                text,
                org,
            }]
        );
        let file = Uuid::new_v4();
        let effects = app.update(Msg::FileUploaded {
            local: 1,
            file_id: file,
            size: 60,
        });
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { text, turn, .. }]
                if text == "what failed?" && turn.files == vec![file]),
            "{effects:?}"
        );
        assert!(app.chips.is_empty(), "sent chips clear");
    }

    #[test]
    fn a_failed_send_puts_its_pasted_text_back_above_the_composer() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        attach_ready(&mut app, "a.png");
        app.update(Msg::AttachPaste {
            name: "paste-1.txt".into(),
            text: "the log".into(),
        });
        app.update(Msg::Submit("look".into()));
        let local = app.chips[1].local;
        let seq = seq_of(&app.update(Msg::FileUploaded {
            local,
            file_id: Uuid::new_v4(),
            size: 7,
        }));
        app.update(send_failed(id, "look", seq));
        assert_eq!(
            last_error(&app),
            "Could not send the message: boom. These files were not sent: a.png. Attach them again. Your pasted text is back above the composer as paste-1.txt; send again to attach it."
        );
        assert_eq!(app.chips.len(), 1, "only the paste comes back");
        assert_eq!(app.chips[0].state, ChipState::Pasted);
        assert_eq!(app.chips[0].pasted.as_deref(), Some("the log"));
        assert!(app.sent_files.is_empty() && app.sent_pastes.is_empty());
        let local = app.chips[0].local;
        assert_eq!(
            app.update(Msg::Submit("look again".into())),
            vec![Effect::UploadText {
                local,
                name: "paste-1.txt".into(),
                text: "the log".into(),
                org,
            }],
            "the resend uploads it again"
        );
    }

    #[test]
    fn a_send_refused_for_its_model_keeps_its_pasted_text_with_the_held_draft() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (thinker, _) = with_efforts(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(types::CodersdkChat {
                id: Some(id),
                last_model_config_id: Some(thinker),
                ..Default::default()
            }),
            messages: vec![],
        });
        app.update(Msg::AttachPaste {
            name: "paste-1.txt".into(),
            text: "the log".into(),
        });
        app.update(Msg::Submit("look".into()));
        let file = Uuid::new_v4();
        let seq = seq_of(&app.update(Msg::FileUploaded {
            local: 1,
            file_id: file,
            size: 7,
        }));
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::ModelUnavailable {
                text: "look".into(),
                files: vec![file],
                message: "Invalid model_config_id: model config not found or disabled.".into(),
                plan_mode: None,
                seq,
            }),
        });
        assert_eq!(app.chips.len(), 1);
        assert_eq!(app.chips[0].state, ChipState::Ready(file));
        assert_eq!(app.chips[0].name, "paste-1.txt");
        assert_eq!(app.chips[0].pasted.as_deref(), Some("the log"));
        assert_eq!(app.chips[0].size, Some(7), "the paste keeps its size");
        assert_eq!(app.chips[0].label(), "paste-1.txt 7 B");
        assert!(app.sent_pastes.is_empty(), "the refused send is settled");
        assert_eq!(
            app.update(Msg::ModelPickerClosed),
            vec![Effect::RestoreComposer("look".into())]
        );
        assert_eq!(
            app.chips.len(),
            1,
            "the paste waits above the composer for the resend"
        );
    }

    #[test]
    fn a_paste_alone_sends_with_no_text_and_the_same_paste_attaches_once() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages: vec![],
        });
        for _ in 0..2 {
            app.update(Msg::AttachPaste {
                name: "paste-1.txt".into(),
                text: "the log".into(),
            });
        }
        assert_eq!(app.chips.len(), 1, "one paste, one chip");
        assert_eq!(
            app.update(Msg::Submit(String::new())),
            vec![Effect::UploadText {
                local: 1,
                name: "paste-1.txt".into(),
                text: "the log".into(),
                org,
            }]
        );
        let file = Uuid::new_v4();
        let effects = app.update(Msg::FileUploaded {
            local: 1,
            file_id: file,
            size: 7,
        });
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { text, turn, .. }]
                if text.is_empty() && turn.files == vec![file]),
            "{effects:?}"
        );
        assert!(
            app.update(Msg::Submit(String::new())).is_empty(),
            "an empty send with no paste still sends nothing"
        );
    }

    #[test]
    fn a_paste_over_the_upload_limit_is_a_failed_chip() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let text = "a".repeat(crate::attachments::MAX_FILE_BYTES as usize + 1);
        assert!(
            app.update(Msg::AttachPaste {
                name: "paste-1.txt".into(),
                text,
            })
            .is_empty()
        );
        assert_eq!(
            app.chips[0].label(),
            "paste-1.txt: the pasted text is 10485761 bytes; the limit is 10485760 bytes."
        );
    }

    fn last_error(app: &App) -> String {
        match app.notices.last() {
            Some(Notice::Error(m)) => m.clone(),
            other => panic!("expected an error notice, got {other:?}"),
        }
    }

    /// The number of the one `Effect::SendMessage` among `effects`.
    fn seq_of(effects: &[Effect]) -> u64 {
        let seqs: Vec<u64> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::SendMessage { seq, .. } => Some(*seq),
                _ => None,
            })
            .collect();
        assert_eq!(seqs.len(), 1, "{effects:?}");
        seqs[0]
    }

    fn send_failed(chat: Uuid, text: &str, seq: u64) -> Msg {
        Msg::ForChat {
            chat,
            msg: Box::new(Msg::SendFailed {
                text: text.into(),
                message: "boom".into(),
                plan_mode: None,
                seq,
                mcp_rejected: false,
            }),
        }
    }

    /// Two sends of "look" to one chat, the first with a.png and the second without.
    fn two_identical_sends(app: &mut App) -> (Uuid, u64, u64) {
        started(app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        attach_ready(app, "a.png");
        let first = seq_of(&app.update(Msg::Submit("look".into())));
        let second = seq_of(&app.update(Msg::Submit("look".into())));
        assert_ne!(first, second, "every send gets its own number");
        (id, first, second)
    }

    #[test]
    fn of_two_identical_sends_the_failed_first_names_its_files() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (id, first, second) = two_identical_sends(&mut app);
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::Sent {
                seq: second,
                then: Box::new(Msg::Refresh),
            }),
        });
        app.update(send_failed(id, "look", first));
        assert_eq!(
            last_error(&app),
            "Could not send the message: boom. These files were not sent: a.png. Attach them again."
        );
        assert!(app.sent_files.is_empty(), "both sends are settled");
    }

    #[test]
    fn of_two_identical_sends_the_failed_second_names_no_files() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (id, first, second) = two_identical_sends(&mut app);
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::Sent {
                seq: first,
                then: Box::new(Msg::Refresh),
            }),
        });
        app.update(send_failed(id, "look", second));
        assert_eq!(last_error(&app), "Could not send the message: boom");
        assert!(app.sent_files.is_empty(), "both sends are settled");
    }

    #[test]
    fn a_send_to_the_previous_chat_that_succeeds_is_settled() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(a),
            messages: vec![],
        });
        attach_ready(&mut app, "a.png");
        let seq = seq_of(&app.update(Msg::Submit("look".into())));
        app.update(Msg::OpenChat(b));
        app.update(Msg::ForChat {
            chat: a,
            msg: Box::new(Msg::Sent {
                seq,
                then: Box::new(Msg::Refresh),
            }),
        });
        assert!(app.sent_files.is_empty());
    }

    #[test]
    fn a_failed_send_names_the_files_that_were_not_sent() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        attach_ready(&mut app, "a.png");
        attach_ready(&mut app, "b.txt");
        let seq = seq_of(&app.update(Msg::Submit("look".into())));
        let effects = app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::SendFailed {
                text: "look".into(),
                message: "boom".into(),
                plan_mode: None,
                seq,
                mcp_rejected: false,
            }),
        });
        assert_eq!(effects, vec![Effect::RestoreComposer("look".into())]);
        assert_eq!(
            last_error(&app),
            "Could not send the message: boom. These files were not sent: a.png, b.txt. Attach them again."
        );
    }

    #[test]
    fn a_failed_send_to_the_previous_chat_names_its_files() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(a),
            messages: vec![],
        });
        attach_ready(&mut app, "a.png");
        let seq = seq_of(&app.update(Msg::Submit("look".into())));
        app.update(Msg::OpenChat(b));
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(b),
            messages: vec![],
        });
        app.update(Msg::ForChat {
            chat: a,
            msg: Box::new(Msg::SendFailed {
                text: "look".into(),
                message: "boom".into(),
                plan_mode: None,
                seq,
                mcp_rejected: false,
            }),
        });
        assert!(
            last_error(&app).ends_with("These files were not sent: a.png. Attach them again."),
            "{}",
            last_error(&app)
        );
    }

    #[test]
    fn a_failed_create_names_the_files_that_were_not_sent() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        attach_ready(&mut app, "a.png");
        let effects = app.update(Msg::Submit("look".into()));
        let [Effect::CreateChat { turn, seq, .. }] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        assert_eq!(turn.files.len(), 1);
        let seq = *seq;
        app.update(Msg::CreateFailed {
            message: "boom".into(),
            seq,
        });
        assert_eq!(
            last_error(&app),
            "Could not create the chat: boom. These files were not sent: a.png. Attach them again."
        );
    }

    #[test]
    fn a_stale_upload_failure_leaves_the_open_chats_waiting_message_alone() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(a),
            messages: vec![],
        });
        app.update(Msg::Command(Command::Attach("/tmp/a.md".into())));
        app.update(Msg::OpenChat(b));
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(b),
            messages: vec![],
        });
        app.update(Msg::Command(Command::Attach("/tmp/b.md".into())));
        assert!(app.update(Msg::Submit("for b".into())).is_empty());
        assert!(
            app.update(Msg::UploadFailed {
                local: 1,
                message: "a.md is gone".into()
            })
            .is_empty(),
            "chat a's upload matches no chip"
        );
        let file = Uuid::new_v4();
        let effects = app.update(Msg::FileUploaded {
            local: 2,
            file_id: file,
            size: 10,
        });
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { chat, text, turn, .. }]
                if *chat == b && text == "for b" && turn.files == vec![file]),
            "{effects:?}"
        );
    }

    #[test]
    fn text_queued_behind_a_load_carries_the_files() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::OpenChat(id));
        let file = attach_ready(&mut app, "a.png");
        app.update(Msg::Submit("hi".into()));
        let effects = app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::SendMessage { text, turn, .. }
                if text == "hi" && turn.files == vec![file])),
            "{effects:?}"
        );
        assert!(app.chips.is_empty());
    }

    #[test]
    fn text_queued_behind_a_load_waits_for_an_upload_still_running() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::OpenChat(id));
        app.update(Msg::Submit("hi".into()));
        app.update(Msg::Command(Command::Attach("/tmp/x.png".into())));
        let effects = app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::SendMessage { .. })),
            "{effects:?}"
        );
        assert_eq!(app.chips.len(), 1, "the uploading chip stays");
        let file = Uuid::new_v4();
        let effects = app.update(Msg::FileUploaded {
            local: 1,
            file_id: file,
            size: 10,
        });
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { text, turn, .. }]
                if text == "hi" && turn.files == vec![file]),
            "{effects:?}"
        );
    }

    #[test]
    fn text_queued_behind_a_create_goes_back_when_an_attachment_failed() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("hi".into()));
        app.update(Msg::Submit("more".into()));
        app.update(Msg::Command(Command::Attach("/tmp/logs.zip".into())));
        let id = Uuid::new_v4();
        let effects = app.update(Msg::ChatCreated(chat(id)));
        assert!(
            effects.contains(&Effect::RestoreComposer("more".into())),
            "{effects:?}"
        );
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::SendMessage { .. })),
            "{effects:?}"
        );
        assert_eq!(
            last_error(&app),
            "Remove the attachment that failed (Backspace on an empty composer), then send."
        );
        assert_eq!(app.chips.len(), 1, "the failed chip stays, with its reason");
    }

    #[test]
    fn text_submitted_twice_while_uploading_goes_as_one_message() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages: vec![],
        });
        app.update(Msg::Command(Command::Attach("/tmp/a.md".into())));
        assert!(app.update(Msg::Submit("one".into())).is_empty());
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info("Sending when uploads finish.".into()))
        );
        assert!(app.update(Msg::Submit("two".into())).is_empty());
        let effects = app.update(Msg::FileUploaded {
            local: 1,
            file_id: Uuid::new_v4(),
            size: 10,
        });
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { text, .. }] if text == "one\n\ntwo"),
            "{effects:?}"
        );
    }

    #[test]
    fn removing_the_uploading_chip_sends_the_waiting_text() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages: vec![],
        });
        app.update(Msg::Command(Command::Attach("/tmp/a.md".into())));
        assert!(app.update(Msg::Submit("go".into())).is_empty());
        let effects = app.update(Msg::RemoveLastChip);
        assert!(
            matches!(effects.as_slice(), [Effect::CancelUpload(1), Effect::SendMessage { text, turn, .. }]
                if text == "go" && turn.files.is_empty()),
            "{effects:?}"
        );
    }

    #[test]
    fn removing_an_uploading_chip_or_leaving_the_chat_stops_its_upload() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
            chat: chat(Uuid::new_v4()),
            messages: vec![],
            has_more: None,
        });
        app.update(Msg::Command(Command::Attach("/tmp/a.md".into())));
        let local = app.chips[0].local;
        assert_eq!(
            app.update(Msg::RemoveLastChip),
            vec![Effect::CancelUpload(local)]
        );
        app.update(Msg::Command(Command::Attach("/tmp/b.md".into())));
        let file = attach_ready(&mut app, "c.md");
        assert!(matches!(app.chips[1].state, ChipState::Ready(id) if id == file));
        let uploading = app.chips[0].local;
        let effects = app.update(Msg::Command(Command::New));
        assert_eq!(
            effects
                .iter()
                .filter(|e| matches!(e, Effect::CancelUpload(_)))
                .collect::<Vec<_>>(),
            [&Effect::CancelUpload(uploading)],
            "only the upload still running stops: {effects:?}"
        );
    }

    #[test]
    fn a_mention_chip_uploads_only_at_the_send_and_the_message_waits_for_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
            has_more: None,
        });
        assert!(
            app.update(Msg::AttachMention("/tmp/notes.md".into()))
                .is_empty(),
            "a mention uploads nothing before the send"
        );
        assert_eq!(app.chips.len(), 1);
        let local = app.chips[0].local;
        let effects = app.update(Msg::Submit("read @/tmp/notes.md".into()));
        assert!(
            matches!(effects.as_slice(), [Effect::UploadFile { local: l, path, .. }]
                if *l == local && path == "/tmp/notes.md"),
            "{effects:?}"
        );
        assert_eq!(app.chips[0].state, ChipState::Uploading);
        let file = Uuid::new_v4();
        let effects = app.update(Msg::FileUploaded {
            local,
            file_id: file,
            size: 1,
        });
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { chat, text, turn, .. }]
                if *chat == id && text == "read @/tmp/notes.md" && turn.files == vec![file]),
            "{effects:?}"
        );
    }

    #[test]
    fn a_mention_chip_removed_before_the_send_never_uploads() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
            chat: chat(Uuid::new_v4()),
            messages: vec![],
            has_more: None,
        });
        let mut effects = app.update(Msg::AttachMention("/tmp/secret.md".into()));
        effects.extend(app.update(Msg::RemoveLastChip));
        effects.extend(app.update(Msg::Submit("never mind".into())));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::UploadFile { .. } | Effect::CancelUpload(_))),
            "{effects:?}"
        );
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { turn, .. }] if turn.files.is_empty()),
            "{effects:?}"
        );
    }

    #[test]
    fn a_send_that_waited_on_uploads_leaves_a_newer_mention_for_its_own_send() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
            chat: chat(Uuid::new_v4()),
            messages: vec![],
            has_more: None,
        });
        app.update(Msg::Command(Command::Attach("/tmp/a.md".into())));
        app.update(Msg::Submit("first".into()));
        app.update(Msg::AttachMention("/tmp/b.md".into()));
        let effects = app.update(Msg::FileUploaded {
            local: app.chips[0].local,
            file_id: Uuid::new_v4(),
            size: 1,
        });
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { text, turn, .. }]
                if text == "first" && turn.files.len() == 1),
            "{effects:?}"
        );
        assert!(
            matches!(app.chips.as_slice(), [Chip { state: ChipState::Held(path), .. }] if path == "/tmp/b.md"),
            "{:?}",
            app.chips
        );
    }

    /// The attached workspace the `/workspace` tests load: `dev`, owned by `nick`, with agent `main`.
    fn workspace_details(id: Uuid) -> Box<types::CodersdkWorkspace> {
        Box::new(serde_json::from_value(json!({
            "id": id, "name": "dev", "owner_name": "nick", "shared_with": [],
            "latest_build": {"resources": [{"agents": [{"name": "main", "apps": [], "display_apps": [],
                "environment_variables": {}, "latency": {}, "log_sources": [], "metadata": [],
                "scripts": [], "subsystems": []}], "metadata": []}]}
        })).unwrap())
    }

    /// Opens `/workspace` for the attached workspace and delivers its details.
    fn reopen_workspace_panel(app: &mut App, chat: Uuid, ws: Uuid) {
        app.update(Msg::Command(Command::Workspace(None)));
        app.update(Msg::ForChat {
            chat,
            msg: Box::new(Msg::WorkspaceDetailsLoaded(workspace_details(ws))),
        });
    }

    #[test]
    fn workspace_with_one_attached_shows_details_and_its_actions() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (id, ws) = (Uuid::new_v4(), Uuid::new_v4());
        let mut open = chat(id);
        open.workspace_id = Some(ws);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: open,
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Workspace(None))),
            vec![
                Effect::ShowWorkspace,
                Effect::FetchWorkspaceDetails {
                    chat: id,
                    workspace: ws
                },
                Effect::FetchSshSuffix
            ]
        );
        app.update(Msg::SshSuffixLoaded(Some("coder".into())));
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::WorkspaceDetailsLoaded(workspace_details(ws))),
        });
        assert_eq!(
            app.update(Msg::WorkspaceAction(WorkspaceAction::CopySsh)),
            vec![Effect::CopyText {
                text: "ssh main.dev.nick.coder".into(),
                what: "the SSH command"
            }]
        );
        // Every action closes the panel, so each one below runs from a reopened panel.
        reopen_workspace_panel(&mut app, id, ws);
        assert_eq!(
            app.update(Msg::WorkspaceAction(WorkspaceAction::OpenWeb)),
            vec![Effect::OpenWorkspaceWeb {
                owner: "nick".into(),
                workspace: "dev".into()
            }]
        );
        reopen_workspace_panel(&mut app, id, ws);
        assert_eq!(
            app.update(Msg::WorkspaceAction(WorkspaceAction::Switch)),
            vec![Effect::ShowPicker(Picker::Workspace)]
        );
        reopen_workspace_panel(&mut app, id, ws);
        assert_eq!(
            app.update(Msg::WorkspaceAction(WorkspaceAction::Detach)),
            vec![Effect::SetWorkspace {
                chat: id,
                workspace: None
            }]
        );
        assert!(app.workspace_panel.is_none());
        assert_eq!(
            app.update(Msg::Command(Command::Workspace(None))),
            vec![Effect::ShowPicker(Picker::Workspace)],
            "after detaching, /workspace is the table again"
        );
    }

    #[test]
    fn every_workspace_action_closes_the_panel_and_late_details_are_dropped() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (id, ws) = (Uuid::new_v4(), Uuid::new_v4());
        let mut open = chat(id);
        open.workspace_id = Some(ws);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: open,
            messages: vec![],
        });
        reopen_workspace_panel(&mut app, id, ws);
        assert!(
            !app.update(Msg::WorkspaceAction(WorkspaceAction::CopySsh))
                .is_empty()
        );
        assert!(app.workspace_panel.is_none(), "copying closes the panel");
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::WorkspaceDetailsLoaded(workspace_details(ws))),
        });
        assert!(
            app.workspace_panel.is_none(),
            "details that arrive after the panel closed are dropped"
        );
        reopen_workspace_panel(&mut app, id, ws);
        assert!(
            !app.update(Msg::WorkspaceAction(WorkspaceAction::OpenWeb))
                .is_empty()
        );
        assert!(
            app.workspace_panel.is_none(),
            "opening the web UI closes the panel"
        );
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::WorkspaceDetailsFailed {
                workspace: ws,
                message: "HTTP 502".into(),
            }),
        });
        assert!(
            app.workspace_panel.is_none(),
            "a late failure is dropped too"
        );
    }

    /// An app with chat `id` open and workspace `ws` attached to it.
    fn attached() -> (App, Uuid, Uuid, Uuid) {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let (id, ws) = (Uuid::new_v4(), Uuid::new_v4());
        let mut open = chat(id);
        open.workspace_id = Some(ws);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: open,
            messages: vec![],
        });
        (app, org, id, ws)
    }

    #[test]
    fn switch_retries_a_workspace_list_that_failed_to_load() {
        let (mut app, org, id, ws) = attached();
        app.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::WorkspacesFailed {
                message: "HTTP 502".into(),
            }),
        });
        reopen_workspace_panel(&mut app, id, ws);
        assert_eq!(
            app.update(Msg::WorkspaceAction(WorkspaceAction::Switch)),
            vec![
                Effect::ShowPicker(Picker::Workspace),
                Effect::FetchWorkspaces(org)
            ]
        );
        assert_eq!(app.workspaces_state, WorkspacesState::Loading);
    }

    #[test]
    fn an_action_before_the_details_load_says_to_run_workspace_again() {
        let (mut app, _, _, _) = attached();
        app.update(Msg::Command(Command::Workspace(None)));
        assert!(
            app.update(Msg::WorkspaceAction(WorkspaceAction::CopySsh))
                .is_empty()
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info(
                "The workspace details have not loaded yet. Run /workspace again.".into()
            ))
        );
    }

    #[test]
    fn an_action_after_the_details_failed_says_workspace_retries() {
        let (mut app, _, id, ws) = attached();
        app.update(Msg::Command(Command::Workspace(None)));
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::WorkspaceDetailsFailed {
                workspace: ws,
                message: "HTTP 502".into(),
            }),
        });
        assert!(
            app.update(Msg::WorkspaceAction(WorkspaceAction::OpenWeb))
                .is_empty()
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error(
                "The workspace details could not load: HTTP 502. /workspace retries.".into()
            ))
        );
    }

    #[test]
    fn a_failed_ssh_suffix_is_fetched_again_but_an_empty_one_is_kept() {
        let (mut app, _, _, _) = attached();
        let opened = |app: &mut App| app.update(Msg::Command(Command::Workspace(None)));
        assert!(opened(&mut app).contains(&Effect::FetchSshSuffix));
        app.update(Msg::WorkspaceClosed);
        app.update(Msg::SshSuffixFailed);
        assert_eq!(app.ssh_suffix, None);
        assert!(
            opened(&mut app).contains(&Effect::FetchSshSuffix),
            "a failure is not cached"
        );
        app.update(Msg::WorkspaceClosed);
        app.update(Msg::SshSuffixLoaded(None));
        assert_eq!(app.ssh_suffix, Some(None));
        assert!(
            !opened(&mut app).contains(&Effect::FetchSshSuffix),
            "a deployment with no suffix is cached"
        );
    }

    #[test]
    fn details_for_another_workspace_are_dropped() {
        let (mut app, _, id, _) = attached();
        app.update(Msg::Command(Command::Workspace(None)));
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::WorkspaceDetailsLoaded(workspace_details(
                Uuid::new_v4(),
            ))),
        });
        assert!(
            matches!(app.workspace_panel, Some(Fetched::Loading)),
            "a reply for an earlier workspace leaves the panel loading"
        );
    }

    #[test]
    fn leaving_the_chat_closes_the_workspace_panel() {
        let (mut app, _, id, ws) = attached();
        reopen_workspace_panel(&mut app, id, ws);
        assert!(app.workspace_panel.is_some());
        app.update(Msg::Command(Command::New));
        assert!(app.workspace_panel.is_none());
    }

    #[test]
    fn git_fetches_the_diff_watches_local_changes_and_merges_their_deltas() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        let mut open = chat(id);
        open.workspace_id = Some(Uuid::new_v4());
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: open,
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Git)),
            vec![
                Effect::ShowGit,
                Effect::FetchDiff {
                    chat: id,
                    generation: 1
                },
                Effect::OpenGitWatch {
                    chat: id,
                    generation: 1
                }
            ]
        );
        let changes = |repos: serde_json::Value| Msg::ForGit {
            chat: id,
            generation: 1,
            msg: Box::new(Msg::GitChanges(Box::new(
                serde_json::from_value(json!({"type": "changes", "repositories": repos})).unwrap(),
            ))),
        };
        app.update(changes(json!([
            {"repo_root": "/a", "branch": "m2", "unified_diff": "+x\n"},
            {"repo_root": "/b", "branch": "main", "unified_diff": "+y\n"}
        ])));
        app.update(changes(
            json!([{"repo_root": "/b", "branch": "", "removed": true}]),
        ));
        let repos: Vec<&String> = app.git_panel.as_ref().unwrap().repos.keys().collect();
        assert_eq!(repos, ["/a"]);
        assert_eq!(
            app.update(watch(
                "diff_status_change",
                listed(id, "t", "2026-09-30T10:00:00Z")
            )),
            vec![Effect::FetchDiff {
                chat: id,
                generation: 2
            }]
        );
        assert_eq!(app.update(Msg::GitClosed), vec![Effect::CloseGitWatch]);
        assert!(app.git_panel.is_none());
    }

    #[test]
    fn git_without_a_workspace_skips_the_local_changes() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Git)),
            vec![
                Effect::ShowGit,
                Effect::FetchDiff {
                    chat: id,
                    generation: 1
                }
            ]
        );
        assert!(matches!(
            app.git_panel.as_ref().map(|p| &p.local),
            Some(crate::panels::LocalGit::NoWorkspace)
        ));
    }

    #[test]
    fn a_reopened_git_panel_drops_the_old_sockets_messages() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        let mut open = chat(id);
        open.workspace_id = Some(Uuid::new_v4());
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: open,
            messages: vec![],
        });
        app.update(Msg::Command(Command::Git));
        app.update(Msg::GitClosed);
        assert!(
            app.update(Msg::Command(Command::Git))
                .contains(&Effect::OpenGitWatch {
                    chat: id,
                    generation: 2
                })
        );
        let changes = |chat, generation| Msg::ForGit {
            chat,
            generation,
            msg: Box::new(Msg::GitChanges(Box::new(
                serde_json::from_value(json!({"type": "changes", "repositories": [
                    {"repo_root": "/a", "branch": "m2", "unified_diff": "+x\n"}
                ]}))
                .unwrap(),
            ))),
        };
        // Still queued from the first socket, which the close aborted.
        app.update(Msg::ForGit {
            chat: id,
            generation: 1,
            msg: Box::new(Msg::GitWatchEnded("the connection closed".into())),
        });
        app.update(changes(id, 1));
        app.update(changes(Uuid::new_v4(), 2));
        let panel = app.git_panel.as_ref().unwrap();
        assert_eq!(panel.local, crate::panels::LocalGit::Connecting);
        assert!(panel.repos.is_empty());
        app.update(changes(id, 2));
        let panel = app.git_panel.as_ref().unwrap();
        assert_eq!(panel.local, crate::panels::LocalGit::Live);
        assert_eq!(panel.repos.len(), 1);
    }

    /// The runtime's reply to `Effect::FetchDiff { chat, generation }`, with `diff` as the
    /// server's diff.
    fn diff_reply(chat: Uuid, generation: u64, diff: &str) -> Msg {
        Msg::ForChat {
            chat,
            msg: Box::new(Msg::DiffLoaded {
                diff: Box::new(serde_json::from_value(json!({"diff": diff})).unwrap()),
                generation,
            }),
        }
    }

    #[test]
    fn diff_pages_the_servers_diff_once_it_loads_or_says_there_is_none() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Diff)),
            vec![Effect::FetchDiff {
                chat: id,
                generation: 1
            }]
        );
        let loaded = |generation, diff: &str| diff_reply(id, generation, diff);
        assert_eq!(
            app.update(loaded(1, "diff --git a/x b/x\n+new\n")),
            vec![Effect::Page("diff --git a/x b/x\n+new\n".into())]
        );
        app.update(Msg::Command(Command::Diff));
        assert!(app.update(loaded(2, "")).is_empty());
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info("No git changes for this chat yet.".into()))
        );
    }

    #[test]
    fn diff_without_a_chat_asks_for_one() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert!(app.update(Msg::Command(Command::Diff)).is_empty());
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error("Start a chat first.".into()))
        );
    }

    #[test]
    fn a_failed_diff_says_why_only_when_diff_asked_for_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        let failed = |generation| Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::DiffFailed {
                message: "HTTP 500".into(),
                generation,
            }),
        };
        app.update(Msg::Command(Command::Diff));
        assert!(app.update(failed(1)).is_empty());
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Error("Could not load the diff: HTTP 500".into()))
        );
        let shown = app.notices.len();
        app.update(Msg::Command(Command::Git));
        assert!(app.update(failed(2)).is_empty());
        assert_eq!(app.notices.len(), shown, "the panel shows a /git failure");
        assert!(matches!(
            app.git_panel.as_ref().map(|p| &p.diff),
            Some(Fetched::Failed(m)) if m == "HTTP 500"
        ));
    }

    #[test]
    fn an_older_diff_never_lands_after_a_newer_one() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        assert!(
            app.update(Msg::Command(Command::Git))
                .contains(&Effect::FetchDiff {
                    chat: id,
                    generation: 1
                })
        );
        assert_eq!(
            app.update(Msg::Command(Command::Diff)),
            vec![Effect::FetchDiff {
                chat: id,
                generation: 2
            }]
        );
        assert_eq!(
            app.update(watch(
                "diff_status_change",
                listed(id, "t", "2026-09-30T10:00:00Z")
            )),
            vec![Effect::FetchDiff {
                chat: id,
                generation: 3
            }]
        );
        // The replies land newest first.
        assert_eq!(
            app.update(diff_reply(id, 3, "+newest\n")),
            vec![Effect::Page("+newest\n".into())]
        );
        assert!(app.update(diff_reply(id, 1, "+oldest\n")).is_empty());
        assert!(app.update(diff_reply(id, 2, "+middle\n")).is_empty());
        let shown = app.notices.len();
        assert!(
            app.update(Msg::ForChat {
                chat: id,
                msg: Box::new(Msg::DiffFailed {
                    message: "HTTP 500".into(),
                    generation: 2,
                }),
            })
            .is_empty()
        );
        assert_eq!(app.notices.len(), shown);
        let panel = app.git_panel.as_ref().unwrap();
        assert_eq!(
            crate::panels::diff_text(panel).as_deref(),
            Some("+newest\n")
        );
    }

    #[test]
    fn an_older_diff_is_not_paged_while_diff_waits_for_its_own() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.update(Msg::Command(Command::Git));
        app.update(Msg::Command(Command::Diff));
        assert!(app.update(diff_reply(id, 1, "+old\n")).is_empty());
        assert_eq!(
            app.update(diff_reply(id, 2, "+new\n")),
            vec![Effect::Page("+new\n".into())]
        );
    }

    #[test]
    fn a_diff_asked_for_in_another_chat_is_not_paged() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let first = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(first),
            messages: vec![],
        });
        app.update(Msg::Command(Command::Diff));
        app.update(Msg::Command(Command::New));
        let second = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(second),
            messages: vec![],
        });
        app.update(Msg::Command(Command::Git));
        assert!(app.update(diff_reply(second, 2, "+x\n")).is_empty());
    }

    #[test]
    fn view_diff_pages_the_panels_diff_or_says_there_is_none() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.update(Msg::Command(Command::Git));
        assert!(app.update(diff_reply(id, 1, "")).is_empty());
        assert!(app.update(Msg::GitAction(GitAction::ViewDiff)).is_empty());
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info("No git changes for this chat yet.".into()))
        );
        app.update(diff_reply(id, 1, "+x\n"));
        assert_eq!(
            app.update(Msg::GitAction(GitAction::ViewDiff)),
            vec![Effect::Page("+x\n".into())]
        );
    }

    #[test]
    fn view_diff_while_the_diff_loads_says_so() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.update(Msg::Command(Command::Git));
        assert!(app.update(Msg::GitAction(GitAction::ViewDiff)).is_empty());
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info("The diff is still loading.".into()))
        );
    }

    #[test]
    fn settings_asks_the_main_loop_to_edit_the_config() {
        let mut app = App::new(BusyBehavior::Queue, true);
        assert_eq!(
            app.update(Msg::Command(Command::Settings)),
            vec![Effect::EditSettings]
        );
        assert!(
            Effect::EditSettings.runs_in_main(),
            "the editor needs the terminal, which only the main loop owns"
        );
    }

    #[test]
    fn the_main_loop_runs_the_effects_that_need_the_terminal_or_end_it() {
        assert!(Effect::Page("+x\n".into()).runs_in_main());
        assert!(Effect::Quit.runs_in_main());
        assert!(!Effect::FetchPrefs.runs_in_main());
        assert!(!Effect::ShowGit.runs_in_main());
    }

    #[test]
    fn mcp_fetches_the_servers_and_health_for_the_chats_organization() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (id, org) = (Uuid::new_v4(), Uuid::new_v4());
        let mut open = chat(id);
        open.organization_id = Some(org);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: open,
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Mcp)),
            vec![
                Effect::ShowMcp,
                Effect::RefreshChat {
                    chat: id,
                    generation: 1
                },
                Effect::FetchMcpServers {
                    chat: id,
                    org,
                    generation: 1
                },
                Effect::FetchMcpHealth {
                    chat: id,
                    generation: 1
                }
            ]
        );
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::McpHealthLoaded {
                outcomes: None,
                generation: 1,
            }),
        });
        assert!(matches!(
            app.mcp_panel.as_ref().map(|p| &p.health),
            Some(Fetched::Loaded(None))
        ));
        app.update(Msg::McpClosed);
        assert!(app.mcp_panel.is_none());
    }

    #[test]
    fn an_mcp_reply_to_an_earlier_open_or_another_chat_is_dropped() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        let health = |chat, generation| Msg::ForChat {
            chat,
            msg: Box::new(Msg::McpHealthLoaded {
                outcomes: None,
                generation,
            }),
        };
        let servers = |chat, generation| Msg::ForChat {
            chat,
            msg: Box::new(Msg::McpServersLoaded {
                servers: vec![],
                generation,
            }),
        };
        app.update(Msg::Command(Command::Mcp));
        app.update(Msg::McpClosed);
        app.update(Msg::Command(Command::Mcp));
        // The first open's replies land in the reopened panel's place and are dropped.
        app.update(health(id, 1));
        app.update(servers(id, 1));
        // So are replies about a chat that is not open.
        app.update(health(Uuid::new_v4(), 2));
        app.update(servers(Uuid::new_v4(), 2));
        let panel = app.mcp_panel.as_ref().unwrap();
        assert!(matches!(panel.health, Fetched::Loading));
        assert!(matches!(panel.servers, Fetched::Loading));
        app.update(health(id, 2));
        app.update(servers(id, 2));
        let panel = app.mcp_panel.as_ref().unwrap();
        assert!(matches!(panel.health, Fetched::Loaded(None)));
        assert!(matches!(panel.servers, Fetched::Loaded(_)));
    }

    #[test]
    fn leaving_the_chat_closes_the_mcp_panel() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.update(Msg::Command(Command::Mcp));
        assert!(app.mcp_panel.is_some());
        app.update(Msg::Command(Command::New));
        assert!(app.mcp_panel.is_none());
        // A reply that was in flight finds no panel to fill.
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::McpHealthLoaded {
                outcomes: None,
                generation: 1,
            }),
        });
        assert!(app.mcp_panel.is_none());
    }

    /// Opens `/mcp` on the loaded chat with GitHub (off by default) and Docs (required).
    fn mcp_servers(app: &mut App, github: Uuid, docs: Uuid) {
        use crate::panels::{Fetched, McpPanel};
        app.mcp_panel = Some(McpPanel {
            servers: Fetched::Loaded(serde_json::from_value(json!([
                {"id": github, "display_name": "GitHub", "availability": "default_off", "tool_allow_list": [], "tool_deny_list": []},
                {"id": docs, "display_name": "Docs", "availability": "force_on", "tool_allow_list": [], "tool_deny_list": []}
            ])).unwrap()),
            health: Fetched::Loaded(None),
        });
    }

    /// Loads chat `id` with `github` selected.
    fn load_with_github(app: &mut App, id: Uuid, github: Uuid) {
        let mut open = chat(id);
        open.mcp_server_ids = vec![github];
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: open,
            messages: vec![],
        });
    }

    /// Loads `org`'s MCP servers for new chats: GitHub on by default, Docs required, and Jira
    /// off by default.
    fn org_mcp(app: &mut App, org: Uuid) -> (Uuid, Uuid, Uuid) {
        let (github, docs, jira) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::OrgMcpLoaded(
                serde_json::from_value(json!([
                    {"id": github, "display_name": "GitHub", "availability": "default_on", "enabled": true, "tool_allow_list": [], "tool_deny_list": []},
                    {"id": docs, "display_name": "Docs", "availability": "force_on", "enabled": true, "tool_allow_list": [], "tool_deny_list": []},
                    {"id": jira, "display_name": "Jira", "availability": "default_off", "enabled": true, "tool_allow_list": [], "tool_deny_list": []}
                ]))
                .unwrap(),
            )),
        });
        (github, docs, jira)
    }

    fn create_turn(effects: &[Effect]) -> &TurnOptions {
        match effects {
            [Effect::CreateChat { turn, .. }] => turn,
            other => panic!("expected one CreateChat, got {other:?}"),
        }
    }

    #[test]
    fn the_lists_load_the_organizations_mcp_servers_too() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let effects = app.update(Msg::Started {
            org_id: Uuid::new_v4(),
            open_chat: None,
        });
        let org = app.org_id.unwrap();
        assert!(
            effects.contains(&Effect::FetchOrgMcpServers(org)),
            "{effects:?}"
        );
        assert!(matches!(app.org_mcp, Fetched::Loading));
        app.update(Msg::ForOrg {
            org: Uuid::new_v4(),
            msg: Box::new(Msg::OrgMcpLoaded(vec![])),
        });
        assert!(
            matches!(app.org_mcp, Fetched::Loading),
            "another organization's list is dropped"
        );
    }

    #[test]
    fn mcp_on_a_blank_chat_lists_the_organizations_servers() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        assert_eq!(
            app.update(Msg::Command(Command::Mcp)),
            vec![Effect::ShowMcp]
        );
        assert!(matches!(
            app.mcp_panel.as_ref().map(|p| &p.servers),
            Some(Fetched::Loading)
        ));
        let (github, docs, jira) = org_mcp(&mut app, org);
        let (groups, note) = crate::panels::mcp_groups(&app);
        assert_eq!(
            groups.len(),
            1,
            "a blank chat has no inline or workspace servers"
        );
        let states: Vec<(Option<Uuid>, &str)> = groups[0]
            .rows
            .iter()
            .map(|r| (r.id, r.state.as_str()))
            .collect();
        assert_eq!(
            states,
            vec![
                (Some(github), "on"),
                (Some(docs), "on (required)"),
                (Some(jira), "off")
            ]
        );
        assert_eq!(
            note.as_deref(),
            Some("The chat's first message turns on the servers shown as on.")
        );
        assert!(
            !app.notices.iter().any(|n| matches!(n, Notice::Error(_))),
            "{:?}",
            app.notices
        );
    }

    #[test]
    fn toggling_on_a_blank_chat_rides_on_the_first_message() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let (github, docs, jira) = org_mcp(&mut app, org);
        app.update(Msg::Command(Command::Mcp));
        app.update(Msg::ToggleMcp(jira));
        app.update(Msg::ToggleMcp(github));
        assert_eq!(app.mcp_next, Some(vec![docs, jira]));
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info(
                "GitHub turns off with your first message.".into()
            ))
        );
        let effects = app.update(Msg::Submit("hello".into()));
        assert_eq!(create_turn(&effects).mcp_servers, Some(vec![docs, jira]));
        app.update(Msg::ChatCreated(chat(Uuid::new_v4())));
        assert_eq!(app.mcp_next, None, "the selection went with the create");
    }

    #[test]
    fn a_blank_chat_sends_the_organization_defaults_once_they_load() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let effects = app.update(Msg::Submit("before the list".into()));
        assert_eq!(
            create_turn(&effects).mcp_servers,
            None,
            "the server picks while the list is unknown"
        );
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let (github, docs, _) = org_mcp(&mut app, org);
        let effects = app.update(Msg::Submit("after the list".into()));
        assert_eq!(create_turn(&effects).mcp_servers, Some(vec![github, docs]));
    }

    #[test]
    fn mcp_on_a_blank_chat_retries_a_failed_list() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        app.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::OrgMcpFailed("HTTP 502".into())),
        });
        assert_eq!(
            app.update(Msg::Command(Command::Mcp)),
            vec![Effect::ShowMcp, Effect::FetchOrgMcpServers(org)]
        );
        assert!(matches!(
            app.mcp_panel.as_ref().map(|p| &p.servers),
            Some(Fetched::Loading)
        ));
        app.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::OrgMcpFailed("HTTP 503".into())),
        });
        assert!(matches!(
            app.mcp_panel.as_ref().map(|p| &p.servers),
            Some(Fetched::Failed(m)) if m == "HTTP 503"
        ));
    }

    #[test]
    fn a_failed_create_refetches_the_servers_and_the_retry_drops_a_disabled_one() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let (github, docs, jira) = org_mcp(&mut app, org);
        app.update(Msg::Command(Command::Mcp));
        app.update(Msg::ToggleMcp(jira));
        let effects = app.update(Msg::Submit("hello".into()));
        let seq = match effects.as_slice() {
            [Effect::CreateChat { seq, turn, .. }] => {
                assert_eq!(turn.mcp_servers, Some(vec![github, docs, jira]));
                *seq
            }
            other => panic!("expected one CreateChat, got {other:?}"),
        };
        let failed = app.update(Msg::CreateFailed {
            message: "One or more MCP server IDs are invalid or disabled.".into(),
            seq,
        });
        assert!(
            failed.contains(&Effect::FetchOrgMcpServers(org)),
            "{failed:?}"
        );
        // Jira was disabled since the list loaded, so the refetch leaves it out.
        app.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::OrgMcpLoaded(
                serde_json::from_value(json!([
                    {"id": github, "display_name": "GitHub", "availability": "default_on", "enabled": true, "tool_allow_list": [], "tool_deny_list": []},
                    {"id": docs, "display_name": "Docs", "availability": "force_on", "enabled": true, "tool_allow_list": [], "tool_deny_list": []}
                ]))
                .unwrap(),
            )),
        });
        let retry = app.update(Msg::Submit("hello".into()));
        assert_eq!(create_turn(&retry).mcp_servers, Some(vec![github, docs]));
    }

    #[test]
    fn switching_organizations_drops_a_blank_chats_mcp_selection() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (product, coder) = two_orgs(&mut app);
        let (_, _, jira) = org_mcp(&mut app, coder.id);
        app.update(Msg::Command(Command::Mcp));
        app.update(Msg::ToggleMcp(jira));
        assert!(app.mcp_next.is_some());
        app.update(Msg::OrganizationChosen(product.id));
        assert_eq!(app.mcp_next, None);
        assert!(matches!(
            app.mcp_panel.as_ref().map(|p| &p.servers),
            Some(Fetched::Loading)
        ));
        let effects = app.update(Msg::Submit("early".into()));
        assert_eq!(
            create_turn(&effects).mcp_servers,
            None,
            "the old organization's IDs never go to the new one"
        );
    }

    #[test]
    fn a_new_organizations_defaults_apply_once_its_list_loads() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (product, coder) = two_orgs(&mut app);
        let (_, _, jira) = org_mcp(&mut app, coder.id);
        app.update(Msg::Command(Command::Mcp));
        app.update(Msg::ToggleMcp(jira));
        assert!(app.mcp_next.is_some());
        app.update(Msg::OrganizationChosen(product.id));
        let (github, docs, _) = org_mcp(&mut app, product.id);
        let effects = app.update(Msg::Submit("late".into()));
        assert_eq!(create_turn(&effects).mcp_servers, Some(vec![github, docs]));
    }

    #[test]
    fn mcp_after_a_failed_chat_load_leaves_the_retry_alone() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (org, missing) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::Started {
            org_id: org,
            open_chat: Some(missing),
        });
        let (_, _, jira) = org_mcp(&mut app, org);
        app.update(Msg::ChatLoadFailed {
            chat_id: missing,
            message: "gone".into(),
        });
        assert!(app.update(Msg::Command(Command::Mcp)).is_empty());
        let failed =
            Notice::Info("The chat did not load. Send a message to retry, then use /mcp.".into());
        assert_eq!(
            app.notices.last(),
            Some(&failed),
            "waiting would never help"
        );
        app.update(Msg::ToggleMcp(jira));
        assert_eq!(app.notices.last(), Some(&failed), "the toggle says so too");
        assert_eq!(app.mcp_next, None);
        assert!(app.mcp_panel.is_none());
    }

    #[test]
    fn mcp_left_open_across_the_create_shows_the_new_chats_servers() {
        use crate::panels::Fetched;
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        org_mcp(&mut app, org);
        app.update(Msg::Command(Command::Mcp));
        app.update(Msg::Submit("hello".into()));
        let id = Uuid::new_v4();
        let effects = app.update(Msg::ChatCreated(chat(id)));
        let panel = app.mcp_panel.as_ref().expect("the panel stays open");
        assert!(matches!(panel.health, Fetched::Loading));
        assert!(matches!(panel.servers, Fetched::Loading));
        let generation = app.mcp_generation;
        assert!(effects.contains(&Effect::FetchMcpHealth {
            chat: id,
            generation
        }));
        assert!(effects.contains(&Effect::FetchMcpServers {
            chat: id,
            org,
            generation,
        }));
        assert!(
            !effects.contains(&Effect::ShowMcp),
            "the open panel keeps its place"
        );
    }

    #[test]
    fn a_create_with_mcp_closed_fetches_no_mcp_panel() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        org_mcp(&mut app, org);
        app.update(Msg::Submit("hello".into()));
        let effects = app.update(Msg::ChatCreated(chat(Uuid::new_v4())));
        assert!(app.mcp_panel.is_none());
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::FetchMcpHealth { .. }))
        );
    }

    #[test]
    fn mcp_waits_while_the_new_chat_is_being_created() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let (github, _, _) = org_mcp(&mut app, org);
        app.update(Msg::Command(Command::Mcp));
        app.update(Msg::Submit("hello".into()));
        assert!(app.update(Msg::Command(Command::Mcp)).is_empty());
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info(
                "Wait for this chat to finish starting, then use /mcp.".into()
            ))
        );
        app.update(Msg::ToggleMcp(github));
        assert_eq!(app.mcp_next, None, "the toggle waits too");
    }

    #[test]
    fn toggling_a_server_rides_on_the_next_message() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (id, github, docs) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        load_with_github(&mut app, id, github);
        mcp_servers(&mut app, github, docs);
        app.update(Msg::ToggleMcp(docs));
        assert_eq!(app.mcp_next, None, "a required server cannot be turned off");
        app.update(Msg::ToggleMcp(github));
        assert_eq!(app.mcp_next, Some(vec![]));
        let effects = app.update(Msg::Submit("no tools this time".into()));
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { turn, .. }] if turn.mcp_servers == Some(vec![])),
            "{effects:?}"
        );
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::Sent {
                seq: seq_of(&effects),
                then: Box::new(Msg::Refresh),
            }),
        });
        assert_eq!(app.mcp_next, None, "the selection went with the message");
        assert_eq!(
            app.chat.as_ref().unwrap().mcp_server_ids,
            Vec::<Uuid>::new(),
            "the panel shows the sent selection before the refetch lands"
        );
    }

    #[test]
    fn toggling_back_to_the_chats_selection_leaves_nothing_pending() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (id, github, docs) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        load_with_github(&mut app, id, github);
        mcp_servers(&mut app, github, docs);
        app.update(Msg::ToggleMcp(github));
        app.update(Msg::ToggleMcp(github));
        assert_eq!(app.mcp_next, None);
        let effects = app.update(Msg::Submit("hi".into()));
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { turn, .. }] if turn.mcp_servers.is_none()),
            "{effects:?}"
        );
    }

    #[test]
    fn a_failed_send_keeps_the_mcp_selection_for_the_next_send() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (id, github, docs) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        load_with_github(&mut app, id, github);
        mcp_servers(&mut app, github, docs);
        app.update(Msg::ToggleMcp(github));
        let first = app.update(Msg::Submit("one".into()));
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::SendFailed {
                text: "one".into(),
                message: "HTTP 500".into(),
                plan_mode: None,
                seq: seq_of(&first),
                mcp_rejected: false,
            }),
        });
        assert_eq!(
            app.mcp_next,
            Some(vec![]),
            "the failed send consumed nothing"
        );
        assert_eq!(app.chat.as_ref().unwrap().mcp_server_ids, vec![github]);
        let second = app.update(Msg::Submit("one".into()));
        assert!(
            matches!(second.as_slice(), [Effect::SendMessage { turn, .. }] if turn.mcp_servers == Some(vec![])),
            "the retry carries the selection: {second:?}"
        );
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::Sent {
                seq: seq_of(&second),
                then: Box::new(Msg::Refresh),
            }),
        });
        assert_eq!(app.mcp_next, None);
    }

    #[test]
    fn switching_chats_drops_a_pending_mcp_selection() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (a, b, github, docs) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        load_with_github(&mut app, a, github);
        mcp_servers(&mut app, github, docs);
        app.update(Msg::ToggleMcp(github));
        let sent_to_a = app.update(Msg::Submit("in a".into()));
        app.update(Msg::OpenChat(b));
        assert_eq!(
            app.mcp_next, None,
            "the selection belonged to the chat left"
        );
        load_with_github(&mut app, b, github);
        let effects = app.update(Msg::Submit("in b".into()));
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { turn, .. }] if turn.mcp_servers.is_none()),
            "{effects:?}"
        );
        mcp_servers(&mut app, github, docs);
        app.update(Msg::ToggleMcp(github));
        app.update(Msg::ForChat {
            chat: a,
            msg: Box::new(Msg::Sent {
                seq: seq_of(&sent_to_a),
                then: Box::new(Msg::Refresh),
            }),
        });
        assert_eq!(
            app.mcp_next,
            Some(vec![]),
            "the first chat's send settles nothing in the second"
        );
        assert_eq!(app.chat.as_ref().unwrap().mcp_server_ids, vec![github]);
    }

    #[test]
    fn a_held_implement_refuses_a_message_with_an_mcp_selection_and_keeps_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (id, github, docs) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        load_with_github(&mut app, id, github);
        mcp_servers(&mut app, github, docs);
        app.update(Msg::ToggleMcp(github));
        app.implement_held = true;
        assert_eq!(
            app.update(Msg::Submit("hello".into())),
            vec![Effect::RestoreComposer("hello".into())]
        );
        assert_eq!(app.mcp_next, Some(vec![]));
    }

    #[test]
    fn an_mcp_selection_waits_with_implement_on_the_plan_mode_queue() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (id, github, docs) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut open = chat(id);
        open.mcp_server_ids = vec![github];
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: open,
            messages: serde_json::from_value(json!(crate::question::tests::proposed(1))).unwrap(),
        });
        mcp_servers(&mut app, github, docs);
        app.update(Msg::ToggleMcp(github));
        app.update(Msg::Command(Command::PlanMode(Some(true))));
        assert!(
            app.update(Msg::Command(Command::Implement)).is_empty(),
            "the message would race the PATCH"
        );
        let effects = app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::PlanModeApplied { on: true }),
        });
        let [
            Effect::SendMessage {
                text, turn, seq, ..
            },
        ] = effects.as_slice()
        else {
            panic!("{effects:?}");
        };
        assert_eq!(text, "Implement the plan.");
        assert_eq!(turn.plan_mode, Some(false));
        assert_eq!(turn.mcp_servers, Some(vec![]));
        assert_eq!(
            app.mcp_next,
            Some(vec![]),
            "pending until the server accepts"
        );
        app.update(Msg::ForPlan {
            chat: id,
            generation: turn.plan_generation,
            msg: Box::new(Msg::Sent {
                seq: *seq,
                then: Box::new(Msg::PlanModeApplied { on: false }),
            }),
        });
        assert_eq!(app.mcp_next, None);
    }

    /// Replies to the send `effects` holds as accepted.
    fn accept(app: &mut App, chat: Uuid, effects: &[Effect]) {
        app.update(Msg::ForChat {
            chat,
            msg: Box::new(Msg::Sent {
                seq: seq_of(effects),
                then: Box::new(Msg::Refresh),
            }),
        });
    }

    #[test]
    fn a_toggle_back_while_a_send_is_in_flight_goes_with_the_next_send() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (id, github, docs) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        load_with_github(&mut app, id, github);
        mcp_servers(&mut app, github, docs);
        app.update(Msg::ToggleMcp(github));
        let first = app.update(Msg::Submit("one".into()));
        app.update(Msg::ToggleMcp(github));
        assert_eq!(
            app.mcp_next,
            Some(vec![github]),
            "the send in flight turns GitHub off, so turning it on is a change"
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info(
                "GitHub turns on with your next message.".into()
            ))
        );
        accept(&mut app, id, &first);
        assert_eq!(app.mcp_next, Some(vec![github]));
        let second = app.update(Msg::Submit("two".into()));
        assert!(
            matches!(second.as_slice(), [Effect::SendMessage { turn, .. }] if turn.mcp_servers == Some(vec![github])),
            "{second:?}"
        );
    }

    #[test]
    fn a_refused_mcp_selection_is_dropped_and_says_to_change_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (id, github, docs) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        load_with_github(&mut app, id, github);
        mcp_servers(&mut app, github, docs);
        app.update(Msg::ToggleMcp(github));
        let sent = app.update(Msg::Submit("one".into()));
        let effects = app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::SendFailed {
                text: "one".into(),
                message: "One or more MCP server IDs are invalid or disabled.".into(),
                plan_mode: None,
                seq: seq_of(&sent),
                mcp_rejected: true,
            }),
        });
        assert_eq!(effects, vec![Effect::RestoreComposer("one".into())]);
        assert_eq!(app.mcp_next, None, "a refused selection is not resent");
        let error = last_error(&app);
        assert!(
            error.contains("The MCP server selection was not accepted") && error.contains("/mcp"),
            "{error}"
        );
        let retry = app.update(Msg::Submit("one".into()));
        assert!(
            matches!(retry.as_slice(), [Effect::SendMessage { turn, .. }] if turn.mcp_servers.is_none()),
            "{retry:?}"
        );
    }

    #[test]
    fn out_of_order_replies_apply_only_the_newest_selection() {
        for newest_first in [false, true] {
            let mut app = App::new(BusyBehavior::Queue, true);
            started(&mut app);
            let (id, github, docs) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
            load_with_github(&mut app, id, github);
            mcp_servers(&mut app, github, docs);
            app.update(Msg::ToggleMcp(github));
            let older = app.update(Msg::Submit("off".into()));
            app.update(Msg::ToggleMcp(github));
            let newer = app.update(Msg::Submit("on".into()));
            let order = if newest_first {
                [&newer, &older]
            } else {
                [&older, &newer]
            };
            for effects in order {
                accept(&mut app, id, effects);
                assert_eq!(
                    app.chat.as_ref().unwrap().mcp_server_ids,
                    vec![github],
                    "the older send never shows its selection; newest_first: {newest_first}"
                );
            }
            assert_eq!(app.mcp_next, None, "newest_first: {newest_first}");
        }
    }

    /// The generation of the one `Effect::LoadOlder` in `effects`.
    fn older_generation(effects: &[Effect]) -> u64 {
        match effects {
            [Effect::LoadOlder { generation, .. }] => *generation,
            other => panic!("expected one LoadOlder, got {other:?}"),
        }
    }

    fn older_page(chat: Uuid, generation: u64, messages: Vec<types::CodersdkChatMessage>) -> Msg {
        Msg::ForChat {
            chat,
            msg: Box::new(Msg::OlderLoaded {
                messages,
                has_more: true,
                generation,
            }),
        }
    }

    /// Opens a chat whose newest page is full, so older history may exist.
    fn long_chat(app: &mut App) -> Uuid {
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: (301..=500).map(message).collect(),
        });
        id
    }

    #[test]
    fn reaching_the_top_of_a_long_chat_loads_the_page_before_it_once() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: (301..=500).map(message).collect(),
        });
        let effects = app.update(Msg::LoadOlder);
        let generation = older_generation(&effects);
        assert_eq!(
            effects,
            vec![Effect::LoadOlder {
                chat: id,
                before_id: 301,
                generation
            }]
        );
        assert!(app.update(Msg::LoadOlder).is_empty(), "one page at a time");
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::OlderLoaded {
                messages: (101..=300).map(message).collect(),
                has_more: false,
                generation,
            }),
        });
        assert_eq!(app.transcript.first_message_id(), Some(101));
        assert!(
            app.update(Msg::LoadOlder).is_empty(),
            "the server has nothing older"
        );
        let short = Uuid::new_v4();
        app.update(Msg::OpenChat(short));
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(short),
            messages: vec![message(1)],
        });
        assert!(
            app.update(Msg::LoadOlder).is_empty(),
            "a short chat is already whole"
        );
    }

    #[test]
    fn an_older_page_for_a_chat_left_and_reopened_is_dropped() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let a = long_chat(&mut app);
        let stale = older_generation(&app.update(Msg::LoadOlder));
        let b = Uuid::new_v4();
        app.update(Msg::OpenChat(b));
        app.update(older_page(a, stale, (101..=300).map(message).collect()));
        app.update(Msg::OpenChat(a));
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(a),
            messages: (301..=500).map(message).collect(),
        });
        app.update(older_page(a, stale, (101..=300).map(message).collect()));
        assert_eq!(
            app.transcript.first_message_id(),
            Some(301),
            "the page asked for on the first visit never lands on the second"
        );
        let fresh = older_generation(&app.update(Msg::LoadOlder));
        assert_ne!(fresh, stale);
        app.update(older_page(a, fresh, (101..=300).map(message).collect()));
        assert_eq!(app.transcript.first_message_id(), Some(101));
    }

    #[test]
    fn an_older_page_asked_for_before_a_history_reset_is_dropped() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = long_chat(&mut app);
        let stale = older_generation(&app.update(Msg::LoadOlder));
        app.update(ev(json!({"type": "history_reset"})));
        for i in 301..=500 {
            app.update(ev(
                json!({"type": "message", "message": {"id": i, "role": "user", "content": []}}),
            ));
        }
        app.update(status("waiting"));
        app.update(older_page(id, stale, (101..=300).map(message).collect()));
        assert_eq!(
            app.transcript.first_message_id(),
            Some(301),
            "a page from before the reset could mix two histories"
        );
        let fresh = older_generation(&app.update(Msg::LoadOlder));
        assert_ne!(
            fresh, stale,
            "the reset also ends the wait for the old page"
        );
    }

    #[test]
    fn an_overlapping_older_page_adds_only_what_is_missing() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        let mut newest: Vec<types::CodersdkChatMessage> = (301..=500).map(message).collect();
        newest[0] = serde_json::from_value(
            json!({"id": 301, "role": "user", "content": [{"type": "text", "text": "kept"}]}),
        )
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: newest,
        });
        let generation = older_generation(&app.update(Msg::LoadOlder));
        let mut page: Vec<types::CodersdkChatMessage> = (250..=310).map(message).collect();
        page.push(page[0].clone());
        app.update(older_page(id, generation, page));
        let ids: Vec<i64> = app.transcript.messages().filter_map(|m| m.id).collect();
        assert_eq!(ids, (250..=500).collect::<Vec<_>>());
        let first_newest = app
            .transcript
            .messages()
            .find(|m| m.id == Some(301))
            .unwrap();
        assert_eq!(
            first_newest.content.first().and_then(|p| p.text.as_deref()),
            Some("kept"),
            "a message already loaded keeps its newer copy"
        );
    }

    #[test]
    fn an_older_page_leaves_the_running_tools_and_the_question_alone() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        let mut newest: Vec<serde_json::Value> = (1001..=1198)
            .map(|i| json!({"id": i, "role": "user", "content": []}))
            .collect();
        newest.extend(
            crate::question::tests::asked()
                .as_array()
                .unwrap()
                .iter()
                .zip([1199, 1200])
                .map(|(m, id)| {
                    let mut m = m.clone();
                    m["id"] = json!(id);
                    m
                }),
        );
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: serde_json::from_value(serde_json::Value::Array(newest)).unwrap(),
        });
        let tools = app.transcript.unresolved_tool_calls();
        let menu = app.question_menu();
        assert_eq!(tools, vec!["ask_user_question".to_owned()]);
        assert!(menu.is_some());
        let generation = older_generation(&app.update(Msg::LoadOlder));
        let page = serde_json::from_value(json!([
            {"id": 997, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "old", "tool_name": "propose_plan", "args": {"path": "PLAN.md"}}
            ]},
            {"id": 998, "role": "tool", "content": [
                {"type": "tool-result", "tool_call_id": "q1", "tool_name": "ask_user_question", "result": {"answer": "x"}}
            ]},
            {"id": 999, "role": "user", "content": [{"type": "text", "text": "older"}]}
        ]))
        .unwrap();
        app.update(older_page(id, generation, page));
        assert_eq!(app.transcript.first_message_id(), Some(997));
        assert_eq!(app.transcript.unresolved_tool_calls(), tools);
        assert_eq!(app.question_menu(), menu);
        assert!(!app.plan_ready());
    }

    #[test]
    fn a_failed_older_page_is_reported_and_can_be_asked_for_again() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = long_chat(&mut app);
        let generation = older_generation(&app.update(Msg::LoadOlder));
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::OlderFailed {
                message: "boom".into(),
                generation,
            }),
        });
        assert!(
            matches!(app.notices.last(), Some(Notice::Error(m)) if m == "Could not load older messages: boom"),
            "{:?}",
            app.notices
        );
        older_generation(&app.update(Msg::LoadOlder));
    }

    #[test]
    fn the_history_edge_follows_the_older_pages() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = long_chat(&mut app);
        assert_eq!(app.history_edge(), Some(HistoryEdge::More));
        let generation = older_generation(&app.update(Msg::LoadOlder));
        assert_eq!(app.history_edge(), Some(HistoryEdge::Loading));
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::OlderLoaded {
                messages: (101..=300).map(message).collect(),
                has_more: false,
                generation,
            }),
        });
        assert_eq!(app.history_edge(), Some(HistoryEdge::Start));
        let short = Uuid::new_v4();
        app.update(Msg::OpenChat(short));
        assert_eq!(
            app.history_edge(),
            None,
            "a switch forgets the old chat's pages"
        );
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(short),
            messages: vec![message(1)],
        });
        assert_eq!(
            app.history_edge(),
            None,
            "a chat that fits in one page has no edge to show"
        );
    }

    /// The generation of the one `Effect::RefreshChat` for `chat` in `effects`.
    fn refresh_of(effects: &[Effect], chat: Uuid) -> u64 {
        let found: Vec<u64> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::RefreshChat {
                    chat: c,
                    generation,
                } if *c == chat => Some(*generation),
                _ => None,
            })
            .collect();
        assert_eq!(found.len(), 1, "{effects:?}");
        found[0]
    }

    /// The runtime's reply to the refresh numbered `generation` of `chat`.
    fn refreshed(chat: Uuid, generation: u64, record: Box<types::CodersdkChat>) -> Msg {
        Msg::ForChat {
            chat,
            msg: Box::new(Msg::ForRefresh {
                generation,
                msg: Box::new(Msg::ChatRefreshed(record)),
            }),
        }
    }

    #[test]
    fn a_workspace_the_agent_binds_reaches_workspace_and_git_after_a_refresh() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (id, ws) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        let generation = refresh_of(&app.update(Msg::Command(Command::Info)), id);
        app.update(Msg::InfoClosed);
        let mut bound = chat(id);
        bound.workspace_id = Some(ws);
        app.update(refreshed(id, generation, bound));
        assert_eq!(app.selected_workspace, Some(ws));
        let effects = app.update(Msg::Command(Command::Workspace(None)));
        assert!(
            effects.contains(&Effect::FetchWorkspaceDetails {
                chat: id,
                workspace: ws
            }),
            "{effects:?}"
        );
        app.update(Msg::WorkspaceClosed);
        let effects = app.update(Msg::Command(Command::Git));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::OpenGitWatch { chat, .. } if *chat == id)),
            "{effects:?}"
        );
    }

    #[test]
    fn a_workspace_the_agent_binds_reaches_git_through_the_watch() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (id, ws) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(listed(id, "t", "2026-09-30T10:00:00Z")),
            messages: vec![],
        });
        let mut bound = listed(id, "t", "2026-09-30T10:05:00Z");
        bound.workspace_id = Some(ws);
        app.update(watch("status_change", bound));
        assert_eq!(app.selected_workspace, Some(ws));
        assert_eq!(app.chat.as_ref().and_then(|c| c.workspace_id), Some(ws));
        let mut older = listed(id, "t", "2026-09-30T10:01:00Z");
        older.workspace_id = Some(Uuid::new_v4());
        app.update(watch("status_change", older));
        assert_eq!(
            app.selected_workspace,
            Some(ws),
            "an older event does not undo a newer binding"
        );
        let effects = app.update(Msg::Command(Command::Git));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::OpenGitWatch { chat, .. } if *chat == id)),
            "{effects:?}"
        );
    }

    #[test]
    fn a_refresh_from_before_a_plan_mode_change_never_undoes_it() {
        for stale_first in [true, false] {
            let mut app = App::new(BusyBehavior::Queue, true);
            started(&mut app);
            let id = Uuid::new_v4();
            app.update(Msg::ChatLoaded {
                has_more: None,
                chat: chat(id),
                messages: vec![],
            });
            let stale = refresh_of(&app.update(Msg::Command(Command::Info)), id);
            app.update(Msg::Command(Command::PlanMode(Some(true))));
            let fresh = refresh_of(
                &app.update(Msg::ForChat {
                    chat: id,
                    msg: Box::new(Msg::PlanModeApplied { on: true }),
                }),
                id,
            );
            let replies = [
                refreshed(id, stale, chat_with_plan(id, "")),
                refreshed(id, fresh, chat_with_plan(id, "plan")),
            ];
            let replies: Vec<Msg> = if stale_first {
                replies.into_iter().collect()
            } else {
                replies.into_iter().rev().collect()
            };
            for reply in replies {
                app.update(reply);
                assert!(app.plan_mode, "stale_first: {stale_first}");
            }
        }
    }

    #[test]
    fn a_refresh_from_before_a_failed_plan_mode_change_leaves_the_failure_to_report() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        let stale = refresh_of(&app.update(Msg::Command(Command::Info)), id);
        app.update(Msg::Command(Command::PlanMode(Some(true))));
        let fresh = refresh_of(
            &app.update(Msg::ForChat {
                chat: id,
                msg: Box::new(Msg::PlanModeFailed {
                    on: true,
                    message: "HTTP 500".into(),
                }),
            }),
            id,
        );
        app.update(refreshed(id, stale, chat_with_plan(id, "")));
        assert!(
            !app.notices
                .iter()
                .any(|n| matches!(n, Notice::Info(m) if m.starts_with("Plan mode is"))),
            "{:?}",
            app.notices
        );
        app.update(refreshed(id, fresh, chat_with_plan(id, "")));
        assert!(!app.plan_mode);
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info("Plan mode is off.".into()))
        );
    }

    #[test]
    fn a_refresh_from_before_an_mcp_send_keeps_the_sent_selection() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (id, github, docs) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        load_with_github(&mut app, id, github);
        let stale = refresh_of(&app.update(Msg::Command(Command::Mcp)), id);
        mcp_servers(&mut app, github, docs);
        app.update(Msg::ToggleMcp(github));
        let effects = app.update(Msg::Submit("hi".into()));
        accept(&mut app, id, &effects);
        assert_eq!(
            app.chat.as_ref().map(|c| c.mcp_server_ids.clone()),
            Some(vec![])
        );
        let mut before = chat(id);
        before.mcp_server_ids = vec![github];
        app.update(refreshed(id, stale, before));
        assert_eq!(
            app.chat.as_ref().map(|c| c.mcp_server_ids.clone()),
            Some(vec![]),
            "a snapshot read before the send keeps the selection it carried"
        );
    }

    #[test]
    fn a_failed_send_now_ends_the_wait() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.update(ev(json!({"type": "queue_update", "queued_messages": [
            {"id": 7, "content": [{"type": "text", "text": "then run the tests"}]}
        ]})));
        app.update(Msg::QueueAction(QueueAction::Promote(7)));
        assert_eq!(app.activity(), Some(Activity::Waiting));
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::ApiFailed {
                action: PROMOTE_QUEUED,
                message: "HTTP 404".into(),
            }),
        });
        assert_eq!(app.activity(), None);
    }

    #[test]
    fn plan_mode_waits_while_implement_is_held() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: serde_json::from_value(json!(crate::question::tests::proposed(1))).unwrap(),
        });
        app.update(Msg::Command(Command::PlanMode(Some(true))));
        assert!(app.update(Msg::Command(Command::Implement)).is_empty());
        assert!(
            app.update(Msg::Command(Command::PlanMode(Some(true))))
                .is_empty()
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info(
                "Wait for the plan to start implementing, then set plan mode.".into()
            ))
        );
        assert!(!app.plan_mode);
        let effects = app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::PlanModeApplied { on: true }),
        });
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { text, turn, .. }]
                if text == "Implement the plan." && turn.plan_mode == Some(false)),
            "{effects:?}"
        );
    }

    #[test]
    fn a_failed_implement_says_to_run_it_again_and_leaves_the_composer() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat_with_plan(id, "plan"),
            messages: serde_json::from_value(json!(crate::question::tests::proposed(1))).unwrap(),
        });
        let seq = seq_of(&app.update(Msg::Command(Command::Implement)));
        let effects = app.update(plan_reply(
            id,
            1,
            Msg::SendFailed {
                text: "Implement the plan.".into(),
                message: "HTTP 500".into(),
                plan_mode: Some(false),
                seq,
                mcp_rejected: false,
            },
        ));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::RestoreComposer(_))),
            "{effects:?}"
        );
        assert_eq!(
            last_error(&app),
            "The plan was not sent: HTTP 500. Run /implement again."
        );
    }

    #[test]
    fn switching_chats_says_the_attachments_were_cleared() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages: vec![],
        });
        attach_ready(&mut app, "a.png");
        attach_ready(&mut app, "b.txt");
        app.update(Msg::OpenChat(Uuid::new_v4()));
        assert!(app.chips.is_empty());
        assert!(
            app.notices.contains(&Notice::Info(
                "The attachments were cleared: a.png, b.txt.".into()
            )),
            "{:?}",
            app.notices
        );
    }

    #[test]
    fn switching_chats_says_which_pasted_text_was_dropped() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages: vec![],
        });
        attach_ready(&mut app, "a.png");
        app.update(Msg::AttachPaste {
            name: "pasted-text-2026-10-05-09-30-00.txt".into(),
            text: "the log".into(),
        });
        app.update(Msg::OpenChat(Uuid::new_v4()));
        assert!(app.chips.is_empty());
        assert!(
            app.notices.contains(&Notice::Info(
                "The attachments were cleared: a.png, pasted-text-2026-10-05-09-30-00.txt. \
                 The pasted text in pasted-text-2026-10-05-09-30-00.txt was not sent; paste it again to send it."
                    .into()
            )),
            "{:?}",
            app.notices
        );
    }

    #[test]
    fn an_effort_chosen_after_a_failed_load_is_not_saved() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let id = Uuid::new_v4();
        app.update(Msg::Started {
            org_id: Uuid::new_v4(),
            open_chat: Some(id),
        });
        with_efforts(&mut app);
        app.update(Msg::ChatLoadFailed {
            chat_id: id,
            message: "HTTP 502".into(),
        });
        assert!(app.update(Msg::EffortChosen("high".into())).is_empty());
        assert_eq!(app.selected_effort.as_deref(), Some("high"));
    }

    #[test]
    fn a_plan_mode_failure_from_an_earlier_visit_says_previous_chat() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        for id in [a, b, a] {
            app.update(Msg::OpenChat(id));
            app.update(Msg::ChatLoaded {
                has_more: None,
                chat: chat(id),
                messages: vec![],
            });
            if id == a && app.plan_generation == 0 {
                app.update(Msg::Command(Command::PlanMode(Some(true))));
            }
        }
        app.update(plan_reply(
            a,
            1,
            Msg::PlanModeFailed {
                on: true,
                message: "HTTP 500".into(),
            },
        ));
        assert_eq!(
            last_error(&app),
            "In the previous chat, could not turn plan mode on: HTTP 500"
        );
    }

    #[test]
    fn new_after_a_successful_organization_retry_creates_a_chat() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::OrganizationsFailed {
            message: "HTTP 502".into(),
            open_chat: None,
        });
        assert_eq!(
            app.update(Msg::Command(Command::New)),
            vec![Effect::FetchOrganizations]
        );
        let only = org("Product", true);
        app.update(Msg::OrganizationsLoaded(vec![only.clone()]));
        app.update(Msg::Command(Command::New));
        let effects = app.update(Msg::Submit("hello".into()));
        assert!(
            matches!(effects.as_slice(), [Effect::CreateChat { org, text, .. }]
                if *org == only.id && text == "hello"),
            "{effects:?}"
        );
    }

    #[test]
    fn a_late_failure_for_an_earlier_workspace_is_dropped() {
        let (mut app, _, id, ws) = attached();
        app.update(Msg::Command(Command::Workspace(None)));
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::WorkspaceDetailsFailed {
                workspace: Uuid::new_v4(),
                message: "HTTP 404".into(),
            }),
        });
        assert!(matches!(app.workspace_panel, Some(Fetched::Loading)));
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::WorkspaceDetailsFailed {
                workspace: ws,
                message: "HTTP 502".into(),
            }),
        });
        assert!(matches!(app.workspace_panel, Some(Fetched::Failed(_))));
    }

    #[test]
    fn copy_ssh_with_several_agents_and_none_named_lists_them() {
        let (mut app, _, id, ws) = attached();
        app.update(Msg::Command(Command::Workspace(None)));
        let agent = |name: &str| {
            json!({"name": name, "apps": [], "display_apps": [], "environment_variables": {},
                "latency": {}, "log_sources": [], "metadata": [], "scripts": [], "subsystems": []})
        };
        let details = serde_json::from_value(json!({
            "id": ws, "name": "dev", "owner_name": "nick", "shared_with": [],
            "latest_build": {"resources": [{"agents": [agent("main"), agent("gpu")], "metadata": []}]}
        }))
        .unwrap();
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::WorkspaceDetailsLoaded(Box::new(details))),
        });
        assert!(
            app.update(Msg::WorkspaceAction(WorkspaceAction::CopySsh))
                .is_empty()
        );
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info(
                "This workspace has several agents (main, gpu) and the chat names none. Run coder ssh nick/dev.<agent> with one of them.".into()
            ))
        );
    }

    #[test]
    fn leaving_a_chat_closes_its_git_panel_and_socket() {
        let (mut app, _, _, _) = attached();
        app.update(Msg::Command(Command::Git));
        assert!(app.git_panel.is_some());
        let effects = app.update(Msg::OpenChat(Uuid::new_v4()));
        assert!(effects.contains(&Effect::CloseGitWatch), "{effects:?}");
        assert!(app.git_panel.is_none());
    }

    #[test]
    fn a_git_error_frame_is_a_notice_and_keeps_the_socket() {
        let (mut app, _, id, _) = attached();
        app.update(Msg::Command(Command::Git));
        app.update(Msg::ForGit {
            chat: id,
            generation: 1,
            msg: Box::new(Msg::GitChanges(Box::new(
                serde_json::from_value(json!({"type": "error", "message": "git not found"}))
                    .unwrap(),
            ))),
        });
        assert!(
            matches!(
                app.git_panel.as_ref().map(|p| &p.local),
                Some(LocalGit::Live)
            ),
            "{:?}",
            app.git_panel.as_ref().map(|p| &p.local)
        );
        assert_eq!(
            last_error(&app),
            "The workspace reported a git error: git not found"
        );
    }

    #[test]
    fn after_a_failed_older_page_the_wheel_waits_for_a_deliberate_page_up() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = long_chat(&mut app);
        let generation = older_generation(&app.update(Msg::ScrolledToTop));
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::OlderFailed {
                message: "boom".into(),
                generation,
            }),
        });
        for _ in 0..3 {
            assert!(app.update(Msg::ScrolledToTop).is_empty());
        }
        let errors = app
            .notices
            .iter()
            .filter(|n| matches!(n, Notice::Error(_)))
            .count();
        assert_eq!(errors, 1, "{:?}", app.notices);
        let generation = older_generation(&app.update(Msg::LoadOlder));
        app.update(older_page(
            id,
            generation,
            (101..=300).map(message).collect(),
        ));
        older_generation(&app.update(Msg::ScrolledToTop));
    }

    #[test]
    fn the_servers_has_more_decides_whether_older_history_loads() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let short = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(short),
            messages: (1..=3).map(message).collect(),
            has_more: Some(true),
        });
        assert_eq!(app.history_edge(), Some(HistoryEdge::More));
        older_generation(&app.update(Msg::LoadOlder));

        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let full = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(full),
            messages: (301..=500).map(message).collect(),
            has_more: Some(false),
        });
        assert_eq!(app.history_edge(), None);
        assert!(app.update(Msg::LoadOlder).is_empty());
    }

    #[test]
    fn a_reopened_git_socket_clears_the_rows_before_its_first_frame() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        let mut record = chat(id);
        record.workspace_id = Some(Uuid::new_v4());
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: record,
            messages: vec![],
        });
        let generation = app
            .update(Msg::Command(Command::Git))
            .iter()
            .find_map(|e| match e {
                Effect::OpenGitWatch { generation, .. } => Some(*generation),
                _ => None,
            })
            .expect("/git opens its socket");
        let tagged = |msg: Msg| Msg::ForGit {
            chat: id,
            generation,
            msg: Box::new(msg),
        };
        let changes = |root: &str| {
            Msg::GitChanges(Box::new(
                serde_json::from_value(json!({"type": "changes", "repositories": [
                    {"repo_root": root, "branch": "m2", "unified_diff": "+x\n"}
                ]}))
                .unwrap(),
            ))
        };
        app.update(tagged(changes("/gone")));
        app.update(tagged(Msg::GitReopened));
        app.update(tagged(changes("/kept")));
        let panel = app.git_panel.as_ref().unwrap();
        assert_eq!(
            panel.repos.keys().collect::<Vec<_>>(),
            ["/kept"],
            "a repository deleted during the handoff does not linger"
        );
    }

    #[test]
    fn the_attached_workspace_is_named_once_the_list_names_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let ws = Uuid::new_v4();
        app.update(Msg::WorkspaceChosen(Some(ws)));
        assert_eq!(app.workspace_name(), None, "the list has not loaded");
        app.update(Msg::WorkspacesLoaded(vec![WorkspaceRef {
            id: ws,
            name: "dev".into(),
            ..Default::default()
        }]));
        assert_eq!(app.workspace_name().as_deref(), Some("dev"));
        app.update(Msg::WorkspaceChosen(None));
        assert_eq!(app.workspace_name(), None);
    }

    /// A model list where `old` is disabled and `fresh` is the enabled default.
    fn old_and_fresh(old: Uuid, fresh: Uuid) -> Vec<types::CodersdkChatModel> {
        serde_json::from_value(json!([
            {"id": old, "display_name": "Old", "enabled": false, "reasoning_efforts": []},
            {"id": fresh, "display_name": "Fresh", "enabled": true, "is_default": true, "reasoning_efforts": []}
        ]))
        .unwrap()
    }

    /// Starts a session, loads `old_and_fresh`, and opens a chat whose last model is `Old`.
    /// Returns the chat, `Old`, and `Fresh`.
    fn chat_on_a_disabled_model(app: &mut App) -> (Uuid, Uuid, Uuid) {
        started(app);
        let (old, fresh) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ModelsLoaded(old_and_fresh(old, fresh)));
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(types::CodersdkChat {
                id: Some(id),
                last_model_config_id: Some(old),
                ..Default::default()
            }),
            messages: vec![],
        });
        (id, old, fresh)
    }

    #[test]
    fn a_chat_whose_model_is_gone_warns_once_and_names_it_when_it_can() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (_, old, fresh) = chat_on_a_disabled_model(&mut app);
        assert_eq!(
            app.unavailable_model(),
            Some(UnavailableModel {
                name: Some("Old".into())
            })
        );
        assert_eq!(
            last_error(&app),
            "This chat's model, Old, is not available. Pick another with /model before you send."
        );
        let warned = app.notices.len();
        app.update(Msg::ModelsLoaded(old_and_fresh(old, fresh)));
        assert_eq!(app.notices.len(), warned, "one warning per chat and model");
        // A deleted model is missing from the list, so nothing names it.
        app.update(Msg::ModelsLoaded(old_and_fresh(old, fresh).split_off(1)));
        assert_eq!(
            app.unavailable_model(),
            Some(UnavailableModel { name: None })
        );
        app.update(Msg::ModelChosen(fresh));
        assert_eq!(app.unavailable_model(), None);
    }

    #[test]
    fn a_model_whose_provider_is_unavailable_is_unavailable_once_the_list_loads() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (provider, keyless) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(types::CodersdkChat {
                id: Some(Uuid::new_v4()),
                last_model_config_id: Some(keyless),
                ..Default::default()
            }),
            messages: vec![],
        });
        assert_eq!(
            app.unavailable_model(),
            None,
            "nothing is known before the list loads"
        );
        app.providers = serde_json::from_value(json!([
            {"id": provider, "display_name": "Provider", "available": false}
        ]))
        .unwrap();
        app.update(Msg::ModelsLoaded(
            serde_json::from_value(json!([
                {"id": keyless, "display_name": "Keyless", "ai_provider_id": provider, "enabled": true, "is_default": true, "reasoning_efforts": []}
            ]))
            .unwrap(),
        ));
        assert_eq!(
            app.unavailable_model(),
            Some(UnavailableModel {
                name: Some("Keyless".into())
            })
        );
        assert_eq!(
            last_error(&app),
            "This chat's model, Keyless, is not available. Pick another with /model before you send."
        );
    }

    #[test]
    fn sending_on_an_unavailable_model_holds_the_draft_and_its_chips_for_a_pick() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (id, _, fresh) = chat_on_a_disabled_model(&mut app);
        let file = attach_ready(&mut app, "a.png");
        assert_eq!(
            app.update(Msg::Submit("look".into())),
            vec![Effect::ShowPicker(Picker::Model)],
            "no request goes to the server"
        );
        assert_eq!(app.chips.len(), 1, "the chips wait with the draft");
        assert_eq!(
            last_error(&app),
            "This chat's model, Old, is not available. Pick a model to send your message, or press Esc to keep editing it."
        );
        match app.update(Msg::ModelChosen(fresh)).as_slice() {
            [
                Effect::SendMessage {
                    chat,
                    text,
                    model,
                    turn,
                    ..
                },
            ] => {
                assert_eq!(*chat, id);
                assert_eq!(text, "look");
                assert_eq!(*model, Some(fresh));
                assert_eq!(turn.files, vec![file]);
            }
            other => panic!("expected the held draft to go, got {other:?}"),
        }
        assert!(app.chips.is_empty(), "the chips went with it");
    }

    #[test]
    fn closing_the_picker_gives_the_held_draft_back_with_its_chips() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (_, _, fresh) = chat_on_a_disabled_model(&mut app);
        attach_ready(&mut app, "a.png");
        app.update(Msg::Submit("look".into()));
        assert_eq!(
            app.update(Msg::ModelPickerClosed),
            vec![Effect::RestoreComposer("look".into())]
        );
        assert_eq!(app.chips.len(), 1);
        assert!(
            app.update(Msg::ModelChosen(fresh)).is_empty(),
            "nothing is held any more"
        );
    }

    #[test]
    fn a_send_the_server_refuses_for_its_model_keeps_the_draft_and_its_files_for_a_pick() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let (thinker, plain) = with_efforts(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(types::CodersdkChat {
                id: Some(id),
                last_model_config_id: Some(thinker),
                ..Default::default()
            }),
            messages: vec![],
        });
        assert_eq!(app.unavailable_model(), None, "the list still offers it");
        let file = attach_ready(&mut app, "a.png");
        let seq = seq_of(&app.update(Msg::Submit("look".into())));
        assert!(app.chips.is_empty(), "the chips went with the send");
        // A mention typed while the send was in flight waits for a later send.
        app.update(Msg::AttachMention("/tmp/later.md".into()));
        let effects = app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::ModelUnavailable {
                text: "look".into(),
                files: vec![file],
                message: "Invalid model_config_id: model config not found or disabled.".into(),
                plan_mode: None,
                seq,
            }),
        });
        assert_eq!(
            effects,
            vec![Effect::ShowPicker(Picker::Model), Effect::FetchModels(org)]
        );
        assert_eq!(
            last_error(&app),
            "The server refused this chat's model: Invalid model_config_id: model config not found or disabled. Pick a model to send your message, or press Esc to keep editing it."
        );
        assert_eq!(
            app.chips
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["a.png", "later.md"],
            "the file is back ahead of the held mention"
        );
        assert_eq!(app.chips[0].state, ChipState::Ready(file));
        assert!(app.sent_files.is_empty(), "the refused send is settled");
        let again = match app.update(Msg::ModelChosen(plain)).as_slice() {
            [
                Effect::SendMessage {
                    text,
                    model,
                    turn,
                    seq: again,
                    ..
                },
            ] => {
                assert_eq!(text, "look");
                assert_eq!(*model, Some(plain));
                assert_eq!(turn.files, vec![file], "no second upload");
                *again
            }
            other => panic!("expected the held draft to go, got {other:?}"),
        };
        assert_ne!(again, seq, "the resend is a send of its own");
        assert_eq!(
            app.sent_files.get(&again),
            Some(&vec!["a.png".to_owned()]),
            "a second failure still names the file"
        );
        assert_eq!(
            app.chips
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["later.md"],
            "the mention stays held for its own send"
        );
    }

    #[test]
    fn a_model_refusal_for_the_previous_chat_fails_as_any_send_does() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        with_efforts(&mut app);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(a),
            messages: vec![],
        });
        let file = attach_ready(&mut app, "a.png");
        let seq = seq_of(&app.update(Msg::Submit("look".into())));
        app.update(Msg::OpenChat(b));
        let effects = app.update(Msg::ForChat {
            chat: a,
            msg: Box::new(Msg::ModelUnavailable {
                text: "look".into(),
                files: vec![file],
                message: "Invalid model config ID.".into(),
                plan_mode: None,
                seq,
            }),
        });
        assert_eq!(effects, vec![Effect::RestoreComposer("look".into())]);
        assert_eq!(
            last_error(&app),
            "Could not send the message to the previous chat: Invalid model config ID. These files were not sent: a.png. Attach them again."
        );
    }

    #[test]
    fn a_model_refusal_that_carried_plan_mode_settles_it_and_still_holds_the_draft() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let (_, plain) = with_efforts(&mut app);
        app.update(Msg::Submit("one".into()));
        app.update(Msg::Submit("two".into()));
        app.update(Msg::Command(Command::PlanMode(Some(true))));
        let id = Uuid::new_v4();
        app.update(Msg::ChatCreated(chat(id)));
        let effects = app.update(Msg::ModelUnavailable {
            text: "two".into(),
            files: vec![],
            message: "Invalid model config ID.".into(),
            plan_mode: Some(true),
            seq: 2,
        });
        assert_eq!(
            effects,
            vec![
                Effect::ShowPicker(Picker::Model),
                Effect::FetchModels(org),
                Effect::RefreshChat {
                    chat: id,
                    generation: 1
                }
            ]
        );
        app.update(Msg::ChatRefreshed(chat_with_plan(id, "")));
        assert!(!app.plan_mode, "the refetch says which state holds");
        let resent = app.update(Msg::ModelChosen(plain));
        assert!(
            matches!(resent.as_slice(), [Effect::SendMessage { text, turn, .. }] if text == "two" && turn.plan_mode.is_none()),
            "{resent:?}"
        );
    }

    #[test]
    fn leaving_the_chat_gives_a_held_draft_back() {
        let mut app = App::new(BusyBehavior::Queue, true);
        chat_on_a_disabled_model(&mut app);
        app.update(Msg::Submit("look".into()));
        let effects = app.update(Msg::OpenChat(Uuid::new_v4()));
        assert!(
            effects.contains(&Effect::RestoreComposer("look".into())),
            "{effects:?}"
        );
        assert!(
            app.update(Msg::ModelPickerClosed).is_empty(),
            "nothing is held any more"
        );
    }

    /// The unavailable-model warning for `name`, as a chat open or a list load gives it.
    fn warning_for(name: &str) -> String {
        format!(
            "This chat's model, {name}, is not available. Pick another with /model before you send."
        )
    }

    #[test]
    fn a_second_hold_joins_the_held_draft_and_keeps_both_sets_of_chips() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (old, fresh) = (Uuid::new_v4(), Uuid::new_v4());
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(types::CodersdkChat {
                id: Some(id),
                last_model_config_id: Some(old),
                ..Default::default()
            }),
            messages: vec![],
        });
        // Sent before the list loads, so nothing can hold it here.
        let carried = attach_ready(&mut app, "c.png");
        let seq = seq_of(&app.update(Msg::Submit("C".into())));
        app.update(Msg::ModelsLoaded(old_and_fresh(old, fresh)));
        // A waits on its upload.
        app.update(Msg::Command(Command::Attach("/tmp/a.png".into())));
        let local = app.chips.last().unwrap().local;
        app.update(Msg::Submit("A".into()));
        app.update(Msg::ModelUnavailable {
            text: "C".into(),
            files: vec![carried],
            message: "Invalid model config ID.".into(),
            plan_mode: None,
            seq,
        });
        // The upload finishes, and A is held too.
        let uploaded = Uuid::new_v4();
        app.update(Msg::FileUploaded {
            local,
            file_id: uploaded,
            size: 10,
        });
        assert_eq!(
            app.chips
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["c.png", "a.png"],
            "both drafts' chips wait"
        );
        match app.update(Msg::ModelChosen(fresh)).as_slice() {
            [Effect::SendMessage { text, turn, .. }] => {
                assert_eq!(text, "C\n\nA", "neither draft replaces the other");
                assert_eq!(turn.files, vec![carried, uploaded]);
            }
            other => panic!("expected the joined draft to go, got {other:?}"),
        }
    }

    #[test]
    fn closing_the_picker_after_a_second_hold_gives_both_drafts_back() {
        let mut app = App::new(BusyBehavior::Queue, true);
        chat_on_a_disabled_model(&mut app);
        app.update(Msg::Submit("one".into()));
        app.update(Msg::Submit("two".into()));
        assert_eq!(
            app.update(Msg::ModelPickerClosed),
            vec![Effect::RestoreComposer("one\n\ntwo".into())]
        );
    }

    #[test]
    fn a_new_chat_from_a_chat_on_an_unavailable_model_leaves_the_model_to_the_server() {
        let mut app = App::new(BusyBehavior::Queue, true);
        chat_on_a_disabled_model(&mut app);
        let before = app.notices.len();
        app.update(Msg::Command(Command::New));
        assert_eq!(
            app.model_name().as_deref(),
            Some("Fresh"),
            "the footer names the default"
        );
        assert!(
            app.notices[before..].contains(&Notice::Error(
                "This chat's model, Old, is not available. The server picks the new chat's model."
                    .into()
            )),
            "{:?}",
            &app.notices[before..]
        );
        match app.update(Msg::Submit("hi".into())).as_slice() {
            [Effect::CreateChat { model, .. }] => assert_eq!(
                *model, None,
                "the server applies the default or the user's own override"
            ),
            other => panic!("expected a create, got {other:?}"),
        }
    }

    #[test]
    fn a_new_chat_says_so_when_the_default_models_provider_is_unavailable() {
        let mut app = App::new(BusyBehavior::Queue, true);
        chat_on_a_disabled_model(&mut app);
        let off = Uuid::new_v4();
        app.providers = serde_json::from_value(json!([
            {"id": off, "display_name": "Off", "available": false}
        ]))
        .unwrap();
        app.update(Msg::ModelsLoaded(
            serde_json::from_value(json!([
                {"id": Uuid::new_v4(), "display_name": "Fresh", "ai_provider_id": off, "enabled": true, "is_default": true, "reasoning_efforts": []}
            ]))
            .unwrap(),
        ));
        let before = app.notices.len();
        app.update(Msg::Command(Command::New));
        assert_eq!(
            app.notices[before..].first(),
            Some(&Notice::Error(
                "This chat's model is not available. The default model, Fresh, is not available either. Pick one with /model."
                    .into()
            )),
            "{:?}",
            &app.notices[before..]
        );
    }

    #[test]
    fn a_new_chat_with_no_default_still_names_no_model() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (old, first) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ModelsLoaded(
            serde_json::from_value(json!([
                {"id": old, "display_name": "Old", "enabled": false, "reasoning_efforts": []},
                {"id": first, "display_name": "First", "enabled": true, "reasoning_efforts": []}
            ]))
            .unwrap(),
        ));
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(types::CodersdkChat {
                id: Some(Uuid::new_v4()),
                last_model_config_id: Some(old),
                ..Default::default()
            }),
            messages: vec![],
        });
        app.update(Msg::Command(Command::New));
        match app.update(Msg::Submit("hi".into())).as_slice() {
            [Effect::CreateChat { model, .. }] => assert_eq!(*model, None),
            other => panic!("expected a create, got {other:?}"),
        }
    }

    #[test]
    fn a_refusal_while_a_draft_is_held_joins_it_with_both_files() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (thinker, plain) = with_efforts(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(types::CodersdkChat {
                id: Some(Uuid::new_v4()),
                last_model_config_id: Some(thinker),
                ..Default::default()
            }),
            messages: vec![],
        });
        let refused = |text: &str, files: Vec<Uuid>, seq| Msg::ModelUnavailable {
            text: text.into(),
            files,
            message: "Invalid model config ID.".into(),
            plan_mode: None,
            seq,
        };
        let a = attach_ready(&mut app, "a.png");
        let first = seq_of(&app.update(Msg::Submit("look".into())));
        app.update(refused("look", vec![a], first));
        // Before the refetch lands, the list still offers the model, so the next message
        // goes out, carrying the restored chip with its own.
        let b = attach_ready(&mut app, "b.png");
        let effects = app.update(Msg::Submit("next".into()));
        let second = seq_of(&effects);
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { turn, .. }] if turn.files == vec![a, b]),
            "{effects:?}"
        );
        app.update(refused("next", vec![a, b], second));
        assert_eq!(
            app.chips
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["a.png", "b.png"],
            "both files survive"
        );
        match app.update(Msg::ModelChosen(plain)).as_slice() {
            [Effect::SendMessage { text, turn, .. }] => {
                assert_eq!(text, "look\n\nnext");
                assert_eq!(turn.files, vec![a, b]);
            }
            other => panic!("expected the joined draft to go, got {other:?}"),
        }
    }

    #[test]
    fn reopening_a_chat_on_an_unavailable_model_warns_again() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (a, old, _) = chat_on_a_disabled_model(&mut app);
        let b = Uuid::new_v4();
        app.update(Msg::OpenChat(b));
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(b),
            messages: vec![],
        });
        app.update(Msg::OpenChat(a));
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(types::CodersdkChat {
                id: Some(a),
                last_model_config_id: Some(old),
                ..Default::default()
            }),
            messages: vec![],
        });
        let warnings = app
            .notices
            .iter()
            .filter(|n| **n == Notice::Error(warning_for("Old")))
            .count();
        assert_eq!(warnings, 2, "each open warns once");
    }

    #[test]
    fn picking_a_dimmed_model_for_a_held_draft_holds_it_again() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (on, off) = (Uuid::new_v4(), Uuid::new_v4());
        let (old, fresh, keyless) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        app.providers = serde_json::from_value(json!([
            {"id": on, "display_name": "On", "available": true},
            {"id": off, "display_name": "Off", "available": false}
        ]))
        .unwrap();
        app.update(Msg::ModelsLoaded(
            serde_json::from_value(json!([
                {"id": old, "display_name": "Old", "ai_provider_id": on, "enabled": false, "reasoning_efforts": []},
                {"id": fresh, "display_name": "Fresh", "ai_provider_id": on, "enabled": true, "is_default": true, "reasoning_efforts": []},
                {"id": keyless, "display_name": "Keyless", "ai_provider_id": off, "enabled": true, "reasoning_efforts": []}
            ]))
            .unwrap(),
        ));
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(types::CodersdkChat {
                id: Some(Uuid::new_v4()),
                last_model_config_id: Some(old),
                ..Default::default()
            }),
            messages: vec![],
        });
        app.update(Msg::Submit("look".into()));
        assert_eq!(
            app.update(Msg::ModelChosen(keyless)),
            vec![Effect::ShowPicker(Picker::Model)],
            "nothing goes to the server"
        );
        assert_eq!(
            last_error(&app),
            "This chat's model, Keyless, is not available. Pick a model to send your message, or press Esc to keep editing it."
        );
        let sent = app.update(Msg::ModelChosen(fresh));
        assert!(
            matches!(sent.as_slice(), [Effect::SendMessage { text, model, .. }] if text == "look" && *model == Some(fresh)),
            "{sent:?}"
        );
    }

    #[test]
    fn a_model_refusal_refetches_the_model_list() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let (thinker, _) = with_efforts(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(types::CodersdkChat {
                id: Some(id),
                last_model_config_id: Some(thinker),
                ..Default::default()
            }),
            messages: vec![],
        });
        let seq = seq_of(&app.update(Msg::Submit("look".into())));
        let effects = app.update(Msg::ModelUnavailable {
            text: "look".into(),
            files: vec![],
            message: "Invalid model_config_id: model config not found or disabled.".into(),
            plan_mode: None,
            seq,
        });
        assert!(effects.contains(&Effect::FetchModels(org)), "{effects:?}");
        // The list catches up, so the table and the footer stop offering the model.
        app.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::ModelsLoaded(
                serde_json::from_value(json!([
                    {"id": thinker, "display_name": "Thinker", "enabled": false, "reasoning_efforts": []}
                ]))
                .unwrap(),
            )),
        });
        assert_eq!(
            app.unavailable_model(),
            Some(UnavailableModel {
                name: Some("Thinker".into())
            })
        );
    }

    #[test]
    fn implement_on_an_unavailable_model_asks_for_a_pick_first() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (old, fresh) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ModelsLoaded(old_and_fresh(old, fresh)));
        let id = Uuid::new_v4();
        let mut planned = chat_with_plan(id, "plan");
        planned.last_model_config_id = Some(old);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: planned,
            messages: serde_json::from_value(json!(crate::question::tests::proposed(1))).unwrap(),
        });
        let plan_mode = app.plan_mode;
        assert_eq!(
            app.update(Msg::Command(Command::Implement)),
            vec![Effect::ShowPicker(Picker::Model)],
            "nothing goes to the server"
        );
        assert_eq!(
            last_error(&app),
            "This chat's model, Old, is not available. Pick another with /model, then run /implement again."
        );
        assert_eq!(app.plan_mode, plan_mode, "plan mode is left alone");
    }

    fn spend_status(spent: i64, limit: Option<i64>) -> Box<types::CodersdkUserAiSpendStatus> {
        let budget = limit.map(|l| json!({"spend_limit_micros": l, "limit_source": "group"}));
        Box::new(
            serde_json::from_value(
                json!({"current_spend_micros": spent, "effective_budget": budget}),
            )
            .unwrap(),
        )
    }

    fn quota_of(used: i64, budget: i64) -> types::CodersdkWorkspaceQuota {
        serde_json::from_value(json!({"credits_consumed": used, "budget": budget})).unwrap()
    }

    #[test]
    fn limits_refresh_together_and_only_the_latest_reply_applies() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        assert_eq!(
            app.update(Msg::RefreshLimits),
            vec![
                Effect::FetchSpend { generation: 1 },
                Effect::FetchQuota { org, generation: 1 }
            ]
        );
        assert_eq!(
            app.update(Msg::RefreshLimits),
            vec![
                Effect::FetchSpend { generation: 2 },
                Effect::FetchQuota { org, generation: 2 }
            ]
        );
        app.update(Msg::SpendLoaded {
            spend: spend_status(1, Some(10)),
            generation: 1,
        });
        assert!(
            matches!(app.spend(), LimitState::Unknown),
            "a reply to an earlier refresh is dropped"
        );
        app.update(Msg::SpendLoaded {
            spend: spend_status(1_200_000, Some(50_000_000)),
            generation: 2,
        });
        assert_eq!(
            app.spend().loaded().and_then(|s| s.current_spend_micros),
            Some(1_200_000)
        );
        app.update(Msg::QuotaLoaded {
            org,
            quota: quota_of(3, 10),
            generation: 2,
        });
        assert_eq!(app.quota().loaded().and_then(|q| q.budget), Some(10));
        app.update(Msg::LimitFailed {
            limit: Limit::Spend,
            refusal: Refusal::Failed("HTTP 502".into()),
            generation: 2,
        });
        assert!(
            app.spend().loaded().is_some(),
            "a failed refresh keeps what loaded"
        );
    }

    #[test]
    fn a_missing_or_unlicensed_limit_is_not_asked_for_again() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::RefreshLimits);
        app.update(Msg::LimitFailed {
            limit: Limit::Spend,
            refusal: Refusal::Unlicensed("AI Gateway is a Premium feature. Contact sales!".into()),
            generation: 1,
        });
        app.update(Msg::LimitFailed {
            limit: Limit::Quota,
            refusal: Refusal::Absent,
            generation: 1,
        });
        assert!(matches!(app.spend(), LimitState::Refused(m) if m.starts_with("AI Gateway")));
        assert!(matches!(app.quota(), LimitState::Absent));
        assert!(app.update(Msg::RefreshLimits).is_empty());
        assert!(app.notices.is_empty(), "neither refusal is a notice");
    }

    #[test]
    fn a_rejected_token_stops_the_limits_with_one_notice() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages: vec![],
        });
        app.cost_in_footer = true;
        app.update(Msg::RefreshLimits);
        for limit in [Limit::Spend, Limit::Quota] {
            app.update(Msg::LimitFailed {
                limit,
                refusal: Refusal::Unauthorized,
                generation: 1,
            });
        }
        let stopped = app
            .notices
            .iter()
            .filter(|n| matches!(n, Notice::Error(m) if m == LIMITS_STOPPED))
            .count();
        assert_eq!(stopped, 1, "{:?}", app.notices);
        assert!(app.update(Msg::RefreshLimits).is_empty(), "refreshes stop");
        assert!(
            app.update(Msg::RefreshCost).is_empty(),
            "the cost stops refreshing too"
        );
        assert!(matches!(app.spend(), LimitState::Failed(m) if m == crate::usage::UNAUTHORIZED));
    }

    #[test]
    fn a_rejected_token_on_the_cost_stops_refreshes_where_both_limits_are_refused() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let id = open_with_cost_in_footer(&mut app);
        app.update(Msg::RefreshLimits);
        for limit in [Limit::Spend, Limit::Quota] {
            app.update(Msg::LimitFailed {
                limit,
                refusal: Refusal::Absent,
                generation: 1,
            });
        }
        assert!(app.update(Msg::RefreshLimits).is_empty());
        app.update(Msg::RefreshCost);
        for _ in 0..2 {
            cost_reply(&mut app, id, Msg::CostUnauthorized { generation: 1 });
        }
        assert!(app.limits_stopped());
        let stopped = app
            .notices
            .iter()
            .filter(|n| matches!(n, Notice::Error(m) if m == LIMITS_STOPPED))
            .count();
        assert_eq!(stopped, 1, "{:?}", app.notices);
        assert!(app.update(Msg::RefreshCost).is_empty(), "the cost stops");
        assert!(app.update(Msg::RefreshLimits).is_empty());
        assert!(
            app.update(watch(
                "status_change",
                listed(id, "t", "2026-09-30T10:00:00Z")
            ))
            .is_empty(),
            "the watch asks for no cost either"
        );
        assert!(
            matches!(&app.chat_cost, Some(CostState::Failed(m)) if m == crate::usage::UNAUTHORIZED)
        );
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        let lines = panels::usage_lines(&app, 0, utc);
        assert_eq!(lines.first().map(|(l, _)| *l), Some("Notice"));
        assert!(
            lines.contains(&("Chat cost", "unavailable".into())),
            "{lines:?}"
        );
    }

    #[test]
    fn the_quota_follows_a_loading_chats_organization_once_the_list_names_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let home = started(&mut app);
        let (known, unknown, other) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats: vec![types::CodersdkChat {
                organization_id: Some(other),
                ..listed(known, "elsewhere", "2026-09-30T10:00:00Z")
            }],
        });
        app.update(Msg::OpenChat(known));
        assert!(app.chat.is_none(), "still loading");
        assert_eq!(app.current_org(), Some(other));
        assert_eq!(
            app.update(Msg::RefreshLimits),
            vec![
                Effect::FetchSpend { generation: 1 },
                Effect::FetchQuota {
                    org: other,
                    generation: 1
                }
            ],
            "no quota is asked for the organization the chat is not in"
        );
        app.update(Msg::OpenChat(unknown));
        assert_eq!(
            app.current_org(),
            Some(home),
            "a chat the list does not hold falls back until it loads"
        );
    }

    #[test]
    fn usage_opens_and_refreshes_the_limits_and_the_chat_cost() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Usage)),
            vec![
                Effect::ShowUsage,
                Effect::FetchSpend { generation: 1 },
                Effect::FetchQuota { org, generation: 1 },
                Effect::FetchCost {
                    chat: id,
                    generation: 1
                },
            ]
        );
        assert!(app.usage_open);
        assert!(matches!(app.chat_cost, Some(CostState::Loading)));
        assert_eq!(
            app.update(Msg::RefreshCost),
            vec![Effect::FetchCost {
                chat: id,
                generation: 2
            }],
            "the cost refreshes after a turn while /usage is open"
        );
        app.update(Msg::UsageClosed);
        assert!(!app.usage_open);
        assert!(app.update(Msg::RefreshCost).is_empty());
    }

    #[test]
    fn usage_after_a_rejected_token_asks_for_nothing() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages: vec![],
        });
        assert!(!app.limits_stopped());
        app.update(Msg::RefreshLimits);
        app.update(Msg::LimitFailed {
            limit: Limit::Spend,
            refusal: Refusal::Unauthorized,
            generation: 1,
        });
        assert!(app.limits_stopped());
        assert_eq!(
            app.update(Msg::Command(Command::Usage)),
            vec![Effect::ShowUsage],
            "the same token would fail the cost request too"
        );
        assert!(app.usage_open);
        assert!(app.update(Msg::RefreshCost).is_empty());
    }

    #[test]
    fn a_quota_reply_for_an_organization_left_behind_is_never_shown() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (product, coder) = two_orgs(&mut app);
        assert!(
            app.update(Msg::RefreshLimits)
                .contains(&Effect::FetchQuota {
                    org: coder.id,
                    generation: 1
                })
        );
        app.update(Msg::OrganizationChosen(product.id));
        app.update(Msg::QuotaLoaded {
            org: coder.id,
            quota: quota_of(3, 10),
            generation: 1,
        });
        assert!(
            matches!(app.quota(), LimitState::Unknown),
            "Coder's credits are not Product's"
        );
        assert!(
            app.update(Msg::RefreshLimits)
                .contains(&Effect::FetchQuota {
                    org: product.id,
                    generation: 2
                })
        );
        assert!(matches!(app.quota(), LimitState::Unknown));
        app.update(Msg::QuotaLoaded {
            org: product.id,
            quota: quota_of(1, 5),
            generation: 2,
        });
        assert_eq!(app.quota().loaded().and_then(|q| q.budget), Some(5));
    }

    #[test]
    fn a_turn_ends_when_the_status_leaves_running() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(Uuid::new_v4()),
            messages: vec![],
        });
        app.update(status("waiting"));
        assert_eq!(app.turns_ended(), 0, "an idle chat ran no turn");
        app.update(status("running"));
        app.update(status("running"));
        assert_eq!(app.turns_ended(), 0);
        app.update(status("waiting"));
        assert_eq!(app.turns_ended(), 1);
        app.update(status("waiting"));
        assert_eq!(app.turns_ended(), 1, "a second idle status is no new turn");
        app.update(status("running"));
        app.update(status("error"));
        assert_eq!(app.turns_ended(), 2, "a failed turn ended too");
    }

    #[test]
    fn the_chat_cost_is_fetched_only_while_the_footer_or_usage_shows_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        assert!(
            app.update(Msg::RefreshCost).is_empty(),
            "nothing shows the cost"
        );
        app.cost_in_footer = true;
        assert_eq!(
            app.update(Msg::RefreshCost),
            vec![Effect::FetchCost {
                chat: id,
                generation: 1
            }]
        );
        assert!(matches!(app.chat_cost, Some(CostState::Loading)));
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::CostLoaded {
                cost: serde_json::from_value(
                    json!({"total_cost_micros": 420000, "request_count": 2}),
                )
                .unwrap(),
                generation: 1,
            }),
        });
        assert!(
            matches!(&app.chat_cost, Some(CostState::Loaded(c)) if c.total_cost_micros == Some(420000))
        );
        assert!(app.info_panel.is_none(), "/info stays closed");
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::CostHidden { generation: 1 }),
        });
        assert!(matches!(app.chat_cost, Some(CostState::Hidden)));
        assert!(
            app.update(Msg::RefreshCost).is_empty(),
            "a refused cost is not asked for again"
        );
        app.update(Msg::Command(Command::New));
        assert!(app.chat_cost.is_none(), "another chat has its own cost");
        app.cost_in_footer = false;
        app.usage_open = true;
        app.update(Msg::UsageClosed);
        assert!(!app.usage_open);
    }

    fn total_cost(micros: i64) -> types::CodersdkChatCost {
        serde_json::from_value(json!({"total_cost_micros": micros, "request_count": 1})).unwrap()
    }

    fn cost_reply(app: &mut App, chat: Uuid, msg: Msg) {
        app.update(Msg::ForChat {
            chat,
            msg: Box::new(msg),
        });
    }

    fn info_total(app: &App) -> Option<i64> {
        match &app.info_panel {
            Some(CostState::Loaded(c)) => c.total_cost_micros,
            _ => None,
        }
    }

    fn footer_total(app: &App) -> Option<i64> {
        match &app.chat_cost {
            Some(CostState::Loaded(c)) => c.total_cost_micros,
            _ => None,
        }
    }

    /// Opens a chat with `cost` in the footer, as the UI does from `statusline.fields`.
    fn open_with_cost_in_footer(app: &mut App) -> Uuid {
        started(app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: chat(id),
            messages: vec![],
        });
        app.cost_in_footer = true;
        id
    }

    #[test]
    fn the_watch_refetches_the_cost_while_the_footer_or_usage_shows_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let id = open_with_cost_in_footer(&mut app);
        let turn = || watch("status_change", listed(id, "t", "2026-09-30T10:00:00Z"));
        assert_eq!(
            app.update(turn()),
            vec![Effect::FetchCost {
                chat: id,
                generation: 1
            }],
            "the footer shows the cost"
        );
        app.cost_in_footer = false;
        app.usage_open = true;
        assert_eq!(
            app.update(turn()),
            vec![Effect::FetchCost {
                chat: id,
                generation: 2
            }],
            "/usage shows the cost"
        );
        app.usage_open = false;
        assert!(app.update(turn()).is_empty(), "nothing shows the cost");
    }

    #[test]
    fn the_watch_never_asks_again_for_a_refused_cost() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let id = open_with_cost_in_footer(&mut app);
        app.update(Msg::RefreshCost);
        cost_reply(&mut app, id, Msg::CostHidden { generation: 1 });
        assert!(
            app.update(watch(
                "status_change",
                listed(id, "t", "2026-09-30T10:00:00Z")
            ))
            .is_empty()
        );
    }

    #[test]
    fn the_watch_stops_refetching_the_cost_after_a_rejected_token() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let id = open_with_cost_in_footer(&mut app);
        app.update(Msg::RefreshLimits);
        app.update(Msg::LimitFailed {
            limit: Limit::Spend,
            refusal: Refusal::Unauthorized,
            generation: 1,
        });
        assert!(
            app.update(watch(
                "status_change",
                listed(id, "t", "2026-09-30T10:00:00Z")
            ))
            .is_empty()
        );
    }

    #[test]
    fn info_opens_on_the_total_the_footer_shows_and_keeps_it_when_the_refetch_fails() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let id = open_with_cost_in_footer(&mut app);
        app.update(Msg::RefreshCost);
        cost_reply(
            &mut app,
            id,
            Msg::CostLoaded {
                cost: total_cost(7),
                generation: 1,
            },
        );
        app.update(Msg::Command(Command::Info));
        assert_eq!(info_total(&app), Some(7), "/info opens on the known total");
        cost_reply(
            &mut app,
            id,
            Msg::CostFailed {
                message: "HTTP 502".into(),
                generation: 2,
            },
        );
        assert_eq!(info_total(&app), Some(7), "/info agrees with the footer");
        assert_eq!(footer_total(&app), Some(7));
    }

    #[test]
    fn a_footer_cost_reply_fills_an_open_info() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let id = open_with_cost_in_footer(&mut app);
        app.update(Msg::Command(Command::Info));
        app.update(Msg::RefreshCost);
        cost_reply(
            &mut app,
            id,
            Msg::CostLoaded {
                cost: total_cost(3),
                generation: 1,
            },
        );
        assert!(
            matches!(app.info_panel, Some(CostState::Loading)),
            "/info's reply was superseded"
        );
        cost_reply(
            &mut app,
            id,
            Msg::CostLoaded {
                cost: total_cost(5),
                generation: 2,
            },
        );
        assert_eq!(info_total(&app), Some(5));
        assert_eq!(footer_total(&app), Some(5));
    }

    #[test]
    fn info_supersedes_a_footer_cost_fetch_in_flight() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let id = open_with_cost_in_footer(&mut app);
        app.update(Msg::RefreshCost);
        assert!(
            app.update(Msg::Command(Command::Info))
                .contains(&Effect::FetchCost {
                    chat: id,
                    generation: 2
                })
        );
        cost_reply(
            &mut app,
            id,
            Msg::CostLoaded {
                cost: total_cost(3),
                generation: 1,
            },
        );
        assert!(matches!(app.chat_cost, Some(CostState::Loading)));
        assert!(matches!(app.info_panel, Some(CostState::Loading)));
        cost_reply(
            &mut app,
            id,
            Msg::CostLoaded {
                cost: total_cost(5),
                generation: 2,
            },
        );
        assert_eq!(info_total(&app), Some(5));
        assert_eq!(footer_total(&app), Some(5));
    }

    #[test]
    fn the_mcp_count_on_a_blank_chat_follows_the_first_message() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        assert_eq!(
            app.mcp_on_count(),
            None,
            "unknown on a blank chat until the list loads"
        );
        let (_, _, jira) = org_mcp(&mut app, org);
        assert_eq!(
            app.mcp_on_count(),
            Some(2),
            "GitHub by default and Docs, required"
        );
        app.update(Msg::Command(Command::Mcp));
        app.update(Msg::ToggleMcp(jira));
        assert_eq!(app.mcp_on_count(), Some(3), "a pending /mcp change counts");
    }

    #[test]
    fn the_mcp_count_waits_for_the_list_even_with_a_pending_change() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.mcp_next = Some(vec![Uuid::new_v4()]);
        assert_eq!(
            app.mcp_on_count(),
            None,
            "the required servers are not known yet"
        );
    }

    /// Opens a chat with `github` selected, one inline server, and one workspace server.
    fn open_mcp_chat(app: &mut App, github: Uuid) {
        let open = serde_json::from_value(json!({
            "id": Uuid::new_v4(), "mcp_server_ids": [github],
            "inline_mcp_servers": [{"slug": "local", "url": "http://localhost:9000", "tool_allow_list": [], "tool_deny_list": []}],
            "context": {"resources": [{"kind": "mcp_server", "source": "playwright", "tools": []}]},
            "children": [], "files": [], "labels": {}
        }))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: open,
            messages: vec![],
        });
    }

    #[test]
    fn the_mcp_count_on_an_open_chat_adds_required_inline_and_workspace_servers() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let (github, docs, _) = org_mcp(&mut app, org);
        open_mcp_chat(&mut app, github);
        assert_eq!(
            app.mcp_on_count(),
            Some(4),
            "the chat's GitHub, the required Docs, the inline server, and the workspace's"
        );
        assert!(
            !app.mcp_selection().contains(&docs),
            "Docs comes from the list, not the chat"
        );
    }

    #[test]
    fn the_mcp_count_on_an_open_chat_waits_for_the_list() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        open_mcp_chat(&mut app, Uuid::new_v4());
        assert_eq!(
            app.mcp_on_count(),
            None,
            "the required servers are not known yet"
        );
        app.org_mcp = Fetched::Failed("boom".into());
        assert_eq!(app.mcp_on_count(), None, "nor after the list fails");
    }

    #[test]
    fn the_mcp_count_skips_disabled_and_missing_servers() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let (github, gone, off) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::OrgMcpLoaded(
                serde_json::from_value(json!([
                    {"id": github, "display_name": "GitHub", "availability": "default_off", "enabled": true, "tool_allow_list": [], "tool_deny_list": []},
                    {"id": off, "display_name": "Off", "availability": "force_on", "enabled": false, "tool_allow_list": [], "tool_deny_list": []}
                ]))
                .unwrap(),
            )),
        });
        let mut open = chat(Uuid::new_v4());
        open.mcp_server_ids = vec![github, gone, off];
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: open,
            messages: vec![],
        });
        assert_eq!(
            app.mcp_on_count(),
            Some(1),
            "only GitHub shows as on in /mcp"
        );
    }

    #[test]
    fn the_mcp_count_hides_after_a_failed_load_and_an_organization_switch() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (org, missing) = (Uuid::new_v4(), Uuid::new_v4());
        app.update(Msg::Started {
            org_id: org,
            open_chat: Some(missing),
        });
        org_mcp(&mut app, org);
        app.update(Msg::ChatLoadFailed {
            chat_id: missing,
            message: "gone".into(),
        });
        assert_eq!(
            app.mcp_on_count(),
            None,
            "the retried chat's selection is unknown"
        );
        let mut app = App::new(BusyBehavior::Queue, true);
        let (product, coder) = two_orgs(&mut app);
        org_mcp(&mut app, coder.id);
        assert_eq!(app.mcp_on_count(), Some(2));
        app.update(Msg::OrganizationChosen(product.id));
        assert_eq!(
            app.mcp_on_count(),
            None,
            "hidden until the new organization's list loads"
        );
    }

    /// Opens a chat whose agent attached `name`, of `media_type`, in message 4, after the
    /// user's message 3. Files save to `/h/Downloads` under the home `/h`.
    fn chat_with_a_file(app: &mut App, name: &str, media_type: &str) -> (Uuid, Uuid) {
        started(app);
        let (id, file) = (Uuid::new_v4(), Uuid::new_v4());
        let chat = serde_json::from_value(json!({"id": id, "title": "t", "children": [],
            "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {},
            "files": [{"id": file, "name": name, "mime_type": media_type, "size_bytes": 6144,
                "created_at": "2026-10-05T12:00:02Z"}]}))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: serde_json::from_value(json!([
                {"id": 3, "role": "user", "created_at": "2026-10-05T12:00:00Z",
                    "content": [{"type": "text", "text": "bundle the logs"}]},
                {"id": 4, "role": "assistant", "created_at": "2026-10-05T12:00:02Z",
                    "content": [{"type": "file", "file_id": file, "media_type": media_type,
                        "name": name, "file_name": ""}]}
            ]))
            .unwrap(),
        });
        app.home = Some(PathBuf::from("/h"));
        app.save_dir = PathBuf::from("/h/Downloads");
        (id, file)
    }

    fn last_notice(app: &App) -> Notice {
        app.notices.last().cloned().expect("a notice")
    }

    #[test]
    fn files_needs_a_chat_and_refreshes_it_when_it_opens() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert!(app.update(Msg::Command(Command::Files)).is_empty());
        assert_eq!(
            last_notice(&app),
            Notice::Error("Start a chat first.".into())
        );
        let (id, _) = chat_with_a_file(&mut app, "build-logs.zip", "application/zip");
        let effects = app.update(Msg::Command(Command::Files));
        assert!(matches!(
            effects.as_slice(),
            [Effect::ShowFiles, Effect::RefreshChat { chat, .. }] if *chat == id
        ));
    }

    #[test]
    fn enter_saves_under_a_safe_name_and_a_second_press_waits() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (_, file) = chat_with_a_file(&mut app, "../../.ssh/authorized_keys", "text/plain");
        assert_eq!(
            app.update(Msg::FileAction(FileAction::Save(file))),
            vec![Effect::SaveFile {
                file,
                name: "authorized_keys.txt".into(),
                to: SaveTo::Dir(PathBuf::from("/h/Downloads")),
                conflict: OnConflict::Ask,
            }]
        );
        assert!(
            app.update(Msg::FileAction(FileAction::Save(file)))
                .is_empty()
        );
        assert_eq!(
            last_notice(&app),
            Notice::Info("Still working on ../../.ssh/authorized_keys.".into())
        );
        app.over_ssh = true;
        app.update(Msg::FileSaved {
            file,
            path: PathBuf::from("/h/Downloads/authorized_keys.txt"),
        });
        assert_eq!(
            last_notice(&app),
            Notice::Info(
                "Saved ~/Downloads/authorized_keys.txt on the machine scuttle runs on.".into()
            ),
            "over SSH, the file is on the other machine"
        );
        app.over_ssh = false;
        assert_eq!(app.update(Msg::FileAction(FileAction::Save(file))).len(), 1);
        app.update(Msg::FileSaved {
            file,
            path: PathBuf::from("/h/Downloads/authorized_keys (1).txt"),
        });
        assert_eq!(
            last_notice(&app),
            Notice::Info("Saved ~/Downloads/authorized_keys (1).txt.".into())
        );
    }

    #[test]
    fn a_taken_name_asks_and_each_answer_acts_once() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (_, file) = chat_with_a_file(&mut app, "build-logs.zip", "application/zip");
        let path = PathBuf::from("/h/Downloads/build-logs.zip");
        let taken = |app: &mut App| {
            app.update(Msg::FileConflict {
                file,
                name: "build-logs.zip".into(),
                path: path.clone(),
            })
        };
        let resave = |conflict| {
            vec![Effect::SaveFile {
                file,
                name: "build-logs.zip".into(),
                to: SaveTo::File(path.clone()),
                conflict,
            }]
        };
        assert!(taken(&mut app).is_empty());
        assert!(app.save_conflict.is_some());
        assert_eq!(
            app.update(Msg::ConflictAnswer(ConflictChoice::KeepBoth)),
            resave(OnConflict::KeepBoth)
        );
        assert!(app.save_conflict.is_none());
        assert!(
            app.update(Msg::ConflictAnswer(ConflictChoice::KeepBoth))
                .is_empty(),
            "a second answer finds no question"
        );
        app.update(Msg::FileFailed {
            file,
            message: "x".into(),
        });
        taken(&mut app);
        assert_eq!(
            app.update(Msg::ConflictAnswer(ConflictChoice::Replace)),
            resave(OnConflict::Replace)
        );
        app.update(Msg::FileFailed {
            file,
            message: "x".into(),
        });
        taken(&mut app);
        assert!(
            app.update(Msg::ConflictAnswer(ConflictChoice::Cancel))
                .is_empty()
        );
        assert!(app.save_conflict.is_none());
        assert!(app.files_busy.is_empty());
    }

    #[test]
    fn save_as_starts_in_the_save_dir_and_expands_a_tilde() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (_, file) = chat_with_a_file(&mut app, "build-logs.zip", "application/zip");
        assert!(
            app.update(Msg::FileAction(FileAction::SaveAs(file)))
                .is_empty()
        );
        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.target, EditTarget::SaveAs(file));
        assert_eq!(editor.line.text(), "~/Downloads/build-logs.zip");
        app.editor.as_mut().unwrap().line = LineEdit::new("   ");
        assert!(app.update(Msg::Edit(Edit::Submit)).is_empty());
        assert_eq!(
            last_notice(&app),
            Notice::Error("Type where to save the file, or press Esc.".into())
        );
        app.editor.as_mut().unwrap().line = LineEdit::new("~/keep/logs.zip");
        assert_eq!(
            app.update(Msg::Edit(Edit::Submit)),
            vec![Effect::SaveFile {
                file,
                name: "build-logs.zip".into(),
                to: SaveTo::Typed(PathBuf::from("/h/keep/logs.zip")),
                conflict: OnConflict::Ask,
            }]
        );
        assert!(app.editor.is_none());
    }

    #[test]
    fn view_pages_text_without_control_characters_and_refuses_other_types() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (_, file) = chat_with_a_file(&mut app, "notes.txt", "text/plain");
        assert_eq!(
            app.update(Msg::FileAction(FileAction::View(file))),
            vec![Effect::ReadFile {
                file,
                name: "notes.txt".into()
            }]
        );
        assert_eq!(
            app.update(Msg::FileText {
                file,
                text: "ok\u{1b}]52;c;eA==\u{7}\nline".into()
            }),
            vec![Effect::Page("ok]52;c;eA==\nline".into())]
        );
        assert!(app.files_busy.is_empty());
        let mut zip = App::new(BusyBehavior::Queue, true);
        let (_, file) = chat_with_a_file(&mut zip, "build-logs.zip", "application/zip");
        assert!(
            zip.update(Msg::FileAction(FileAction::View(file)))
                .is_empty()
        );
        assert_eq!(
            last_notice(&zip),
            Notice::Info(
                "build-logs.zip is not text, so it cannot be shown here. Press Enter to save it."
                    .into()
            )
        );
    }

    #[test]
    fn an_expired_file_is_refused_with_why() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (id, _) = chat_with_a_file(&mut app, "new.zip", "application/zip");
        let gone = Uuid::new_v4();
        app.update(Msg::ForStream {
            chat: id,
            generation: app.stream_generation(),
            msg: Box::new(ev(json!({"type": "message", "message": {
                "id": 2, "role": "user", "created_at": "2026-10-05T10:00:00Z",
                "content": [{"type": "file", "file_id": gone, "media_type": "text/plain",
                    "name": "old.txt"}]}}))),
        });
        assert!(
            app.update(Msg::FileAction(FileAction::Save(gone)))
                .is_empty()
        );
        assert!(
            matches!(last_notice(&app), Notice::Error(t) if t.starts_with("old.txt is no longer available")),
            "{:?}",
            last_notice(&app)
        );
    }

    #[test]
    fn jump_pages_older_history_until_the_message_loads_or_says_it_is_gone() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (_, file) = chat_with_a_file(&mut app, "build-logs.zip", "application/zip");
        assert_eq!(
            app.update(Msg::FileAction(FileAction::Jump(file))),
            vec![Effect::ScrollToMessage(4)]
        );
        let mut long = App::new(BusyBehavior::Queue, true);
        started(&mut long);
        let (id, file) = (Uuid::new_v4(), Uuid::new_v4());
        let mut record = chat(id);
        record.files = serde_json::from_value(json!([{"id": file, "name": "a.txt",
            "mime_type": "text/plain", "size_bytes": 1, "created_at": "2026-10-05T09:00:00Z"}]))
        .unwrap();
        long.update(Msg::ChatLoaded {
            has_more: Some(true),
            chat: record,
            messages: vec![message(300)],
        });
        let effects = long.update(Msg::FileAction(FileAction::Jump(file)));
        let generation = older_generation(&effects);
        assert_eq!(
            last_notice(&long),
            Notice::Info("Loading older messages to find it.".into())
        );
        let carrier: types::CodersdkChatMessage = serde_json::from_value(json!({"id": 5,
            "role": "user", "content": [{"type": "file", "file_id": file,
                "media_type": "text/plain", "name": "a.txt"}]}))
        .unwrap();
        let effects = long.update(older_page(id, generation, vec![message(200)]));
        let generation = older_generation(&effects);
        assert_eq!(
            long.update(older_page(id, generation, vec![carrier])),
            vec![Effect::ScrollToMessage(5)]
        );
        let other = Uuid::new_v4();
        long.chat.as_mut().unwrap().files = serde_json::from_value(json!([{"id": other,
            "name": "b.txt", "mime_type": "text/plain", "size_bytes": 1}]))
        .unwrap();
        let effects = long.update(Msg::FileAction(FileAction::Jump(other)));
        let generation = older_generation(&effects);
        assert!(
            long.update(Msg::ForChat {
                chat: id,
                msg: Box::new(Msg::OlderLoaded {
                    messages: vec![message(1)],
                    has_more: false,
                    generation,
                }),
            })
            .is_empty()
        );
        assert_eq!(
            last_notice(&long),
            Notice::Info("Its message is not in this chat's history any more.".into())
        );
    }

    #[test]
    fn jump_to_a_file_in_the_live_turn_scrolls_to_the_end() {
        let mut app = App::new(BusyBehavior::Queue, true);
        chat_with_a_file(&mut app, "build-logs.zip", "application/zip");
        app.history_more = true;
        let live = Uuid::new_v4();
        app.transcript
            .live
            .blocks
            .push(crate::live::LiveBlock::File {
                file_id: Some(live),
                name: Some("shot.png".into()),
                media_type: Some("image/png".into()),
            });
        assert_eq!(
            app.update(Msg::FileAction(FileAction::Jump(live))),
            vec![Effect::ScrollToLatest],
            "no older page is loaded for a file the live turn carries"
        );
    }

    #[test]
    fn leaving_the_chat_drops_a_jump_in_flight() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (id, file) = (Uuid::new_v4(), Uuid::new_v4());
        let mut record = chat(id);
        record.files = serde_json::from_value(json!([{"id": file, "name": "a.txt",
            "mime_type": "text/plain", "size_bytes": 1, "created_at": "2026-10-05T09:00:00Z"}]))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: Some(true),
            chat: record,
            messages: vec![message(300)],
        });
        app.update(Msg::FileAction(FileAction::Jump(file)));
        assert_eq!(app.jump, Some((id, file)));
        app.update(Msg::Command(Command::New));
        assert_eq!(app.jump, None, "a jump belongs to the chat it was asked in");
    }

    fn long_chat_with_file(app: &mut App) -> (Uuid, Uuid) {
        started(app);
        let (id, file) = (Uuid::new_v4(), Uuid::new_v4());
        let mut record = chat(id);
        record.files = serde_json::from_value(json!([{"id": file, "name": "a.txt",
            "mime_type": "text/plain", "size_bytes": 1, "created_at": "2026-10-05T09:00:00Z"}]))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: Some(true),
            chat: record,
            messages: vec![message(300)],
        });
        (id, file)
    }

    #[test]
    fn a_history_reset_ends_a_jump_and_says_so() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (id, file) = long_chat_with_file(&mut app);
        let generation = older_generation(&app.update(Msg::FileAction(FileAction::Jump(file))));
        app.update(ev(json!({"type": "history_reset"})));
        assert_eq!(app.jump, None);
        assert_eq!(
            last_notice(&app),
            Notice::Info("Stopped looking for the file's message.".into())
        );
        let carrier: types::CodersdkChatMessage = serde_json::from_value(json!({"id": 5,
            "role": "user", "content": [{"type": "file", "file_id": file,
                "media_type": "text/plain", "name": "a.txt"}]}))
        .unwrap();
        assert!(
            app.update(older_page(id, generation, vec![carrier]))
                .is_empty()
        );
    }

    #[test]
    fn a_user_scroll_ends_a_jump_and_says_so() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (id, file) = long_chat_with_file(&mut app);
        let generation = older_generation(&app.update(Msg::FileAction(FileAction::Jump(file))));
        assert!(app.update(Msg::UserScrolled).is_empty());
        assert_eq!(app.jump, None);
        assert_eq!(
            last_notice(&app),
            Notice::Info("Stopped looking for the file's message.".into())
        );
        let carrier: types::CodersdkChatMessage = serde_json::from_value(json!({"id": 5,
            "role": "user", "content": [{"type": "file", "file_id": file,
                "media_type": "text/plain", "name": "a.txt"}]}))
        .unwrap();
        assert!(
            app.update(older_page(id, generation, vec![carrier]))
                .is_empty(),
            "the page still lands, but nothing scrolls to the file"
        );
        let before = app.notices.len();
        app.update(Msg::UserScrolled);
        assert_eq!(
            app.notices.len(),
            before,
            "with no jump, a scroll says nothing"
        );
    }

    #[test]
    fn a_second_conflict_never_replaces_the_open_question() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (_, file) = chat_with_a_file(&mut app, "build-logs.zip", "application/zip");
        let first = PathBuf::from("/h/Downloads/build-logs.zip");
        app.update(Msg::FileConflict {
            file,
            name: "build-logs.zip".into(),
            path: first.clone(),
        });
        let other = Uuid::new_v4();
        app.update(Msg::FileConflict {
            file: other,
            name: "b.zip".into(),
            path: PathBuf::from("/h/Downloads/b.zip"),
        });
        let open = app.save_conflict.as_ref().unwrap();
        assert_eq!((open.file, &open.path), (file, &first));
        assert_eq!(
            last_notice(&app),
            Notice::Info("b.zip was not saved: answer the open save question first.".into())
        );
        assert!(!app.files_busy.contains(&other));
    }

    #[test]
    fn a_save_waits_while_the_save_question_is_open() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (_, file) = chat_with_a_file(&mut app, "build-logs.zip", "application/zip");
        app.update(Msg::FileConflict {
            file,
            name: "build-logs.zip".into(),
            path: PathBuf::from("/h/Downloads/build-logs.zip"),
        });
        for action in [FileAction::Save(file), FileAction::SaveAs(file)] {
            assert!(app.update(Msg::FileAction(action)).is_empty());
            assert_eq!(
                last_notice(&app),
                Notice::Error("Answer the open save question first.".into())
            );
        }
        assert!(app.editor.is_none());
    }

    #[test]
    fn a_view_reply_for_a_chat_left_is_dropped() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (_, file) = chat_with_a_file(&mut app, "notes.txt", "text/plain");
        app.update(Msg::FileAction(FileAction::View(file)));
        app.update(Msg::Command(Command::New));
        assert!(
            app.update(Msg::FileText {
                file,
                text: "late".into()
            })
            .is_empty()
        );
        assert!(app.files_busy.is_empty());
    }

    #[test]
    fn the_pager_text_loses_bidi_overrides_but_keeps_an_emoji_joiner() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (_, file) = chat_with_a_file(&mut app, "notes.txt", "text/plain");
        app.update(Msg::FileAction(FileAction::View(file)));
        assert_eq!(
            app.update(Msg::FileText {
                file,
                text: "a\u{202e}b\u{200b}c 👩\u{200d}💻\ttab".into()
            }),
            vec![Effect::Page("abc 👩\u{200d}💻\ttab".into())]
        );
    }

    #[test]
    fn a_typed_save_path_is_relative_to_the_save_dir_and_never_names_another_user() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (_, file) = chat_with_a_file(&mut app, "build-logs.zip", "application/zip");
        let submit = |app: &mut App, text: &str| {
            app.update(Msg::FileAction(FileAction::SaveAs(file)));
            app.editor.as_mut().unwrap().line = LineEdit::new(text);
            app.update(Msg::Edit(Edit::Submit))
        };
        let typed = |path: &str| {
            vec![Effect::SaveFile {
                file,
                name: "build-logs.zip".into(),
                to: SaveTo::Typed(PathBuf::from(path)),
                conflict: OnConflict::Ask,
            }]
        };
        assert_eq!(submit(&mut app, "logs.zip"), typed("/h/Downloads/logs.zip"));
        app.files_busy.clear();
        assert_eq!(submit(&mut app, "~"), typed("/h"));
        app.files_busy.clear();
        assert_eq!(submit(&mut app, "/abs/x.zip"), typed("/abs/x.zip"));
        app.files_busy.clear();
        assert!(submit(&mut app, "~bob/x.zip").is_empty());
        assert!(
            app.editor.is_some(),
            "the editor stays open to fix the path"
        );
        assert_eq!(
            last_notice(&app),
            Notice::Error("Cannot expand ~bob; type the full path.".into())
        );
        app.editor = None;
        assert!(
            submit(&mut app, "").is_empty(),
            "an empty path saves nothing"
        );
        assert!(app.editor.is_some(), "the editor stays open to type a path");
        assert_eq!(
            last_notice(&app),
            Notice::Error("Type where to save the file, or press Esc.".into())
        );
        assert!(app.files_busy.is_empty());
    }

    #[test]
    fn submitting_save_as_rechecks_busy_and_expired() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (_, file) = chat_with_a_file(&mut app, "build-logs.zip", "application/zip");
        app.update(Msg::FileAction(FileAction::SaveAs(file)));
        app.files_busy.insert(file);
        assert!(app.update(Msg::Edit(Edit::Submit)).is_empty());
        assert_eq!(
            last_notice(&app),
            Notice::Info("Still working on build-logs.zip.".into())
        );
        app.files_busy.clear();
        app.chat.as_mut().unwrap().files = serde_json::from_value(json!([{"id": Uuid::new_v4(),
            "name": "new.txt", "mime_type": "text/plain", "size_bytes": 1,
            "created_at": "2026-10-05T13:00:00Z"}]))
        .unwrap();
        app.update(Msg::Edit(Edit::Submit));
        assert!(
            matches!(last_notice(&app), Notice::Error(t) if t.starts_with("build-logs.zip is no longer available")),
            "{:?}",
            last_notice(&app)
        );
    }

    #[test]
    fn workspace_arguments_name_each_workspace_then_none_and_say_while_loading() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let shown = |app: &App| {
            app.argument_menu("/workspace")
                .into_iter()
                .map(|e| format!("{} | {} | {:?}", e.label, e.description, e.kind))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            shown(&app),
            [
                "none | Detach the workspace | Argument",
                "Loading workspaces… |  | Note"
            ]
        );
        app.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::WorkspacesLoaded(vec![
                WorkspaceRef {
                    id: Uuid::new_v4(),
                    name: "build3".into(),
                    last_used: Some(1),
                    ..Default::default()
                },
                WorkspaceRef {
                    id: Uuid::new_v4(),
                    name: "dev-2".into(),
                    template: "Docker".into(),
                    status: "running".into(),
                    last_used: Some(2),
                },
            ])),
        });
        assert_eq!(
            shown(&app),
            [
                "dev-2 | running · Docker | Argument",
                "build3 |  | Argument",
                "none | Detach the workspace | Argument"
            ],
            "most recently used first, as the /workspace table lists them"
        );
        assert!(
            app.argument_menu("/model").is_empty(),
            "only /workspace completes its argument"
        );
        app.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::WorkspacesFailed {
                message: "HTTP 502".into(),
            }),
        });
        assert_eq!(
            shown(&app).last().map(String::as_str),
            Some("Workspaces failed to load: HTTP 502 |  | Note")
        );
    }

    #[test]
    fn wanting_workspace_arguments_fetches_again_only_a_list_that_failed() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let wanted = |app: &mut App| {
            app.update(Msg::ArgumentsWanted {
                command: "/workspace",
            })
        };
        assert!(wanted(&mut app).is_empty(), "the first load is on its way");
        app.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::WorkspacesFailed {
                message: "HTTP 502".into(),
            }),
        });
        assert_eq!(wanted(&mut app), vec![Effect::FetchWorkspaces(org)]);
        assert_eq!(app.workspaces_state, WorkspacesState::Loading);
        assert!(wanted(&mut app).is_empty(), "one fetch at a time");
        app.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::WorkspacesLoaded(vec![])),
        });
        assert!(
            wanted(&mut app).is_empty(),
            "a loaded list is used as it is"
        );
        assert!(
            app.update(Msg::ArgumentsWanted { command: "/model" })
                .is_empty()
        );
    }

    #[test]
    fn the_workspace_failure_note_shows_no_control_characters() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        app.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::WorkspacesFailed {
                message: "bad\u{1b}[31m gateway\u{202e}".into(),
            }),
        });
        assert_eq!(
            app.argument_menu("/workspace")
                .last()
                .map(|e| e.label.as_str()),
            Some("Workspaces failed to load: bad[31m gateway")
        );
    }

    /// The model list for the threshold tests: `Alpha`, whose default threshold is 70%, with
    /// a 200,000-token window and low and high effort, and `Beta`, whose default is 30%.
    fn threshold_catalog(alpha: Uuid, beta: Uuid) -> Msg {
        let provider = Uuid::new_v4();
        Msg::CatalogLoaded(Box::new(
            serde_json::from_value(json!({
                "models": [
                    {"id": alpha, "display_name": "Alpha", "ai_provider_id": provider, "enabled": true, "is_default": true, "context_limit": 200000, "compression_threshold": 70, "reasoning_efforts": ["low", "high"]},
                    {"id": beta, "display_name": "Beta", "ai_provider_id": provider, "enabled": true, "context_limit": 1000000, "compression_threshold": 30, "reasoning_efforts": []}
                ],
                "providers": [{"id": provider, "display_name": "Provider", "available": true}],
                "unsupported_providers": []
            }))
            .unwrap(),
        ))
    }

    /// An app with the threshold catalog loaded and the user's overrides read: Beta at 50%.
    /// Returns the app, Alpha, and Beta.
    fn with_thresholds() -> (App, Uuid, Uuid) {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let (alpha, beta) = (Uuid::new_v4(), Uuid::new_v4());
        let effects = app.update(Msg::ForOrg {
            org,
            msg: Box::new(threshold_catalog(alpha, beta)),
        });
        let [Effect::FetchThresholds { generation }] = effects.as_slice() else {
            panic!("the overrides load with the model list: {effects:?}");
        };
        app.update(Msg::ThresholdsLoaded {
            thresholds: vec![(beta, 50)],
            generation: *generation,
        });
        (app, alpha, beta)
    }

    fn model_row(app: &App, id: Uuid) -> ModelRow {
        app.model_groups("")
            .into_iter()
            .flat_map(|g| g.models)
            .find(|m| m.id == id)
            .expect("a /model row")
    }

    #[test]
    fn the_model_list_loads_the_threshold_overrides_and_rows_show_the_one_in_effect() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let (alpha, beta) = (Uuid::new_v4(), Uuid::new_v4());
        let effects = app.update(Msg::ForOrg {
            org,
            msg: Box::new(threshold_catalog(alpha, beta)),
        });
        let [Effect::FetchThresholds { generation }] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        let generation = *generation;
        assert_eq!(model_row(&app, alpha).compaction, Shown::Loading);
        app.update(Msg::ThresholdsLoaded {
            thresholds: vec![(beta, 50)],
            generation,
        });
        let alpha_row = model_row(&app, alpha);
        assert_eq!(
            alpha_row.compaction,
            Shown::Known {
                percent: 70,
                default: true
            }
        );
        assert_eq!(alpha_row.context.as_deref(), Some("200.0k tokens"));
        assert_eq!(alpha_row.efforts, ["low", "high"]);
        let beta_row = model_row(&app, beta);
        assert_eq!(
            beta_row.compaction,
            Shown::Known {
                percent: 50,
                default: false
            }
        );
        assert!(beta_row.efforts.is_empty());
    }

    #[test]
    fn threshold_edits_save_once_when_committed_and_closing_commits() {
        let (mut app, alpha, beta) = with_thresholds();
        for _ in 0..3 {
            assert!(
                app.update(Msg::ThresholdStep {
                    model: alpha,
                    up: true
                })
                .is_empty()
            );
        }
        assert_eq!(
            model_row(&app, alpha).compaction,
            Shown::Known {
                percent: 85,
                default: false
            }
        );
        let effects = app.update(Msg::ThresholdCommit);
        assert!(
            matches!(
                effects.as_slice(),
                [Effect::SaveThreshold(compaction::Save {
                    model,
                    change: compaction::Change::Set(85),
                    ..
                })] if *model == alpha
            ),
            "{effects:?}"
        );
        assert!(
            app.update(Msg::ThresholdCommit).is_empty(),
            "nothing is left to send"
        );
        assert!(app.update(Msg::ThresholdReset { model: beta }).is_empty());
        assert_eq!(
            model_row(&app, beta).compaction,
            Shown::Known {
                percent: 30,
                default: true
            }
        );
        let effects = app.update(Msg::ModelPickerClosed);
        assert!(
            matches!(
                effects.as_slice(),
                [Effect::SaveThreshold(compaction::Save {
                    model,
                    change: compaction::Change::Reset,
                    ..
                })] if *model == beta
            ),
            "{effects:?}"
        );
    }

    #[test]
    fn picking_a_model_saves_the_threshold_edited_on_it_first() {
        let (mut app, alpha, _) = with_thresholds();
        app.update(Msg::ThresholdStep {
            model: alpha,
            up: false,
        });
        let effects = app.update(Msg::ModelChosen(alpha));
        assert!(
            matches!(
                effects.as_slice(),
                [Effect::SaveThreshold(compaction::Save {
                    model,
                    change: compaction::Change::Set(65),
                    ..
                })] if *model == alpha
            ),
            "{effects:?}"
        );
        assert_eq!(app.selected_model, Some(alpha));
    }

    #[test]
    fn a_failed_threshold_save_says_so_and_shows_the_server_value_again() {
        let (mut app, _, beta) = with_thresholds();
        app.update(Msg::ThresholdStep {
            model: beta,
            up: true,
        });
        let effects = app.update(Msg::ThresholdCommit);
        let [Effect::SaveThreshold(save)] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        assert_eq!(save.change, compaction::Change::Set(55));
        app.update(Msg::ThresholdFailed {
            model: beta,
            message: "Model config is disabled.".into(),
            generation: save.generation,
        });
        assert_eq!(
            model_row(&app, beta).compaction,
            Shown::Known {
                percent: 50,
                default: false
            },
            "the value the server holds"
        );
        assert!(
            matches!(
                app.notices.last(),
                Some(Notice::Error(m))
                    if m == "Could not save the compaction threshold for Beta: Model config is disabled."
            ),
            "{:?}",
            app.notices.last()
        );
    }

    #[test]
    fn a_stale_threshold_reply_never_overwrites_a_newer_edit() {
        let (mut app, alpha, _) = with_thresholds();
        app.update(Msg::ThresholdStep {
            model: alpha,
            up: true,
        });
        let effects = app.update(Msg::ThresholdCommit);
        let [Effect::SaveThreshold(first)] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        let first = *first;
        app.update(Msg::ThresholdStep {
            model: alpha,
            up: true,
        });
        assert!(
            app.update(Msg::ThresholdCommit).is_empty(),
            "one save per model at a time"
        );
        let notices = app.notices.len();
        assert!(
            app.update(Msg::ThresholdSaved {
                model: alpha,
                percent: Some(40),
                generation: first.generation + 100,
            })
            .is_empty()
        );
        app.update(Msg::ThresholdFailed {
            model: alpha,
            message: "late".into(),
            generation: first.generation + 100,
        });
        assert_eq!(app.notices.len(), notices, "a stale failure says nothing");
        assert_eq!(
            model_row(&app, alpha).compaction,
            Shown::Known {
                percent: 80,
                default: false
            }
        );
        let effects = app.update(Msg::ThresholdSaved {
            model: alpha,
            percent: Some(75),
            generation: first.generation,
        });
        assert!(
            matches!(
                effects.as_slice(),
                [Effect::SaveThreshold(compaction::Save {
                    model,
                    change: compaction::Change::Set(80),
                    generation,
                })] if *model == alpha && *generation > first.generation
            ),
            "the edit that waited goes next: {effects:?}"
        );
    }

    #[test]
    fn a_threshold_edit_before_the_overrides_load_waits_and_a_failed_load_retries() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let (alpha, beta) = (Uuid::new_v4(), Uuid::new_v4());
        let effects = app.update(Msg::ForOrg {
            org,
            msg: Box::new(threshold_catalog(alpha, beta)),
        });
        let [Effect::FetchThresholds { generation }] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        let generation = *generation;
        assert!(
            app.update(Msg::ThresholdStep {
                model: alpha,
                up: true
            })
            .is_empty()
        );
        assert!(
            matches!(app.notices.last(), Some(Notice::Info(m)) if m == "Compaction thresholds are still loading."),
            "{:?}",
            app.notices.last()
        );
        app.update(Msg::ThresholdsFailed {
            message: "HTTP 502".into(),
            generation,
        });
        assert_eq!(model_row(&app, alpha).compaction, Shown::Unknown);
        let effects = app.update(Msg::ThresholdStep {
            model: alpha,
            up: true,
        });
        let [Effect::FetchThresholds { generation: again }] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        assert!(*again > generation);
        assert_eq!(model_row(&app, alpha).compaction, Shown::Loading);
    }

    #[test]
    fn stepping_a_model_without_a_default_threshold_says_there_is_none_to_change() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let (provider, bare) = (Uuid::new_v4(), Uuid::new_v4());
        let effects = app.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::CatalogLoaded(Box::new(
                serde_json::from_value(json!({
                    "models": [{"id": bare, "display_name": "Bare", "ai_provider_id": provider, "enabled": true, "reasoning_efforts": []}],
                    "providers": [{"id": provider, "display_name": "Provider", "available": true}],
                    "unsupported_providers": []
                }))
                .unwrap(),
            ))),
        });
        let [Effect::FetchThresholds { generation }] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        app.update(Msg::ThresholdsLoaded {
            thresholds: vec![],
            generation: *generation,
        });
        assert_eq!(model_row(&app, bare).compaction, Shown::Unknown);
        assert!(
            app.update(Msg::ThresholdStep {
                model: bare,
                up: true
            })
            .is_empty()
        );
        assert!(
            matches!(
                app.notices.last(),
                Some(Notice::Info(m)) if m == "There is no compaction threshold to change for Bare."
            ),
            "{:?}",
            app.notices.last()
        );
        assert!(
            app.update(Msg::ThresholdCommit).is_empty(),
            "nothing to send"
        );
    }
}
