//! The overlays drawn over the transcript: which one is open, its rows, and its keys.

use std::collections::HashSet;
use std::time::Duration;

use coder_sdk::{ChatStatus, types};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use scuttle_core::app::{App, ChatAction, GitAction, Msg, Picker, QueueAction, WorkspaceAction};
use scuttle_core::attachments::size_label;
use scuttle_core::chat_list::{
    ChatRow, Filter, ListQuery, Load, PrBadge, PrState, chat_status, pr_badge,
};
use scuttle_core::compaction::{self, Shown};
use scuttle_core::config::{FieldList, StatuslineConfig, Thresholds};
use scuttle_core::files::{self, ConflictChoice, FileAction, Place, SaveConflict, Sender};
use scuttle_core::fuzzy;
use unicode_width::UnicodeWidthStr;
use uuid::Uuid;

use crate::activity::spinner_frame;
use crate::icons::{self, Icon, IconSet};
use crate::table::{self, Row, RowKey, Spinner, TableKey, TableState, TableView};
use crate::theme::Theme;

/// Why a dimmed organization row is dim; choosing it anyway shows the core's refusal.
const NO_CHAT_PERMISSION: &str = "no permission to create chats";

/// What overlay rows are built from.
pub struct ViewCtx<'a> {
    pub app: &'a App,
    pub theme: &'a Theme,
    /// Seconds since the Unix epoch, for relative times.
    pub now_unix: i64,
    /// The local UTC offset, for times shown as a date and a minute.
    pub offset: chrono::FixedOffset,
    /// Time since the UI started, which picks spinner frames.
    pub elapsed: Duration,
    /// The terminal's width, which decides the optional columns, so they appear at the widths
    /// `/help` names.
    pub width: u16,
    /// The marker before a pinned chat in `/chats`, from `chats.pin_icon`.
    pub pin_icon: &'a str,
}

/// What a key pressed in an overlay asks the UI to do.
#[derive(Debug)]
pub enum OverlayOutcome {
    Stay,
    Close,
    /// Closes the overlay, then sends the message to the core.
    CloseWith(Msg),
    /// Sends the message to the core and keeps the overlay open.
    Send(Msg),
    /// Keeps the overlay open, and has the UI apply and save these footer settings.
    Statusline(StatuslineConfig),
}

pub enum Overlay {
    Model(TableState),
    Workspace(TableState),
    Organization(TableState),
    Chats(Box<ChatsState>),
    Subagents(SubagentsState),
    Queue(TableState),
    /// The read-only `/info` panel.
    Info(TableState),
    /// The attached workspace's details and actions.
    WorkspaceDetails(TableState),
    /// The `/git` panel: the chat's branch, pull request, and local changes.
    Git(TableState),
    /// The `/mcp` panel: the open chat's MCP servers and what is known of their health, or on
    /// a blank chat the organization's servers as the first message turns them on.
    Mcp(TableState),
    /// The `/statusline` editor: the footer's fields, their order, and their warnings.
    Statusline(StatuslineState),
    /// The read-only `/usage` panel.
    Usage(TableState),
    /// The `/files` panel: the chat's files, newest first.
    Files(TableState),
}

/// The columns `/model` draws, richest first. Each narrower set drops a column, so the model
/// name keeps its whole width for as long as it can.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelColumns {
    /// Name, tag, context window, threshold, and reasoning efforts.
    All,
    /// The efforts give way first.
    NoEfforts,
    /// Then the context window.
    Threshold,
    /// Then the threshold's note, so `70% (default)` reads `70%*`.
    Short,
}

/// The column the `/model` tag takes, wide enough for `current` and `default`.
const MODEL_TAG_WIDTH: u16 = 8;

/// The richest `/model` columns that leave the whole name visible in a terminal `width`
/// columns wide, else [`ModelColumns::Short`], which leaves the name at least the room it had
/// beside the tag and a five-column context window.
fn model_columns(width: u16, groups: &[scuttle_core::app::ModelGroup]) -> ModelColumns {
    let widest = |cell: &dyn Fn(&scuttle_core::app::ModelRow) -> String| {
        groups
            .iter()
            .flat_map(|g| &g.models)
            .map(|m| cell(m).width())
            .max()
            .unwrap_or(0)
    };
    let name = widest(&|m| format!("  {}", m.name));
    let context = widest(&|m| m.context.clone().unwrap_or_default());
    let label = widest(&|m| compaction_label(m.compaction));
    // The efforts may be cut, but get at least a few columns before they are drawn at all.
    let efforts = widest(&|m| m.efforts.join(", ")).min(6);
    // The margin, the border, and the selection mark take six columns, and one separates
    // each pair of columns.
    let room = usize::from(width).saturating_sub(6);
    let tag = usize::from(MODEL_TAG_WIDTH);
    let fits = |cells: &[usize]| cells.iter().sum::<usize>() + cells.len() - 1 <= room;
    if fits(&[name, tag, context, label, efforts]) {
        ModelColumns::All
    } else if fits(&[name, tag, context, label]) {
        ModelColumns::NoEfforts
    } else if fits(&[name, tag, label]) {
        ModelColumns::Threshold
    } else {
        ModelColumns::Short
    }
}

fn model_view(ctx: &ViewCtx, filter: &str) -> TableView {
    let app = ctx.app;
    let groups = app.model_groups(filter);
    let columns = model_columns(ctx.width, &groups);
    let widest = |cell: &dyn Fn(&scuttle_core::app::ModelRow) -> String| {
        groups
            .iter()
            .flat_map(|g| &g.models)
            .map(|m| cell(m).width())
            .max()
            .unwrap_or(0) as u16
    };
    let widths = match columns {
        ModelColumns::All => vec![
            Constraint::Length(widest(&|m| format!("  {}", m.name))),
            Constraint::Length(MODEL_TAG_WIDTH),
            Constraint::Length(widest(&|m| m.context.clone().unwrap_or_default())),
            Constraint::Length(widest(&|m| compaction_label(m.compaction))),
            Constraint::Fill(1),
        ],
        ModelColumns::NoEfforts => vec![
            Constraint::Fill(1),
            Constraint::Length(MODEL_TAG_WIDTH),
            Constraint::Length(widest(&|m| m.context.clone().unwrap_or_default())),
            Constraint::Length(widest(&|m| compaction_label(m.compaction))),
        ],
        ModelColumns::Threshold => vec![
            Constraint::Fill(1),
            Constraint::Length(MODEL_TAG_WIDTH),
            Constraint::Length(widest(&|m| compaction_label(m.compaction))),
        ],
        ModelColumns::Short => vec![
            Constraint::Fill(1),
            Constraint::Length(MODEL_TAG_WIDTH),
            Constraint::Length(widest(&|m| compaction_short_label(m.compaction))),
        ],
    };
    let mut rows = Vec::new();
    for group in groups {
        // The provider's row names it for every model under it, and says why its models are
        // dimmed when it cannot be used. It spans the table, so neither is cut to the width of
        // the model names.
        rows.push(Row::header(vec![Line::from(match &group.reason {
            Some(reason) => format!("{} \u{b7} {reason}", group.provider),
            None => group.provider.clone(),
        })]));
        for m in group.models {
            let tag = if m.current {
                "current"
            } else if m.default {
                "default"
            } else {
                ""
            };
            let mut cells = vec![Line::from(format!("  {}", m.name)), Line::from(tag)];
            match columns {
                ModelColumns::All => cells.extend([
                    Line::from(m.context.clone().unwrap_or_default()),
                    Line::from(compaction_label(m.compaction)),
                    Line::from(m.efforts.join(", ")),
                ]),
                ModelColumns::NoEfforts => cells.extend([
                    Line::from(m.context.clone().unwrap_or_default()),
                    Line::from(compaction_label(m.compaction)),
                ]),
                ModelColumns::Threshold => cells.push(Line::from(compaction_label(m.compaction))),
                ModelColumns::Short => {
                    cells.push(Line::from(compaction_short_label(m.compaction)));
                }
            }
            rows.push(if m.usable {
                Row::item(RowKey::Model(m.id), cells)
            } else {
                Row::disabled(RowKey::Model(m.id), cells)
            });
        }
    }
    let unsupported: Vec<String> = app
        .unsupported_providers
        .iter()
        .filter_map(|p| p.display_name.as_deref().or(p.provider.as_deref()))
        .map(files::display_name)
        .collect();
    let notes: Vec<String> = [
        (!unsupported.is_empty())
            .then(|| format!("Configured but not usable here: {}", unsupported.join(", "))),
        match app.compaction.state() {
            compaction::State::Failed(message) => Some(format!(
                "Compaction thresholds did not load: {}. Left or Right tries again.",
                files::display_name(message)
            )),
            _ => None,
        },
    ]
    .into_iter()
    .flatten()
    .collect();
    let note = (!notes.is_empty()).then(|| notes.join(" "));
    let status = if rows.is_empty() {
        Some(match &note {
            Some(note) => format!("No models match. {note}"),
            None => "No models match.".to_owned(),
        })
    } else {
        note
    };
    TableView {
        title: "Model".into(),
        widths,
        rows,
        status,
        hint: Some(first_fit(
            vec![
                "Enter picks the model for the next message, Left and Right move its compaction threshold by 5%, Delete restores the default, Esc closes".into(),
                "Enter picks · ←→ compaction threshold · Delete default · Esc closes".into(),
                "Enter · ←→ compaction · Esc".into(),
            ],
            usize::from(ctx.width).saturating_sub(4),
        )),
        filterable: true,
        ..Default::default()
    }
}

/// A model's compaction threshold as its `/model` cell shows it: the percent, what 100% and
/// 0% do, and `default` when no override of the user's applies.
/// A model's compaction threshold in a narrow `/model`: the percent alone, with `*` when it is
/// the model's default.
fn compaction_short_label(shown: Shown) -> String {
    match shown {
        Shown::Known { percent, default } => {
            format!("{percent}%{}", if default { "*" } else { "" })
        }
        other => compaction_label(other),
    }
}

fn compaction_label(shown: Shown) -> String {
    match shown {
        Shown::Loading => "…".to_owned(),
        Shown::Unknown => "?".to_owned(),
        Shown::Known { percent, default } => {
            let notes: Vec<&str> = [
                (percent == 100).then_some("never"),
                (percent == 0).then_some("every turn"),
                default.then_some("default"),
            ]
            .into_iter()
            .flatten()
            .collect();
            if notes.is_empty() {
                format!("{percent}%")
            } else {
                format!("{percent}% ({})", notes.join(", "))
            }
        }
    }
}

/// A key in `/model`. Left and Right step the highlighted model's compaction threshold and
/// Delete restores its default; the core holds the edit until the selection moves to another
/// row or the table closes, so a burst of presses sends one request. Backspace stays with the
/// filter, as in every filterable table.
fn model_key(state: &mut TableState, key: KeyEvent, ctx: &ViewCtx) -> OverlayOutcome {
    fn highlighted(state: &TableState, view: &TableView) -> Option<Uuid> {
        match state.selected_row(view).map(|r| r.key.clone()) {
            Some(RowKey::Model(id)) => Some(id),
            _ => None,
        }
    }
    let filter = state.filter.clone();
    let view = model_view(ctx, &filter);
    let before = highlighted(state, &view);
    let plain = !key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    match (key.code, before) {
        (KeyCode::Left | KeyCode::Right, Some(model)) if plain => {
            return OverlayOutcome::Send(Msg::ThresholdStep {
                model,
                up: key.code == KeyCode::Right,
            });
        }
        (KeyCode::Delete, Some(model)) if plain => {
            return OverlayOutcome::Send(Msg::ThresholdReset { model });
        }
        _ => {}
    }
    match state.handle_key(key, &view) {
        // A draft held for a model pick goes back to the composer, and an edit saves.
        TableKey::Esc => OverlayOutcome::CloseWith(Msg::ModelPickerClosed),
        TableKey::Enter => match before {
            Some(id) => OverlayOutcome::CloseWith(Msg::ModelChosen(id)),
            None => OverlayOutcome::Stay,
        },
        // Only a changed filter changes the rows, so only then is the view built again.
        TableKey::Handled
            if (if state.filter == filter {
                highlighted(state, &view)
            } else {
                highlighted(state, &model_view(ctx, &state.filter))
            }) != before =>
        {
            OverlayOutcome::Send(Msg::ThresholdCommit)
        }
        TableKey::Handled | TableKey::Unhandled => OverlayOutcome::Stay,
    }
}

fn workspace_view(ctx: &ViewCtx, filter: &str) -> TableView {
    use scuttle_core::app::WorkspacesState;
    let app = ctx.app;
    // "none" is hidden while filtering, so Enter picks the first matching workspace.
    let mut rows = Vec::new();
    if filter.trim().is_empty() {
        let mut none = vec![Line::from("none (no workspace)")];
        // The icon sits in the status column, where the other rows have theirs.
        if let Some(icon) = icons::lead(ctx.theme, Icon::NoWorkspace, ctx.theme.dim) {
            none.extend([Line::default(), Line::from(icon)]);
        }
        rows.push(Row::item(RowKey::Workspace(None), none));
    }
    rows.extend(
        fuzzy::rank(filter, app.workspaces.iter().collect(), |w| w.name.clone())
            .into_iter()
            .map(|w| {
                let used = w
                    .last_used
                    .map(|t| scuttle_core::time::relative(t, ctx.now_unix))
                    .unwrap_or_default();
                Row::item(
                    RowKey::Workspace(Some(w.id)),
                    vec![
                        Line::from(w.name.clone()),
                        Line::from(Span::styled(w.template.clone(), ctx.theme.dim)),
                        workspace_status(&w.status, ctx),
                        Line::from(Span::styled(used, ctx.theme.dim)),
                    ],
                )
            }),
    );
    // With "none" shown, the status line sits below it.
    let status = match &app.workspaces_state {
        WorkspacesState::Loading => Some("Loading workspaces…".to_owned()),
        WorkspacesState::Failed(message) => Some(format!(
            "Workspaces failed to load: {message}. /workspace retries."
        )),
        WorkspacesState::Loaded if app.workspaces.is_empty() => Some(format!(
            "You have no workspaces in {}. The agent can create one.",
            app.org_label(app.current_org())
        )),
        WorkspacesState::Loaded => None,
    };
    TableView {
        title: "Workspace".into(),
        widths: vec![
            Constraint::Fill(2),
            Constraint::Fill(1),
            // The longest status, and its icon's slot with Nerd Font icons.
            Constraint::Length(9 + icons::slot(ctx.theme.icons, Icon::WorkspaceBusy).width),
            // Fits `now` through `999d`.
            Constraint::Length(4),
        ],
        rows,
        status,
        filterable: true,
        ..Default::default()
    }
}

/// A workspace's status cell: the word, led with Nerd Font icons by its state's icon in the
/// state's color.
fn workspace_status(status: &str, ctx: &ViewCtx) -> Line<'static> {
    let (icon, style) = match status {
        "" => return Line::from(String::new()),
        "running" => (Icon::WorkspaceRunning, ctx.theme.ok),
        "failed" => (Icon::Error, ctx.theme.error),
        "stopped" | "canceled" | "deleted" => (Icon::WorkspaceStopped, ctx.theme.dim),
        _ => (Icon::WorkspaceBusy, ctx.theme.warn),
    };
    let mut spans: Vec<Span<'static>> = icons::lead(ctx.theme, icon, style).into_iter().collect();
    spans.push(Span::raw(status.to_owned()));
    Line::from(spans)
}

fn organization_view(ctx: &ViewCtx) -> TableView {
    let app = ctx.app;
    let rows = app
        .organizations
        .iter()
        .map(|o| {
            let mut marks = Vec::new();
            if o.is_default {
                marks.push("default");
            }
            if Some(o.id) == app.org_id {
                marks.push("current");
            }
            let label = if marks.is_empty() {
                o.label().to_owned()
            } else {
                format!("{} ({})", o.label(), marks.join(", "))
            };
            if o.can_create_chats {
                Row::item(RowKey::Organization(o.id), vec![Line::from(label)])
            } else {
                Row::dimmed(
                    RowKey::Organization(o.id),
                    vec![
                        Line::from(label),
                        Line::from(Span::styled(NO_CHAT_PERMISSION, ctx.theme.dim)),
                    ],
                )
            }
        })
        .collect();
    // The reason column appears only when some row gives one, and is sized to fit it.
    let mut widths = vec![Constraint::Fill(1)];
    if app.organizations.iter().any(|o| !o.can_create_chats) {
        widths.push(Constraint::Length(NO_CHAT_PERMISSION.width() as u16));
    }
    TableView {
        title: "Organization".into(),
        widths,
        rows,
        ..Default::default()
    }
}

/// A panel's label and value pairs as read-only rows, the label dim.
fn label_rows(lines: Vec<(&'static str, String)>, theme: &Theme) -> Vec<Row> {
    lines
        .into_iter()
        .map(|(label, value)| {
            Row::text(vec![
                Line::from(Span::styled(label, theme.dim)),
                Line::from(value),
            ])
        })
        .collect()
}

/// The first column of a panel, wide enough for every label and action in it, so none is cut.
fn label_column(rows: &[Row]) -> Constraint {
    let widest = rows
        .iter()
        .filter_map(|r| r.cells.first())
        .map(Line::width)
        .max()
        .unwrap_or(0);
    Constraint::Length(widest as u16)
}

fn info_view(ctx: &ViewCtx) -> TableView {
    let rows = label_rows(
        scuttle_core::panels::info_lines(ctx.app, ctx.now_unix, ctx.offset),
        ctx.theme,
    );
    TableView {
        title: "Chat info".into(),
        widths: vec![label_column(&rows), Constraint::Fill(1)],
        rows,
        hint: Some("Up and Down scroll, Esc closes".into()),
        ..Default::default()
    }
}

fn usage_view(ctx: &ViewCtx) -> TableView {
    let rows = label_rows(
        scuttle_core::panels::usage_lines(ctx.app, ctx.now_unix, ctx.offset),
        ctx.theme,
    );
    TableView {
        title: "Usage".into(),
        widths: vec![label_column(&rows), Constraint::Fill(1)],
        rows,
        hint: Some("Up and Down scroll, Esc closes".into()),
        ..Default::default()
    }
}

/// The `/workspace` panel's actions: the row id, its label, and what it runs.
const WORKSPACE_ACTIONS: [(&str, &str, WorkspaceAction); 4] = [
    ("ssh", "Copy SSH command", WorkspaceAction::CopySsh),
    ("web", "Open in web", WorkspaceAction::OpenWeb),
    ("detach", "Detach", WorkspaceAction::Detach),
    ("switch", "Switch workspace", WorkspaceAction::Switch),
];

fn workspace_details_view(ctx: &ViewCtx) -> TableView {
    let mut rows = label_rows(scuttle_core::panels::workspace_lines(ctx.app), ctx.theme);
    rows.extend(
        WORKSPACE_ACTIONS
            .iter()
            .map(|(id, label, _)| Row::item(RowKey::Action(id), vec![Line::from(*label)])),
    );
    TableView {
        title: "Workspace".into(),
        widths: vec![label_column(&rows), Constraint::Fill(1)],
        rows,
        hint: Some("Enter runs the action, Esc closes".into()),
        ..Default::default()
    }
}

fn git_view(ctx: &ViewCtx) -> TableView {
    let mut rows = label_rows(scuttle_core::panels::git_lines(ctx.app), ctx.theme);
    // With Nerd Font icons the pull request leads with its state's glyph, as in `/chats`;
    // text icons draw none, so the row stays as `git_lines` words it.
    if let Some(pr) = ctx.app.chat.as_deref().and_then(pr_badge)
        && let Some(glyph) =
            icons::lead(ctx.theme, icons::pr_icon(pr.state), ctx.theme.pr(pr.state))
        && let Some(row) = rows.iter_mut().find(|r| {
            r.cells
                .first()
                .is_some_and(|c| c.to_string() == scuttle_core::panels::PULL_REQUEST)
        })
        && let Some(value) = row.cells.get_mut(1)
    {
        let text = value.to_string();
        *value = Line::from(vec![glyph, Span::raw(text)]);
    }
    rows.push(Row::item(
        RowKey::Action("pr"),
        vec![Line::from("Open pull request")],
    ));
    rows.push(Row::item(
        RowKey::Action("diff"),
        vec![Line::from("View diff")],
    ));
    TableView {
        title: "Git".into(),
        widths: vec![label_column(&rows), Constraint::Fill(1)],
        rows,
        hint: Some("Enter runs the action, Esc closes".into()),
        ..Default::default()
    }
}

fn mcp_view(ctx: &ViewCtx) -> TableView {
    let (groups, note) = scuttle_core::panels::mcp_groups(ctx.app);
    let mut rows = Vec::new();
    for group in groups {
        rows.push(Row::header(vec![Line::from(group.title)]));
        for r in group.rows {
            let (icon, style) = if r.on {
                (Icon::ServerOn, ctx.theme.ok)
            } else {
                (Icon::ServerOff, ctx.theme.dim)
            };
            let mut state: Vec<Span<'static>> =
                icons::lead(ctx.theme, icon, style).into_iter().collect();
            state.push(Span::raw(r.state.clone()));
            let mut detail: Vec<Span<'static>> = if r.failed {
                icons::lead(ctx.theme, Icon::Error, ctx.theme.error)
                    .into_iter()
                    .collect()
            } else {
                Vec::new()
            };
            detail.push(Span::styled(r.detail.clone(), ctx.theme.dim));
            let cells = vec![
                Line::from(format!("  {}", r.name)),
                Line::from(Span::styled(r.url.clone(), ctx.theme.dim)),
                Line::from(state),
                Line::from(detail),
            ];
            rows.push(match r.id {
                Some(id) => Row::item(RowKey::Mcp(id), cells),
                None => Row::text(cells),
            });
        }
    }
    TableView {
        title: "MCP servers".into(),
        widths: vec![
            Constraint::Fill(1),
            Constraint::Fill(1),
            // Fits "off (next message)", and its icon's slot with Nerd Font icons.
            Constraint::Length(18 + icons::slot(ctx.theme.icons, Icon::ServerOff).width),
            Constraint::Fill(2),
        ],
        rows,
        status: note,
        hint: Some(
            "Enter or Space turns a server on or off for the next message, Esc closes".into(),
        ),
        filterable: false,
        ..Default::default()
    }
}

/// The `/statusline` editor's state: every footer field in order, with whether it shows, and
/// the warnings, as edited so far.
pub struct StatuslineState {
    pub table: TableState,
    pub list: FieldList,
    pub thresholds: Thresholds,
}

impl StatuslineState {
    pub fn new(statusline: &StatuslineConfig) -> StatuslineState {
        StatuslineState {
            table: TableState::default(),
            list: FieldList::new(statusline),
            thresholds: statusline.thresholds,
        }
    }

    /// The settings as edited so far.
    pub fn config(&self) -> StatuslineConfig {
        StatuslineConfig {
            fields: self.list.fields(),
            thresholds: self.thresholds,
        }
    }

    /// Moves the field at row `at` one place up or down. A shown field swaps with the next
    /// shown one that way, past any hidden rows, so every move changes the footer; a hidden
    /// field moves one row, which the footer never sees.
    fn move_field(&mut self, at: usize, up: bool) {
        let rows = &mut self.list.rows;
        if !rows[at].1 {
            self.list.move_by(at, up);
            return;
        }
        let next = if up {
            rows[..at].iter().rposition(|r| r.1)
        } else {
            rows[at + 1..].iter().position(|r| r.1).map(|i| at + 1 + i)
        };
        if let Some(to) = next {
            rows.swap(at, to);
        }
    }

    /// Space or Enter shows or hides the selected field; `[` and `]`, or Alt or Shift with Up
    /// or Down, move it; and Left and Right step its warning. Every change goes back to the
    /// UI to apply and save; a key that changes nothing stays.
    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx) -> OverlayOutcome {
        let view = statusline_view(self, ctx.theme);
        let fallback = |table: &mut TableState| match table.handle_key(key, &view) {
            TableKey::Esc => OverlayOutcome::Close,
            _ => OverlayOutcome::Stay,
        };
        let Some(at) = self.table.index(&view) else {
            return fallback(&mut self.table);
        };
        let field = self.list.rows[at].0;
        let before = self.config();
        let moves = key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Up | KeyCode::Down if moves => self.move_field(at, key.code == KeyCode::Up),
            // Every terminal sends these unchanged, unlike Alt with an arrow, which some send
            // as Esc first.
            KeyCode::Char(c @ ('[' | ']')) => self.move_field(at, c == '['),
            KeyCode::Char(' ') | KeyCode::Enter => self.list.toggle(at),
            KeyCode::Left | KeyCode::Right if field.takes_threshold() => {
                self.thresholds.step(field, key.code == KeyCode::Right);
            }
            _ => return fallback(&mut self.table),
        }
        // The selection follows the field, wherever it moved.
        self.table.selected = Some(RowKey::Action(field.name()));
        let after = self.config();
        if after == before {
            OverlayOutcome::Stay
        } else {
            OverlayOutcome::Statusline(after)
        }
    }
}

/// The `/statusline` editor's rows: a check box and the field's name, what it shows, and its
/// warning.
fn statusline_view(state: &StatuslineState, theme: &Theme) -> TableView {
    let rows = state
        .list
        .rows
        .iter()
        .map(|&(field, shown)| {
            let mark = if shown { "[x] " } else { "[ ] " };
            let warning = match state.thresholds.get(field) {
                Some(percent) => format!("warns at {percent}%"),
                None if field.takes_threshold() => "no warning".to_owned(),
                None => String::new(),
            };
            Row::item(
                RowKey::Action(field.name()),
                vec![
                    Line::from(format!("{mark}{}", field.name())),
                    Line::from(Span::styled(field.description(), theme.dim)),
                    Line::from(Span::styled(warning, theme.dim)),
                ],
            )
        })
        .collect();
    TableView {
        title: "Status line".into(),
        widths: vec![
            // Fits "[x] organization".
            Constraint::Length(16),
            Constraint::Fill(1),
            // Fits "warns at 100%".
            Constraint::Length(13),
        ],
        rows,
        hint: Some("Space toggles · [ ] move · ←→ warning · Esc closes".into()),
        ..Default::default()
    }
}

fn queue_view(app: &App) -> TableView {
    let rows: Vec<Row> = app
        .transcript
        .queued
        .iter()
        .enumerate()
        .filter_map(|(n, q)| {
            Some(Row::item(
                RowKey::Queued(q.id?),
                vec![
                    Line::from(format!("{}.", n + 1)),
                    Line::from(crate::transcript_view::queued_text(q)),
                ],
            ))
        })
        .collect();
    TableView {
        title: "Queue".into(),
        widths: vec![Constraint::Length(3), Constraint::Fill(1)],
        status: rows.is_empty().then(|| "Nothing is queued.".to_owned()),
        rows,
        hint: Some(
            "Enter sends now, interrupting a running turn; Delete or Backspace removes; Esc closes"
                .into(),
        ),
        filterable: false,
        ..Default::default()
    }
}

/// The state of `/chats` beyond its table: the tab and the expanded roots.
pub struct ChatsState {
    pub table: TableState,
    pub filter: Filter,
    pub expanded: HashSet<Uuid>,
    /// The archive confirmation, while it shows over the rows.
    pub archive: Option<ArchivePrompt>,
}

/// The widest the archive confirmation draws, so it reads as a box over `/chats`.
const ARCHIVE_WIDTH: u16 = 72;
const ARCHIVE_ROW: &str = "archive";
const DELETE_ROW: &str = "archive-delete";
const CANCEL_ROW: &str = "cancel";

/// What the delete choice's label ends with, so the risk shows on the row itself.
const UNDONE_FLAG: &str = "(can't be undone)";

/// `text` cut to `max` display columns, with an ellipsis in place of what was cut. With no
/// columns at all, nothing is left.
pub(crate) fn ellipsize(text: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if text.width() <= max {
        return text.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > max {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('\u{2026}');
    out
}

/// The `/files` rows: every file of the chat, newest first. A file the server no longer has is
/// dimmed.
fn files_view(ctx: &ViewCtx) -> TableView {
    let app = ctx.app;
    let mut rows: Vec<Row> = files::file_rows(app.chat.as_deref(), &app.transcript)
        .into_iter()
        .map(|f| {
            let name = files::shown_name(&f.name);
            let name = if f.expired {
                format!("{name} (no longer available)")
            } else {
                name.to_owned()
            };
            let cells = vec![
                Line::from(name),
                Line::from(files::badge(&f.media_type)),
                Line::from(f.size.map(size_label).unwrap_or_default()),
                Line::from(match f.from {
                    Some(Sender::You) => "you",
                    Some(Sender::Agent) => "agent",
                    None => "",
                }),
                Line::from(match &f.place {
                    Place::Message { label, .. } => label.clone(),
                    Place::Live => "this turn".into(),
                    Place::NotLoaded => "not loaded".into(),
                }),
                Line::from(match (f.created, &f.place) {
                    (Some(t), _) => scuttle_core::time::relative(t, ctx.now_unix),
                    (None, Place::Live) => "now".into(),
                    _ => String::new(),
                }),
            ];
            if f.expired {
                Row::dimmed(RowKey::File(f.id), cells)
            } else {
                Row::item(RowKey::File(f.id), cells)
            }
        })
        .collect();
    if !rows.is_empty() {
        rows.insert(
            0,
            Row::header(
                ["Name", "Type", "Size", "From", "Message", "When"]
                    .into_iter()
                    .map(Line::from)
                    .collect(),
            ),
        );
    }
    let dir = files::display_path(&app.save_dir, app.home.as_deref());
    TableView {
        title: "Files".into(),
        widths: vec![
            Constraint::Fill(2),
            Constraint::Length(4),
            Constraint::Length(9),
            Constraint::Length(5),
            Constraint::Fill(1),
            Constraint::Length(4),
        ],
        status: rows
            .is_empty()
            .then(|| "No files in this chat yet.".to_owned()),
        rows,
        hint: Some(first_fit(
            vec![
                format!(
                    "Enter saves to {dir}; s saves as; v views text; g goes to its message; Esc closes"
                ),
                format!("Enter saves to {dir}; s saves as; v views; g goes to it; Esc closes"),
                "Enter saves; s saves as; v views text; g goes to its message; Esc closes".into(),
                "Enter saves; s saves as; v views; g goes to it; Esc closes".into(),
                "Enter · s · v · g · Esc".into(),
            ],
            usize::from(ctx.width).saturating_sub(4),
        )),
        filterable: false,
        ..Default::default()
    }
}

/// The widest the "name is taken" question draws.
const CONFLICT_WIDTH: u16 = 72;

/// The "name is taken" box's title within a box `width` wide: `table::render` puts a space on
/// each side of it, inside the two corners, so a long path is cut with an ellipsis there, as
/// `archive_title` cuts a long chat title.
fn conflict_title(path: &str, width: u16) -> String {
    let room = usize::from(width).saturating_sub(4);
    let fixed = " already exists".width();
    if room > fixed {
        format!("{} already exists", ellipsize(path, room - fixed))
    } else {
        ellipsize("Name taken", room)
    }
}

/// The question a save asks when its name is taken, for a box `width` wide: keep both,
/// replace, or cancel, each answered by the letter before it. Every choice is one line that
/// says what it does, and the hint is one line; the risk shows on the "Replace" label itself.
/// Nothing is selected, since Enter answers nothing here.
pub fn conflict_view(
    conflict: &SaveConflict,
    home: Option<&std::path::Path>,
    theme: &Theme,
    width: u16,
) -> TableView {
    let hint_room = table::line_room(width);
    // After the letter and the space after it.
    let row_room = hint_room.saturating_sub(2);
    let path = files::display_path(&conflict.path, home);
    let taken = conflict
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| conflict.name.clone());
    let kept = files::numbered(&taken, 1);
    let rows: Vec<Row> = ConflictChoice::ALL
        .iter()
        .map(|choice| {
            let (letter, label) = match choice {
                ConflictChoice::KeepBoth => (
                    "k",
                    Span::raw(ellipsize(
                        &first_fit(
                            vec![
                                format!("Keep both: save as {kept}, or the next free number"),
                                format!("Keep both: save as {kept}"),
                                "Keep both".into(),
                            ],
                            row_room,
                        ),
                        row_room,
                    )),
                ),
                // Shortened by dropping words, never by cutting the flag.
                ConflictChoice::Replace => (
                    "r",
                    Span::styled(
                        first_fit(
                            vec![
                                format!("Replace: write over the file there {UNDONE_FLAG}"),
                                format!("Replace {UNDONE_FLAG}"),
                                format!("{UNDONE_FLAG} Replace"),
                            ],
                            row_room,
                        ),
                        theme.error,
                    ),
                ),
                ConflictChoice::Cancel => (
                    "c",
                    Span::raw(ellipsize(
                        &first_fit(
                            vec!["Cancel: save nothing".into(), "Cancel".into()],
                            row_room,
                        ),
                        row_room,
                    )),
                ),
            };
            Row::text(vec![
                Line::from(Span::styled(letter, theme.accent)),
                Line::from(label),
            ])
        })
        .collect();
    TableView {
        title: conflict_title(&path, width),
        widths: vec![Constraint::Length(1), Constraint::Fill(1)],
        rows,
        hint: Some(first_fit(
            vec![
                "Press k to keep both, r to replace, c or Esc to cancel".into(),
                "k keeps both, r replaces, c or Esc cancels".into(),
                "k, r, or c; Esc cancels".into(),
            ],
            hint_room,
        )),
        filterable: false,
        ..Default::default()
    }
}

/// Where the "name is taken" question draws in `area`: centered, at most `CONFLICT_WIDTH`
/// wide, and as tall as its one-line rows and hint.
pub fn conflict_rect(
    area: Rect,
    conflict: &SaveConflict,
    home: Option<&std::path::Path>,
    theme: &Theme,
) -> Rect {
    let width = area.width.min(CONFLICT_WIDTH);
    let height =
        table::height(&conflict_view(conflict, home, theme, width), width).min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// Draws the "name is taken" question as a box in the middle of `area`.
pub fn render_conflict_prompt(
    f: &mut Frame,
    area: Rect,
    conflict: &SaveConflict,
    home: Option<&std::path::Path>,
    theme: &Theme,
) {
    let rect = conflict_rect(area, conflict, home, theme);
    let view = conflict_view(conflict, home, theme, rect.width);
    table::render(f, rect, &view, &TableState::default(), theme, None);
}

/// The first of `forms` that fits in `max` columns, else the last, which is the shortest.
fn first_fit(forms: Vec<String>, max: usize) -> String {
    let last = forms.last().cloned().unwrap_or_default();
    forms.into_iter().find(|f| f.width() <= max).unwrap_or(last)
}

/// The archive box's title within a box `width` wide: `table::render` puts a space on each
/// side of it, inside the two corners, so a long chat title is cut with an ellipsis there.
fn archive_title(title: &str, width: u16) -> String {
    let room = usize::from(width).saturating_sub(4);
    let fixed = "Archive \u{201c}\u{201d}?".width();
    if room > fixed {
        format!(
            "Archive \u{201c}{}\u{201d}?",
            ellipsize(title, room - fixed)
        )
    } else {
        ellipsize("Archive", room)
    }
}

/// The archive confirmation `/chats` draws over its rows: archive, archive and delete the
/// chat's workspace, or cancel. It starts on "Archive", and deleting takes Enter to arm and then D.
pub struct ArchivePrompt {
    pub chat: Uuid,
    pub title: String,
    /// The chat's workspace; `None` leaves out the choice that deletes it.
    pub workspace: Option<Uuid>,
    /// The workspace's name, when the workspace list holds it.
    pub workspace_name: Option<String>,
    pub table: TableState,
    /// Set by Enter on the delete choice. Only D then deletes, so no run of held Enters can;
    /// any move or other key clears it.
    pub armed: bool,
}

impl ArchivePrompt {
    fn new(id: Uuid, chat: &types::CodersdkChat, app: &App) -> ArchivePrompt {
        let workspace = chat.workspace_id;
        ArchivePrompt {
            chat: id,
            title: chat
                .title
                .clone()
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| "Untitled".into()),
            workspace,
            workspace_name: workspace
                .and_then(|ws| app.workspaces.iter().find(|w| w.id == ws))
                .map(|w| w.name.clone()),
            table: TableState::with_selected(RowKey::Action(ARCHIVE_ROW)),
            armed: false,
        }
    }

    /// How the confirmation names the workspace, with its name cut to `room` columns.
    fn workspace_phrase_within(&self, room: usize) -> String {
        match (&self.workspace_name, self.workspace) {
            (Some(name), _) => format!("workspace \u{201c}{}\u{201d}", ellipsize(name, room)),
            // The workspace list does not hold it, so the id is all there is to name it by.
            (None, Some(id)) => format!("workspace {}", &id.to_string()[..8]),
            (None, None) => "the chat's workspace".into(),
        }
    }

    /// How the confirmation names the workspace, whole.
    fn workspace_phrase(&self) -> String {
        self.workspace_phrase_within(usize::MAX)
    }

    /// The delete choice's label within `max` columns. The workspace's name gives way before
    /// the flag does, and when nothing else fits the flag leads, so it is never cut.
    fn delete_label(&self, max: usize) -> String {
        let label = |lead: &str, room: usize| {
            format!("{lead}{} {UNDONE_FLAG}", self.workspace_phrase_within(room))
        };
        let fitted = |lead: &str| {
            // With no room, the name is left out, so this is the width of the rest.
            let fixed = label(lead, 0).width();
            label(lead, max.saturating_sub(fixed).max(1))
        };
        first_fit(
            vec![
                fitted("Archive and delete "),
                fitted("Delete "),
                format!("{UNDONE_FLAG} Archive and delete"),
                format!("{UNDONE_FLAG} Delete"),
            ],
            max,
        )
    }
}

/// The archive confirmation's rows for a box `width` wide. Every row and the hint are one
/// line, so moving between the choices or arming the delete never changes the box's size.
pub fn archive_view(prompt: &ArchivePrompt, theme: &Theme, width: u16) -> TableView {
    let row_room = table::item_room(width);
    let hint_room = table::line_room(width);
    let mut rows = vec![Row::item(
        RowKey::Action(ARCHIVE_ROW),
        vec![Line::from("Archive")],
    )];
    if prompt.workspace.is_some() {
        rows.push(Row::item(
            RowKey::Action(DELETE_ROW),
            vec![Line::from(Span::styled(
                prompt.delete_label(row_room),
                theme.error,
            ))],
        ));
    }
    rows.push(Row::item(
        RowKey::Action(CANCEL_ROW),
        vec![Line::from("Cancel")],
    ));
    let deleting = prompt.table.selected == Some(RowKey::Action(DELETE_ROW));
    // Shortened by dropping words, never by dropping a key's name.
    let hint = match (deleting, prompt.armed) {
        (_, true) => first_fit(
            vec![
                format!(
                    "Press D to delete {} for good, Esc cancels",
                    prompt.workspace_phrase()
                ),
                "Press D to delete for good, Esc cancels".into(),
                "D deletes, Esc cancels".into(),
            ],
            hint_room,
        ),
        (true, false) => first_fit(
            vec![
                "Up and Down choose, Enter arms the delete, Esc cancels".into(),
                "Enter arms the delete, Esc cancels".into(),
                "Enter arms, Esc cancels".into(),
            ],
            hint_room,
        ),
        (false, false) => first_fit(
            vec![
                "Up and Down choose, Enter confirms, Esc cancels".into(),
                "Enter confirms, Esc cancels".into(),
            ],
            hint_room,
        ),
    };
    TableView {
        title: archive_title(&prompt.title, width),
        widths: vec![Constraint::Fill(1)],
        rows,
        hint: Some(hint),
        hint_alarm: prompt.armed,
        filterable: false,
        ..Default::default()
    }
}

/// Where the archive confirmation draws in `area`: centered, at most `ARCHIVE_WIDTH` wide,
/// and as tall as its one-line rows and hint. It depends only on `area` and on whether the
/// chat has a workspace, both fixed while the box is open.
pub fn archive_rect(area: Rect, prompt: &ArchivePrompt, theme: &Theme) -> Rect {
    let width = area.width.min(ARCHIVE_WIDTH);
    let height = table::height(&archive_view(prompt, theme, width), width).min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// Draws `prompt` as a box in the middle of `area`, over the `/chats` rows.
pub fn render_archive_prompt(f: &mut Frame, area: Rect, prompt: &ArchivePrompt, theme: &Theme) {
    let rect = archive_rect(area, prompt, theme);
    let view = archive_view(prompt, theme, rect.width);
    table::render(f, rect, &view, &prompt.table, theme, None);
}

/// Whether a chat in `status` shows the activity spinner.
fn spins(status: Option<&ChatStatus>) -> bool {
    matches!(status, Some(ChatStatus::Running | ChatStatus::Interrupting))
}

/// A chat's status marker in `base`: the spinner frame for `ctx.elapsed` while the agent
/// works, the icon for an error or a question, else one blank cell. The table repaints a
/// spinning marker on timer frames, and the spinner stays one cell in both icon sets.
fn status_span(status: Option<&ChatStatus>, ctx: &ViewCtx, base: Style) -> Span<'static> {
    let icon = match status {
        _ if spins(status) => return Span::styled(spinner_frame(ctx.elapsed), base),
        Some(ChatStatus::RequiresAction) => Icon::Asking,
        Some(ChatStatus::Error) => Icon::Failed,
        _ => return Span::styled(" ", base),
    };
    icons::lead(ctx.theme, icon, base).unwrap_or_else(|| Span::styled(" ", base))
}

/// The column of `chat_cells` that holds the subagent count and the busiest marker.
const FAMILY_COLUMN: usize = 3;

/// The column of `chat_cells` that holds the status marker or the unread dot.
const STATUS_COLUMN: usize = 1;

/// The status column's width: the two-cell unread dot or icon slot, or a one-cell marker.
const STATUS_WIDTH: u16 = 2;

/// The pin column's width: `icon`'s display width, never under two cells, so the default 📌
/// fits and an empty or one-cell icon still keeps every title in line. East Asian Ambiguous
/// characters, such as a Nerd Font glyph, are counted as a CJK terminal draws them, except
/// that a trailing space takes their spill, as it does in an icon slot, so the column leaves
/// room for the wider glyph instead of pushing the title.
fn pin_width(icon: &str) -> u16 {
    u16::try_from(icon.width().max(icon.trim_end().width_cjk()))
        .unwrap_or(u16::MAX)
        .max(2)
}

/// A `/chats` row's status cell, most important first: the spinner while the agent works,
/// `!` after an error, `?` while it waits on the user, then the unread dot, else blank. A chat has
/// one status, so the order decides only against unread: a working chat keeps its spinner,
/// and the dot shows once it goes idle.
fn status_cell(r: &ChatRow, ctx: &ViewCtx) -> Line<'static> {
    let attention = spins(r.status.as_ref())
        || matches!(
            r.status,
            Some(ChatStatus::Error | ChatStatus::RequiresAction)
        );
    // The open chat's stream keeps it read, whatever a refetch says.
    let unread = r.unread && Some(r.id) != ctx.app.chat_id;
    if attention {
        Line::from(status_span(r.status.as_ref(), ctx, ctx.theme.accent))
    } else if unread {
        // The emoji keeps its own colors; the glyph takes the accent, so it follows the theme.
        let style = match ctx.theme.icons {
            IconSet::Nerd => icons::style(ctx.theme, ctx.theme.accent),
            IconSet::Text => Style::default(),
        };
        Line::from(Span::styled(
            icons::slot(ctx.theme.icons, Icon::Unread).text,
            style,
        ))
    } else {
        Line::from(" ")
    }
}

/// The subagent count before the busiest subagent's marker, such as `+2 `.
fn family_prefix(children: usize) -> String {
    format!("+{children} ")
}

/// The narrowest terminal whose `/chats` shows each chat's summary; a narrower one drops that
/// column before any other.
const SUMMARY_MIN_WIDTH: u16 = 100;

/// The column of `chat_cells` that holds the pull request's reference, when the overlay
/// shows it; its state is the next column, or this one when no row has a reference.
const PR_COLUMN: usize = 6;

/// The narrowest terminal whose `/chats` shows the pull request column. It is wider than
/// `SUMMARY_MIN_WIDTH`, so a narrowing overlay drops the pull requests before the summaries.
const PR_MIN_WIDTH: u16 = 120;

/// The most cells a pull request's reference takes in a terminal `width` columns wide: a
/// sixth of it, so `owner/repo#123` shows on wide terminals and the owner goes first as it
/// narrows.
fn pr_reference_budget(width: u16) -> usize {
    usize::from(width) / 6
}

/// How `/chats` fits its pull request references: at most `max` cells each, and with owners
/// only while every listed reference fits whole, so the rows never mix the two forms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PrFit {
    max: usize,
    owners: bool,
    /// Whether any row has a reference to show; with none, the reference column stays out
    /// and the state takes its place.
    references: bool,
}

impl PrFit {
    /// The fit for `rows` in a terminal `width` columns wide.
    fn for_rows(rows: &[ChatRow], width: u16) -> PrFit {
        let max = pr_reference_budget(width);
        let owners = rows
            .iter()
            .filter_map(|r| r.pr.as_ref()?.reference.as_ref())
            .all(|r| r.text(usize::MAX).len() <= max);
        PrFit {
            max,
            owners,
            references: true,
        }
    }

    /// `reference` as this fit shows it.
    fn text(self, reference: &scuttle_core::forge::PrRef) -> String {
        if self.owners {
            return reference.text(self.max);
        }
        // One cell short of the whole form leaves no room for the owner.
        let whole = reference.text(usize::MAX).len();
        reference.text(self.max.min(whole.saturating_sub(1)))
    }
}

/// A chat's pull request reference cell, empty for a chat without a pull request: with Nerd
/// Font icons, the forge's glyph, or a blank slot as wide for a forge without one, then the
/// dim reference as `fit` shows it; with text icons, the reference alone. A URL that names no
/// forge leaves the bare `#123`.
fn pr_cell(pr: Option<&PrBadge>, ctx: &ViewCtx, fit: PrFit) -> Line<'static> {
    let Some(pr) = pr else {
        return Line::default();
    };
    let reference = match &pr.reference {
        Some(r) => fit.text(r),
        None => pr
            .number
            .map(|n| scuttle_core::forge::bare('#', n, fit.max))
            .unwrap_or_default(),
    };
    let mut spans = Vec::new();
    if ctx.theme.icons == IconSet::Nerd {
        let forge = pr.reference.as_ref().map(|r| icons::forge_icon(r.forge));
        spans.push(match forge {
            Some(icon) => Span::styled(
                icons::slot(IconSet::Nerd, icon).text,
                icons::style(ctx.theme, ctx.theme.dim),
            ),
            None => {
                Span::raw(" ".repeat(usize::from(icons::slot(IconSet::Nerd, Icon::GitHub).width)))
            }
        });
    }
    spans.push(Span::styled(reference, ctx.theme.dim));
    Line::from(spans)
}

/// The width of the pull request state column: a glyph's slot, or the longest state word and
/// a cell that keeps it off the summary.
fn pr_state_width(set: IconSet) -> u16 {
    match set {
        IconSet::Nerd => icons::slot(IconSet::Nerd, Icon::PrOpen).width,
        IconSet::Text => {
            let longest = [
                PrState::Open,
                PrState::Draft,
                PrState::Merged,
                PrState::Closed,
            ]
            .map(|s| s.label().len())
            .into_iter()
            .max()
            .unwrap_or(0);
            longest as u16 + 1
        }
    }
}

/// A chat's pull request state, in its color: its glyph, or with text icons its word. Empty
/// for a chat without a pull request.
fn pr_state_cell(pr: Option<&PrBadge>, ctx: &ViewCtx) -> Line<'static> {
    let Some(pr) = pr else {
        return Line::default();
    };
    let style = ctx.theme.pr(pr.state);
    match icons::lead(ctx.theme, icons::pr_icon(pr.state), style) {
        Some(glyph) => Line::from(glyph),
        None => Line::from(Span::styled(pr.state.label(), style)),
    }
}

/// A chat row's subagent count and the busiest subagent's marker, dim, with the same icons
/// as the status column; empty for a chat without subagents.
fn family_cell(r: &ChatRow, ctx: &ViewCtx) -> Line<'static> {
    if r.children == 0 {
        return Line::default();
    }
    Line::from(vec![
        Span::styled(family_prefix(r.children), ctx.theme.dim),
        status_span(r.busiest_child.as_ref(), ctx, ctx.theme.dim),
    ])
}

/// A chat row's cells: the pin, the status, the title, the subagent count, the archived tag,
/// and the age; `prs` adds the pull request's reference, fitted, and its state, and
/// `summaries` adds the dim summary as the last.
fn chat_cells(
    r: &ChatRow,
    ctx: &ViewCtx,
    prs: Option<PrFit>,
    summaries: bool,
) -> Vec<Line<'static>> {
    // A subagent is indented inside the title cell, so the pin and status columns stay put.
    let indent = if r.depth > 0 { "   " } else { "" };
    let pin = if r.pinned { ctx.pin_icon } else { "" };
    let family = family_cell(r, ctx);
    let when = r
        .updated_unix
        .map(|t| scuttle_core::time::relative(t, ctx.now_unix))
        .unwrap_or_default();
    let mut cells = vec![
        Line::from(pin.to_owned()),
        status_cell(r, ctx),
        Line::from(format!("{indent}{}", r.title)),
        family,
        Line::from(Span::styled(
            if r.archived {
                icons::slot(ctx.theme.icons, Icon::Archived).text
            } else {
                ""
            },
            icons::style(ctx.theme, ctx.theme.dim),
        )),
        Line::from(Span::styled(when, ctx.theme.dim)),
    ];
    if let Some(fit) = prs {
        debug_assert_eq!(cells.len(), PR_COLUMN);
        if fit.references {
            cells.push(pr_cell(r.pr.as_ref(), ctx, fit));
        }
        cells.push(pr_state_cell(r.pr.as_ref(), ctx));
    }
    if summaries {
        cells.push(Line::from(Span::styled(
            r.summary.clone().unwrap_or_default(),
            ctx.theme.dim,
        )));
    }
    cells
}

/// The cells of row `index` that show a spinner, for the table to repaint on timer frames.
fn chat_spinners(index: usize, r: &ChatRow) -> Vec<Spinner> {
    let mut spinners = Vec::new();
    if spins(r.status.as_ref()) {
        spinners.push(Spinner {
            row: index,
            column: STATUS_COLUMN,
            offset: 0,
        });
    }
    if r.children > 0 && spins(r.busiest_child.as_ref()) {
        spinners.push(Spinner {
            row: index,
            column: FAMILY_COLUMN,
            offset: family_prefix(r.children).width() as u16,
        });
    }
    spinners
}

fn chats_view(state: &ChatsState, ctx: &ViewCtx) -> TableView {
    let list = &ctx.app.chats;
    let query = state.table.filter.as_str();
    let summaries = ctx.width >= SUMMARY_MIN_WIDTH;
    // A server search in progress or already answered for this exact text replaces the
    // locally ranked rows; the search row itself always stays last.
    let searched = list.search_rows(query);
    let chat_rows = match searched.as_ref() {
        Some((rows, _)) => rows.clone(),
        None => list.rows(state.filter, query, &state.expanded),
    };
    // Wide enough for the largest family's count and its marker.
    let family_width = chat_rows
        .iter()
        .map(|r| family_cell(r, ctx).width())
        .max()
        .unwrap_or(0) as u16;
    // From `PR_MIN_WIDTH`, while a listed chat has a pull request; the reference column is as
    // wide as its widest cell.
    let mut fit = PrFit::for_rows(&chat_rows, ctx.width);
    let pr_width = chat_rows
        .iter()
        .map(|r| pr_cell(r.pr.as_ref(), ctx, fit).width())
        .max()
        .unwrap_or(0) as u16;
    fit.references = pr_width > 0;
    let prs =
        (ctx.width >= PR_MIN_WIDTH && chat_rows.iter().any(|r| r.pr.is_some())).then_some(fit);
    let spinners = chat_rows
        .iter()
        .enumerate()
        .flat_map(|(i, r)| chat_spinners(i, r))
        .collect();
    let mut rows: Vec<Row> = chat_rows
        .iter()
        .map(|r| Row::item(RowKey::Chat(r.id), chat_cells(r, ctx, prs, summaries)))
        .collect();
    if !query.trim().is_empty() {
        rows.push(Row::item(
            RowKey::SearchAll,
            vec![
                Line::default(),
                Line::default(),
                Line::from(Span::styled(
                    format!("Search all chats for \u{201c}{query}\u{201d}"),
                    ctx.theme.accent,
                )),
            ],
        ));
    }
    let local = match list.page(&state.filter.query()).map(|p| &p.load) {
        Some(Load::Failed(message)) => Some(format!("{message} Press Tab to retry.")),
        _ if !chat_rows.is_empty() => None,
        Some(Load::Idle | Load::Loading) => Some("Loading chats…".into()),
        _ if query.is_empty() && state.filter == Filter::All => {
            Some("No chats yet. Type a message to start one.".into())
        }
        _ => Some("No chats match.".into()),
    };
    let listing = match searched.as_ref() {
        Some((_, Load::Loading)) => Some("Searching all chats…".to_owned()),
        Some((_, Load::Failed(message))) => Some(message.to_string()),
        Some((found, _)) if found.is_empty() => {
            Some(format!("No chats match \u{201c}{query}\u{201d}."))
        }
        Some(_) => None,
        None => local,
    };
    let tabs = Filter::ALL
        .iter()
        .map(|f| {
            if *f == state.filter {
                format!("[{}]", f.label())
            } else {
                f.label().to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" | ");
    // Without the watch socket, the list changes only when it is refetched.
    let paused = if list.watch_live {
        ""
    } else {
        "   live updates paused"
    };
    let mut widths = vec![
        Constraint::Length(pin_width(ctx.pin_icon)),
        Constraint::Length(STATUS_WIDTH),
        Constraint::Fill(if summaries { 3 } else { 1 }),
        Constraint::Length(family_width),
        // "archived", or its icon's two-cell slot.
        Constraint::Length(icons::slot(ctx.theme.icons, Icon::Archived).width),
        // Fits `now` through `999d`.
        Constraint::Length(4),
    ];
    if prs.is_some() {
        if fit.references {
            widths.push(Constraint::Length(pr_width));
        }
        widths.push(Constraint::Length(pr_state_width(ctx.theme.icons)));
    }
    if summaries {
        widths.push(Constraint::Fill(2));
    }
    TableView {
        title: format!("Chats   {tabs}{paused}"),
        widths,
        rows,
        status: listing,
        hint: Some(
            "Tab filters, Left/Right subagents, Ctrl+A/E/P/U archive/rename/pin/read, Esc closes"
                .into(),
        ),
        filterable: true,
        filter_label: Some("Search: "),
        placeholder: Some("Type to filter, or search all chats"),
        spinners,
        ..Default::default()
    }
}

/// The `/subagents` popup: the list, and how far the preview below it is scrolled up.
pub struct SubagentsState {
    pub table: TableState,
    /// Lines scrolled up from the bottom of the preview.
    pub scroll: usize,
}

fn subagents_view(ctx: &ViewCtx) -> TableView {
    let (parent, children) = ctx.app.subagents();
    let listed: Vec<(Uuid, &types::CodersdkChat)> =
        children.iter().filter_map(|c| Some((c.id?, c))).collect();
    let spinners = listed
        .iter()
        .enumerate()
        .filter(|(_, (_, c))| spins(chat_status(c).as_ref()))
        .map(|(row, _)| Spinner {
            row,
            column: 0,
            offset: 0,
        })
        .collect();
    let rows: Vec<Row> = listed
        .iter()
        .map(|(id, c)| {
            let status = chat_status(c);
            let when = c
                .updated_at
                .map(|t| scuttle_core::time::relative(t.timestamp(), ctx.now_unix))
                .unwrap_or_default();
            Row::item(
                RowKey::Chat(*id),
                vec![
                    Line::from(status_span(status.as_ref(), ctx, ctx.theme.accent)),
                    Line::from(c.title.clone().unwrap_or_else(|| "Untitled".into())),
                    Line::from(Span::styled(when, ctx.theme.dim)),
                ],
            )
        })
        .collect();
    TableView {
        title: match parent {
            Some(parent) => format!("Subagents of \u{201c}{parent}\u{201d}"),
            None => "Subagents".into(),
        },
        widths: vec![
            // The one-cell marker, or an icon's two-cell slot.
            Constraint::Length(match ctx.theme.icons {
                IconSet::Nerd => STATUS_WIDTH,
                IconSet::Text => 1,
            }),
            Constraint::Fill(1),
            // Fits `now` through `999d`.
            Constraint::Length(4),
        ],
        status: if !ctx.app.parent_listed() {
            Some("The parent chat is not loaded, so only this subagent is listed.".to_owned())
        } else {
            rows.is_empty()
                .then(|| "This chat has no subagents.".to_owned())
        },
        rows,
        hint: Some(
            "Up and Down preview, Enter opens, PageUp and PageDown scroll, Esc closes".into(),
        ),
        filterable: false,
        filter_label: None,
        placeholder: None,
        spinners,
        hint_alarm: false,
    }
}

impl SubagentsState {
    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx) -> OverlayOutcome {
        match key.code {
            KeyCode::PageUp => {
                self.scroll += 10;
                return OverlayOutcome::Stay;
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(10);
                return OverlayOutcome::Stay;
            }
            _ => {}
        }
        let view = subagents_view(ctx);
        let before = self.table.selected_row(&view).map(|r| r.key.clone());
        match self.table.handle_key(key, &view) {
            TableKey::Esc => OverlayOutcome::CloseWith(Msg::PreviewChat(None)),
            // Opening the subagent closes the preview with the rest of the old chat's state.
            TableKey::Enter => match before {
                Some(RowKey::Chat(id)) => OverlayOutcome::CloseWith(Msg::OpenChat(id)),
                _ => OverlayOutcome::Stay,
            },
            TableKey::Handled | TableKey::Unhandled => {
                match self.table.selected_row(&view).map(|r| r.key.clone()) {
                    Some(RowKey::Chat(id)) if before != Some(RowKey::Chat(id)) => {
                        self.scroll = 0;
                        OverlayOutcome::Send(Msg::PreviewChat(Some(id)))
                    }
                    _ => OverlayOutcome::Stay,
                }
            }
        }
    }
}

impl ChatsState {
    /// A key while the archive confirmation shows, which holds the keyboard until a choice,
    /// Cancel, or Esc. Up and Down move between the choices, as the wheel does.
    fn archive_key(&mut self, key: KeyEvent, ctx: &ViewCtx) -> OverlayOutcome {
        let Some(prompt) = self.archive.as_mut() else {
            return OverlayOutcome::Stay;
        };
        // Most terminals report a held key as repeated presses, so the D key below is the
        // real guard; this one drops a repeat where a terminal does report it.
        if key.kind == KeyEventKind::Repeat {
            return OverlayOutcome::Stay;
        }
        let ctrl_a =
            key.code == KeyCode::Char('a') && key.modifiers.contains(KeyModifiers::CONTROL);
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if matches!(key.code, KeyCode::Char('d' | 'D')) && plain {
            let (chat, armed, workspace) = (prompt.chat, prompt.armed, prompt.workspace);
            return match workspace {
                // Only D confirms, and only once Enter armed the delete: a key held down
                // sends Enter, never D, so no length of hold deletes.
                Some(workspace) if armed => {
                    self.archive = None;
                    OverlayOutcome::Send(Msg::ChatAction(ChatAction::ArchiveAndDeleteWorkspace {
                        chat,
                        workspace,
                    }))
                }
                _ => OverlayOutcome::Stay,
            };
        }
        let chosen = match key.code {
            // A second Ctrl+A archives, as the old double press did; it never deletes.
            _ if ctrl_a => Some(RowKey::Action(ARCHIVE_ROW)),
            KeyCode::Esc => None,
            KeyCode::Enter => {
                let view = archive_view(prompt, ctx.theme, ARCHIVE_WIDTH);
                prompt.table.selected_row(&view).map(|r| r.key.clone())
            }
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
                let view = archive_view(prompt, ctx.theme, ARCHIVE_WIDTH);
                prompt.table.handle_key(key, &view);
                prompt.armed = false;
                return OverlayOutcome::Stay;
            }
            _ => {
                prompt.armed = false;
                return OverlayOutcome::Stay;
            }
        };
        let chat = prompt.chat;
        match chosen {
            // Enter arms the delete and does nothing more: D is what confirms.
            Some(RowKey::Action(DELETE_ROW)) => {
                prompt.armed = true;
                OverlayOutcome::Stay
            }
            Some(RowKey::Action(ARCHIVE_ROW)) => {
                self.archive = None;
                OverlayOutcome::Send(Msg::ChatAction(ChatAction::Archive(chat)))
            }
            // Cancel and Esc close the box and leave `/chats` open.
            _ => {
                self.archive = None;
                OverlayOutcome::Stay
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx) -> OverlayOutcome {
        if self.archive.is_some() {
            return self.archive_key(key, ctx);
        }
        let view = chats_view(self, ctx);
        let selected_key = self.table.selected_row(&view).map(|r| r.key.clone());
        let selected = match selected_key {
            Some(RowKey::Chat(id)) => Some(id),
            _ => None,
        };
        if key.code == KeyCode::End {
            // The last loaded chat, not the search row that may follow it, so End also
            // reaches the row that pages a search in progress.
            let last_chat = view
                .rows
                .iter()
                .rposition(|r| matches!(r.key, RowKey::Chat(_)));
            let target = last_chat.or_else(|| view.rows.iter().rposition(Row::selectable));
            if let Some(i) = target {
                self.table.selected = Some(view.rows[i].key.clone());
            }
            return self.load_more(&view, ctx);
        }
        match self.table.handle_key(key, &view) {
            TableKey::Esc => OverlayOutcome::Close,
            TableKey::Enter => match selected_key {
                Some(RowKey::Chat(id)) => OverlayOutcome::CloseWith(Msg::OpenChat(id)),
                Some(RowKey::SearchAll) => {
                    OverlayOutcome::Send(Msg::SearchChats(self.table.filter.trim().to_owned()))
                }
                _ => OverlayOutcome::Stay,
            },
            // A move keeps the rows, so `view` still holds them.
            TableKey::Handled if matches!(key.code, KeyCode::Down | KeyCode::PageDown) => {
                self.load_more(&view, ctx)
            }
            TableKey::Handled => OverlayOutcome::Stay,
            TableKey::Unhandled => self.chat_key(key, selected, ctx),
        }
    }

    /// Loads the next page once a move reaches the last loaded chat row. With no filter text
    /// this pages the current tab; with filter text that a server search already answered, it
    /// pages that search instead, until its own last page comes back short. Typed text with no
    /// matching search in flight never pages, as it ranks only what is already loaded.
    fn load_more(&self, view: &TableView, ctx: &ViewCtx) -> OverlayOutcome {
        let last = view
            .rows
            .iter()
            .rposition(|r| matches!(r.key, RowKey::Chat(_)));
        if last.is_none() || self.table.index(view) != last {
            return OverlayOutcome::Stay;
        }
        let filter = self.table.filter.trim();
        if filter.is_empty() {
            return OverlayOutcome::Send(Msg::LoadChats {
                query: self.filter.query(),
                more: true,
            });
        }
        let query = ListQuery::Search(filter.to_owned());
        match ctx.app.chats.page(&query) {
            Some(page) if !page.exhausted => {
                OverlayOutcome::Send(Msg::LoadChats { query, more: true })
            }
            _ => OverlayOutcome::Stay,
        }
    }

    fn chat_key(&mut self, key: KeyEvent, selected: Option<Uuid>, ctx: &ViewCtx) -> OverlayOutcome {
        let root_of = |id: Uuid| {
            ctx.app
                .chats
                .find(id)
                .and_then(|c| c.parent_chat_id)
                .unwrap_or(id)
        };
        match key.code {
            KeyCode::Tab => {
                let query = self.filter.query();
                if let Some(Load::Failed(_)) = ctx.app.chats.page(&query).map(|p| &p.load) {
                    return OverlayOutcome::Send(Msg::LoadChats { query, more: false });
                }
                self.filter = self.filter.next();
                self.table.selected = None;
                let idle = ctx
                    .app
                    .chats
                    .page(&self.filter.query())
                    .is_some_and(|p| p.load == Load::Idle);
                if idle {
                    OverlayOutcome::Send(Msg::LoadChats {
                        query: self.filter.query(),
                        more: false,
                    })
                } else {
                    OverlayOutcome::Stay
                }
            }
            KeyCode::Right => {
                if let Some(id) = selected {
                    self.expanded.insert(root_of(id));
                }
                OverlayOutcome::Stay
            }
            KeyCode::Left => {
                if let Some(id) = selected {
                    let root = root_of(id);
                    self.expanded.remove(&root);
                    self.table.selected = Some(RowKey::Chat(root));
                }
                OverlayOutcome::Stay
            }
            KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let Some(id) = selected else {
                    return OverlayOutcome::Stay;
                };
                // A child or a running family cannot be archived, so the core's own refusal
                // is the answer on the first press; asking to confirm first would ask a
                // question whose answer is already known.
                let blocked = ctx
                    .app
                    .chats
                    .find(id)
                    .is_some_and(|c| c.parent_chat_id.is_some())
                    || ctx.app.chats.family_running(id);
                let listed = ctx.app.chats.find(id);
                let archived = listed.is_some_and(|c| c.archived == Some(true));
                match c {
                    // Unarchiving needs no question, and neither does a refusal the core
                    // already knows.
                    'a' if blocked || archived => {
                        OverlayOutcome::Send(Msg::ChatAction(ChatAction::ToggleArchive(id)))
                    }
                    'a' => match listed {
                        Some(chat) => {
                            self.archive = Some(ArchivePrompt::new(id, chat, ctx.app));
                            OverlayOutcome::Stay
                        }
                        // Every archive goes through the box, so a chat the list does not
                        // hold is left alone.
                        None => OverlayOutcome::Stay,
                    },
                    'e' => OverlayOutcome::Send(Msg::ChatAction(ChatAction::Rename(id))),
                    'p' => OverlayOutcome::Send(Msg::ChatAction(ChatAction::TogglePin(id))),
                    'u' => OverlayOutcome::Send(Msg::ChatAction(ChatAction::ToggleRead(id))),
                    _ => OverlayOutcome::Stay,
                }
            }
            _ => OverlayOutcome::Stay,
        }
    }
}

impl Overlay {
    /// Whether `kind` opens a table overlay; the effort slider is `PickerState`'s instead.
    pub fn is_table(kind: Picker) -> bool {
        !matches!(kind, Picker::Effort)
    }

    /// The table for `kind`, or `None` for the effort slider, which `PickerState` draws.
    pub fn open(kind: Picker, app: &App) -> Option<Overlay> {
        if !Self::is_table(kind) {
            return None;
        }
        Some(match kind {
            Picker::Model => Overlay::Model(TableState::default()),
            Picker::Workspace => Overlay::Workspace(TableState::default()),
            Picker::Organization => Overlay::Organization(match app.org_id {
                Some(id) => TableState::with_selected(RowKey::Organization(id)),
                None => TableState::default(),
            }),
            Picker::Effort => return None,
        })
    }

    /// The `/statusline` editor, starting from the footer's settings in effect.
    pub fn statusline(statusline: &StatuslineConfig) -> Overlay {
        Overlay::Statusline(StatuslineState::new(statusline))
    }

    /// `/chats` with `query` already typed, starting on the open chat when it is listed. An
    /// open subagent's root starts expanded, so its row can be selected.
    pub fn chats(query: String, app: &App) -> Overlay {
        let expanded = app
            .chat_id
            .and_then(|id| app.chats.find(id))
            .and_then(|c| c.parent_chat_id)
            .into_iter()
            .collect();
        Overlay::Chats(Box::new(ChatsState {
            table: TableState {
                filter: query,
                selected: app.chat_id.map(RowKey::Chat),
                ..TableState::default()
            },
            filter: Filter::All,
            expanded,
            archive: None,
        }))
    }

    /// Adds pasted `text` to the filter of an overlay that has one, and drops it otherwise.
    /// In `/model`, a paste that moves the highlight to another model returns
    /// `Msg::ThresholdCommit`, so the edit left behind saves as it does on a typed filter.
    pub fn paste_filter(&mut self, text: &str, ctx: &ViewCtx) -> Option<Msg> {
        if !matches!(
            self,
            Overlay::Model(_) | Overlay::Workspace(_) | Overlay::Chats(_)
        ) {
            return None;
        }
        // The archive confirmation holds the keyboard, so a paste goes nowhere.
        if let Overlay::Chats(chats) = self
            && chats.archive.is_some()
        {
            return None;
        }
        let model = |o: &Overlay| match o {
            Overlay::Model(state) => match state
                .selected_row(&model_view(ctx, &state.filter))
                .map(|r| r.key.clone())
            {
                Some(RowKey::Model(id)) => Some(id),
                _ => None,
            },
            _ => None,
        };
        let before = model(self);
        let state = self.state_mut();
        state.filter.push_str(text);
        state.selected = None;
        (matches!(self, Overlay::Model(_)) && model(self) != before).then_some(Msg::ThresholdCommit)
    }

    /// Whether the overlay covers the whole transcript area. `/model` does, as `/chats` does,
    /// so its columns have room.
    pub fn full_height(&self) -> bool {
        matches!(
            self,
            Overlay::Chats(_)
                | Overlay::Model(_)
                | Overlay::Subagents(_)
                | Overlay::Info(_)
                | Overlay::Git(_)
                | Overlay::Mcp(_)
                | Overlay::Usage(_)
                | Overlay::Files(_)
        )
    }

    /// The message that tells the core this overlay closed, for overlays whose state lives there.
    pub fn close_msg(&self) -> Option<Msg> {
        match self {
            Overlay::Subagents(_) => Some(Msg::PreviewChat(None)),
            Overlay::Info(_) => Some(Msg::InfoClosed),
            Overlay::WorkspaceDetails(_) => Some(Msg::WorkspaceClosed),
            Overlay::Git(_) => Some(Msg::GitClosed),
            Overlay::Mcp(_) => Some(Msg::McpClosed),
            Overlay::Usage(_) => Some(Msg::UsageClosed),
            // A draft held for a model pick goes back to the composer.
            Overlay::Model(_) => Some(Msg::ModelPickerClosed),
            _ => None,
        }
    }

    /// Whether the overlay shows a spinner, so the screen keeps redrawing.
    pub fn animates(&self, app: &App) -> bool {
        match self {
            Overlay::Chats(_) => app.chats.any_running(),
            Overlay::Subagents(_) => app
                .subagents()
                .1
                .iter()
                .any(|c| spins(chat_status(c).as_ref())),
            _ => false,
        }
    }

    /// Whether the overlay shows relative times, so it redraws when the minute turns.
    pub fn shows_times(&self) -> bool {
        matches!(
            self,
            Overlay::Chats(_) | Overlay::Subagents(_) | Overlay::Files(_)
        )
    }

    /// How far the preview is scrolled, for the overlays that show one.
    pub fn preview_scroll(&self) -> Option<usize> {
        match self {
            Overlay::Subagents(s) => Some(s.scroll),
            _ => None,
        }
    }

    /// Keeps the scroll within the `max` the last draw reported.
    pub fn clamp_scroll(&mut self, max: usize) {
        let state = self.state_mut();
        state.scroll = state.scroll.min(max);
    }

    pub fn state(&self) -> &TableState {
        match self {
            Overlay::Model(s)
            | Overlay::Workspace(s)
            | Overlay::Organization(s)
            | Overlay::Queue(s)
            | Overlay::Info(s)
            | Overlay::WorkspaceDetails(s)
            | Overlay::Git(s)
            | Overlay::Mcp(s)
            | Overlay::Usage(s)
            | Overlay::Files(s) => s,
            Overlay::Chats(c) => &c.table,
            Overlay::Subagents(s) => &s.table,
            Overlay::Statusline(s) => &s.table,
        }
    }

    fn state_mut(&mut self) -> &mut TableState {
        match self {
            Overlay::Model(s)
            | Overlay::Workspace(s)
            | Overlay::Organization(s)
            | Overlay::Queue(s)
            | Overlay::Info(s)
            | Overlay::WorkspaceDetails(s)
            | Overlay::Git(s)
            | Overlay::Mcp(s)
            | Overlay::Usage(s)
            | Overlay::Files(s) => s,
            Overlay::Chats(c) => &mut c.table,
            Overlay::Subagents(s) => &mut s.table,
            Overlay::Statusline(s) => &mut s.table,
        }
    }

    pub fn view(&self, ctx: &ViewCtx) -> TableView {
        match self {
            Overlay::Model(s) => model_view(ctx, &s.filter),
            Overlay::Workspace(s) => workspace_view(ctx, &s.filter),
            Overlay::Organization(_) => organization_view(ctx),
            Overlay::Chats(c) => chats_view(c, ctx),
            Overlay::Subagents(_) => subagents_view(ctx),
            Overlay::Queue(_) => queue_view(ctx.app),
            Overlay::Info(_) => info_view(ctx),
            Overlay::WorkspaceDetails(_) => workspace_details_view(ctx),
            Overlay::Git(_) => git_view(ctx),
            Overlay::Mcp(_) => mcp_view(ctx),
            Overlay::Statusline(s) => statusline_view(s, ctx.theme),
            Overlay::Usage(_) => usage_view(ctx),
            Overlay::Files(_) => files_view(ctx),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, ctx: &ViewCtx) -> OverlayOutcome {
        if let Overlay::Model(state) = self {
            return model_key(state, key, ctx);
        }
        if let Overlay::Chats(chats) = self {
            return chats.handle_key(key, ctx);
        }
        if let Overlay::Subagents(s) = self {
            return s.handle_key(key, ctx);
        }
        if let Overlay::Statusline(s) = self {
            return s.handle_key(key, ctx);
        }
        let view = self.view(ctx);
        let close = self.close_msg();
        let state = self.state_mut();
        match state.handle_key(key, &view) {
            TableKey::Esc => match close {
                Some(msg) => OverlayOutcome::CloseWith(msg),
                None => OverlayOutcome::Close,
            },
            TableKey::Enter => match state.selected_row(&view).map(|r| r.key.clone()) {
                Some(RowKey::Model(id)) => OverlayOutcome::CloseWith(Msg::ModelChosen(id)),
                Some(RowKey::Workspace(ws)) => OverlayOutcome::CloseWith(Msg::WorkspaceChosen(ws)),
                // A dimmed organization is sent too, so the core refuses it with the reason.
                Some(RowKey::Organization(id)) => {
                    OverlayOutcome::CloseWith(Msg::OrganizationChosen(id))
                }
                Some(RowKey::Chat(id)) => OverlayOutcome::CloseWith(Msg::OpenChat(id)),
                Some(RowKey::Queued(id)) => {
                    OverlayOutcome::Send(Msg::QueueAction(QueueAction::Promote(id)))
                }
                // Only organization servers get this key; inline and workspace servers are
                // drawn as text rows, since a message cannot change them.
                Some(RowKey::Mcp(id)) => OverlayOutcome::Send(Msg::ToggleMcp(id)),
                Some(RowKey::File(id)) => {
                    OverlayOutcome::Send(Msg::FileAction(FileAction::Save(id)))
                }
                Some(RowKey::Action("pr")) => {
                    OverlayOutcome::Send(Msg::GitAction(GitAction::OpenPr))
                }
                Some(RowKey::Action("diff")) => {
                    OverlayOutcome::Send(Msg::GitAction(GitAction::ViewDiff))
                }
                Some(RowKey::Action(id)) => {
                    match WORKSPACE_ACTIONS.iter().find(|(a, _, _)| *a == id) {
                        Some((_, _, action)) => {
                            OverlayOutcome::CloseWith(Msg::WorkspaceAction(*action))
                        }
                        None => OverlayOutcome::Stay,
                    }
                }
                // Only `Overlay::Chats` ever draws this row, and its own `handle_key` above
                // handles its Enter before reaching here.
                // `RowKey::None` stands for an unselectable row, such as a group header, and
                // never reaches here either, since headers are never selected.
                Some(RowKey::SearchAll) | Some(RowKey::None) | None => OverlayOutcome::Stay,
            },
            TableKey::Unhandled if matches!(key.code, KeyCode::Delete | KeyCode::Backspace) => {
                match state.selected_row(&view).map(|r| r.key.clone()) {
                    Some(RowKey::Queued(id)) => {
                        OverlayOutcome::Send(Msg::QueueAction(QueueAction::Remove(id)))
                    }
                    _ => OverlayOutcome::Stay,
                }
            }
            TableKey::Unhandled
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                match (state.selected_row(&view).map(|r| r.key.clone()), key.code) {
                    // Space acts as Enter does. The panel has no filter, so a space is never text.
                    (Some(RowKey::Mcp(id)), KeyCode::Char(' ')) => {
                        OverlayOutcome::Send(Msg::ToggleMcp(id))
                    }
                    (Some(RowKey::File(id)), KeyCode::Char('s')) => {
                        OverlayOutcome::Send(Msg::FileAction(FileAction::SaveAs(id)))
                    }
                    (Some(RowKey::File(id)), KeyCode::Char('v')) => {
                        OverlayOutcome::Send(Msg::FileAction(FileAction::View(id)))
                    }
                    (Some(RowKey::File(id)), KeyCode::Char('g')) => {
                        OverlayOutcome::CloseWith(Msg::FileAction(FileAction::Jump(id)))
                    }
                    _ => OverlayOutcome::Stay,
                }
            }
            TableKey::Handled | TableKey::Unhandled => OverlayOutcome::Stay,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use scuttle_core::app::{OrgRef, WorkspaceRef};
    use scuttle_core::config::BusyBehavior;
    use scuttle_core::files::{FileAction, SaveConflict};
    use serde_json::json;

    use crate::table::{self, RowKey, RowKind};

    /// The text-mode unread dot, U+1F535, two cells wide under both `width` and `width_cjk`.
    const UNREAD: &str = "\u{1f535}";

    pub(crate) fn press(o: &mut Overlay, app: &App, code: KeyCode) -> OverlayOutcome {
        let theme = Theme::terminal(true);
        let ctx = ViewCtx {
            app,
            theme: &theme,
            now_unix: 0,
            offset: chrono::FixedOffset::east_opt(0).unwrap(),
            elapsed: Duration::ZERO,
            width: 80,
            pin_icon: "📌",
        };
        o.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &ctx)
    }

    /// Loads `list` as the model catalog, giving every model with no `ai_provider_id` a shared,
    /// resolvable provider, since an orphan provider is now dropped rather than grouped under
    /// "Other".
    fn models(app: &mut App, list: serde_json::Value) {
        let provider = uuid::Uuid::new_v4();
        app.providers = serde_json::from_value(json!([
            {"id": provider, "display_name": "Provider", "available": true}
        ]))
        .unwrap();
        let mut parsed: Vec<types::CodersdkChatModel> = serde_json::from_value(list).unwrap();
        for m in &mut parsed {
            if m.ai_provider_id.is_none() {
                m.ai_provider_id = Some(provider);
            }
        }
        app.update(Msg::ModelsLoaded(parsed));
    }

    fn org(id: uuid::Uuid, name: &str, is_default: bool, can_create_chats: bool) -> OrgRef {
        OrgRef {
            id,
            name: name.to_lowercase(),
            display_name: name.into(),
            is_default,
            can_create_chats,
        }
    }

    #[test]
    fn model_rows_filter_fuzzily_and_enter_chooses() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (sonnet, gpt) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        models(
            &mut app,
            json!([
                {"id": gpt, "display_name": "GPT-5", "enabled": true, "reasoning_efforts": []},
                {"id": sonnet, "display_name": "Claude Sonnet", "enabled": true, "reasoning_efforts": []}
            ]),
        );
        let mut o = Overlay::open(Picker::Model, &app).unwrap();
        for c in "son".chars() {
            press(&mut o, &app, KeyCode::Char(c));
        }
        assert!(matches!(
            press(&mut o, &app, KeyCode::Enter),
            OverlayOutcome::CloseWith(Msg::ModelChosen(id)) if id == sonnet
        ));
    }

    #[test]
    fn model_rows_select_with_arrows_and_enter() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        models(
            &mut app,
            json!([
                {"id": a, "display_name": "A", "enabled": true, "reasoning_efforts": []},
                {"id": b, "display_name": "B", "enabled": true, "reasoning_efforts": []}
            ]),
        );
        let mut o = Overlay::open(Picker::Model, &app).unwrap();
        press(&mut o, &app, KeyCode::Down);
        assert!(matches!(
            press(&mut o, &app, KeyCode::Enter),
            OverlayOutcome::CloseWith(Msg::ModelChosen(id)) if id == b
        ));
    }

    #[test]
    fn disabled_models_are_not_offered() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let provider = uuid::Uuid::new_v4();
        app.providers = serde_json::from_value(
            json!([{"id": provider, "display_name": "Prov", "available": true}]),
        )
        .unwrap();
        app.models = serde_json::from_value(json!([
            {"id": a, "display_name": "A", "ai_provider_id": provider, "enabled": true, "reasoning_efforts": []},
            {"id": b, "display_name": "B", "ai_provider_id": provider, "enabled": false, "reasoning_efforts": []}
        ]))
        .unwrap();
        let theme = Theme::terminal(true);
        let ctx = ViewCtx {
            app: &app,
            theme: &theme,
            now_unix: 0,
            offset: chrono::FixedOffset::east_opt(0).unwrap(),
            elapsed: Duration::ZERO,
            width: 80,
            pin_icon: "📌",
        };
        let mut o = Overlay::open(Picker::Model, &app).unwrap();
        let view = o.view(&ctx);
        assert_eq!(
            view.rows.len(),
            2,
            "the provider header and A; a disabled model is not offered"
        );
        assert!(matches!(
            o.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx),
            OverlayOutcome::CloseWith(Msg::ModelChosen(id)) if id == a
        ));
    }

    #[test]
    fn the_model_table_groups_by_provider_and_cannot_pick_an_unusable_model() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (anthropic, openai, sonnet, gpt) = (
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
        );
        app.update(Msg::CatalogLoaded(Box::new(serde_json::from_value(json!({
            "models": [
                {"id": gpt, "display_name": "GPT-5", "ai_provider_id": openai, "enabled": true, "reasoning_efforts": []},
                {"id": sonnet, "display_name": "Claude Sonnet", "ai_provider_id": anthropic, "enabled": true, "is_default": true, "context_limit": 200000, "reasoning_efforts": []}
            ],
            "providers": [
                {"id": openai, "display_name": "OpenAI", "available": false, "unavailable_reason": "missing_api_key"},
                {"id": anthropic, "display_name": "Anthropic", "available": true}
            ],
            "unsupported_providers": []
        })).unwrap())));
        let theme = Theme::terminal(true);
        let ctx = ViewCtx {
            app: &app,
            theme: &theme,
            now_unix: 0,
            offset: chrono::FixedOffset::east_opt(0).unwrap(),
            elapsed: Duration::ZERO,
            width: 80,
            pin_icon: "📌",
        };
        let mut o = Overlay::open(Picker::Model, &app).unwrap();
        let view = o.view(&ctx);
        let text: Vec<String> = view
            .rows
            .iter()
            .map(|r| {
                r.cells
                    .iter()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .collect();
        assert_eq!(
            text,
            [
                "Anthropic",
                "  Claude Sonnet|current|200.0k tokens|…|",
                "OpenAI \u{b7} no API key is configured",
                "  GPT-5|||…|"
            ],
            "groups sort alphabetically by provider with an empty filter"
        );
        assert!(
            matches!(o.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx),
                OverlayOutcome::CloseWith(Msg::ModelChosen(id)) if id == sonnet),
            "Enter immediately chooses the current model, the only usable one"
        );
    }

    #[test]
    fn models_without_a_name_are_not_offered() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (named, bare) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let provider = uuid::Uuid::new_v4();
        app.providers = serde_json::from_value(
            json!([{"id": provider, "display_name": "Prov", "available": true}]),
        )
        .unwrap();
        app.models = serde_json::from_value(json!([
            {"id": bare, "ai_provider_id": provider, "enabled": true, "reasoning_efforts": []},
            {"id": named, "model": "gpt-5", "ai_provider_id": provider, "enabled": true, "reasoning_efforts": []}
        ]))
        .unwrap();
        let theme = Theme::terminal(true);
        let ctx = ViewCtx {
            app: &app,
            theme: &theme,
            now_unix: 0,
            offset: chrono::FixedOffset::east_opt(0).unwrap(),
            elapsed: Duration::ZERO,
            width: 80,
            pin_icon: "📌",
        };
        let view = Overlay::open(Picker::Model, &app).unwrap().view(&ctx);
        let keys: Vec<RowKey> = view.rows.iter().map(|r| r.key.clone()).collect();
        assert_eq!(
            keys,
            [RowKey::None, RowKey::Model(named)],
            "the provider header and the named model; a nameless model is not offered"
        );
        assert_eq!(view.rows[1].cells[0].to_string(), "  gpt-5");
    }

    #[test]
    fn a_model_with_an_unresolved_provider_is_not_offered() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let orphan = uuid::Uuid::new_v4();
        app.models = serde_json::from_value(json!([
            {"id": uuid::Uuid::new_v4(), "display_name": "Ghost", "ai_provider_id": orphan, "enabled": true, "reasoning_efforts": []}
        ]))
        .unwrap();
        let theme = Theme::terminal(true);
        let ctx = ViewCtx {
            app: &app,
            theme: &theme,
            now_unix: 0,
            offset: chrono::FixedOffset::east_opt(0).unwrap(),
            elapsed: Duration::ZERO,
            width: 80,
            pin_icon: "📌",
        };
        let view = Overlay::open(Picker::Model, &app).unwrap().view(&ctx);
        assert!(
            view.rows.is_empty(),
            "a model whose provider id names no provider is dropped entirely: {:?}",
            view.rows.iter().map(|r| r.key.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_filter_with_no_matches_still_names_the_unsupported_providers() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let provider = uuid::Uuid::new_v4();
        app.providers = serde_json::from_value(
            json!([{"id": provider, "display_name": "Prov", "available": true}]),
        )
        .unwrap();
        app.models = serde_json::from_value(json!([
            {"id": uuid::Uuid::new_v4(), "display_name": "A", "ai_provider_id": provider, "enabled": true, "reasoning_efforts": []}
        ]))
        .unwrap();
        app.unsupported_providers =
            serde_json::from_value(json!([{"display_name": "Copilot", "provider": "copilot"}]))
                .unwrap();
        let theme = Theme::terminal(true);
        let ctx = ViewCtx {
            app: &app,
            theme: &theme,
            now_unix: 0,
            offset: chrono::FixedOffset::east_opt(0).unwrap(),
            elapsed: Duration::ZERO,
            width: 80,
            pin_icon: "📌",
        };
        let mut o = Overlay::open(Picker::Model, &app).unwrap();
        o.state_mut().filter = "zzz".into();
        let view = o.view(&ctx);
        assert!(view.rows.is_empty());
        let status = view.status.as_deref().unwrap_or_default();
        assert!(status.contains("No models match."), "{status}");
        assert!(
            status.contains("Configured but not usable here: Copilot"),
            "{status}"
        );
    }

    #[test]
    fn filtering_workspaces_hides_none_so_enter_picks_the_match() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (dev, prod) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        app.update(Msg::WorkspacesLoaded(vec![
            WorkspaceRef {
                id: prod,
                name: "prod".into(),
                ..Default::default()
            },
            WorkspaceRef {
                id: dev,
                name: "dev".into(),
                ..Default::default()
            },
        ]));
        let theme = Theme::terminal(true);
        let ctx = ViewCtx {
            app: &app,
            theme: &theme,
            now_unix: 0,
            offset: chrono::FixedOffset::east_opt(0).unwrap(),
            elapsed: Duration::ZERO,
            width: 80,
            pin_icon: "📌",
        };
        let mut o = Overlay::open(Picker::Workspace, &app).unwrap();
        for c in "dev".chars() {
            press(&mut o, &app, KeyCode::Char(c));
        }
        let keys: Vec<RowKey> = o.view(&ctx).rows.iter().map(|r| r.key.clone()).collect();
        assert_eq!(keys, [RowKey::Workspace(Some(dev))]);
        for _ in 0..3 {
            press(&mut o, &app, KeyCode::Backspace);
        }
        assert_eq!(
            o.view(&ctx).rows[0].key,
            RowKey::Workspace(None),
            "clearing the filter brings none back"
        );
        for c in "dev".chars() {
            press(&mut o, &app, KeyCode::Char(c));
        }
        assert!(matches!(
            press(&mut o, &app, KeyCode::Enter),
            OverlayOutcome::CloseWith(Msg::WorkspaceChosen(Some(id))) if id == dev
        ));
    }

    #[test]
    fn workspace_rows_offer_none_first_and_escape_closes() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let ws = uuid::Uuid::new_v4();
        app.update(Msg::WorkspacesLoaded(vec![WorkspaceRef {
            id: ws,
            name: "dev".into(),
            ..Default::default()
        }]));
        let mut o = Overlay::open(Picker::Workspace, &app).unwrap();
        assert!(matches!(
            press(&mut o, &app, KeyCode::Enter),
            OverlayOutcome::CloseWith(Msg::WorkspaceChosen(None))
        ));
        let mut o = Overlay::open(Picker::Workspace, &app).unwrap();
        press(&mut o, &app, KeyCode::Down);
        assert!(matches!(
            press(&mut o, &app, KeyCode::Enter),
            OverlayOutcome::CloseWith(Msg::WorkspaceChosen(Some(id))) if id == ws
        ));
        assert!(matches!(
            press(&mut o, &app, KeyCode::Esc),
            OverlayOutcome::Close
        ));
    }

    #[test]
    fn the_workspace_table_shows_template_status_and_last_used() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::WorkspacesLoaded(vec![WorkspaceRef {
            id: uuid::Uuid::new_v4(),
            name: "dev".into(),
            template: "Docker".into(),
            status: "running".into(),
            last_used: Some(1_000_000 - 7200),
        }]));
        let theme = Theme::terminal(true);
        let ctx = ViewCtx {
            app: &app,
            theme: &theme,
            now_unix: 1_000_000,
            offset: chrono::FixedOffset::east_opt(0).unwrap(),
            elapsed: Duration::ZERO,
            width: 80,
            pin_icon: "📌",
        };
        let view = Overlay::open(Picker::Workspace, &app).unwrap().view(&ctx);
        let cells: Vec<String> = view.rows[1].cells.iter().map(|c| c.to_string()).collect();
        assert_eq!(cells, ["dev", "Docker", "running", "2h"]);
        let empty = App::new(BusyBehavior::Queue, true);
        let ctx = ViewCtx { app: &empty, ..ctx };
        let view = Overlay::open(Picker::Workspace, &empty).unwrap().view(&ctx);
        assert_eq!(view.status.as_deref(), Some("Loading workspaces…"));
    }

    #[test]
    fn a_workspace_unused_for_100_days_shows_the_full_age() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::WorkspacesLoaded(vec![WorkspaceRef {
            id: uuid::Uuid::new_v4(),
            name: "dev".into(),
            last_used: Some(10_000_000 - 100 * 86_400),
            ..Default::default()
        }]));
        let theme = Theme::terminal(true);
        let ctx = ViewCtx {
            app: &app,
            theme: &theme,
            now_unix: 10_000_000,
            offset: chrono::FixedOffset::east_opt(0).unwrap(),
            elapsed: Duration::ZERO,
            width: 80,
            pin_icon: "📌",
        };
        let o = Overlay::open(Picker::Workspace, &app).unwrap();
        let view = o.view(&ctx);
        let mut term = Terminal::new(TestBackend::new(60, 8)).unwrap();
        term.draw(|f| {
            table::render(f, f.area(), &view, o.state(), &theme, None);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let text = |y: u16| (0..60u16).map(|x| buf[(x, y)].symbol()).collect::<String>();
        let row = (0..8u16).find(|&y| text(y).contains("dev")).unwrap();
        assert!(text(row).contains("100d"), "{}", text(row));
    }

    #[test]
    fn organization_rows_mark_the_default_and_the_current_one() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (product, coder) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        app.update(Msg::OrganizationsLoaded(vec![
            org(product, "Product", false, true),
            org(coder, "Coder", true, true),
            org(uuid::Uuid::new_v4(), "Legal", false, false),
        ]));
        app.update(Msg::Started {
            org_id: coder,
            open_chat: None,
        });
        let theme = Theme::terminal(true);
        let ctx = ViewCtx {
            app: &app,
            theme: &theme,
            now_unix: 0,
            offset: chrono::FixedOffset::east_opt(0).unwrap(),
            elapsed: Duration::ZERO,
            width: 80,
            pin_icon: "📌",
        };
        let mut o = Overlay::open(Picker::Organization, &app).unwrap();
        let view = o.view(&ctx);
        let labels: Vec<String> = view.rows.iter().map(|r| r.cells[0].to_string()).collect();
        assert_eq!(labels, ["Product", "Coder (default, current)", "Legal"]);
        assert!(
            view.rows[2].selectable(),
            "a denied organization can still be chosen, so the core can say why"
        );
        assert_eq!(view.rows[2].kind, RowKind::Dimmed);
        assert_eq!(
            view.rows[2].cells[1].to_string(),
            "no permission to create chats"
        );
        assert!(
            matches!(o.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &ctx),
                OverlayOutcome::CloseWith(Msg::OrganizationChosen(id)) if id == coder),
            "the table starts on the current organization"
        );
    }

    #[test]
    fn an_organization_without_chat_permission_is_greyed_out_with_the_reason() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (product, coder) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        app.update(Msg::OrganizationsLoaded(vec![
            org(product, "Product", false, false),
            org(coder, "Coder", true, true),
        ]));
        app.update(Msg::Started {
            org_id: coder,
            open_chat: None,
        });
        let theme = Theme::terminal(true);
        let ctx = ViewCtx {
            app: &app,
            theme: &theme,
            now_unix: 0,
            offset: chrono::FixedOffset::east_opt(0).unwrap(),
            elapsed: Duration::ZERO,
            width: 80,
            pin_icon: "📌",
        };
        let o = Overlay::open(Picker::Organization, &app).unwrap();
        let view = o.view(&ctx);
        let kinds: Vec<RowKind> = view.rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, [RowKind::Dimmed, RowKind::Item]);
        let mut term = Terminal::new(TestBackend::new(50, 10)).unwrap();
        term.draw(|f| {
            table::render(f, f.area(), &view, o.state(), &theme, None);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let text = |y: u16| (0..50u16).map(|x| buf[(x, y)].symbol()).collect::<String>();
        let row = (0..10u16).find(|&y| text(y).contains("Product")).unwrap();
        assert!(
            text(row).contains("no permission to create chats"),
            "the row gives the reason: {}",
            text(row)
        );
        let x = (0..50u16).find(|&x| buf[(x, row)].symbol() == "P").unwrap();
        assert_eq!(Some(buf[(x, row)].fg), theme.dim.fg, "the row is dimmed");
    }

    /// An app listing `titles` as root chats, newest first, the first with `children`.
    fn listed_app(titles: &[&str], children: serde_json::Value) -> (App, Vec<uuid::Uuid>) {
        let mut app = App::new(BusyBehavior::Queue, true);
        let ids: Vec<uuid::Uuid> = titles.iter().map(|_| uuid::Uuid::new_v4()).collect();
        let chats: Vec<serde_json::Value> = titles
            .iter()
            .zip(&ids)
            .enumerate()
            .map(|(i, (title, id))| {
                json!({"id": id, "title": title, "status": "waiting",
                    "updated_at": format!("2026-09-30T10:{:02}:00Z", 59 - i),
                    "children": if i == 0 { children.clone() } else { json!([]) },
                    "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})
            })
            .collect();
        app.update(Msg::ChatsLoaded {
            query: scuttle_core::chat_list::ListQuery::Default,
            offset: 0,
            chats: serde_json::from_value(serde_json::Value::Array(chats)).unwrap(),
        });
        (app, ids)
    }

    fn child(id: uuid::Uuid, parent: uuid::Uuid, title: &str, status: &str) -> serde_json::Value {
        json!({"id": id, "title": title, "status": status, "parent_chat_id": parent,
            "updated_at": "2026-09-30T10:00:00Z", "children": [], "files": [],
            "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})
    }

    fn ctx_for<'a>(app: &'a App, theme: &'a Theme) -> ViewCtx<'a> {
        ViewCtx {
            app,
            theme,
            now_unix: 0,
            offset: chrono::FixedOffset::east_opt(0).unwrap(),
            elapsed: Duration::ZERO,
            width: 80,
            pin_icon: "📌",
        }
    }

    fn statusline_rows(o: &Overlay) -> Vec<(scuttle_core::config::StatusField, bool)> {
        match o {
            Overlay::Statusline(s) => s.list.rows.clone(),
            _ => panic!("not the statusline editor"),
        }
    }

    #[test]
    fn brackets_move_a_shown_field_past_hidden_rows() {
        use scuttle_core::config::{StatusField, StatuslineConfig};
        let app = App::new(BusyBehavior::Queue, true);
        let theme = Theme::terminal(true);
        let ctx = ctx_for(&app, &theme);
        let mut o = Overlay::statusline(&StatuslineConfig::default());
        let send = |o: &mut Overlay, code, mods| o.handle_key(KeyEvent::new(code, mods), &ctx);
        assert!(
            matches!(
                send(&mut o, KeyCode::Char('['), KeyModifiers::NONE),
                OverlayOutcome::Stay
            ),
            "the first shown field cannot move up"
        );
        match send(&mut o, KeyCode::Char(']'), KeyModifiers::NONE) {
            OverlayOutcome::Statusline(s) => {
                assert_eq!(s.fields[..2], [StatusField::Effort, StatusField::Model]);
            }
            other => panic!("{other:?}"),
        }
        match send(&mut o, KeyCode::Char('['), KeyModifiers::NONE) {
            OverlayOutcome::Statusline(s) => {
                assert_eq!(s.fields[..2], [StatusField::Model, StatusField::Effort]);
            }
            other => panic!("{other:?}"),
        }
        // Hide effort, the row below model, then move model down past it.
        send(&mut o, KeyCode::Down, KeyModifiers::NONE);
        send(&mut o, KeyCode::Char(' '), KeyModifiers::NONE);
        send(&mut o, KeyCode::Up, KeyModifiers::NONE);
        for (code, mods) in [
            (KeyCode::Char(']'), KeyModifiers::NONE),
            (KeyCode::Down, KeyModifiers::ALT),
            (KeyCode::Down, KeyModifiers::SHIFT),
        ] {
            let before = statusline_rows(&o);
            match send(&mut o, code, mods) {
                OverlayOutcome::Statusline(s) => assert_ne!(
                    s.fields,
                    before
                        .iter()
                        .filter(|r| r.1)
                        .map(|r| r.0)
                        .collect::<Vec<_>>(),
                    "{code:?} {mods:?} changed the footer"
                ),
                other => panic!("{code:?} {mods:?} skipped no hidden row: {other:?}"),
            }
        }
        let rows = statusline_rows(&o);
        assert_eq!(
            rows[1],
            (StatusField::Effort, false),
            "the hidden row stays put"
        );
        assert_eq!(
            rows.iter()
                .filter(|r| r.1)
                .map(|r| r.0)
                .take(4)
                .collect::<Vec<_>>(),
            [
                StatusField::Context,
                StatusField::Workspace,
                StatusField::Organization,
                StatusField::Model
            ]
        );
        match &o {
            Overlay::Statusline(s) => assert_eq!(
                s.table.selected,
                Some(RowKey::Action("model")),
                "the selection follows the field"
            ),
            _ => unreachable!(),
        }
        // Past the last shown field, only hidden rows are below, so nothing moves.
        let mut o = Overlay::statusline(&StatuslineConfig {
            fields: vec![StatusField::Model],
            ..StatuslineConfig::default()
        });
        assert!(matches!(
            send(&mut o, KeyCode::Char(']'), KeyModifiers::NONE),
            OverlayOutcome::Stay
        ));
        assert_eq!(statusline_rows(&o)[0], (StatusField::Model, true));
    }

    #[test]
    fn the_statusline_hint_fits_at_80_columns() {
        use scuttle_core::config::StatuslineConfig;
        let app = App::new(BusyBehavior::Queue, true);
        let theme = Theme::terminal(true);
        let o = Overlay::statusline(&StatuslineConfig::default());
        let view = o.view(&ctx_for(&app, &theme));
        let (width, height) = (80u16, 16u16);
        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        term.draw(|f| {
            table::render(f, f.area(), &view, o.state(), &theme, None);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let text = |y: u16| (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>();
        let lines: Vec<String> = (0..height).map(text).collect();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("Space toggles · [ ] move · ←→ warning · Esc closes")),
            "{lines:#?}"
        );
    }

    #[test]
    fn the_statusline_editor_moves_toggles_and_steps_warnings() {
        use scuttle_core::config::{StatusField, StatuslineConfig};
        let app = App::new(BusyBehavior::Queue, true);
        let theme = Theme::terminal(true);
        let ctx = ctx_for(&app, &theme);
        let mut o = Overlay::statusline(&StatuslineConfig::default());
        let send = |o: &mut Overlay, code, mods| o.handle_key(KeyEvent::new(code, mods), &ctx);
        assert!(
            matches!(
                send(&mut o, KeyCode::Up, KeyModifiers::ALT),
                OverlayOutcome::Stay
            ),
            "the first row cannot move up, so nothing is saved"
        );
        match send(&mut o, KeyCode::Down, KeyModifiers::ALT) {
            OverlayOutcome::Statusline(s) => {
                assert_eq!(s.fields[..2], [StatusField::Effort, StatusField::Model]);
            }
            other => panic!("{other:?}"),
        }
        match send(&mut o, KeyCode::Char(' '), KeyModifiers::NONE) {
            OverlayOutcome::Statusline(s) => assert!(
                !s.fields.contains(&StatusField::Model),
                "the selection followed the moved row"
            ),
            other => panic!("{other:?}"),
        }
        send(&mut o, KeyCode::Up, KeyModifiers::SHIFT);
        assert_eq!(statusline_rows(&o)[0], (StatusField::Model, false));
        send(&mut o, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            statusline_rows(&o)[0],
            (StatusField::Model, true),
            "Enter toggles too"
        );
        assert!(
            matches!(
                send(&mut o, KeyCode::Right, KeyModifiers::NONE),
                OverlayOutcome::Stay
            ),
            "the model takes no warning"
        );
        send(&mut o, KeyCode::Down, KeyModifiers::NONE);
        send(&mut o, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(statusline_rows(&o)[2].0, StatusField::Context);
        match send(&mut o, KeyCode::Right, KeyModifiers::NONE) {
            OverlayOutcome::Statusline(s) => assert_eq!(s.thresholds.context, Some(50)),
            other => panic!("{other:?}"),
        }
        match send(&mut o, KeyCode::Left, KeyModifiers::NONE) {
            OverlayOutcome::Statusline(s) => assert_eq!(s.thresholds.context, None),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            send(&mut o, KeyCode::Esc, KeyModifiers::NONE),
            OverlayOutcome::Close
        ));
    }

    #[test]
    fn chats_load_more_only_when_moving_past_the_last_row_unfiltered() {
        let (app, _) = listed_app(&["alpha", "beta"], json!([]));
        let loads_more = |o: &OverlayOutcome| {
            matches!(o, OverlayOutcome::Send(Msg::LoadChats { more: true, .. }))
        };
        let mut o = Overlay::chats(String::new(), &app);
        assert!(
            !loads_more(&press(&mut o, &app, KeyCode::Char('b'))),
            "typing never pages"
        );
        press(&mut o, &app, KeyCode::Backspace);
        assert!(!loads_more(&press(&mut o, &app, KeyCode::Up)));
        assert!(
            loads_more(&press(&mut o, &app, KeyCode::Down)),
            "Down onto the last row"
        );
        let mut o = Overlay::chats(String::new(), &app);
        assert!(
            loads_more(&press(&mut o, &app, KeyCode::End)),
            "End jumps to the last row"
        );
        let mut o = Overlay::chats(String::new(), &app);
        assert!(loads_more(&press(&mut o, &app, KeyCode::PageDown)));
        let mut o = Overlay::chats("a".into(), &app);
        assert!(
            !loads_more(&press(&mut o, &app, KeyCode::Down)),
            "a filtered list does not page"
        );
    }

    #[test]
    fn chats_search_results_page_until_exhausted() {
        use scuttle_core::chat_list::{ListQuery, PAGE_SIZE};
        let mut app = App::new(BusyBehavior::Queue, true);
        let page = |n: usize| -> serde_json::Value {
            serde_json::Value::Array(
                (0..n)
                    .map(|i| {
                        json!({"id": uuid::Uuid::new_v4(), "title": format!("match {i}"),
                            "children": [], "files": [], "mcp_server_ids": [],
                            "inline_mcp_servers": [], "labels": {}})
                    })
                    .collect(),
            )
        };
        let theme = Theme::terminal(true);
        // Puts the selection one row before the last search result, so a single Down lands
        // exactly on it, the same way the unfiltered test lands on the last loaded chat.
        let select_second_to_last = |app: &App| -> Overlay {
            let mut o = Overlay::chats("watch".into(), app);
            let ctx = ctx_for(app, &theme);
            if let Overlay::Chats(state) = &mut o {
                let view = chats_view(state, &ctx);
                let last = view
                    .rows
                    .iter()
                    .rposition(|r| matches!(r.key, RowKey::Chat(_)))
                    .expect("at least one search result");
                state.table.selected = Some(view.rows[last - 1].key.clone());
            }
            o
        };
        let loads_more_search = |o: &OverlayOutcome| {
            matches!(
                o,
                OverlayOutcome::Send(Msg::LoadChats {
                    query: ListQuery::Search(_),
                    more: true
                })
            )
        };

        app.update(Msg::SearchChats("watch".into()));
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Search("watch".into()),
            offset: 0,
            chats: serde_json::from_value(page(PAGE_SIZE as usize)).unwrap(),
        });
        let mut o = select_second_to_last(&app);
        assert!(
            loads_more_search(&press(&mut o, &app, KeyCode::Down)),
            "a second page is requested at the last row"
        );

        // A short follow-up page exhausts the search.
        app.update(Msg::SearchChats("watch".into()));
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Search("watch".into()),
            offset: 0,
            chats: serde_json::from_value(page(2)).unwrap(),
        });
        let mut o = select_second_to_last(&app);
        assert!(
            !loads_more_search(&press(&mut o, &app, KeyCode::Down)),
            "nothing is requested once the search page is exhausted"
        );
    }

    #[test]
    fn the_family_marker_draws_the_status_columns_icons() {
        let root_id = uuid::Uuid::new_v4();
        let family = |status: &str| {
            let mut app = App::new(BusyBehavior::Queue, true);
            app.update(Msg::ChatsLoaded {
                query: scuttle_core::chat_list::ListQuery::Default,
                offset: 0,
                chats: serde_json::from_value(json!([{"id": root_id, "title": "root",
                    "status": "waiting", "updated_at": "2026-09-30T10:00:00Z",
                    "children": [child(uuid::Uuid::new_v4(), root_id, "sub", status)],
                    "files": [], "mcp_server_ids": [], "inline_mcp_servers": [],
                    "labels": {}}]))
                .unwrap(),
            });
            app
        };
        for (status, text, nerd) in [
            ("error", "+1 !", "+1 \u{f421} "),
            ("requires_action", "+1 ?", "+1 \u{f420} "),
        ] {
            let app = family(status);
            for (icons, want) in [(IconSet::Text, text), (IconSet::Nerd, nerd)] {
                let theme = Theme {
                    icons,
                    ..Theme::terminal(true)
                };
                let view = Overlay::chats(String::new(), &app).view(&ctx_for(&app, &theme));
                assert_eq!(
                    cell_text(&view, 0, FAMILY_COLUMN),
                    want,
                    "{status} {icons:?}"
                );
                assert_eq!(
                    view.widths[FAMILY_COLUMN],
                    Constraint::Length(want.width() as u16),
                    "the column fits the marker's slot: {status} {icons:?}"
                );
            }
        }
    }

    #[test]
    fn a_large_family_keeps_its_busiest_marker_visible() {
        let root_id = uuid::Uuid::new_v4();
        let kids: Vec<serde_json::Value> = (0..12)
            .map(|i| {
                let status = if i == 0 { "running" } else { "waiting" };
                child(uuid::Uuid::new_v4(), root_id, &format!("sub {i}"), status)
            })
            .collect();
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::ChatsLoaded {
            query: scuttle_core::chat_list::ListQuery::Default,
            offset: 0,
            chats: serde_json::from_value(json!([{"id": root_id, "title": "big",
                "status": "waiting", "updated_at": "2026-09-30T10:00:00Z", "children": kids,
                "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}]))
            .unwrap(),
        });
        let theme = Theme::terminal(true);
        let o = Overlay::chats(String::new(), &app);
        let view = o.view(&ctx_for(&app, &theme));
        let mut term = Terminal::new(TestBackend::new(60, 8)).unwrap();
        term.draw(|f| {
            table::render(f, f.area(), &view, o.state(), &theme, Some("X"));
        })
        .unwrap();
        let buf = term.backend().buffer();
        let shown: String = (0..8u16)
            .map(|y| (0..60u16).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n")
            .collect();
        assert!(shown.contains("+12 X"), "{shown}");
    }

    #[test]
    fn the_open_chat_never_shows_an_unread_dot() {
        let (mut app, ids) = listed_app(&["open", "other"], json!([]));
        app.chats.set_read(ids[0], false);
        app.chats.set_read(ids[1], false);
        app.chat_id = Some(ids[0]);
        let theme = Theme::terminal(true);
        let view = Overlay::chats(String::new(), &app).view(&ctx_for(&app, &theme));
        let dot = |i: usize| view.rows[i].cells[STATUS_COLUMN].to_string();
        assert_eq!(dot(0), " ", "the open chat is being read");
        assert_eq!(dot(1), UNREAD);
    }

    #[test]
    fn chats_opens_on_the_open_subagent() {
        let (root, sub) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::ChatsLoaded {
            query: scuttle_core::chat_list::ListQuery::Default,
            offset: 0,
            chats: serde_json::from_value(json!([{"id": root, "title": "root",
                "status": "waiting", "updated_at": "2026-09-30T10:00:00Z",
                "children": [child(sub, root, "explore", "waiting")],
                "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}]))
            .unwrap(),
        });
        app.chat_id = Some(sub);
        let theme = Theme::terminal(true);
        let o = Overlay::chats(String::new(), &app);
        let view = o.view(&ctx_for(&app, &theme));
        assert_eq!(
            o.state().selected_row(&view).map(|r| r.key.clone()),
            Some(RowKey::Chat(sub))
        );
    }

    #[test]
    fn the_queue_lists_messages_in_order_and_enter_sends_now_delete_removes() {
        let mut app = App::new(BusyBehavior::Queue, true);
        assert!(queue_view(&app).status.as_deref() == Some("Nothing is queued."));
        app.transcript.queued = serde_json::from_value(json!([
            {"id": 7, "content": [{"type": "text", "text": "then run the tests"}]},
            {"id": 8, "content": [{"type": "text", "text": "and open a PR"}]}
        ]))
        .unwrap();
        let view = queue_view(&app);
        assert_eq!(view.rows[0].cells[1].to_string(), "then run the tests");
        assert!(
            view.hint
                .as_deref()
                .is_some_and(|h| h.contains("sends now, interrupting a running turn")),
            "{:?}",
            view.hint
        );
        let mut o = Overlay::Queue(TableState::default());
        press(&mut o, &app, KeyCode::Down);
        assert!(matches!(
            press(&mut o, &app, KeyCode::Enter),
            OverlayOutcome::Send(Msg::QueueAction(scuttle_core::app::QueueAction::Promote(8)))
        ));
        assert!(matches!(
            press(&mut o, &app, KeyCode::Delete),
            OverlayOutcome::Send(Msg::QueueAction(scuttle_core::app::QueueAction::Remove(8)))
        ));
    }

    #[test]
    fn workspace_details_list_the_actions_and_enter_runs_one() {
        let app = App::new(BusyBehavior::Queue, true);
        let mut o = Overlay::WorkspaceDetails(TableState::default());
        let theme = Theme::terminal(true);
        let ctx = ViewCtx {
            app: &app,
            theme: &theme,
            now_unix: 0,
            offset: chrono::FixedOffset::east_opt(0).unwrap(),
            elapsed: Duration::ZERO,
            width: 80,
            pin_icon: "📌",
        };
        let actions: Vec<String> = o
            .view(&ctx)
            .rows
            .iter()
            .filter(|r| r.selectable())
            .map(|r| r.cells[0].to_string())
            .collect();
        assert_eq!(
            actions,
            [
                "Copy SSH command",
                "Open in web",
                "Detach",
                "Switch workspace"
            ]
        );
        press(&mut o, &app, KeyCode::Down);
        assert!(matches!(
            press(&mut o, &app, KeyCode::Enter),
            OverlayOutcome::CloseWith(Msg::WorkspaceAction(
                scuttle_core::app::WorkspaceAction::OpenWeb
            ))
        ));
    }

    #[test]
    fn the_git_panel_opens_the_pull_request_and_esc_closes_its_socket() {
        let app = App::new(BusyBehavior::Queue, true);
        let mut o = Overlay::Git(TableState::default());
        assert!(o.full_height());
        let theme = Theme::terminal(true);
        let actions: Vec<String> = o
            .view(&ctx_for(&app, &theme))
            .rows
            .iter()
            .filter(|r| r.selectable())
            .map(|r| r.cells[0].to_string())
            .collect();
        assert_eq!(actions, ["Open pull request", "View diff"]);
        assert!(matches!(
            press(&mut o, &app, KeyCode::Enter),
            OverlayOutcome::Send(Msg::GitAction(scuttle_core::app::GitAction::OpenPr))
        ));
        press(&mut o, &app, KeyCode::Down);
        assert!(matches!(
            press(&mut o, &app, KeyCode::Enter),
            OverlayOutcome::Send(Msg::GitAction(scuttle_core::app::GitAction::ViewDiff))
        ));
        assert!(matches!(
            press(&mut o, &app, KeyCode::Esc),
            OverlayOutcome::CloseWith(Msg::GitClosed)
        ));
    }

    #[test]
    fn the_mcp_panel_groups_the_servers_and_esc_closes_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let github = uuid::Uuid::new_v4();
        let chat = serde_json::from_value(json!({
            "id": uuid::Uuid::new_v4(), "mcp_server_ids": [github],
            "inline_mcp_servers": [{"slug": "local-tools", "url": "http://localhost:9000/mcp",
                "tool_allow_list": [], "tool_deny_list": []}],
            "children": [], "files": [], "labels": {}
        }))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: vec![],
        });
        app.mcp_panel = Some(scuttle_core::panels::McpPanel {
            servers: scuttle_core::panels::Fetched::Loaded(vec![
                serde_json::from_value(json!({"id": github, "display_name": "GitHub",
                    "url": "https://github.example/mcp", "tool_allow_list": [], "tool_deny_list": []}))
                .unwrap(),
            ]),
            health: scuttle_core::panels::Fetched::Loaded(None),
        });
        let mut o = Overlay::Mcp(TableState::default());
        assert!(o.full_height());
        let theme = Theme::terminal(true);
        let view = o.view(&ctx_for(&app, &theme));
        let rows: Vec<(String, bool)> = view
            .rows
            .iter()
            .map(|r| (r.cells[0].to_string(), r.selectable()))
            .collect();
        assert_eq!(
            rows,
            [
                ("Organization servers".to_owned(), false),
                ("  GitHub".to_owned(), true),
                ("Inline servers".to_owned(), false),
                ("  local-tools".to_owned(), false),
            ]
        );
        assert_eq!(view.rows[1].key, RowKey::Mcp(github));
        assert_eq!(
            view.status.as_deref(),
            Some("Connection health is not reported by the server.")
        );
        assert_eq!(
            view.hint.as_deref(),
            Some("Enter or Space turns a server on or off for the next message, Esc closes")
        );
        assert!(matches!(
            press(&mut o, &app, KeyCode::Enter),
            OverlayOutcome::Send(Msg::ToggleMcp(id)) if id == github
        ));
        assert!(matches!(
            press(&mut o, &app, KeyCode::Esc),
            OverlayOutcome::CloseWith(Msg::McpClosed)
        ));
    }

    /// An app with one organization server and the `/mcp` panel loaded; `open_chat` opens a chat
    /// that carries `inline` servers, and false leaves the blank pre-create chat.
    fn mcp_app(open_chat: bool, inline: bool) -> (App, uuid::Uuid) {
        let mut app = App::new(BusyBehavior::Queue, true);
        let github = uuid::Uuid::new_v4();
        if open_chat {
            let inline_servers = if inline {
                json!([{"slug": "local-tools", "url": "http://localhost:9000/mcp",
                    "tool_allow_list": [], "tool_deny_list": []}])
            } else {
                json!([])
            };
            let chat = serde_json::from_value(json!({
                "id": uuid::Uuid::new_v4(), "mcp_server_ids": [],
                "inline_mcp_servers": inline_servers,
                "children": [], "files": [], "labels": {}
            }))
            .unwrap();
            app.update(Msg::ChatLoaded {
                has_more: None,
                chat: Box::new(chat),
                messages: vec![],
            });
        }
        // With inline servers there is no organization row, so the panel has only text rows.
        let servers = if inline {
            vec![]
        } else {
            vec![
                serde_json::from_value(json!({"id": github, "display_name": "GitHub",
                "url": "https://github.example/mcp", "tool_allow_list": [], "tool_deny_list": []}))
                .unwrap(),
            ]
        };
        app.mcp_panel = Some(scuttle_core::panels::McpPanel {
            servers: scuttle_core::panels::Fetched::Loaded(servers),
            health: scuttle_core::panels::Fetched::Loaded(None),
        });
        (app, github)
    }

    #[test]
    fn space_toggles_an_organization_server_in_an_open_chat() {
        let (app, github) = mcp_app(true, false);
        let mut o = Overlay::Mcp(TableState::default());
        assert!(matches!(
            press(&mut o, &app, KeyCode::Char(' ')),
            OverlayOutcome::Send(Msg::ToggleMcp(id)) if id == github
        ));
    }

    #[test]
    fn space_toggles_an_organization_server_on_a_blank_chat() {
        let (app, github) = mcp_app(false, false);
        let mut o = Overlay::Mcp(TableState::default());
        assert!(matches!(
            press(&mut o, &app, KeyCode::Char(' ')),
            OverlayOutcome::Send(Msg::ToggleMcp(id)) if id == github
        ));
    }

    #[test]
    fn space_does_nothing_on_inline_server_rows() {
        let (app, _) = mcp_app(true, true);
        let mut o = Overlay::Mcp(TableState::default());
        assert!(matches!(
            press(&mut o, &app, KeyCode::Char(' ')),
            OverlayOutcome::Stay
        ));
    }

    #[test]
    fn the_mcp_hint_names_enter_and_space() {
        let (app, _) = mcp_app(true, false);
        let o = Overlay::Mcp(TableState::default());
        let theme = Theme::terminal(true);
        let view = o.view(&ctx_for(&app, &theme));
        assert_eq!(
            view.hint.as_deref(),
            Some("Enter or Space turns a server on or off for the next message, Esc closes")
        );
    }

    #[test]
    fn the_mcp_panel_fits_a_pending_selection_in_80_columns() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (github, linear) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let chat = serde_json::from_value(json!({
            "id": uuid::Uuid::new_v4(), "mcp_server_ids": [github], "inline_mcp_servers": [],
            "children": [], "files": [], "labels": {}
        }))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: vec![],
        });
        app.mcp_panel = Some(scuttle_core::panels::McpPanel {
            servers: scuttle_core::panels::Fetched::Loaded(
                serde_json::from_value(json!([
                    {"id": github, "display_name": "GitHub", "url": "https://github.example/mcp",
                        "tool_allow_list": [], "tool_deny_list": []},
                    {"id": linear, "display_name": "Linear", "url": "https://linear.example/mcp",
                        "tool_allow_list": [], "tool_deny_list": []}
                ]))
                .unwrap(),
            ),
            health: scuttle_core::panels::Fetched::Loaded(Some(vec![])),
        });
        app.update(Msg::ToggleMcp(github));
        let o = Overlay::Mcp(TableState::default());
        let theme = Theme::terminal(true);
        let view = o.view(&ctx_for(&app, &theme));
        let (width, height) = (80u16, 10u16);
        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        term.draw(|f| {
            table::render(f, f.area(), &view, o.state(), &theme, None);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let text = |y: u16| (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>();
        let lines: Vec<String> = (0..height).map(text).collect();
        let line = |needle: &str| {
            lines
                .iter()
                .find(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("no line has {needle:?}: {lines:#?}"))
        };
        assert!(line("GitHub").contains("off (next message)"), "{lines:#?}");
        let linear_row = line("Linear");
        assert!(
            linear_row.contains("off") && !linear_row.contains("(next message)"),
            "an untouched row is not marked: {linear_row}"
        );
        line("Your next message sends the organization servers shown as on.");
        line("Enter or Space turns a server on or off for the next message, Esc closes");
    }

    #[test]
    fn old_chats_keep_the_unit_of_their_age() {
        let (app, _) = listed_app(&["Ancient"], json!([]));
        let theme = Theme::terminal(true);
        let then = chrono::DateTime::parse_from_rfc3339("2026-09-30T10:59:00Z")
            .unwrap()
            .timestamp();
        let ctx = ViewCtx {
            now_unix: then + 272 * 86_400,
            ..ctx_for(&app, &theme)
        };
        let o = Overlay::chats(String::new(), &app);
        let view = o.view(&ctx);
        let mut term = Terminal::new(TestBackend::new(80, 8)).unwrap();
        term.draw(|f| {
            table::render(f, f.area(), &view, o.state(), &theme, None);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let text = |y: u16| (0..80u16).map(|x| buf[(x, y)].symbol()).collect::<String>();
        let row = (0..8u16).find(|&y| text(y).contains("Ancient")).unwrap();
        assert!(text(row).contains("272d"), "{}", text(row));
        let subagents = Overlay::Subagents(SubagentsState {
            table: TableState::default(),
            scroll: 0,
        });
        assert_eq!(
            subagents.view(&ctx).widths.last(),
            Some(&Constraint::Length(4)),
            "/subagents fits `999d` too"
        );
    }

    #[test]
    fn every_list_hint_says_how_to_close_it_and_the_queue_names_both_keys() {
        let (app, _) = listed_app(&["One"], json!([]));
        let theme = Theme::terminal(true);
        let ctx = ctx_for(&app, &theme);
        let hint = |o: Overlay| o.view(&ctx).hint.unwrap_or_default();
        assert!(
            hint(Overlay::chats(String::new(), &app)).ends_with("Esc closes"),
            "{}",
            hint(Overlay::chats(String::new(), &app))
        );
        let queue = hint(Overlay::Queue(TableState::default()));
        assert!(queue.contains("Delete or Backspace removes"), "{queue}");
    }

    /// An app listing one pinned chat, "alpha", whose last turn is summed up as `summary`.
    fn pinned_app(summary: &str) -> App {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats: serde_json::from_value(json!([{"id": uuid::Uuid::new_v4(), "title": "alpha",
                "status": "waiting", "pin_order": 1, "last_turn_summary": summary,
                "updated_at": "2026-09-30T10:00:00Z", "children": [], "files": [],
                "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}]))
            .unwrap(),
        });
        app
    }

    fn cell_text(view: &TableView, row: usize, cell: usize) -> String {
        view.rows[row].cells[cell]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn a_pinned_chat_shows_the_pin_icon_and_an_empty_icon_hides_it() {
        let app = pinned_app("");
        let theme = Theme::terminal(true);
        let o = Overlay::chats(String::new(), &app);
        let view = o.view(&ctx_for(&app, &theme));
        assert_eq!(cell_text(&view, 0, 0), "📌");
        assert_eq!(
            cell_text(&view, 0, 2),
            "alpha",
            "the pin is not in the title cell"
        );
        let nerd = ViewCtx {
            pin_icon: "\u{f0403}",
            ..ctx_for(&app, &theme)
        };
        let view = o.view(&nerd);
        assert_eq!(cell_text(&view, 0, 0), "\u{f0403}");
        assert_eq!(cell_text(&view, 0, 2), "alpha");
        let none = ViewCtx {
            pin_icon: "",
            ..ctx_for(&app, &theme)
        };
        let view = o.view(&none);
        assert_eq!(cell_text(&view, 0, 0), "");
        assert_eq!(cell_text(&view, 0, 2), "alpha");
    }

    #[test]
    fn the_summary_is_a_dim_last_column_on_a_wide_overlay_only() {
        let app = pinned_app("Fixing the CI\nnow");
        let theme = Theme::terminal(true);
        let o = Overlay::chats(String::new(), &app);
        let wide = o.view(&ViewCtx {
            width: SUMMARY_MIN_WIDTH,
            ..ctx_for(&app, &theme)
        });
        assert_eq!(wide.widths.len(), 7);
        assert_eq!(cell_text(&wide, 0, 6), "Fixing the CI now");
        assert_eq!(wide.rows[0].cells[6].spans[0].style, theme.dim);
        let narrow = o.view(&ViewCtx {
            width: SUMMARY_MIN_WIDTH - 1,
            ..ctx_for(&app, &theme)
        });
        assert_eq!(
            narrow.widths.len(),
            6,
            "a narrow overlay drops the summary first"
        );
        assert!(
            (0..narrow.rows[0].cells.len()).all(|c| !cell_text(&narrow, 0, c).contains("Fixing")),
            "{:?}",
            narrow.rows[0].cells
        );
    }

    #[test]
    fn the_wide_pin_takes_two_cells_and_leaves_the_other_columns_in_place() {
        use unicode_width::UnicodeWidthStr;
        assert_eq!(
            ("📌".width(), "📌".width_cjk()),
            (2, 2),
            "the pin is two cells under both width rules"
        );
        let app = pinned_app("Fixing the CI");
        let theme = Theme::terminal(true);
        let o = Overlay::chats(String::new(), &app);
        let updated = chrono::DateTime::parse_from_rfc3339("2026-09-30T10:00:00Z")
            .unwrap()
            .timestamp();
        for width in [SUMMARY_MIN_WIDTH - 1, SUMMARY_MIN_WIDTH] {
            let view = o.view(&ViewCtx {
                width,
                now_unix: updated + 5 * 60,
                ..ctx_for(&app, &theme)
            });
            let mut term = Terminal::new(TestBackend::new(width, 8)).unwrap();
            term.draw(|f| {
                table::render(f, f.area(), &view, o.state(), &theme, None);
            })
            .unwrap();
            let buf = term.backend().buffer();
            let text = |y: u16| (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>();
            let row = (0..8u16).find(|&y| text(y).contains("alpha")).unwrap();
            let pin = (0..width)
                .find(|&x| buf[(x, row)].symbol() == "📌")
                .unwrap();
            assert_eq!(
                (2..11)
                    .map(|dx| buf[(pin + dx, row)].symbol())
                    .collect::<String>(),
                "    alpha",
                "the title starts six cells after the pin, past the status column, at width {width}"
            );
            assert!(text(row).contains("5m"), "{}", text(row));
            assert_eq!(
                text(row).contains("Fixing the CI"),
                width >= SUMMARY_MIN_WIDTH,
                "{}",
                text(row)
            );
        }
    }

    /// An app listing one chat for each `/chats` marker, titled by what it shows: pinned,
    /// plain (with a summary), unread, running and unread, errored and unread, waiting on the
    /// user and unread, and a parent whose subagent `t-child` lists under it once expanded.
    /// Returns the app and the parent's id.
    fn marked_app() -> (App, uuid::Uuid) {
        let parent_id = uuid::Uuid::new_v4();
        let chat = |title: &str, status: &str, minute: u32| {
            json!({"id": uuid::Uuid::new_v4(), "title": title, "status": status,
                "updated_at": format!("2026-09-30T10:{minute:02}:00Z"), "children": [],
                "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})
        };
        let mut pinned = chat("t-pinned", "waiting", 50);
        pinned["pin_order"] = json!(1);
        let mut plain = chat("t-plain", "waiting", 45);
        plain["last_turn_summary"] = json!("Fixing the CI");
        let mut unread = chat("t-unread", "waiting", 49);
        let mut running = chat("t-running", "running", 48);
        let mut errored = chat("t-error", "error", 47);
        let mut asking = chat("t-asking", "requires_action", 46);
        for c in [&mut unread, &mut running, &mut errored, &mut asking] {
            c["has_unread"] = json!(true);
        }
        let mut parent = chat("t-parent", "waiting", 44);
        parent["id"] = json!(parent_id);
        parent["children"] = json!([child(uuid::Uuid::new_v4(), parent_id, "t-child", "waiting")]);
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats: serde_json::from_value(json!([
                pinned, unread, running, errored, asking, plain, parent
            ]))
            .unwrap(),
        });
        (app, parent_id)
    }

    /// `/chats` over `app` with `parent` expanded, so its subagent is listed.
    fn expanded_chats(app: &App, parent: uuid::Uuid) -> Overlay {
        let mut o = Overlay::chats(String::new(), app);
        if let Overlay::Chats(state) = &mut o {
            state.expanded.insert(parent);
        }
        o
    }

    /// `o` drawn by the table `ctx.width` columns wide and 16 rows high, with `frame` painted
    /// into the spinner cells.
    fn drawn_chats(o: &Overlay, ctx: &ViewCtx, frame: Option<&str>) -> ratatui::buffer::Buffer {
        let view = o.view(ctx);
        let mut term = Terminal::new(TestBackend::new(ctx.width, 16)).unwrap();
        term.draw(|f| {
            table::render(f, f.area(), &view, o.state(), ctx.theme, frame);
        })
        .unwrap();
        term.backend().buffer().clone()
    }

    /// The column and row where `text` starts in `buf`.
    fn start_of(buf: &ratatui::buffer::Buffer, text: &str) -> (u16, u16) {
        let area = buf.area;
        (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .find(|&(x, y)| {
                (x..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .starts_with(text)
            })
            .unwrap_or_else(|| panic!("{text:?} is not drawn"))
    }

    #[test]
    fn every_title_starts_in_one_column_after_the_pin_and_status_columns() {
        let (app, parent) = marked_app();
        let theme = Theme::terminal(true);
        let o = expanded_chats(&app, parent);
        let buf = drawn_chats(&o, &ctx_for(&app, &theme), Some("X"));
        let (x, _) = start_of(&buf, "t-plain");
        for title in [
            "t-pinned",
            "t-unread",
            "t-running",
            "t-error",
            "t-asking",
            "t-parent",
        ] {
            assert_eq!(
                start_of(&buf, title).0,
                x,
                "{title} starts in the title column"
            );
        }
        assert_eq!(
            start_of(&buf, "t-child").0,
            x + 3,
            "a subagent keeps its indent inside the title cell"
        );
        // The pin and status columns are two cells each, and the table puts one cell after
        // each column.
        let (pin, status) = (x - 6, x - 3);
        let at =
            |title: &str, column: u16| buf[(column, start_of(&buf, title).1)].symbol().to_owned();
        assert_eq!(at("t-pinned", pin), "📌");
        assert_eq!(at("t-unread", status), UNREAD);
        assert_eq!(
            at("t-running", status),
            "X",
            "the table paints the turn's spinner in the status column"
        );
        assert_eq!(at("t-error", status), "!");
        assert_eq!(at("t-asking", status), "?");
        for title in ["t-plain", "t-parent", "t-child"] {
            assert_eq!(
                (at(title, pin), at(title, status)),
                (" ".to_owned(), " ".to_owned()),
                "{title} is unpinned, read, and idle"
            );
        }
    }

    #[test]
    fn the_unread_dot_shows_only_for_an_unread_idle_chat() {
        use unicode_width::UnicodeWidthStr;
        assert_eq!(
            (UNREAD.width(), UNREAD.width_cjk()),
            (2, 2),
            "the dot is two cells under both width rules"
        );
        assert_eq!(usize::from(STATUS_WIDTH), UNREAD.width());
        let (app, parent) = marked_app();
        let theme = Theme::terminal(true);
        let view = expanded_chats(&app, parent).view(&ctx_for(&app, &theme));
        assert_eq!(view.widths[STATUS_COLUMN], Constraint::Length(STATUS_WIDTH));
        let status = |title: &str| {
            let row = (0..view.rows.len())
                .find(|&i| cell_text(&view, i, 2).trim_start() == title)
                .unwrap_or_else(|| panic!("no row for {title}"));
            cell_text(&view, row, STATUS_COLUMN)
        };
        assert_eq!(status("t-unread"), UNREAD);
        assert_eq!(
            status("t-running"),
            spinner_frame(Duration::ZERO),
            "a working chat keeps its spinner while unread"
        );
        assert_eq!(status("t-error"), "!", "an error outranks unread");
        assert_eq!(status("t-asking"), "?", "a question outranks unread");
        for title in ["t-pinned", "t-plain", "t-parent", "t-child"] {
            assert_eq!(status(title), " ", "{title} is read and idle");
        }
    }

    #[test]
    fn a_running_unread_chat_shows_the_spinner_until_it_stops_then_the_dot() {
        let id = uuid::Uuid::new_v4();
        let load = |app: &mut App, status: &str| {
            app.update(Msg::ChatsLoaded {
                query: ListQuery::Default,
                offset: 0,
                chats: serde_json::from_value(json!([{"id": id, "title": "busy",
                    "status": status, "has_unread": true,
                    "updated_at": "2026-09-30T10:00:00Z", "children": [], "files": [],
                    "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}]))
                .unwrap(),
            });
        };
        let mut app = App::new(BusyBehavior::Queue, true);
        let theme = Theme::terminal(true);
        load(&mut app, "running");
        let view = Overlay::chats(String::new(), &app).view(&ctx_for(&app, &theme));
        assert_eq!(
            cell_text(&view, 0, STATUS_COLUMN),
            spinner_frame(Duration::ZERO)
        );
        assert_eq!(
            view.spinners,
            vec![Spinner {
                row: 0,
                column: STATUS_COLUMN,
                offset: 0
            }],
            "the table paints the spinner in the status column"
        );
        load(&mut app, "waiting");
        let view = Overlay::chats(String::new(), &app).view(&ctx_for(&app, &theme));
        assert_eq!(cell_text(&view, 0, STATUS_COLUMN), UNREAD);
        assert!(view.spinners.is_empty());
    }

    #[test]
    fn a_one_cell_or_empty_pin_icon_keeps_a_two_cell_pin_column() {
        let (app, parent) = marked_app();
        let theme = Theme::terminal(true);
        let o = expanded_chats(&app, parent);
        let default_x = start_of(&drawn_chats(&o, &ctx_for(&app, &theme), None), "t-plain").0;
        for icon in ["*", "\u{f0403}", ""] {
            let ctx = ViewCtx {
                pin_icon: icon,
                ..ctx_for(&app, &theme)
            };
            assert_eq!(o.view(&ctx).widths[0], Constraint::Length(2), "{icon:?}");
            let buf = drawn_chats(&o, &ctx, None);
            let (x, y) = start_of(&buf, "t-pinned");
            assert_eq!(
                x, default_x,
                "{icon:?} leaves the titles where 📌 puts them"
            );
            assert_eq!(start_of(&buf, "t-plain").0, x, "{icon:?}");
            let shown = if icon.is_empty() { " " } else { icon };
            assert_eq!(buf[(x - 6, y)].symbol(), shown, "{icon:?}");
        }
        let wide = ViewCtx {
            pin_icon: "PIN",
            ..ctx_for(&app, &theme)
        };
        assert_eq!(
            o.view(&wide).widths[0],
            Constraint::Length(3),
            "a wider icon widens the column for every row"
        );
    }

    #[test]
    fn an_ambiguous_width_pin_icon_gets_the_cells_a_cjk_terminal_draws() {
        use unicode_width::UnicodeWidthStr;
        let stars = "\u{2605}\u{2605}";
        assert_eq!(
            (stars.width(), stars.width_cjk()),
            (2, 4),
            "two East Asian Ambiguous stars"
        );
        assert_eq!(
            pin_width(stars),
            4,
            "room for the wider rule, as a slot leaves"
        );
        assert_eq!(pin_width("\u{f0403}"), 2);
        assert_eq!(
            pin_width(icons::slot(IconSet::Nerd, Icon::Pin).text),
            2,
            "a slot's space takes its glyph's spill"
        );
        assert_eq!(pin_width("📌"), 2);
        assert_eq!(pin_width("PIN"), 3);
    }

    #[test]
    fn narrow_chats_drop_the_summary_first_and_keep_the_titles_in_line() {
        let (app, parent) = marked_app();
        let theme = Theme::terminal(true);
        let o = expanded_chats(&app, parent);
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-30T11:00:00Z")
            .unwrap()
            .timestamp();
        let mut starts = Vec::new();
        for width in [60, 79, SUMMARY_MIN_WIDTH - 1, SUMMARY_MIN_WIDTH] {
            let ctx = ViewCtx {
                width,
                now_unix: now,
                ..ctx_for(&app, &theme)
            };
            assert_eq!(
                o.view(&ctx).widths.len(),
                if width >= SUMMARY_MIN_WIDTH { 7 } else { 6 },
                "at width {width}"
            );
            let buf = drawn_chats(&o, &ctx, None);
            let (x, y) = start_of(&buf, "t-plain");
            let row: String = (0..width).map(|x| buf[(x, y)].symbol()).collect();
            assert!(row.contains("15m"), "the age stays at width {width}: {row}");
            assert_eq!(
                row.contains("Fixing the CI"),
                width >= SUMMARY_MIN_WIDTH,
                "the summary drops first: {row}"
            );
            for title in ["t-pinned", "t-running", "t-parent"] {
                assert_eq!(start_of(&buf, title).0, x, "{title} at width {width}");
            }
            starts.push(x);
        }
        assert!(
            starts.windows(2).all(|w| w[0] == w[1]),
            "the fixed columns never move: {starts:?}"
        );
    }

    /// An app listing one chat per pull request state and one with no pull request, each with
    /// a summary, titled by what it shows. The merged one is on GitHub, the open one on a
    /// self-hosted GitLab with a subgroup, the draft on Gitea, and the closed one's URL names
    /// no forge.
    fn pr_app() -> App {
        let chat = |title: &str, minute: u32, pr: serde_json::Value| {
            json!({"id": uuid::Uuid::new_v4(), "title": title, "status": "waiting",
                "updated_at": format!("2026-09-30T10:{minute:02}:00Z"), "children": [],
                "last_turn_summary": "Fixing the CI", "diff_status": pr, "files": [],
                "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})
        };
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats: serde_json::from_value(json!([
                chat(
                    "t-merged",
                    50,
                    json!({"pr_number": 12, "pull_request_state": "merged",
                    "pull_request_draft": false,
                    "url": "https://github.com/coder/coder/pull/12"})
                ),
                chat(
                    "t-open",
                    49,
                    json!({"pr_number": 34, "pull_request_state": "open",
                    "pull_request_draft": false,
                    "url": "https://gitlab.example.com/gitlab-org/sub/project/-/merge_requests/34"})
                ),
                chat(
                    "t-draft",
                    48,
                    json!({"pr_number": 56, "pull_request_state": "open",
                    "pull_request_draft": true,
                    "url": "https://gitea.com/gitea/tea/pulls/56"})
                ),
                chat(
                    "t-closed",
                    47,
                    json!({"pr_number": 7800, "pull_request_state": "closed",
                    "pull_request_draft": false, "url": "https://example.com/elsewhere/7800"})
                ),
                chat("t-none", 46, json!({"pull_request_draft": false})),
            ]))
            .unwrap(),
        });
        app
    }

    /// Wide enough that every reference in `pr_app` keeps its owner.
    const ROOMY: u16 = 150;

    #[test]
    fn a_wide_chats_list_shows_each_forge_reference_then_its_state() {
        let app = pr_app();
        let theme = Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal(true)
        };
        let ctx = ViewCtx {
            width: ROOMY,
            ..ctx_for(&app, &theme)
        };
        let view = Overlay::chats(String::new(), &app).view(&ctx);
        assert_eq!(
            view.widths.len(),
            9,
            "pin, status, title, family, archived, age, reference, state, summary"
        );
        let pr = |title| cell_text(&view, chat_row(&view, title), PR_COLUMN);
        let state = |title| cell_text(&view, chat_row(&view, title), PR_COLUMN + 1);
        assert_eq!(pr("t-merged"), "\u{f09b} coder/coder#12");
        assert_eq!(pr("t-open"), "\u{f296} gitlab-org/sub/project!34");
        assert_eq!(pr("t-draft"), "\u{f339} gitea/tea#56");
        assert_eq!(
            pr("t-closed"),
            "  #7800",
            "an unknown forge keeps a blank slot"
        );
        assert_eq!(pr("t-none"), "");
        assert_eq!(state("t-merged"), "\u{f419} ");
        assert_eq!(state("t-open"), "\u{f407} ");
        assert_eq!(state("t-draft"), "\u{f4dd} ");
        assert_eq!(state("t-closed"), "\u{f4dc} ");
        assert_eq!(state("t-none"), "");
        assert_eq!(
            cell_text(&view, chat_row(&view, "t-open"), PR_COLUMN + 2),
            "Fixing the CI",
            "the summary stays last"
        );
        assert_eq!(
            view.widths[PR_COLUMN],
            Constraint::Length(2 + 25),
            "the slot and the widest reference"
        );
        assert_eq!(view.widths[PR_COLUMN + 1], Constraint::Length(2));
    }

    #[test]
    fn text_icons_drop_the_forge_slot_and_spell_out_the_pr_state() {
        let app = pr_app();
        let theme = Theme::terminal(true);
        let ctx = ViewCtx {
            width: ROOMY,
            ..ctx_for(&app, &theme)
        };
        let view = Overlay::chats(String::new(), &app).view(&ctx);
        let pr = |title| cell_text(&view, chat_row(&view, title), PR_COLUMN);
        let state = |title| cell_text(&view, chat_row(&view, title), PR_COLUMN + 1);
        assert_eq!(pr("t-merged"), "coder/coder#12");
        assert_eq!(pr("t-open"), "gitlab-org/sub/project!34");
        assert_eq!(pr("t-draft"), "gitea/tea#56");
        assert_eq!(pr("t-closed"), "#7800");
        assert_eq!(state("t-merged"), "merged");
        assert_eq!(state("t-open"), "open");
        assert_eq!(state("t-draft"), "draft");
        assert_eq!(state("t-closed"), "closed");
        assert_eq!(view.widths[PR_COLUMN], Constraint::Length(25));
        assert_eq!(
            view.widths[PR_COLUMN + 1],
            Constraint::Length(7),
            "a cell past the longest word keeps the words off the summary"
        );
    }

    #[test]
    fn a_tight_pr_column_drops_every_rows_owner_first() {
        let app = pr_app();
        let theme = Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal(true)
        };
        let ctx = ViewCtx {
            width: PR_MIN_WIDTH,
            ..ctx_for(&app, &theme)
        };
        let view = Overlay::chats(String::new(), &app).view(&ctx);
        let pr = |title| cell_text(&view, chat_row(&view, title), PR_COLUMN);
        assert_eq!(
            pr("t-open"),
            "\u{f296} project!34",
            "the group path goes at {PR_MIN_WIDTH} columns"
        );
        assert_eq!(
            pr("t-merged"),
            "\u{f09b} coder#12",
            "one reference too long drops every row's owner, so the forms never mix"
        );
        assert_eq!(pr("t-draft"), "\u{f339} tea#56");
        assert_eq!(view.widths[PR_COLUMN], Constraint::Length(2 + 10));
    }

    #[test]
    fn text_icons_leave_out_a_reference_column_with_nothing_in_it() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats: serde_json::from_value(json!([{"id": uuid::Uuid::new_v4(),
                "title": "t-bare", "status": "waiting", "updated_at": "2026-09-30T10:00:00Z",
                "children": [], "last_turn_summary": "Fixing the CI",
                "diff_status": {"pull_request_state": "open", "pull_request_draft": false},
                "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}]))
            .unwrap(),
        });
        let theme = Theme::terminal(true);
        let view = Overlay::chats(String::new(), &app).view(&ViewCtx {
            width: ROOMY,
            ..ctx_for(&app, &theme)
        });
        assert_eq!(
            view.widths.len(),
            8,
            "no number and no URL leave no reference column, only the state"
        );
        let row = chat_row(&view, "t-bare");
        assert_eq!(cell_text(&view, row, PR_COLUMN), "open");
        assert_eq!(cell_text(&view, row, PR_COLUMN + 1), "Fixing the CI");
        assert_eq!(view.widths[PR_COLUMN], Constraint::Length(7));
    }

    #[test]
    fn the_pr_state_lines_up_whatever_the_reference_length() {
        let app = pr_app();
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-30T11:00:00Z")
            .unwrap()
            .timestamp();
        let o = Overlay::chats(String::new(), &app);
        for icons in [IconSet::Nerd, IconSet::Text] {
            let theme = Theme {
                icons,
                ..Theme::terminal(true)
            };
            for width in [PR_MIN_WIDTH, ROOMY, 200] {
                let ctx = ViewCtx {
                    width,
                    now_unix: now,
                    ..ctx_for(&app, &theme)
                };
                let buf = drawn_chats(&o, &ctx, None);
                let states: Vec<u16> = [
                    ("t-merged", "\u{f419}", "merged"),
                    ("t-open", "\u{f407}", "open"),
                    ("t-draft", "\u{f4dd}", "draft"),
                    ("t-closed", "\u{f4dc}", "closed"),
                ]
                .into_iter()
                .map(|(title, glyph, word)| {
                    let y = start_of(&buf, title).1;
                    let mark = if icons == IconSet::Nerd { glyph } else { word };
                    (0..width)
                        .rev()
                        .find(|&x| {
                            (x..width)
                                .map(|x| buf[(x, y)].symbol())
                                .collect::<String>()
                                .starts_with(mark)
                        })
                        .unwrap_or_else(|| panic!("{title}'s state at width {width}"))
                })
                .collect();
                assert!(
                    states.windows(2).all(|w| w[0] == w[1]),
                    "{icons:?} at width {width}: {states:?}"
                );
            }
        }
    }

    #[test]
    fn the_pr_state_takes_its_color_unless_no_color_is_set() {
        use crate::theme::Colors;
        use ratatui::style::Color;
        let app = pr_app();
        for (colors, merged, closed) in [
            (Colors::Ansi256, Some(Color::Magenta), Some(Color::Red)),
            (Colors::None, None, None),
        ] {
            for icons in [IconSet::Nerd, IconSet::Text] {
                let theme = Theme {
                    icons,
                    ..Theme::terminal_with(true, colors)
                };
                let ctx = ViewCtx {
                    width: PR_MIN_WIDTH,
                    ..ctx_for(&app, &theme)
                };
                let view = Overlay::chats(String::new(), &app).view(&ctx);
                let state = |title| {
                    view.rows[chat_row(&view, title)].cells[PR_COLUMN + 1].spans[0]
                        .style
                        .fg
                };
                assert_eq!(state("t-merged"), merged, "{colors:?} {icons:?}");
                assert_eq!(state("t-closed"), closed, "{colors:?} {icons:?}");
            }
        }
    }

    #[test]
    fn the_pr_column_drops_before_the_summary() {
        let app = pr_app();
        let theme = Theme::terminal(true);
        let o = Overlay::chats(String::new(), &app);
        let at = |width: u16| {
            o.view(&ViewCtx {
                width,
                ..ctx_for(&app, &theme)
            })
        };
        assert_eq!(at(PR_MIN_WIDTH).widths.len(), 9);
        let view = at(PR_MIN_WIDTH - 1);
        assert_eq!(view.widths.len(), 7, "the pull requests go first");
        assert_eq!(
            cell_text(&view, chat_row(&view, "t-open"), 6),
            "Fixing the CI",
            "the summary takes cell 6 again"
        );
        assert_eq!(at(SUMMARY_MIN_WIDTH - 1).widths.len(), 6);
        let no_prs = pinned_app("Fixing the CI");
        let view = Overlay::chats(String::new(), &no_prs).view(&ViewCtx {
            width: PR_MIN_WIDTH,
            ..ctx_for(&no_prs, &theme)
        });
        assert_eq!(
            view.widths.len(),
            7,
            "with no pull request listed, the column stays out"
        );
    }

    #[test]
    fn the_git_panel_leads_the_pull_request_with_its_state_icon() {
        use scuttle_core::panels::{Fetched, GitPanel, LocalGit};
        let mut app = App::new(BusyBehavior::Queue, true);
        let chat = serde_json::from_value(json!({
            "id": uuid::Uuid::new_v4(),
            "diff_status": {"pr_number": 12, "pull_request_title": "Fix the watch",
                "pull_request_state": "merged", "pull_request_draft": false},
            "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}
        }))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: vec![],
        });
        app.git_panel = Some(GitPanel {
            diff: Fetched::Loading,
            repos: Default::default(),
            local: LocalGit::NoWorkspace,
        });
        let pr_row = |theme: &Theme| {
            Overlay::Git(TableState::default())
                .view(&ctx_for(&app, theme))
                .rows
                .into_iter()
                .find(|r| r.cells[0].to_string() == "Pull request")
                .expect("a pull request row")
        };
        assert_eq!(
            pr_row(&Theme::terminal(true)).cells[1].to_string(),
            "#12 Fix the watch (merged)",
            "text mode keeps the row as it was"
        );
        let nerd = Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal(true)
        };
        let row = pr_row(&nerd);
        assert_eq!(
            row.cells[1].to_string(),
            "\u{f419} #12 Fix the watch (merged)"
        );
        assert_eq!(
            row.cells[1].spans[0].style.fg,
            Some(ratatui::style::Color::Magenta),
            "the glyph takes the state's color, as in /chats"
        );
    }

    /// The test theme with Nerd Font icons.
    fn nerd_theme() -> Theme {
        Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal(true)
        }
    }

    /// The row of `view` whose title cell reads `title`.
    fn chat_row(view: &TableView, title: &str) -> usize {
        (0..view.rows.len())
            .find(|&i| cell_text(view, i, 2).trim_start() == title)
            .unwrap_or_else(|| panic!("no row for {title}"))
    }

    #[test]
    fn nerd_icons_mark_the_chat_status_and_unread_and_keep_the_titles_in_line() {
        let (app, parent) = marked_app();
        let o = expanded_chats(&app, parent);
        let text_theme = Theme::terminal(true);
        let text_x = start_of(
            &drawn_chats(&o, &ctx_for(&app, &text_theme), None),
            "t-plain",
        )
        .0;
        let theme = nerd_theme();
        let pin = icons::slot(IconSet::Nerd, Icon::Pin).text;
        let nerd = ViewCtx {
            pin_icon: pin,
            ..ctx_for(&app, &theme)
        };
        let buf = drawn_chats(&o, &nerd, Some("X"));
        let (x, _) = start_of(&buf, "t-plain");
        assert_eq!(
            x, text_x,
            "nerd icons leave the titles where text puts them"
        );
        for title in [
            "t-pinned",
            "t-unread",
            "t-running",
            "t-error",
            "t-asking",
            "t-parent",
        ] {
            assert_eq!(
                start_of(&buf, title).0,
                x,
                "{title} starts in the title column"
            );
        }
        let (pin_x, status) = (x - 6, x - 3);
        let at =
            |title: &str, column: u16| buf[(column, start_of(&buf, title).1)].symbol().to_owned();
        assert_eq!(at("t-pinned", pin_x), "\u{f435}");
        assert_eq!(at("t-unread", status), "\u{f111}");
        assert_eq!(
            at("t-running", status),
            "X",
            "the spinner keeps its one cell"
        );
        assert_eq!(at("t-error", status), "\u{f421}");
        assert_eq!(at("t-asking", status), "\u{f420}");
        let view = o.view(&nerd);
        assert_eq!(
            view.widths[0],
            Constraint::Length(2),
            "the pin's slot is two cells"
        );
        assert_eq!(view.widths[STATUS_COLUMN], Constraint::Length(STATUS_WIDTH));
        let unread = chat_row(&view, "t-unread");
        let dot = &view.rows[unread].cells[STATUS_COLUMN].spans[0];
        assert_eq!(dot.content, "\u{f111} ");
        assert_eq!(
            dot.style, theme.accent,
            "the dot follows the theme's accent"
        );
        let plain = Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal_with(true, crate::theme::Colors::None)
        };
        let view = o.view(&ViewCtx {
            pin_icon: pin,
            ..ctx_for(&app, &plain)
        });
        let style =
            |title: &str| view.rows[chat_row(&view, title)].cells[STATUS_COLUMN].spans[0].style;
        let none = ratatui::style::Style::new();
        assert_eq!(style("t-unread"), none, "NO_COLOR leaves the glyph plain");
        assert_eq!(style("t-error"), none);
    }

    #[test]
    fn an_archived_chat_shows_the_archive_icon_in_a_two_cell_column() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::ChatsLoaded {
            query: ListQuery::Archived,
            offset: 0,
            chats: serde_json::from_value(json!([{"id": uuid::Uuid::new_v4(), "title": "old",
                "status": "waiting", "archived": true, "updated_at": "2026-09-30T10:00:00Z",
                "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [],
                "labels": {}}]))
            .unwrap(),
        });
        let mut o = Overlay::chats(String::new(), &app);
        if let Overlay::Chats(state) = &mut o {
            state.filter = Filter::Archived;
        }
        let text_theme = Theme::terminal(true);
        let view = o.view(&ctx_for(&app, &text_theme));
        assert_eq!(cell_text(&view, chat_row(&view, "old"), 4), "archived");
        assert_eq!(view.widths[4], Constraint::Length(8));
        let theme = nerd_theme();
        let view = o.view(&ctx_for(&app, &theme));
        assert_eq!(cell_text(&view, chat_row(&view, "old"), 4), "\u{f411} ");
        assert_eq!(
            view.widths[4],
            Constraint::Length(2),
            "the icon gives the title six more columns"
        );
    }

    #[test]
    fn nerd_icons_mark_each_subagents_status_in_a_two_cell_column() {
        let parent = uuid::Uuid::new_v4();
        let kid = |title: &str, status: &str| {
            json!({"id": uuid::Uuid::new_v4(), "parent_chat_id": parent, "title": title,
                "status": status, "children": [], "files": [], "mcp_server_ids": [],
                "inline_mcp_servers": [], "labels": {}})
        };
        let chat = serde_json::from_value(json!({"id": parent, "title": "root",
            "children": [kid("k-error", "error"), kid("k-asking", "requires_action"),
                kid("k-running", "running")],
            "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}))
        .unwrap();
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: vec![],
        });
        let o = Overlay::Subagents(SubagentsState {
            table: TableState::default(),
            scroll: 0,
        });
        let markers = |theme: &Theme| {
            let view = o.view(&ctx_for(&app, theme));
            let cells: Vec<String> = view.rows.iter().map(|r| r.cells[0].to_string()).collect();
            (view.widths[0], cells, view.spinners)
        };
        let (width, cells, spinners) = markers(&Theme::terminal(true));
        assert_eq!(width, Constraint::Length(1));
        assert_eq!(cells, ["!", "?", spinner_frame(Duration::ZERO)]);
        let (width, cells, nerd_spinners) = markers(&nerd_theme());
        assert_eq!(width, Constraint::Length(STATUS_WIDTH));
        assert_eq!(
            cells,
            ["\u{f421} ", "\u{f420} ", spinner_frame(Duration::ZERO)]
        );
        assert_eq!(nerd_spinners, spinners, "the spinner stays in its one cell");
    }

    #[test]
    fn nerd_icons_lead_each_workspace_status_and_the_none_row() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let ws = |name: &str, status: &str| WorkspaceRef {
            id: uuid::Uuid::new_v4(),
            name: name.into(),
            status: status.into(),
            ..Default::default()
        };
        app.update(Msg::WorkspacesLoaded(vec![
            ws("up", "running"),
            ws("down", "stopped"),
            ws("broken", "failed"),
            ws("booting", "starting"),
        ]));
        let o = Overlay::open(Picker::Workspace, &app).unwrap();
        let status = |view: &TableView, name: &str| {
            let row = view
                .rows
                .iter()
                .find(|r| r.cells[0].to_string() == name)
                .unwrap_or_else(|| panic!("no row for {name}"));
            row.cells[2].to_string()
        };
        let text_theme = Theme::terminal(true);
        let view = o.view(&ctx_for(&app, &text_theme));
        assert_eq!(status(&view, "up"), "running");
        assert_eq!(status(&view, "broken"), "failed");
        assert_eq!(view.rows[0].cells.len(), 1, "none has no status in text");
        assert_eq!(view.widths[2], Constraint::Length(9));
        let theme = nerd_theme();
        let view = o.view(&ctx_for(&app, &theme));
        assert_eq!(status(&view, "up"), "\u{eb7b} running");
        assert_eq!(status(&view, "down"), "\u{eb7a} stopped");
        assert_eq!(status(&view, "broken"), "\u{ea87} failed");
        assert_eq!(status(&view, "booting"), "\u{eb19} starting");
        assert_eq!(view.rows[0].cells[2].to_string(), "\u{eabd} ");
        assert_eq!(view.widths[2], Constraint::Length(11));
        let up = view
            .rows
            .iter()
            .find(|r| r.cells[0].to_string() == "up")
            .unwrap();
        assert_eq!(up.cells[2].spans[0].style, theme.ok);
    }

    #[test]
    fn nerd_icons_show_each_mcp_server_on_or_off_and_mark_a_failure() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (github, linear) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let chat = serde_json::from_value(json!({
            "id": uuid::Uuid::new_v4(), "mcp_server_ids": [github], "inline_mcp_servers": [],
            "children": [], "files": [], "labels": {}
        }))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: vec![],
        });
        app.mcp_panel = Some(scuttle_core::panels::McpPanel {
            servers: scuttle_core::panels::Fetched::Loaded(
                serde_json::from_value(json!([
                    {"id": github, "display_name": "GitHub", "url": "https://github.example/mcp",
                        "tool_allow_list": [], "tool_deny_list": []},
                    {"id": linear, "display_name": "Linear", "url": "https://linear.example/mcp",
                        "tool_allow_list": [], "tool_deny_list": []}
                ]))
                .unwrap(),
            ),
            health: scuttle_core::panels::Fetched::Loaded(Some(vec![
                coder_sdk::McpConnectOutcome {
                    config_id: github,
                    slug: "github".into(),
                    outcome: "error".into(),
                    tool_count: 0,
                    error: "refused".into(),
                },
            ])),
        });
        let o = Overlay::Mcp(TableState::default());
        let row = |view: &TableView, name: &str| {
            let row = view
                .rows
                .iter()
                .find(|r| r.cells[0].to_string() == format!("  {name}"))
                .unwrap_or_else(|| panic!("no row for {name}"));
            (row.cells[2].to_string(), row.cells[3].to_string())
        };
        let text_theme = Theme::terminal(true);
        let view = o.view(&ctx_for(&app, &text_theme));
        assert_eq!(
            row(&view, "GitHub"),
            ("on".to_owned(), "failed: refused".to_owned())
        );
        assert_eq!(row(&view, "Linear"), ("off".to_owned(), String::new()));
        assert_eq!(view.widths[2], Constraint::Length(18));
        let theme = nerd_theme();
        let view = o.view(&ctx_for(&app, &theme));
        assert_eq!(
            row(&view, "GitHub"),
            (
                "\u{ebb3} on".to_owned(),
                "\u{ea87} failed: refused".to_owned()
            )
        );
        assert_eq!(
            row(&view, "Linear"),
            ("\u{ebb5} off".to_owned(), String::new())
        );
        assert_eq!(
            view.widths[2],
            Constraint::Length(20),
            "fits off (next message) and its icon"
        );
    }

    #[test]
    fn closing_the_model_table_tells_the_core() {
        let mut app = App::new(BusyBehavior::Queue, true);
        models(
            &mut app,
            json!([{"id": uuid::Uuid::new_v4(), "display_name": "A", "enabled": true, "reasoning_efforts": []}]),
        );
        let mut o = Overlay::open(Picker::Model, &app).unwrap();
        assert!(matches!(
            press(&mut o, &app, KeyCode::Esc),
            OverlayOutcome::CloseWith(Msg::ModelPickerClosed)
        ));
    }

    /// A chat whose agent attached `build-logs.zip` after the user's message, and whose older
    /// `notes.md` message is not loaded.
    fn app_with_files() -> (App, uuid::Uuid, uuid::Uuid) {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (zip, notes) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [],
            "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}, "files": [
                {"id": notes, "name": "notes.md", "mime_type": "text/markdown",
                    "size_bytes": 2048, "created_at": "1970-01-01T00:00:00Z"},
                {"id": zip, "name": "build-logs.zip", "mime_type": "application/zip",
                    "size_bytes": 6144, "created_at": "1970-01-01T00:04:00Z"}]}))
        .unwrap();
        app.update(Msg::ChatLoaded {
            has_more: Some(true),
            chat: Box::new(chat),
            messages: serde_json::from_value(json!([
                {"id": 3, "role": "user", "created_at": "1970-01-01T00:03:00Z",
                    "content": [{"type": "text", "text": "bundle the logs"}]},
                {"id": 4, "role": "assistant", "created_at": "1970-01-01T00:04:00Z",
                    "content": [{"type": "file", "file_id": zip, "media_type": "application/zip",
                        "name": "build-logs.zip", "file_name": ""}]}
            ]))
            .unwrap(),
        });
        app.home = Some("/h".into());
        app.save_dir = "/h/Downloads".into();
        (app, zip, notes)
    }

    fn rows_text(view: &TableView) -> Vec<String> {
        view.rows
            .iter()
            .map(|r| {
                r.cells
                    .iter()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .collect()
    }

    fn files_ctx<'a>(app: &'a App, theme: &'a Theme) -> ViewCtx<'a> {
        ViewCtx {
            app,
            theme,
            now_unix: 600,
            offset: chrono::FixedOffset::east_opt(0).unwrap(),
            elapsed: Duration::ZERO,
            width: 100,
            pin_icon: "📌",
        }
    }

    #[test]
    fn the_files_overlay_lists_newest_first_with_who_sent_each_and_where() {
        let (app, _, _) = app_with_files();
        let theme = Theme::terminal(true);
        let view = Overlay::Files(TableState::default()).view(&files_ctx(&app, &theme));
        assert_eq!(
            rows_text(&view),
            [
                "Name|Type|Size|From|Message|When",
                "build-logs.zip|ZIP|6 KiB|agent|\u{201c}bundle the logs\u{201d}|6m",
                "notes.md|MD|2 KiB||not loaded|10m",
            ]
        );
        assert_eq!(
            view.hint.as_deref(),
            Some(
                "Enter saves to ~/Downloads; s saves as; v views text; g goes to its message; Esc closes"
            )
        );
        let empty = App::new(BusyBehavior::Queue, true);
        let view = Overlay::Files(TableState::default()).view(&files_ctx(&empty, &theme));
        assert!(view.rows.is_empty());
        assert_eq!(view.status.as_deref(), Some("No files in this chat yet."));
    }

    #[test]
    fn the_files_hint_fits_at_80_columns() {
        let (mut app, _, _) = app_with_files();
        let theme = Theme::terminal(true);
        let hint = |app: &App, width: u16| {
            let ctx = ViewCtx {
                width,
                ..files_ctx(app, &theme)
            };
            Overlay::Files(TableState::default())
                .view(&ctx)
                .hint
                .unwrap_or_default()
        };
        let at_80 = hint(&app, 80);
        assert!(at_80.width() <= 76, "{at_80}");
        assert_eq!(
            at_80,
            "Enter saves to ~/Downloads; s saves as; v views; g goes to it; Esc closes"
        );
        app.save_dir = "/h/a/much/longer/place/to/keep/downloads".into();
        let long = hint(&app, 80);
        assert!(long.width() <= 76, "{long}");
        assert!(long.ends_with("Esc closes"), "{long}");
    }

    #[test]
    fn files_keys_act_on_the_selected_file_and_start_on_the_newest() {
        let (app, zip, notes) = app_with_files();
        let mut o = Overlay::Files(TableState::default());
        assert!(matches!(
            press(&mut o, &app, KeyCode::Enter),
            OverlayOutcome::Send(Msg::FileAction(FileAction::Save(id))) if id == zip
        ));
        for (c, want) in [('s', FileAction::SaveAs(zip)), ('v', FileAction::View(zip))] {
            assert!(
                matches!(press(&mut o, &app, KeyCode::Char(c)),
                    OverlayOutcome::Send(Msg::FileAction(got)) if got == want),
                "{c}"
            );
        }
        press(&mut o, &app, KeyCode::Down);
        assert!(matches!(
            press(&mut o, &app, KeyCode::Char('g')),
            OverlayOutcome::CloseWith(Msg::FileAction(FileAction::Jump(id))) if id == notes
        ));
        for c in ['o', 'x'] {
            assert!(
                matches!(press(&mut o, &app, KeyCode::Char(c)), OverlayOutcome::Stay),
                "{c} does nothing; no key opens a file"
            );
        }
    }

    #[test]
    fn a_file_the_server_no_longer_has_says_so() {
        let (mut app, zip, _) = app_with_files();
        // A newer stored file makes the older message's file unavailable.
        app.chat
            .as_mut()
            .unwrap()
            .files
            .retain(|f| f.id != Some(zip));
        let newer = uuid::Uuid::new_v4();
        app.chat.as_mut().unwrap().files.push(
            serde_json::from_value(json!({"id": newer, "name": "new.txt",
                "mime_type": "text/plain", "size_bytes": 1,
                "created_at": "1970-01-01T01:00:00Z"}))
            .unwrap(),
        );
        let theme = Theme::terminal(true);
        let view = Overlay::Files(TableState::default()).view(&files_ctx(&app, &theme));
        let shown = rows_text(&view).join("\n");
        assert!(shown.contains("no longer available"), "{shown}");
        assert!(!shown.contains("expired"), "{shown}");
    }

    #[test]
    fn the_save_question_names_the_taken_path_and_the_letter_of_each_answer() {
        let theme = Theme::terminal(true);
        let home = Some(std::path::Path::new("/h"));
        let conflict = SaveConflict {
            file: uuid::Uuid::new_v4(),
            name: "build-logs.zip".into(),
            path: "/h/Downloads/build-logs.zip".into(),
        };
        let view = conflict_view(&conflict, home, &theme, CONFLICT_WIDTH);
        assert_eq!(view.title, "~/Downloads/build-logs.zip already exists");
        assert_eq!(
            rows_text(&view),
            [
                "k|Keep both: save as build-logs (1).zip, or the next free number",
                "r|Replace: write over the file there (can't be undone)",
                "c|Cancel: save nothing",
            ]
        );
        assert!(
            view.rows.iter().all(|r| !r.selectable()),
            "nothing is highlighted, since Enter answers nothing"
        );
        assert_eq!(
            view.rows[1].cells[1].spans[0].style.fg, theme.error.fg,
            "the risk shows on the Replace label itself"
        );
        assert_eq!(
            view.hint.as_deref(),
            Some("Press k to keep both, r to replace, c or Esc to cancel")
        );
        let narrow = conflict_view(&conflict, home, &theme, 30);
        assert_eq!(
            rows_text(&narrow)[1],
            "r|Replace (can't be undone)",
            "the flag is never cut"
        );
        assert_eq!(narrow.hint.as_deref(), Some("k, r, or c; Esc cancels"));
        assert!(
            narrow.title.ends_with("\u{2026} already exists"),
            "the path gives way first: {}",
            narrow.title
        );
    }

    #[test]
    fn model_rows_show_context_threshold_and_efforts_and_the_arrows_edit_the_threshold() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (provider, alpha) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let effects = app.update(Msg::CatalogLoaded(Box::new(
            serde_json::from_value(json!({
                "models": [{"id": alpha, "display_name": "Alpha", "ai_provider_id": provider, "enabled": true, "is_default": true, "context_limit": 1000000, "compression_threshold": 30, "reasoning_efforts": ["low", "high"]}],
                "providers": [{"id": provider, "display_name": "Provider", "available": true}],
                "unsupported_providers": []
            }))
            .unwrap(),
        )));
        let generation = effects
            .iter()
            .find_map(|e| match e {
                scuttle_core::app::Effect::FetchThresholds { generation } => Some(*generation),
                _ => None,
            })
            .expect("the overrides load with the model list");
        app.update(Msg::ThresholdsLoaded {
            thresholds: vec![],
            generation,
        });
        let theme = Theme::terminal(true);
        let ctx = ctx_for(&app, &theme);
        let mut o = Overlay::open(Picker::Model, &app).unwrap();
        let cells: Vec<String> = o.view(&ctx).rows[1]
            .cells
            .iter()
            .map(|c| c.to_string())
            .collect();
        assert_eq!(
            cells,
            [
                "  Alpha",
                "current",
                "1.0M tokens",
                "30% (default)",
                "low, high"
            ]
        );
        assert!(matches!(
            press(&mut o, &app, KeyCode::Right),
            OverlayOutcome::Send(Msg::ThresholdStep { model, up: true }) if model == alpha
        ));
        assert!(matches!(
            press(&mut o, &app, KeyCode::Left),
            OverlayOutcome::Send(Msg::ThresholdStep { model, up: false }) if model == alpha
        ));
        assert!(matches!(
            press(&mut o, &app, KeyCode::Delete),
            OverlayOutcome::Send(Msg::ThresholdReset { model }) if model == alpha
        ));
        assert!(
            matches!(press(&mut o, &app, KeyCode::Down), OverlayOutcome::Stay),
            "the only row: the selection stays, so nothing saves"
        );
        assert!(matches!(
            press(&mut o, &app, KeyCode::Esc),
            OverlayOutcome::CloseWith(Msg::ModelPickerClosed)
        ));
    }

    #[test]
    fn compaction_labels_say_never_and_every_turn_and_mark_the_default() {
        let known = |percent, default| compaction_label(Shown::Known { percent, default });
        assert_eq!(known(70, true), "70% (default)");
        assert_eq!(known(55, false), "55%");
        assert_eq!(known(100, false), "100% (never)");
        assert_eq!(known(0, true), "0% (every turn, default)");
        assert_eq!(compaction_label(Shown::Loading), "…");
        assert_eq!(compaction_label(Shown::Unknown), "?");
    }

    #[test]
    fn a_narrow_model_table_drops_columns_before_the_name_and_keeps_the_threshold() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (provider, sonnet) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let effects = app.update(Msg::CatalogLoaded(Box::new(
            serde_json::from_value(json!({
                "models": [{"id": sonnet, "display_name": "Claude Sonnet 4.5", "ai_provider_id": provider, "enabled": true, "is_default": true, "context_limit": 200000, "compression_threshold": 70, "reasoning_efforts": ["low", "medium", "high"]}],
                "providers": [{"id": provider, "display_name": "Provider", "available": true}],
                "unsupported_providers": []
            }))
            .unwrap(),
        )));
        let generation = effects
            .iter()
            .find_map(|e| match e {
                scuttle_core::app::Effect::FetchThresholds { generation } => Some(*generation),
                _ => None,
            })
            .unwrap();
        app.update(Msg::ThresholdsLoaded {
            thresholds: vec![],
            generation,
        });
        let theme = Theme::terminal(true);
        let cells = |width: u16| {
            let ctx = ViewCtx {
                width,
                ..ctx_for(&app, &theme)
            };
            let view = Overlay::open(Picker::Model, &app).unwrap().view(&ctx);
            assert_eq!(view.widths.len(), view.rows[1].cells.len(), "{width}");
            view.rows[1]
                .cells
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
        };
        let name = "  Claude Sonnet 4.5";
        for width in [40, 44] {
            assert_eq!(cells(width), [name, "current", "70%*"], "{width}");
        }
        for width in [50, 60] {
            assert_eq!(cells(width), [name, "current", "70% (default)"], "{width}");
        }
        assert_eq!(
            cells(80),
            [
                name,
                "current",
                "200.0k tokens",
                "70% (default)",
                "low, medium, high"
            ]
        );
    }

    #[test]
    fn a_provider_row_shows_its_whole_name_and_reason_beside_short_model_names() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (provider, o3) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        app.update(Msg::CatalogLoaded(Box::new(
            serde_json::from_value(json!({
                "models": [{"id": o3, "display_name": "o3", "ai_provider_id": provider, "enabled": true, "reasoning_efforts": []}],
                "providers": [{"id": provider, "display_name": "Google Vertex AI", "available": false, "unavailable_reason": "missing_api_key"}],
                "unsupported_providers": []
            }))
            .unwrap(),
        )));
        let theme = Theme::terminal(true);
        for width in [60u16, 120] {
            let ctx = ViewCtx {
                width,
                ..ctx_for(&app, &theme)
            };
            let o = Overlay::open(Picker::Model, &app).unwrap();
            let view = o.view(&ctx);
            let mut term =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 12)).unwrap();
            term.draw(|f| {
                table::render(f, f.area(), &view, o.state(), &theme, None);
            })
            .unwrap();
            let buf = term.backend().buffer().clone();
            let shown: Vec<String> = (0..12)
                .map(|y| {
                    (0..width)
                        .map(|x| buf[(x, y)].symbol().to_owned())
                        .collect()
                })
                .collect();
            assert!(
                shown
                    .iter()
                    .any(|l| l.contains("Google Vertex AI")
                        && l.contains("no API key is configured")),
                "{width}: {shown:#?}"
            );
        }
    }

    #[test]
    fn model_and_provider_names_drop_hidden_characters() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (provider, model) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        app.update(Msg::CatalogLoaded(Box::new(
            serde_json::from_value(json!({
                "models": [{"id": model, "display_name": "Son\u{1b}[31mnet\u{202e}", "ai_provider_id": provider, "enabled": true, "reasoning_efforts": []}],
                "providers": [{"id": provider, "display_name": "Anth\u{202e}ropic", "available": true}],
                "unsupported_providers": [{"display_name": "Bed\u{7}rock"}]
            }))
            .unwrap(),
        )));
        let theme = Theme::terminal(true);
        let view = Overlay::open(Picker::Model, &app)
            .unwrap()
            .view(&ctx_for(&app, &theme));
        let text = rows_text(&view);
        assert_eq!(text[0], "Anthropic");
        assert!(text[1].starts_with("  Son[31mnet|"), "{text:?}");
        let status = view.status.unwrap_or_default();
        assert!(
            status.contains("Configured but not usable here: Bedrock"),
            "{status}"
        );
    }

    #[test]
    fn ellipsize_never_draws_past_its_width() {
        assert_eq!(ellipsize("abcdef", 0), "");
        assert_eq!(ellipsize("abcdef", 1), "\u{2026}");
        assert_eq!(ellipsize("abcdef", 4), "abc\u{2026}");
        assert_eq!(ellipsize("abc", 3), "abc");
        assert_eq!(ellipsize("", 0), "");
    }

    #[test]
    fn short_compaction_labels_mark_the_default_with_a_star() {
        let short = |percent, default| compaction_short_label(Shown::Known { percent, default });
        assert_eq!(short(70, true), "70%*");
        assert_eq!(short(55, false), "55%");
        assert_eq!(short(100, false), "100%");
        assert_eq!(compaction_short_label(Shown::Loading), "…");
        assert_eq!(compaction_short_label(Shown::Unknown), "?");
    }
}
