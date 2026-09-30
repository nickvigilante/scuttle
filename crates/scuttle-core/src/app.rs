//! The headless application: every input is a `Msg`, every side effect is an `Effect`.

use std::time::Duration;

use coder_sdk::{ChatStatus, StreamEvent, StreamEventType, types};
use uuid::Uuid;

use crate::commands::{self, Command};
use crate::config::BusyBehavior;
use crate::density::DisplayPrefs;
use crate::live::{Applied, LiveBlock};
use crate::transcript::Transcript;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRef {
    pub id: Uuid,
    pub name: String,
}

/// One of the user's organizations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgRef {
    pub id: Uuid,
    pub name: String,
    pub display_name: String,
    pub is_default: bool,
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

/// The organization new chats go to: the saved one while the user is still a member, else
/// the default one, else the first. The server returns memberships in no stable order, so
/// the first alone is not a choice. The web UI's Agents page picks in the same order.
pub fn pick_organization(saved: Option<Uuid>, orgs: &[OrgRef]) -> Option<Uuid> {
    saved
        .filter(|id| orgs.iter().any(|o| o.id == *id))
        .or_else(|| orgs.iter().find(|o| o.is_default).map(|o| o.id))
        .or_else(|| orgs.first().map(|o| o.id))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyTarget {
    LastMessage,
    CodeBlock(usize),
}

/// Options that ride along with a new chat or a sent message.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TurnOptions {
    /// The reasoning effort chosen with `/effort`; `None` leaves the server's default.
    pub effort: Option<String>,
    /// Switches the chat's plan mode with this request: `Some(true)` on, `Some(false)` off,
    /// `None` no change.
    pub plan_mode: Option<bool>,
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
    /// The user's organizations, sent once at startup, before `Started`.
    OrganizationsLoaded(Vec<OrgRef>),
    /// An organization picked for new chats, from `/organization <name>` or the picker.
    OrganizationChosen(Uuid),
    /// A reply about the model or workspace list of `org`, applied only while those lists still
    /// belong to it.
    ForOrg {
        org: Uuid,
        msg: Box<Msg>,
    },
    /// A reply about `chat`, applied only while it is the open chat. The runtime wraps stream
    /// events in this, so one already queued when the user leaves the chat cannot reach the
    /// next one.
    ForChat {
        chat: Uuid,
        msg: Box<Msg>,
    },
    ChatLoaded {
        chat: Box<types::CodersdkChat>,
        messages: Vec<types::CodersdkChatMessage>,
    },
    /// The runtime sends this when `Effect::LoadChat` fails to load the chat or its messages.
    ChatLoadFailed {
        chat_id: Uuid,
        message: String,
    },
    ChatCreated(Box<types::CodersdkChat>),
    /// The runtime sends this when `Effect::CreateChat` fails.
    CreateFailed {
        message: String,
    },
    /// The runtime sends this when `Effect::SendMessage` fails, with the text it tried to send
    /// and the plan mode change it carried, which therefore did not happen.
    SendFailed {
        text: String,
        message: String,
        plan_mode: Option<bool>,
    },
    Stream(StreamEvent),
    StreamEnded {
        error: Option<String>,
    },
    PrefsLoaded(DisplayPrefs),
    ModelsLoaded(Vec<types::CodersdkChatModel>),
    /// The runtime sends this when `Effect::FetchModels` fails.
    ModelsFailed {
        message: String,
    },
    WorkspacesLoaded(Vec<WorkspaceRef>),
    ModelChosen(Uuid),
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
    /// The runtime sends this after `Effect::OpenWeb`, with the chat's URL and why no browser
    /// opened, if none did.
    WebOpened {
        url: String,
        outcome: Result<(), String>,
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
    OpenStream {
        chat: Uuid,
        after_id: Option<i64>,
    },
    ReconnectAfter {
        chat: Uuid,
        after_id: Option<i64>,
        delay: Duration,
    },
    /// Stops the chat stream without touching the chat on the server.
    CloseStream,
    CreateChat {
        org: Uuid,
        text: String,
        model: Option<Uuid>,
        workspace: Option<Uuid>,
        turn: TurnOptions,
    },
    SendMessage {
        chat: Uuid,
        text: String,
        model: Option<Uuid>,
        busy: BusyBehavior,
        turn: TurnOptions,
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
    },
    OpenWeb(Uuid),
    FetchPrefs,
    FetchModels(Uuid),
    FetchWorkspaces(Uuid),
    ShowPicker(Picker),
    ShowHelp,
    Copy(CopyTarget),
    SetMouse(bool),
    /// Saves the organization new chats go to in the local config.
    SaveOrganization(Uuid),
    /// Puts text back in the composer after a failed chat creation or send.
    RestoreComposer(String),
    /// Clears what the UI keeps about the chat on screen: expanded blocks, the selection,
    /// the scroll position, and the composer's place in its history.
    ClearView,
    Quit,
}

/// Reconnect delay: 500 ms doubling per attempt, capped at 10 s. The runtime adds jitter.
pub fn backoff(attempt: u32) -> Duration {
    let exp = attempt.saturating_sub(1).min(10);
    Duration::from_millis((500u64 << exp).min(10_000))
}

/// Whether the server reports plan mode on for `chat`.
fn is_plan(chat: &types::CodersdkChat) -> bool {
    chat.plan_mode.as_ref().is_some_and(|p| p.0 == "plan")
}

#[derive(Debug, Default)]
pub struct App {
    pub org_id: Option<Uuid>,
    pub chat_id: Option<Uuid>,
    pub chat: Option<Box<types::CodersdkChat>>,
    pub transcript: Transcript,
    pub prefs: DisplayPrefs,
    pub models: Vec<types::CodersdkChatModel>,
    pub workspaces: Vec<WorkspaceRef>,
    pub organizations: Vec<OrgRef>,
    /// The organization `models` and `workspaces` were requested for.
    lists_org: Option<Uuid>,
    pub selected_model: Option<Uuid>,
    pub selected_workspace: Option<Uuid>,
    /// The reasoning effort chosen with `/effort`, sent only while the current model offers it.
    pub selected_effort: Option<String>,
    /// Whether plan mode is on for the chat, or for the chat the next message creates.
    pub plan_mode: bool,
    pub notices: Vec<Notice>,
    pub connection: Connection,
    pub busy: BusyBehavior,
    pub mouse: bool,
    pub models_state: ModelsState,
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
    /// Set by a submit that sends or queues a message, and cleared once the chat reports a
    /// status other than `waiting`, an error, or a failed send.
    awaiting_reply: bool,
    /// The id of the echoed user message the wait is for, once the stream has sent it; an
    /// assistant message with a higher id is the reply.
    sent_id: Option<i64>,
    reconnect_attempt: u32,
}

impl App {
    pub fn new(busy: BusyBehavior, mouse: bool) -> App {
        App {
            busy,
            mouse,
            ..App::default()
        }
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

    /// The reasoning efforts the current model offers, lowest first.
    pub fn efforts(&self) -> &[String] {
        self.current_model()
            .map(|m| m.reasoning_efforts.as_slice())
            .unwrap_or(&[])
    }

    /// The effort sent with the next message: the chosen one, if the current model offers it.
    pub fn effort(&self) -> Option<String> {
        self.selected_effort
            .clone()
            .filter(|e| self.efforts().contains(e))
    }

    /// The effort to show as current: the chosen one, else the chat's last one, else the
    /// model's default, each only if the current model offers it.
    pub fn effort_label(&self) -> Option<String> {
        let offered = |e: &String| self.efforts().contains(e);
        self.effort()
            .or_else(|| {
                self.chat
                    .as_ref()
                    .and_then(|c| c.last_reasoning_effort.clone())
                    .filter(offered)
            })
            .or_else(|| {
                self.current_model()
                    .and_then(|m| m.model_config.as_ref())
                    .and_then(|c| c.reasoning_effort.as_ref())
                    .and_then(|r| r.default.clone())
                    .filter(offered)
            })
    }

    /// Options for the next message sent to an existing chat.
    fn turn(&self) -> TurnOptions {
        TurnOptions {
            effort: self.effort(),
            plan_mode: None,
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
                _ => Activity::Working,
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

    /// The open chat's organization, else the one new chats go to.
    pub fn current_org(&self) -> Option<Uuid> {
        self.chat
            .as_ref()
            .and_then(|c| c.organization_id)
            .or(self.org_id)
    }

    /// The name of organization `id`, or a generic phrase when it is unknown or unnamed.
    pub fn org_label(&self, id: Option<Uuid>) -> String {
        id.and_then(|id| self.organizations.iter().find(|o| o.id == id))
            .map(|o| o.label().trim())
            .filter(|label| !label.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| "this organization".into())
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
        self.workspaces.clear();
        self.models_state = ModelsState::Loading;
        vec![Effect::FetchModels(org), Effect::FetchWorkspaces(org)]
    }

    fn info(&mut self, text: impl Into<String>) {
        self.notices.push(Notice::Info(text.into()));
    }

    fn error(&mut self, text: impl Into<String>) {
        self.notices.push(Notice::Error(text.into()));
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
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
            Msg::OrganizationsLoaded(organizations) => {
                self.organizations = organizations;
                vec![]
            }
            Msg::OrganizationChosen(id) => {
                let Some(label) = self
                    .organizations
                    .iter()
                    .find(|o| o.id == id)
                    .map(|o| o.label().to_owned())
                else {
                    return vec![];
                };
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
                    self.info(format!("New chats will use {label}."));
                    let lists = self.load_lists_for(id);
                    if !lists.is_empty() {
                        // Workspaces belong to one organization too.
                        self.selected_workspace = None;
                    }
                    effects.extend(lists);
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
                    Msg::SendFailed { text, message, .. } => {
                        self.error(format!(
                            "Could not send the message to the previous chat: {message}"
                        ));
                        vec![Effect::RestoreComposer(text)]
                    }
                    Msg::ApiFailed { action, message } => {
                        self.error(format!(
                            "In the previous chat, could not {action}: {message}"
                        ));
                        vec![]
                    }
                    _ => vec![],
                }
            }
            Msg::ChatLoaded { chat, messages } => {
                let Some(id) = chat.id else {
                    self.error("The server returned a chat without an id.");
                    self.connection = Connection::Idle;
                    self.failed_load = self.loading.take().or(self.failed_load);
                    return self.restore_pending();
                };
                self.loading = None;
                self.failed_load = None;
                let lists = chat
                    .organization_id
                    .map(|org| self.load_lists_for(org))
                    .unwrap_or_default();
                self.selected_model = self.selected_model.or(chat.last_model_config_id);
                self.selected_workspace = chat.workspace_id;
                self.plan_mode = is_plan(&chat);
                self.chat_id = Some(id);
                self.chat = Some(chat);
                self.transcript.load(messages);
                self.connection = Connection::Connecting;
                let mut effects = vec![Effect::OpenStream {
                    chat: id,
                    after_id: self.transcript.last_message_id(),
                }];
                effects.extend(lists);
                if let Some(text) = self.pending_text.take() {
                    effects.push(Effect::SendMessage {
                        chat: id,
                        text,
                        model: self.selected_model,
                        busy: self.busy,
                        turn: self.turn(),
                    });
                }
                effects
            }
            Msg::ChatLoadFailed { chat_id, message } => {
                self.loading = None;
                self.failed_load = Some(chat_id);
                self.connection = Connection::Idle;
                self.error(format!("Could not load chat {chat_id}: {message}"));
                self.restore_pending()
            }
            Msg::ChatCreated(chat) => {
                let Some(id) = chat.id else {
                    return self.fail_create("the server returned a chat without an id".into());
                };
                let workspace_mismatch = self.selected_workspace != chat.workspace_id;
                let plan_mismatch = is_plan(&chat) != self.plan_mode;
                self.creating = None;
                self.chat_id = Some(id);
                self.chat = Some(chat);
                self.connection = Connection::Connecting;
                let mut effects = vec![Effect::OpenStream {
                    chat: id,
                    after_id: None,
                }];
                if workspace_mismatch {
                    effects.push(Effect::SetWorkspace {
                        chat: id,
                        workspace: self.selected_workspace,
                    });
                }
                match self.pending_text.take() {
                    // The queued message carries the plan mode change, so the two cannot race.
                    Some(text) => effects.push(Effect::SendMessage {
                        chat: id,
                        text,
                        model: self.selected_model,
                        busy: self.busy,
                        turn: TurnOptions {
                            plan_mode: plan_mismatch.then_some(self.plan_mode),
                            ..self.turn()
                        },
                    }),
                    None if plan_mismatch => effects.push(Effect::SetPlanMode {
                        chat: id,
                        on: self.plan_mode,
                    }),
                    None => {}
                }
                effects
            }
            Msg::CreateFailed { message } => self.fail_create(message),
            Msg::SendFailed {
                text,
                message,
                plan_mode,
            } => {
                self.awaiting_reply = false;
                let mut error = format!("Could not send the message: {message}");
                if let Some(on) = plan_mode {
                    self.revert_plan_mode(on);
                    let state = if self.plan_mode { "on" } else { "off" };
                    error.push_str(&format!(" Plan mode is still {state}."));
                }
                self.error(error);
                vec![Effect::RestoreComposer(text)]
            }
            Msg::Stream(ev) => match self.transcript.apply(&ev) {
                Applied::Reconnect(_) => match self.chat_id {
                    Some(chat) => {
                        self.reconnect_attempt += 1;
                        self.transcript.live.clear();
                        self.connection = Connection::Reconnecting {
                            attempt: self.reconnect_attempt,
                        };
                        let delay = if self.reconnect_attempt == 1 {
                            Duration::ZERO
                        } else {
                            backoff(self.reconnect_attempt)
                        };
                        vec![Effect::ReconnectAfter {
                            chat,
                            after_id: self.transcript.last_message_id(),
                            delay,
                        }]
                    }
                    None => vec![],
                },
                _ => {
                    self.connection = Connection::Live;
                    self.reconnect_attempt = 0;
                    self.last_stream_error = None;
                    if self.awaiting_reply && self.ends_wait(&ev) {
                        self.awaiting_reply = false;
                    }
                    vec![]
                }
            },
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
                vec![Effect::ReconnectAfter {
                    chat,
                    after_id: self.transcript.last_message_id(),
                    delay: backoff(self.reconnect_attempt),
                }]
            }
            Msg::PrefsLoaded(prefs) => {
                self.prefs = prefs;
                vec![]
            }
            Msg::ModelsLoaded(models) => {
                self.models = models
                    .into_iter()
                    .filter(|m| m.enabled != Some(false))
                    .collect();
                self.models_state = ModelsState::Loaded;
                vec![]
            }
            Msg::ModelsFailed { message } => {
                self.models_state = ModelsState::Failed;
                self.error(format!("Could not load models: {message}"));
                vec![]
            }
            Msg::WorkspacesLoaded(workspaces) => {
                self.workspaces = workspaces;
                vec![]
            }
            Msg::ModelChosen(id) => {
                self.selected_model = Some(id);
                if let Some(name) = self.model_name() {
                    self.info(format!("Model set to {name}"));
                }
                if self
                    .selected_effort
                    .as_ref()
                    .is_some_and(|e| !self.efforts().contains(e))
                {
                    let effort = self.selected_effort.take().unwrap_or_default();
                    self.info(format!(
                        "This model does not offer {effort} reasoning effort; using its default."
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
                match found {
                    Some(effort) => {
                        self.info(format!("Reasoning effort set to {effort}"));
                        self.selected_effort = Some(effort);
                    }
                    None => {
                        let offered = self.efforts().join(", ");
                        self.error(format!(
                            "No reasoning effort named {level:?}; choose one of {offered}."
                        ));
                    }
                }
                vec![]
            }
            Msg::WorkspaceChosen(ws) => self.set_workspace(ws),
            Msg::ApiFailed { action, message } => {
                self.error(format!("Could not {action}: {message}"));
                vec![]
            }
            Msg::PlanModeFailed { on, message } => {
                self.revert_plan_mode(on);
                let word = if on { "on" } else { "off" };
                self.error(format!("Could not turn plan mode {word}: {message}"));
                vec![]
            }
            Msg::WebOpened { url, outcome } => {
                match outcome {
                    Ok(()) => self.info(format!("Opened {url}")),
                    Err(why) => self.info(format!("Open {url} ({why})")),
                }
                vec![]
            }
            Msg::Submit(text) => self.submit(text),
            Msg::Command(cmd) => self.command(cmd),
            Msg::Interrupt => match self.chat_id {
                Some(chat) if self.can_interrupt() => vec![Effect::Interrupt(chat)],
                _ => vec![],
            },
            Msg::Refresh => vec![],
        }
    }

    /// Restores whatever text was typed for the failed `Effect::CreateChat` (and anything
    /// queued behind it) to the composer, and records the failure.
    fn fail_create(&mut self, message: String) -> Vec<Effect> {
        self.awaiting_reply = false;
        let in_flight = self.creating.take().unwrap_or_default();
        let restored = match self.pending_text.take() {
            Some(pending) => format!("{in_flight}\n\n{pending}"),
            None => in_flight,
        };
        self.error(format!("Could not create the chat: {message}"));
        if restored.is_empty() {
            vec![]
        } else {
            vec![Effect::RestoreComposer(restored)]
        }
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

    fn submit(&mut self, text: String) -> Vec<Effect> {
        let text = text.trim().to_owned();
        if text.is_empty() {
            return vec![];
        }
        if text.starts_with('/') {
            return match commands::parse(&text) {
                Ok(cmd) => self.command(cmd),
                Err(e) => {
                    self.error(e);
                    vec![]
                }
            };
        }
        if let Some(chat) = self.chat_id {
            self.start_wait();
            return vec![Effect::SendMessage {
                chat,
                text,
                model: self.selected_model,
                busy: self.busy,
                turn: self.turn(),
            }];
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
        vec![Effect::CreateChat {
            org,
            text,
            model: self.selected_model,
            workspace: self.selected_workspace,
            turn: TurnOptions {
                plan_mode: self.plan_mode.then_some(true),
                ..self.turn()
            },
        }]
    }

    fn set_workspace(&mut self, ws: Option<Uuid>) -> Vec<Effect> {
        self.selected_workspace = ws;
        match self.chat_id {
            Some(chat) => vec![Effect::SetWorkspace {
                chat,
                workspace: ws,
            }],
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

    /// Takes back a plan mode change to `on` that did not reach the server, unless a later
    /// change already replaced it.
    fn revert_plan_mode(&mut self, on: bool) {
        if self.plan_mode == on {
            self.plan_mode = !on;
        }
    }

    fn plan_mode_command(&mut self, wanted: Option<bool>) -> Vec<Effect> {
        // Loading replaces `plan_mode` with the chat's own, which would drop this change.
        if self.loading.is_some() || (self.chat_id.is_none() && self.failed_load.is_some()) {
            self.info("Wait for the chat to load, then set plan mode.");
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
                vec![Effect::SetPlanMode { chat, on }]
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

    fn organization_command(&mut self, name: Option<String>) -> Vec<Effect> {
        if self.organizations.len() < 2 {
            match self.organizations.first().map(|o| o.label().to_owned()) {
                Some(label) => self.info(format!("You belong to one organization, {label}.")),
                None => self.info("Your organizations have not loaded."),
            }
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
            self.info("This is already a new chat.");
            return vec![];
        }
        self.chat_id = None;
        self.chat = None;
        self.failed_load = None;
        self.pending_text = None;
        self.transcript = Transcript::default();
        self.connection = Connection::Idle;
        self.last_stream_error = None;
        self.reconnect_attempt = 0;
        self.awaiting_reply = false;
        self.sent_id = None;
        // A new chat has no workspace and no plan mode until they are chosen, as on launch.
        self.selected_workspace = None;
        self.plan_mode = false;
        let mut effects = vec![Effect::CloseStream, Effect::ClearView];
        if let Some(org) = self.org_id {
            effects.extend(self.load_lists_for(org));
        }
        self.info("New chat. Type a message to start it.");
        effects
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
            Command::Workspace(None) => vec![Effect::ShowPicker(Picker::Workspace)],
            Command::Workspace(Some(name)) if name == "none" => self.set_workspace(None),
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
            Command::Web | Command::Compact | Command::Clear => match self.chat_id {
                Some(chat) => vec![match cmd {
                    Command::Web => Effect::OpenWeb(chat),
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
            Command::Organization(name) => self.organization_command(name),
            Command::New => self.new_chat(),
            Command::Help => vec![Effect::ShowHelp],
            Command::Quit => vec![Effect::Quit],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
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
        with_efforts(&mut app);
        assert!(
            app.update(Msg::Command(Command::Effort(Some("HIGH".into()))))
                .is_empty()
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
                }
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
                turn: TurnOptions::default()
            }]
        );
    }

    #[test]
    fn switching_to_a_model_without_the_effort_drops_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let (_, plain) = with_efforts(&mut app);
        app.update(Msg::EffortChosen("low".into()));
        app.update(Msg::ModelChosen(plain));
        assert_eq!(app.selected_effort, None);
        assert!(
            matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("does not offer low"))
        );
    }

    #[test]
    fn effort_waits_for_the_model_list() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert!(app.update(Msg::Command(Command::Effort(None))).is_empty());
        assert!(matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("still loading")));
    }

    #[test]
    fn the_effort_label_prefers_the_choice_then_the_chat_then_the_default() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        with_efforts(&mut app);
        assert_eq!(app.effort_label().as_deref(), Some("medium"));
        let with_last: Box<types::CodersdkChat> = Box::new(
            serde_json::from_value(json!({"id": Uuid::new_v4(), "title": "t", "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}, "last_reasoning_effort": "low"}))
                .unwrap(),
        );
        app.update(Msg::ChatLoaded {
            chat: with_last,
            messages: vec![],
        });
        assert_eq!(app.effort_label().as_deref(), Some("low"));
        app.update(Msg::EffortChosen("high".into()));
        assert_eq!(app.effort_label().as_deref(), Some("high"));
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
                Effect::FetchWorkspaces(product.id)
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
    fn a_loaded_chat_uses_its_own_organization_for_models_and_workspaces() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let home = started(&mut app);
        let other = Uuid::new_v4();
        let loaded: Box<types::CodersdkChat> = Box::new(
            serde_json::from_value(json!({"id": Uuid::new_v4(), "title": "t", "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}, "organization_id": other}))
                .unwrap(),
        );
        let effects = app.update(Msg::ChatLoaded {
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
            chat: chat(id),
            messages: vec![message(3), message(9)],
        });
        assert_eq!(
            effects,
            vec![Effect::OpenStream {
                chat: id,
                after_id: Some(9)
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
                turn: TurnOptions::default()
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
            after_id: None
        }));
        assert!(effects.contains(&Effect::SendMessage {
            chat: id,
            text: "two".into(),
            model: None,
            busy: BusyBehavior::Queue,
            turn: TurnOptions::default()
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
                turn: TurnOptions::default()
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
                turn: TurnOptions::default()
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
            chat: chat(id),
            messages: vec![message(5)],
        });
        let first = app.update(Msg::StreamEnded { error: None });
        assert_eq!(
            first,
            vec![Effect::ReconnectAfter {
                chat: id,
                after_id: Some(5),
                delay: Duration::from_millis(500)
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
                delay: Duration::from_millis(1000)
            }]
        );
        assert_eq!(app.connection, Connection::Reconnecting { attempt: 2 });
    }

    #[test]
    fn stream_event_after_reconnect_resets_backoff() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
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
                delay: Duration::from_millis(500)
            }]
        );
    }

    #[test]
    fn stream_gap_reconnects_immediately() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
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
                delay: Duration::ZERO
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
                delay: Duration::ZERO
            }]
        );
        assert_eq!(app.connection, Connection::Reconnecting { attempt: 1 });
        let second = app.update(ev(part(9)));
        assert_eq!(
            second,
            vec![Effect::ReconnectAfter {
                chat: id,
                after_id: None,
                delay: backoff(2)
            }]
        );
        assert_eq!(app.connection, Connection::Reconnecting { attempt: 2 });
    }

    #[test]
    fn stream_gap_clears_the_live_turn() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
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
        }]));
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
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
        app.update(Msg::WebOpened {
            url: url.clone(),
            outcome: Err("over SSH".into()),
        });
        assert_eq!(
            app.notices.last(),
            Some(&Notice::Info(format!("Open {url} (over SSH)")))
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
            chat: chat(requested),
            messages: vec![message(4)],
        });
        assert_eq!(
            effects,
            vec![
                Effect::OpenStream {
                    chat: requested,
                    after_id: Some(4)
                },
                Effect::SendMessage {
                    chat: requested,
                    text: "reply".into(),
                    model: None,
                    busy: BusyBehavior::Queue,
                    turn: TurnOptions::default()
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
            chat: chat(requested),
            messages: vec![],
        });
        assert!(effects.contains(&Effect::SendMessage {
            chat: requested,
            text: "early".into(),
            model: None,
            busy: BusyBehavior::Queue,
            turn: TurnOptions::default()
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
    fn a_failed_send_or_create_stops_the_wait() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("hi".into()));
        assert_eq!(app.activity(), Some(Activity::Waiting));
        app.update(Msg::CreateFailed {
            message: "HTTP 500".into(),
        });
        assert_eq!(app.activity(), None);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
        });
        app.update(Msg::Submit("again".into()));
        app.update(Msg::SendFailed {
            text: "again".into(),
            message: "HTTP 409".into(),
            plan_mode: None,
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
            chat: chat(id),
            messages: vec![],
        });
        assert!(!app.plan_mode);
        assert_eq!(
            app.update(Msg::Command(Command::PlanMode(None))),
            vec![Effect::SetPlanMode { chat: id, on: true }]
        );
        assert!(app.plan_mode);
        assert!(
            app.update(Msg::Command(Command::PlanMode(Some(true))))
                .is_empty()
        );
        assert!(matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("already on")));
        assert_eq!(
            app.update(Msg::Command(Command::PlanMode(Some(false)))),
            vec![Effect::SetPlanMode {
                chat: id,
                on: false
            }]
        );
        assert!(!app.plan_mode);
    }

    #[test]
    fn a_chat_loaded_in_plan_mode_shows_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
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
                }
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
            effects.contains(&Effect::SetPlanMode { chat: id, on: true }),
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
                ..Default::default()
            }
        }));
    }

    #[test]
    fn a_failed_send_that_carried_plan_mode_reverts_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("one".into()));
        app.update(Msg::Submit("two".into()));
        app.update(Msg::Command(Command::PlanMode(Some(true))));
        app.update(Msg::ChatCreated(chat(Uuid::new_v4())));
        assert!(app.plan_mode);
        let effects = app.update(Msg::SendFailed {
            text: "two".into(),
            message: "HTTP 500".into(),
            plan_mode: Some(true),
        });
        assert_eq!(effects, vec![Effect::RestoreComposer("two".into())]);
        assert!(!app.plan_mode);
        assert!(
            matches!(app.notices.last(), Some(Notice::Error(m)) if m.contains("Could not send the message") && m.contains("Plan mode is still off")),
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
        app.update(Msg::Refresh);
        app.update(Msg::Submit("continue".into()));
        app.update(Msg::SendFailed {
            text: "continue".into(),
            message: "HTTP 500".into(),
            plan_mode: None,
        });
        assert!(app.plan_mode);
    }

    #[test]
    fn a_failed_send_without_plan_mode_leaves_it_alone() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat_with_plan(id, "plan"),
            messages: vec![],
        });
        app.update(Msg::Submit("two".into()));
        app.update(Msg::SendFailed {
            text: "two".into(),
            message: "HTTP 500".into(),
            plan_mode: None,
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
    fn a_failed_plan_mode_update_reverts() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::ChatLoaded {
            chat: chat(Uuid::new_v4()),
            messages: vec![],
        });
        app.update(Msg::Command(Command::PlanMode(Some(true))));
        app.update(Msg::PlanModeFailed {
            on: true,
            message: "HTTP 500".into(),
        });
        assert!(!app.plan_mode);
        assert!(matches!(app.notices.last(), Some(Notice::Error(m)) if m.contains("plan mode on")));
    }

    #[test]
    fn stream_events_from_the_old_chat_never_reach_a_new_one() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let old = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(old),
            messages: vec![message(1)],
        });
        app.update(ev(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        let effects = app.update(Msg::Command(Command::New));
        assert!(effects.contains(&Effect::CloseStream), "{effects:?}");
        assert!(effects.contains(&Effect::ClearView), "{effects:?}");
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Interrupt(_))),
            "/new leaves the old chat running"
        );
        assert_eq!(app.chat_id, None);
        assert_eq!(app.transcript.messages().count(), 0);
        let late = |msg: Msg| Msg::ForChat {
            chat: old,
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
            chat: chat(id),
            messages: vec![],
        });
        app.update(Msg::ForChat {
            chat: id,
            msg: Box::new(ev(
                json!({"type": "status", "status": {"status": "running"}}),
            )),
        });
        assert_eq!(app.activity(), Some(Activity::Working));
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
        }]));
        app.update(Msg::ChatLoaded {
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
            chat: chat_with_plan(old, "plan"),
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::PlanMode(Some(false)))),
            vec![Effect::SetPlanMode {
                chat: old,
                on: false
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
        assert!(
            !app.notices
                .iter()
                .any(|n| matches!(n, Notice::Error(m) if m.contains("plan mode")))
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
            chat: open,
            messages: vec![],
        });
        let effects = app.update(Msg::Command(Command::New));
        assert_eq!(effects, vec![Effect::CloseStream, Effect::ClearView]);
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
}
