//! The rows of the read-only panels: `/info`, `/usage`, `/workspace`, `/git`, and `/mcp`.

use std::collections::BTreeMap;

use coder_sdk::types;
use uuid::Uuid;

use crate::app::{App, CostState, is_required, workspace_mcp};
use crate::time;
use crate::usage;
use crate::usage::LimitState;

/// A panel section fetched when the panel opens.
#[derive(Debug, Clone)]
pub enum Fetched<T> {
    Loading,
    Loaded(T),
    Failed(String),
}

// Not derived: a derive would also require `T: Default`.
#[expect(clippy::derivable_impls)]
impl<T> Default for Fetched<T> {
    fn default() -> Self {
        Fetched::Loading
    }
}

/// The web UI's SSH command, or `coder ssh` when the agent or hostname suffix is unknown.
/// The fallback names the agent when it is known, since a workspace with several agents
/// refuses `coder ssh owner/workspace`.
pub fn ssh_command(
    agent: Option<&str>,
    workspace: &str,
    owner: &str,
    suffix: Option<&str>,
) -> String {
    match (agent, suffix.filter(|s| !s.is_empty())) {
        (Some(agent), Some(suffix)) => format!("ssh {agent}.{workspace}.{owner}.{suffix}"),
        (Some(agent), None) => format!("coder ssh {owner}/{workspace}.{agent}"),
        (None, _) => format!("coder ssh {owner}/{workspace}"),
    }
}

/// The agents in the workspace's latest build.
fn agents(ws: &types::CodersdkWorkspace) -> Vec<&types::CodersdkWorkspaceAgent> {
    ws.latest_build
        .iter()
        .flat_map(|b| b.resources.iter())
        .flat_map(|r| r.agents.iter())
        .collect()
}

/// The names of the agents in the workspace's latest build.
pub fn workspace_agent_names(ws: &types::CodersdkWorkspace) -> Vec<String> {
    agents(ws).iter().filter_map(|a| a.name.clone()).collect()
}

/// The name of agent `agent` in the workspace's latest build, else of its only agent. With
/// several agents and none named, no agent is guessed.
pub fn workspace_agent(ws: &types::CodersdkWorkspace, agent: Option<Uuid>) -> Option<String> {
    let agents = agents(ws);
    agent
        .and_then(|id| agents.iter().find(|a| a.id == Some(id)))
        .or(match agents.as_slice() {
            [only] => Some(only),
            _ => None,
        })
        .and_then(|a| a.name.clone())
}

/// The `/workspace` details as label and value pairs.
pub fn workspace_lines(app: &App) -> Vec<(&'static str, String)> {
    let ws = match app.workspace_panel.as_ref() {
        Some(Fetched::Loaded(ws)) => ws,
        Some(Fetched::Failed(message)) => {
            return vec![("Workspace", format!("unavailable: {message}"))];
        }
        _ => return vec![("Workspace", "Loading…".into())],
    };
    let name = ws.name.clone().unwrap_or_default();
    let owner = ws.owner_name.clone().unwrap_or_default();
    let template = ws
        .template_display_name
        .clone()
        .filter(|t| !t.trim().is_empty())
        .or(ws.template_name.clone())
        .unwrap_or_default();
    let outdated = if ws.outdated == Some(true) {
        " (outdated)"
    } else {
        ""
    };
    let status = ws
        .latest_build
        .as_ref()
        .and_then(|b| b.status.as_ref())
        .map(|s| s.0.clone())
        .unwrap_or_else(|| "unknown".into());
    let health = match ws.health.as_ref().and_then(|h| h.healthy) {
        Some(true) => ", healthy",
        Some(false) => ", unhealthy",
        None => "",
    };
    let agent = app.chat.as_ref().and_then(|c| c.agent_id);
    vec![
        ("Workspace", format!("{name}, owned by {owner}")),
        ("Template", format!("{template}{outdated}")),
        ("Status", format!("{status}{health}")),
        (
            "Agent",
            workspace_agent(ws, agent).unwrap_or_else(|| "none".into()),
        ),
    ]
}

/// The context row of `/info` and `/usage`.
fn context_text(app: &App) -> String {
    match usage::context_usage(app.transcript.messages()) {
        Some(u) => match u.limit {
            Some(limit) => format!(
                "{} of {} tokens",
                usage::format_tokens(u.used),
                usage::format_tokens(limit)
            ),
            None => format!("{} tokens", usage::format_tokens(u.used)),
        },
        None => "unknown".into(),
    }
}

/// The cost row of `/info` and `/usage`, saying for a subagent that it is the whole tree's.
fn cost_text(cost: &types::CodersdkChatCost, subagent: bool) -> String {
    let mut text = format!(
        "{} over {}",
        usage::format_cost_micros(cost.total_cost_micros.unwrap_or(0)),
        usage::count(cost.request_count.unwrap_or(0), "request")
    );
    if subagent {
        text.push_str(", for the whole chat tree");
    }
    if let Some(n) = cost.unpriced_request_count.filter(|n| *n > 0) {
        text.push_str(&format!(
            ". Excludes unpriced usage from {}.",
            usage::count(n, "request")
        ));
    }
    text
}

/// The `/info` panel as label and value pairs, in the order shown. Times are shown at
/// `offset`, which the caller reads from the local time zone.
pub fn info_lines(
    app: &App,
    now_unix: i64,
    offset: chrono::FixedOffset,
) -> Vec<(&'static str, String)> {
    let Some(chat) = app.chat.as_deref() else {
        return vec![("Chat", "Start a chat first.".into())];
    };
    let mut lines = vec![
        (
            "Title",
            chat.title.clone().unwrap_or_else(|| "Untitled".into()),
        ),
        ("ID", chat.id.map(|id| id.to_string()).unwrap_or_default()),
    ];
    let summary = chat
        .summary
        .as_deref()
        .or(chat.last_turn_summary.as_deref())
        .unwrap_or_default();
    // Each line of the summary is its own row, continuing under the label.
    let summary_lines = summary.lines().map(str::trim_end).filter(|l| !l.is_empty());
    for (i, line) in summary_lines.enumerate() {
        lines.push((if i == 0 { "Summary" } else { "" }, line.to_owned()));
    }
    if let Some(parent) = chat.parent_chat_id {
        let title = app.chats.find(parent).and_then(|p| p.title.clone());
        lines.push(("Parent", title.unwrap_or_else(|| parent.to_string())));
    }
    lines.push(("Organization", app.org_label(chat.organization_id)));
    lines.push(("Owner", chat.owner_username.clone().unwrap_or_default()));
    let model = chat
        .last_model_config_id
        .and_then(|id| app.models.iter().find(|m| m.id == Some(id)))
        .and_then(|m| m.display_name.clone().or_else(|| m.model.clone()))
        .unwrap_or_else(|| "unknown".into());
    let effort = chat
        .last_reasoning_effort
        .as_ref()
        .map(|e| format!(", {e} effort"))
        .unwrap_or_default();
    lines.push(("Model", format!("{model}{effort}")));
    lines.push(("Plan mode", if app.plan_mode { "on" } else { "off" }.into()));
    let workspace = chat.workspace_id.map(|id| {
        app.workspaces
            .iter()
            .find(|w| w.id == id)
            .map(|w| w.name.clone())
            .unwrap_or_else(|| id.to_string())
    });
    lines.push(("Workspace", workspace.unwrap_or_else(|| "none".into())));
    for (label, when) in [("Created", chat.created_at), ("Updated", chat.updated_at)] {
        if let Some(when) = when {
            let ago = time::ago(when.timestamp(), now_unix);
            lines.push((label, format!("{} ({ago})", time::local(when, offset))));
        }
    }
    lines.push(("Context", context_text(app)));
    match app.info_panel.as_ref() {
        Some(CostState::Loading) => lines.push(("Cost", "…".into())),
        Some(CostState::Loaded(cost)) => {
            lines.push(("Cost", cost_text(cost, chat.parent_chat_id.is_some())))
        }
        Some(CostState::Failed(message)) => lines.push(("Cost", format!("unavailable: {message}"))),
        Some(CostState::Hidden) | None => {}
    }
    if let Some(diff) = chat.diff_status.as_ref() {
        let what = diff
            .pr_number
            .map(|n| format!("#{n}"))
            .unwrap_or_else(|| "branch".into());
        lines.push((
            "Changes",
            format!(
                "{what}, +{} -{}",
                diff.additions.unwrap_or(0),
                diff.deletions.unwrap_or(0)
            ),
        ));
    }
    if !chat.warnings.is_empty() {
        lines.push(("Warnings", chat.warnings.join("; ")));
    }
    lines
}

/// Pushes the row of a limit in `state`: none when the deployment lacks it, the server's
/// message when it refuses it, and `loaded`'s rows once it is known. After a rejected token
/// (`stopped`) nothing is asked for again, so a limit still unknown reads "unavailable"
/// instead of loading forever, under the Notice row that says why.
fn limit_row<T>(
    lines: &mut Vec<(&'static str, String)>,
    label: &'static str,
    state: &LimitState<T>,
    stopped: bool,
    loaded: impl FnOnce(&T, &mut Vec<(&'static str, String)>),
) {
    match state {
        LimitState::Absent => {}
        LimitState::Unknown if stopped => lines.push((label, "unavailable".into())),
        LimitState::Failed(message) if stopped && message == usage::UNAUTHORIZED => {
            lines.push((label, "unavailable".into()))
        }
        LimitState::Unknown => lines.push((label, "Loading…".into())),
        LimitState::Refused(message) => lines.push((label, message.clone())),
        LimitState::Failed(message) => lines.push((label, format!("unavailable: {message}"))),
        LimitState::Loaded(value) => loaded(value, lines),
    }
}

/// The `/usage` panel as label and value pairs: the AI spend with its period and budget, the
/// open chat's cost and context, and the workspace quota. A limit the deployment lacks shows
/// no row, and one it refuses shows the server's message. After a rejected token, the first
/// row says the limits stopped updating. Times are shown at `offset`.
pub fn usage_lines(
    app: &App,
    now_unix: i64,
    offset: chrono::FixedOffset,
) -> Vec<(&'static str, String)> {
    let stopped = app.limits_stopped();
    let mut lines = Vec::new();
    if stopped {
        lines.push(("Notice", crate::app::LIMITS_STOPPED.into()));
    }
    limit_row(
        &mut lines,
        "AI spend",
        app.spend(),
        stopped,
        |spend, lines| {
            lines.push(("AI spend", usage::spend_summary(spend)));
            if let (Some(start), Some(end)) = (spend.period_start, spend.period_end) {
                lines.push((
                    "Period",
                    format!(
                        "{} to {}, resets {}",
                        time::local(start, offset),
                        time::local(end, offset),
                        time::until(end.timestamp(), now_unix)
                    ),
                ));
            }
            if let Some(source) = usage::budget_source(spend) {
                lines.push(("Budget", source));
            }
        },
    );
    let loading = app.chat.is_none() && app.is_loading_chat();
    match (app.chat.as_deref(), app.chat_cost.as_ref()) {
        (None, _) if loading => lines.push(("Chat cost", "Loading…".into())),
        (None, _) => lines.push(("Chat cost", "Start a chat first.".into())),
        (Some(chat), Some(CostState::Loaded(cost))) => {
            lines.push(("Chat cost", cost_text(cost, chat.parent_chat_id.is_some())))
        }
        (Some(_), Some(CostState::Failed(message)))
            if stopped && message == usage::UNAUTHORIZED =>
        {
            lines.push(("Chat cost", "unavailable".into()))
        }
        (Some(_), Some(CostState::Failed(message))) => {
            lines.push(("Chat cost", format!("unavailable: {message}")))
        }
        (Some(_), Some(CostState::Hidden)) => {}
        (Some(_), Some(CostState::Loading) | None) if stopped => {
            lines.push(("Chat cost", "unavailable".into()))
        }
        (Some(_), Some(CostState::Loading) | None) => lines.push(("Chat cost", "Loading…".into())),
    }
    if app.chat.is_some() {
        lines.push(("Context", context_text(app)));
    } else if loading {
        lines.push(("Context", "Loading…".into()));
    }
    limit_row(
        &mut lines,
        "Workspace quota",
        app.quota(),
        stopped,
        |quota, lines| lines.push(("Workspace quota", usage::quota_summary(quota))),
    );
    lines
}

/// The workspace's local changes, streamed while `/git` is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalGit {
    Connecting,
    Live,
    NoWorkspace,
    /// The socket ended or was refused, with why.
    Ended(String),
}

/// The `/git` panel: the chat's diff from the server and the workspace's local changes.
#[derive(Debug, Clone)]
pub struct GitPanel {
    pub diff: Fetched<Box<types::CodersdkChatDiffContents>>,
    /// Local changes by repository root, merged from the socket's deltas.
    pub repos: BTreeMap<String, types::CodersdkWorkspaceAgentRepoChanges>,
    pub local: LocalGit,
}

/// Added and removed lines in a unified diff, not counting the `+++` and `---` headers.
pub fn diff_counts(diff: &str) -> (usize, usize) {
    diff.lines().fold((0, 0), |(add, del), line| {
        if line.starts_with("+++") || line.starts_with("---") {
            (add, del)
        } else if line.starts_with('+') {
            (add + 1, del)
        } else if line.starts_with('-') {
            (add, del + 1)
        } else {
            (add, del)
        }
    })
}

/// The diff to page: the server's diff, else the local changes, else nothing.
pub fn diff_text(panel: &GitPanel) -> Option<String> {
    let server = match &panel.diff {
        Fetched::Loaded(d) => d.diff.clone().filter(|d| !d.trim().is_empty()),
        _ => None,
    };
    server.or_else(|| {
        let local: String = panel
            .repos
            .values()
            .filter_map(|r| r.unified_diff.clone())
            .collect::<Vec<_>>()
            .join("\n");
        (!local.trim().is_empty()).then_some(local)
    })
}

/// The label of the `/git` row that names the pull request, which the TUI finds to lead it
/// with the state's icon.
pub const PULL_REQUEST: &str = "Pull request";

/// The `/git` panel as label and value pairs.
pub fn git_lines(app: &App) -> Vec<(&'static str, String)> {
    let Some(panel) = app.git_panel.as_ref() else {
        return vec![];
    };
    let status = app.chat.as_ref().and_then(|c| c.diff_status.as_ref());
    let mut lines = Vec::new();
    let contents = match &panel.diff {
        Fetched::Loaded(d) => Some(d),
        Fetched::Loading => {
            lines.push(("Diff", "Loading…".into()));
            None
        }
        Fetched::Failed(message) => {
            lines.push(("Diff", format!("unavailable: {message}")));
            None
        }
    };
    if let Some(d) = contents {
        if let Some(origin) = d.remote_origin.clone().filter(|o| !o.is_empty()) {
            lines.push(("Repository", origin));
        }
        if let Some(provider) = d.provider.clone().filter(|p| !p.is_empty()) {
            lines.push(("Provider", provider));
        }
    }
    let head = status
        .and_then(|s| s.head_branch.clone())
        .or_else(|| contents.and_then(|d| d.branch.clone()))
        .filter(|b| !b.is_empty());
    if let Some(head) = head {
        let base = status
            .and_then(|s| s.base_branch.clone())
            .filter(|b| !b.is_empty());
        lines.push((
            "Branch",
            match base {
                Some(base) => format!("{head} into {base}"),
                None => head,
            },
        ));
    }
    if let Some(s) = status {
        if let Some(n) = s.pr_number {
            let mut marks: Vec<String> = s.pull_request_state.iter().cloned().collect();
            if s.pull_request_draft == Some(true) {
                marks.push("draft".into());
            }
            if s.approved == Some(true) {
                marks.push("approved".into());
            }
            if s.changes_requested == Some(true) {
                marks.push("changes requested".into());
            }
            let title = s.pull_request_title.clone().unwrap_or_default();
            lines.push((PULL_REQUEST, format!("#{n} {title} ({})", marks.join(", "))));
        }
        lines.push((
            "Size",
            format!(
                "+{} -{}, {} files, {} commits",
                s.additions.unwrap_or(0),
                s.deletions.unwrap_or(0),
                s.changed_files.unwrap_or(0),
                s.commits.unwrap_or(0)
            ),
        ));
    }
    if let Some(d) = contents
        && d.diff.as_deref().unwrap_or_default().trim().is_empty()
    {
        let changed =
            status.is_some_and(|s| s.additions.unwrap_or(0) + s.deletions.unwrap_or(0) > 0);
        let text = match d.provider.clone().filter(|p| !p.is_empty()) {
            Some(provider) if changed => {
                format!("Link your {provider} account in Coder to see the diff.")
            }
            _ => "No git changes for this chat yet.".into(),
        };
        lines.push(("Diff", text));
    }
    match &panel.local {
        LocalGit::NoWorkspace => lines.push((
            "Local changes",
            "Attach a workspace to see local changes.".into(),
        )),
        LocalGit::Connecting if panel.repos.is_empty() => {
            lines.push(("Local changes", "Connecting…".into()))
        }
        LocalGit::Ended(message) if panel.repos.is_empty() => {
            lines.push(("Local changes", message.clone()))
        }
        _ if panel.repos.is_empty() => lines.push(("Local changes", "none".into())),
        _ => {
            for (root, repo) in &panel.repos {
                let (add, del) = diff_counts(repo.unified_diff.as_deref().unwrap_or_default());
                let branch = repo.branch.clone().unwrap_or_default();
                lines.push(("Local changes", format!("{root} ({branch}): +{add} -{del}")));
            }
            if let LocalGit::Ended(message) = &panel.local {
                lines.push(("Local changes", format!("stopped updating: {message}")));
            }
        }
    }
    lines
}

/// The `/mcp` panel's fetched sections.
#[derive(Debug, Clone)]
pub struct McpPanel {
    pub servers: Fetched<Vec<types::CodersdkMcpServerConfig>>,
    /// The newest debug run's connect outcomes; `Loaded(None)` when the server reports none.
    pub health: Fetched<Option<Vec<coder_sdk::McpConnectOutcome>>>,
}

/// One MCP server in the `/mcp` panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpRow {
    /// Set for an organization server, which `Msg::ToggleMcp` turns on or off.
    pub id: Option<Uuid>,
    pub name: String,
    pub url: String,
    pub state: String,
    pub detail: String,
    /// Whether the server is on for the next message, which `state` words.
    pub on: bool,
    /// Whether the server's last connection failed, which `detail` explains.
    pub failed: bool,
}

/// A titled group of `/mcp` rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpGroup {
    pub title: &'static str,
    pub rows: Vec<McpRow>,
}

fn outcome_text(o: &coder_sdk::McpConnectOutcome) -> String {
    match o.outcome.as_str() {
        "connected" => format!("connected, {} tools", o.tool_count),
        "no_tools" => "connected, no tools".into(),
        "timeout" => "timed out connecting".into(),
        "error" => format!("failed: {}", o.error),
        other => other.replace('_', " "),
    }
}

fn tool_lists(allow: &[String], deny: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    if !allow.is_empty() {
        out.push(format!("allow: {}", allow.join(", ")));
    }
    if !deny.is_empty() {
        out.push(format!("deny: {}", deny.join(", ")));
    }
    out
}

/// The `/mcp` groups, empty groups left out, and a note for the status line. On a blank chat
/// the organization servers show as the chat's first message turns them on, and there are no
/// inline or workspace servers yet.
pub fn mcp_groups(app: &App) -> (Vec<McpGroup>, Option<String>) {
    let Some(panel) = app.mcp_panel.as_ref() else {
        return (vec![], None);
    };
    let chat = app.chat.as_deref();
    let outcomes = match &panel.health {
        Fetched::Loaded(Some(list)) => list.as_slice(),
        _ => &[],
    };
    let mut groups = Vec::new();
    // A blank chat has no connect outcomes to report yet.
    let mut note = match (&panel.health, chat) {
        (_, None) => None,
        (Fetched::Loaded(None), Some(_)) => {
            Some("Connection health is not reported by the server.".to_owned())
        }
        (Fetched::Failed(message), Some(_)) => {
            Some(format!("Connection health is unavailable: {message}"))
        }
        _ => None,
    };
    match &panel.servers {
        Fetched::Loaded(servers) => {
            let current = app.mcp_selection();
            let selected = app.mcp_next.as_deref().unwrap_or(current.as_slice());
            let rows: Vec<McpRow> = servers
                .iter()
                .filter(|s| s.enabled != Some(false))
                .map(|s| {
                    let required = is_required(s);
                    let on = required || s.id.is_some_and(|id| selected.contains(&id));
                    // Marked only where the next message changes an open chat's row.
                    let pending = chat.is_some()
                        && app.mcp_next.is_some()
                        && on != s.id.is_some_and(|id| current.contains(&id));
                    let mut detail = Vec::new();
                    let outcome = outcomes.iter().find(|o| Some(o.config_id) == s.id);
                    let reconnect = s.auth_connected == Some(false);
                    if reconnect {
                        detail.push("needs reconnecting in the web UI".to_owned());
                    } else if let Some(o) = outcome {
                        detail.push(outcome_text(o));
                    }
                    detail.extend(tool_lists(&s.tool_allow_list, &s.tool_deny_list));
                    McpRow {
                        id: s.id,
                        name: s
                            .display_name
                            .clone()
                            .or(s.slug.clone())
                            .unwrap_or_default(),
                        url: s.url.clone().unwrap_or_default(),
                        state: match (on, required, pending) {
                            (true, true, _) => "on (required)".into(),
                            (true, false, true) => "on (next message)".into(),
                            (false, _, true) => "off (next message)".into(),
                            (true, false, false) => "on".into(),
                            (false, _, false) => "off".into(),
                        },
                        detail: detail.join("; "),
                        on,
                        failed: !reconnect && outcome.is_some_and(|o| o.outcome == "error"),
                    }
                })
                .collect();
            // The status line is one row, so these take it from the health note.
            if chat.is_none() {
                note = Some("The chat's first message turns on the servers shown as on.".into());
            } else if app.mcp_next.is_some() {
                note = Some("Your next message sends the organization servers shown as on.".into());
            }
            if !rows.is_empty() {
                groups.push(McpGroup {
                    title: "Organization servers",
                    rows,
                });
            }
        }
        Fetched::Loading => note = Some("Loading MCP servers…".into()),
        Fetched::Failed(message) => {
            note = Some(format!("Organization servers failed to load: {message}"))
        }
    }
    let inline: Vec<McpRow> = chat
        .map(|chat| {
            chat.inline_mcp_servers
                .iter()
                .map(|s| McpRow {
                    id: None,
                    name: s.slug.clone().unwrap_or_default(),
                    url: s.url.clone().unwrap_or_default(),
                    state: "on".into(),
                    detail: tool_lists(&s.tool_allow_list, &s.tool_deny_list).join("; "),
                    on: true,
                    failed: false,
                })
                .collect()
        })
        .unwrap_or_default();
    if !inline.is_empty() {
        groups.push(McpGroup {
            title: "Inline servers",
            rows: inline,
        });
    }
    let workspace: Vec<McpRow> = chat
        .map(|chat| {
            workspace_mcp(chat)
                .map(|r| {
                    let status = r
                        .status
                        .as_ref()
                        .map(|s| s.as_str().to_owned())
                        .unwrap_or_default();
                    let tools: Vec<String> =
                        r.tools.iter().filter_map(|t| t.name.clone()).collect();
                    let error = r.error.clone().filter(|e| !e.is_empty());
                    let failed = error.is_some();
                    let detail = match error {
                        Some(error) => format!("{status}: {error}"),
                        None if !tools.is_empty() => {
                            format!("{} tools: {}", tools.len(), tools.join(", "))
                        }
                        None => status,
                    };
                    McpRow {
                        id: None,
                        name: r.source.clone().unwrap_or_default(),
                        url: String::new(),
                        state: "on".into(),
                        detail,
                        on: true,
                        failed,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    if !workspace.is_empty() {
        groups.push(McpGroup {
            title: "Workspace servers",
            rows: workspace,
        });
    }
    if groups.is_empty() && matches!(panel.servers, Fetched::Loaded(_)) {
        note = Some(
            match chat {
                Some(_) => "This chat uses no MCP servers.",
                None => "Your organization has no MCP servers.",
            }
            .into(),
        );
    }
    (groups, note)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{CostState, Msg};
    use crate::config::BusyBehavior;
    use serde_json::json;

    fn value<'l>(lines: &'l [(&'static str, String)], label: &str) -> Option<&'l str> {
        lines
            .iter()
            .find(|(l, _)| *l == label)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn a_summary_shows_each_of_its_lines() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let chat = serde_json::from_value(json!({
            "id": uuid::Uuid::new_v4(), "title": "explore",
            "summary": "Found every caller.\n\nFixed two.\r\n",
            "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}
        }))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: vec![],
        });
        let lines = info_lines(&app, 0, chrono::FixedOffset::east_opt(0).unwrap());
        let at = lines.iter().position(|(l, _)| *l == "Summary").unwrap();
        assert_eq!(
            lines[at..at + 3],
            [
                ("Summary", "Found every caller.".to_owned()),
                ("", "Fixed two.".to_owned()),
                ("Organization", "this organization".to_owned()),
            ],
            "blank lines are dropped and the rest continue under the label"
        );
    }

    #[test]
    fn usage_shows_spend_its_period_and_budget_the_chat_cost_context_and_quota() {
        use crate::usage::LimitState;
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = uuid::Uuid::new_v4();
        app.update(Msg::Started {
            org_id: org,
            open_chat: None,
        });
        let chat = serde_json::from_value(json!({
            "id": uuid::Uuid::new_v4(), "parent_chat_id": uuid::Uuid::new_v4(),
            "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}
        }))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: serde_json::from_value(json!([{"id": 1, "role": "assistant", "content": [], "usage": {"input_tokens": 12000, "context_limit": 200000}}])).unwrap(),
        });
        app.update(Msg::RefreshLimits);
        app.update(Msg::SpendLoaded {
            spend: Box::new(
                serde_json::from_value(json!({
                    "current_spend_micros": 1_200_000,
                    "effective_budget": {"spend_limit_micros": 50_000_000, "limit_source": "user_override"},
                    "period_start": "2026-10-01T00:00:00Z", "period_end": "2026-11-01T00:00:00Z"
                }))
                .unwrap(),
            ),
            generation: 1,
        });
        app.update(Msg::QuotaLoaded {
            org,
            quota: serde_json::from_value(json!({"credits_consumed": 3, "budget": -1})).unwrap(),
            generation: 1,
        });
        assert!(matches!(app.quota(), LimitState::Loaded(_)));
        app.chat_cost = Some(CostState::Loaded(
            serde_json::from_value(json!({"total_cost_micros": 1230000, "request_count": 4, "unpriced_request_count": 1})).unwrap(),
        ));
        let now = "2026-10-02T12:00:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap()
            .timestamp();
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        let lines = usage_lines(&app, now, utc);
        assert_eq!(value(&lines, "AI spend"), Some("$1.20 of $50.00 (2%)"));
        assert_eq!(
            value(&lines, "Period"),
            Some("2026-10-01 00:00 to 2026-11-01 00:00, resets in 29d")
        );
        assert_eq!(
            value(&lines, "Budget"),
            Some("Set for you, in place of your group's budget")
        );
        assert_eq!(
            value(&lines, "Chat cost"),
            Some(
                "$1.23 over 4 requests, for the whole chat tree. Excludes unpriced usage from 1 request."
            )
        );
        assert_eq!(value(&lines, "Context"), Some("12.0k of 200.0k tokens"));
        assert_eq!(
            value(&lines, "Workspace quota"),
            Some("No quota applies; your workspaces use 3 credits")
        );
    }

    #[test]
    fn usage_hides_what_the_deployment_lacks_and_says_what_it_refuses() {
        use crate::usage::{Limit, Refusal};
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        let lines = usage_lines(&app, 0, utc);
        assert_eq!(value(&lines, "AI spend"), Some("Loading…"));
        assert_eq!(value(&lines, "Chat cost"), Some("Start a chat first."));
        assert_eq!(
            value(&lines, "Context"),
            None,
            "a blank chat has no context"
        );
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
        let lines = usage_lines(&app, 0, utc);
        assert_eq!(
            value(&lines, "AI spend"),
            Some("AI Gateway is a Premium feature. Contact sales!")
        );
        assert_eq!(
            value(&lines, "Workspace quota"),
            None,
            "an open-source deployment has no quota"
        );
        app.update(Msg::RefreshLimits);
        app.update(Msg::LimitFailed {
            limit: Limit::Spend,
            refusal: Refusal::Failed("HTTP 502".into()),
            generation: 2,
        });
        assert_eq!(
            value(&usage_lines(&app, 0, utc), "AI spend"),
            Some("AI Gateway is a Premium feature. Contact sales!"),
            "a refusal is not asked for again, so its message stays"
        );
    }

    #[test]
    fn usage_says_loading_while_a_switched_to_chat_loads() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        let lines = usage_lines(&app, 0, utc);
        assert_eq!(value(&lines, "Chat cost"), Some("Start a chat first."));
        assert_eq!(value(&lines, "Context"), None);
        app.update(Msg::OpenChat(uuid::Uuid::new_v4()));
        let lines = usage_lines(&app, 0, utc);
        assert_eq!(value(&lines, "Chat cost"), Some("Loading…"));
        assert_eq!(
            value(&lines, "Context"),
            Some("Loading…"),
            "the row stays through the switch"
        );
    }

    #[test]
    fn usage_says_loading_for_a_cost_in_flight_and_what_a_rejected_token_stopped() {
        use crate::usage::{Limit, Refusal};
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let chat = serde_json::from_value(json!({
            "id": uuid::Uuid::new_v4(),
            "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}
        }))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: vec![],
        });
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        let lines = usage_lines(&app, 0, utc);
        assert_eq!(value(&lines, "Chat cost"), Some("Loading…"));
        assert_eq!(value(&lines, "Workspace quota"), Some("Loading…"));
        assert_eq!(value(&lines, "Notice"), None);
        app.update(Msg::RefreshLimits);
        app.update(Msg::LimitFailed {
            limit: Limit::Spend,
            refusal: Refusal::Unauthorized,
            generation: 1,
        });
        let lines = usage_lines(&app, 0, utc);
        assert_eq!(value(&lines, "Notice"), Some(crate::app::LIMITS_STOPPED));
        // The Notice row says why, so the rows under it do not repeat it.
        let rejected = "unavailable";
        assert_eq!(value(&lines, "AI spend"), Some(rejected));
        assert_eq!(
            value(&lines, "Workspace quota"),
            Some(rejected),
            "nothing will be asked for, so no row waits forever"
        );
        assert_eq!(value(&lines, "Chat cost"), Some(rejected));
    }

    #[test]
    fn info_shows_the_chat_and_its_cost() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let parent = uuid::Uuid::new_v4();
        let chat = serde_json::from_value(json!({
            "id": uuid::Uuid::new_v4(), "title": "explore", "parent_chat_id": parent,
            "summary": "Found every watch caller.", "owner_username": "nick",
            "last_reasoning_effort": "high", "updated_at": "2026-09-30T14:00:00Z",
            "diff_status": {"pr_number": 42, "additions": 12, "deletions": 3},
            "warnings": ["The workspace is stopped."],
            "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}
        }))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: vec![],
        });
        app.info_panel = Some(CostState::Loaded(
            serde_json::from_value(json!({"total_cost_micros": 1230000, "request_count": 4, "unpriced_request_count": 1})).unwrap(),
        ));
        let now = "2026-09-30T14:05:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap()
            .timestamp();
        let offset = chrono::FixedOffset::west_opt(4 * 3600).unwrap();
        let lines = info_lines(&app, now, offset);
        assert_eq!(value(&lines, "Title"), Some("explore"));
        assert_eq!(value(&lines, "Summary"), Some("Found every watch caller."));
        assert_eq!(value(&lines, "Parent"), Some(parent.to_string().as_str()));
        assert_eq!(value(&lines, "Owner"), Some("nick"));
        assert_eq!(value(&lines, "Model"), Some("unknown, high effort"));
        assert_eq!(value(&lines, "Workspace"), Some("none"));
        assert!(value(&lines, "Updated").unwrap().ends_with("(5m ago)"));
        assert_eq!(
            value(&lines, "Updated"),
            Some("2026-09-30 10:00 (5m ago)"),
            "times are shown at the offset given"
        );
        assert_eq!(
            value(&lines, "Cost"),
            Some(
                "$1.23 over 4 requests, for the whole chat tree. Excludes unpriced usage from 1 request."
            )
        );
        assert_eq!(value(&lines, "Changes"), Some("#42, +12 -3"));
        assert_eq!(value(&lines, "Warnings"), Some("The workspace is stopped."));
        app.info_panel = Some(CostState::Loaded(
            serde_json::from_value(
                json!({"total_cost_micros": 4200, "request_count": 1, "unpriced_request_count": 2}),
            )
            .unwrap(),
        ));
        assert_eq!(
            value(&info_lines(&app, now, offset), "Cost"),
            Some(
                "$0.0042 over 1 request, for the whole chat tree. Excludes unpriced usage from 2 requests."
            ),
            "a sub-cent cost and singular and plural counts read like the web"
        );
        app.info_panel = Some(CostState::Hidden);
        assert_eq!(
            value(&info_lines(&app, now, offset), "Cost"),
            None,
            "no cost row without access"
        );
    }

    #[test]
    fn the_ssh_command_matches_the_web_ui_or_falls_back_to_coder_ssh() {
        assert_eq!(
            ssh_command(Some("main"), "dev", "nick", Some("coder")),
            "ssh main.dev.nick.coder"
        );
        assert_eq!(
            ssh_command(Some("main"), "dev", "nick", Some("")),
            "coder ssh nick/dev.main",
            "the fallback names the agent it knows"
        );
        assert_eq!(
            ssh_command(Some("main"), "dev", "nick", None),
            "coder ssh nick/dev.main"
        );
        assert_eq!(
            ssh_command(None, "dev", "nick", Some("coder")),
            "coder ssh nick/dev"
        );
    }

    #[test]
    fn workspace_details_name_the_template_status_and_the_chats_agent() {
        let agent = uuid::Uuid::new_v4();
        let ws: coder_sdk::types::CodersdkWorkspace = serde_json::from_value(json!({
            "name": "dev", "owner_name": "nick", "template_display_name": "Docker", "outdated": true,
            "health": {"healthy": false, "failing_agents": []}, "shared_with": [],
            "latest_build": {"status": "running", "resources": [{"agents": [
                {"id": uuid::Uuid::new_v4(), "name": "other", "apps": [], "display_apps": [], "environment_variables": {}, "latency": {}, "log_sources": [], "metadata": [], "scripts": [], "subsystems": []},
                {"id": agent, "name": "main", "apps": [], "display_apps": [], "environment_variables": {}, "latency": {}, "log_sources": [], "metadata": [], "scripts": [], "subsystems": []}
            ], "metadata": []}]}
        }))
        .unwrap();
        assert_eq!(workspace_agent(&ws, Some(agent)).as_deref(), Some("main"));
        assert_eq!(
            workspace_agent(&ws, None),
            None,
            "with several agents and none set, no agent is guessed"
        );
        let mut app = App::new(BusyBehavior::Queue, true);
        app.workspace_panel = Some(Fetched::Loaded(Box::new(ws)));
        let lines = workspace_lines(&app);
        assert_eq!(value(&lines, "Workspace"), Some("dev, owned by nick"));
        assert_eq!(value(&lines, "Template"), Some("Docker (outdated)"));
        assert_eq!(value(&lines, "Status"), Some("running, unhealthy"));
    }

    #[test]
    fn a_workspace_with_one_agent_names_it_without_one_set() {
        let ws: coder_sdk::types::CodersdkWorkspace = serde_json::from_value(json!({
            "name": "dev", "owner_name": "nick", "shared_with": [],
            "latest_build": {"resources": [{"agents": [
                {"id": uuid::Uuid::new_v4(), "name": "main", "apps": [], "display_apps": [], "environment_variables": {}, "latency": {}, "log_sources": [], "metadata": [], "scripts": [], "subsystems": []}
            ], "metadata": []}]}
        }))
        .unwrap();
        assert_eq!(workspace_agent(&ws, None).as_deref(), Some("main"));
        assert_eq!(
            workspace_agent(&ws, Some(uuid::Uuid::new_v4())).as_deref(),
            Some("main"),
            "an agent id not in the build falls back to the only agent"
        );
    }

    #[test]
    fn failed_workspace_details_say_why() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.workspace_panel = Some(Fetched::Failed("HTTP 502".into()));
        assert_eq!(
            value(&workspace_lines(&app), "Workspace"),
            Some("unavailable: HTTP 502")
        );
    }

    #[test]
    fn diff_counts_skip_the_file_headers() {
        let diff = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1,2 @@\n-old\n+new\n+more\n";
        assert_eq!(diff_counts(diff), (2, 1));
    }

    #[test]
    fn git_lines_explain_a_missing_token_and_list_local_changes() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let chat = serde_json::from_value(json!({
            "id": uuid::Uuid::new_v4(), "workspace_id": uuid::Uuid::new_v4(),
            "diff_status": {"pr_number": 7, "pull_request_title": "Fix the watch", "pull_request_state": "open",
                "pull_request_draft": true, "additions": 12, "deletions": 3, "changed_files": 2, "commits": 1,
                "head_branch": "m2", "base_branch": "main"},
            "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}
        }))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: vec![],
        });
        let mut repos = std::collections::BTreeMap::new();
        repos.insert(
            "/home/coder/scuttle".to_owned(),
            serde_json::from_value(json!({"repo_root": "/home/coder/scuttle", "branch": "m2",
                "unified_diff": "@@ -1 +1 @@\n-a\n+b\n"}))
            .unwrap(),
        );
        app.git_panel = Some(GitPanel {
            diff: Fetched::Loaded(Box::new(
                serde_json::from_value(
                    json!({"provider": "github", "remote_origin": "https://github.com/x/scuttle",
                    "branch": "m2", "diff": ""}),
                )
                .unwrap(),
            )),
            repos,
            local: LocalGit::Live,
        });
        let lines = git_lines(&app);
        assert_eq!(
            value(&lines, "Repository"),
            Some("https://github.com/x/scuttle")
        );
        assert_eq!(value(&lines, "Branch"), Some("m2 into main"));
        assert_eq!(
            value(&lines, "Pull request"),
            Some("#7 Fix the watch (open, draft)")
        );
        assert_eq!(value(&lines, "Size"), Some("+12 -3, 2 files, 1 commits"));
        assert_eq!(
            value(&lines, "Diff"),
            Some("Link your github account in Coder to see the diff.")
        );
        assert_eq!(
            value(&lines, "Local changes"),
            Some("/home/coder/scuttle (m2): +1 -1")
        );
    }

    #[test]
    fn mcp_groups_join_the_selection_with_health_and_list_every_source() {
        let (github, linear, docs) = (
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
        );
        let mut app = App::new(BusyBehavior::Queue, true);
        let chat = serde_json::from_value(json!({
            "id": uuid::Uuid::new_v4(), "mcp_server_ids": [github],
            "inline_mcp_servers": [{"slug": "local-tools", "url": "http://localhost:9000/mcp", "tool_allow_list": [], "tool_deny_list": []}],
            "context": {"resources": [{"kind": "mcp_server", "source": "playwright", "status": "error",
                "error": "spawn failed", "tools": []}]},
            "children": [], "files": [], "labels": {}
        }))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: vec![],
        });
        let server = |id,
                      name: &str,
                      availability: &str,
                      auth: bool|
         -> coder_sdk::types::CodersdkMcpServerConfig {
            serde_json::from_value(
                json!({"id": id, "display_name": name, "url": format!("https://{name}.example/mcp"),
                "availability": availability, "enabled": true, "auth_connected": auth,
                "tool_allow_list": [], "tool_deny_list": ["delete_repo"]}),
            )
            .unwrap()
        };
        app.mcp_panel = Some(McpPanel {
            servers: Fetched::Loaded(vec![
                server(github, "GitHub", "default_off", true),
                server(linear, "Linear", "default_off", false),
                server(docs, "Docs", "force_on", true),
            ]),
            health: Fetched::Loaded(Some(vec![coder_sdk::McpConnectOutcome {
                config_id: github,
                slug: "github".into(),
                outcome: "connected".into(),
                tool_count: 12,
                error: String::new(),
            }])),
        });
        let (groups, note) = mcp_groups(&app);
        assert_eq!(note, None);
        let titles: Vec<&str> = groups.iter().map(|g| g.title).collect();
        assert_eq!(
            titles,
            [
                "Organization servers",
                "Inline servers",
                "Workspace servers"
            ]
        );
        let org: Vec<(&str, &str, &str)> = groups[0]
            .rows
            .iter()
            .map(|r| (r.name.as_str(), r.state.as_str(), r.detail.as_str()))
            .collect();
        assert_eq!(
            org,
            [
                ("GitHub", "on", "connected, 12 tools; deny: delete_repo"),
                (
                    "Linear",
                    "off",
                    "needs reconnecting in the web UI; deny: delete_repo"
                ),
                ("Docs", "on (required)", "deny: delete_repo"),
            ]
        );
        assert_eq!(groups[1].rows[0].url, "http://localhost:9000/mcp");
        assert_eq!(groups[2].rows[0].detail, "error: spawn failed");
        app.mcp_panel.as_mut().unwrap().health = Fetched::Loaded(None);
        assert_eq!(
            mcp_groups(&app).1.as_deref(),
            Some("Connection health is not reported by the server.")
        );
        app.mcp_next = Some(vec![linear]);
        let (groups, note) = mcp_groups(&app);
        assert_eq!(groups[0].rows[0].state, "off (next message)");
        assert_eq!(groups[0].rows[1].state, "on (next message)");
        assert_eq!(groups[0].rows[2].state, "on (required)");
        assert_eq!(
            note.as_deref(),
            Some("Your next message sends the organization servers shown as on."),
            "the pending selection's note takes the single status line from the health note"
        );
        app.mcp_next = Some(vec![github, linear]);
        let (groups, _) = mcp_groups(&app);
        assert_eq!(groups[0].rows[0].state, "on", "GitHub does not change");
        assert_eq!(groups[0].rows[1].state, "on (next message)");
    }

    #[test]
    fn mcp_rows_say_whether_a_server_is_on_and_whether_it_failed() {
        let (github, linear) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let mut app = App::new(BusyBehavior::Queue, true);
        let chat = serde_json::from_value(json!({
            "id": uuid::Uuid::new_v4(), "mcp_server_ids": [github],
            "inline_mcp_servers": [{"slug": "local-tools", "url": "http://localhost:9000/mcp",
                "tool_allow_list": [], "tool_deny_list": []}],
            "context": {"resources": [{"kind": "mcp_server", "source": "playwright",
                "status": "error", "error": "spawn failed", "tools": []}]},
            "children": [], "files": [], "labels": {}
        }))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: vec![],
        });
        let server = |id, name: &str| -> coder_sdk::types::CodersdkMcpServerConfig {
            serde_json::from_value(json!({"id": id, "display_name": name,
                "url": format!("https://{name}.example/mcp"), "availability": "default_off",
                "enabled": true, "auth_connected": true, "tool_allow_list": [],
                "tool_deny_list": []}))
            .unwrap()
        };
        let outcome = |id, outcome: &str, error: &str| coder_sdk::McpConnectOutcome {
            config_id: id,
            slug: String::new(),
            outcome: outcome.into(),
            tool_count: 0,
            error: error.into(),
        };
        app.mcp_panel = Some(McpPanel {
            servers: Fetched::Loaded(vec![server(github, "GitHub"), server(linear, "Linear")]),
            health: Fetched::Loaded(Some(vec![
                outcome(github, "error", "refused"),
                outcome(linear, "connected", ""),
            ])),
        });
        let (groups, _) = mcp_groups(&app);
        let rows: Vec<(&str, bool, bool)> = groups
            .iter()
            .flat_map(|g| &g.rows)
            .map(|r| (r.name.as_str(), r.on, r.failed))
            .collect();
        assert_eq!(
            rows,
            [
                ("GitHub", true, true),
                ("Linear", false, false),
                ("local-tools", true, false),
                ("playwright", true, true),
            ]
        );
        assert_eq!(groups[0].rows[0].detail, "failed: refused");
    }
}
