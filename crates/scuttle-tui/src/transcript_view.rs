//! Turns core state into transcript lines, the welcome block, and click targets.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;

use coder_sdk::types;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use scuttle_core::app::{Activity, App, HistoryEdge};
use scuttle_core::chat_list::ChatList;
use scuttle_core::density::{BlockKind, Density, DisplayPrefs, density_for};
use scuttle_core::live::LiveBlock;
use scuttle_core::transcript::Transcript;

use crate::icons::{self, Icon, IconSet};
use crate::markdown;
use crate::subagent::{self, Phase, Titles};
use crate::theme::Theme;
use crate::wrap::{cells_width, cols_on_rows, wrap_line, wrap_rows};

/// A block: (message ID, or `None` for the live turn; index of the block within it).
pub type BlockId = (Option<i64>, usize);

#[derive(Debug, Clone, PartialEq)]
pub enum HitTarget {
    Toggle(BlockId),
    CopyCode(String),
    /// An attached file's line; a click saves the file.
    SaveFile(uuid::Uuid),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub lines: Range<usize>,
    pub target: HitTarget,
}

/// Link text on one transcript row: a click on columns `cols` of row `line` opens `url`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkHit {
    pub line: usize,
    pub cols: Range<u16>,
    pub url: String,
    /// Which link of the view this row is part of: every row of one link, however it wraps
    /// or breaks, has the same `link`, and no other link has it.
    pub link: usize,
}

/// What the TUI needs to know about one transcript row beyond its text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LineMeta {
    /// The row continues a soft-wrapped line from the row before it.
    pub continuation: bool,
    /// The row belongs to one of the user's own messages, which the TUI tints.
    pub user: bool,
    /// The row is a rule between a turn's work and its answer, which a selection leaves out.
    pub rule: bool,
    /// How many cells at the end of the row's text are a click hint, which shows but is never
    /// copied.
    pub hint: u16,
}

#[derive(Debug, Clone, Default)]
pub struct View {
    pub lines: Vec<Line<'static>>,
    /// One entry per row of `lines`.
    pub meta: Vec<LineMeta>,
    /// Rows whose first cell is the marker of a block in progress: the thinking line of the
    /// reasoning the agent is in, or a tool call of the latest turn without a finished result
    /// while the agent works. The TUI paints the spinner frame there on every draw, so a timer
    /// frame animates them without rebuilding `lines`, which keep the static glyph.
    pub spinners: Vec<usize>,
    /// The rows among `spinners` that mark reasoning rather than a tool call.
    pub thinking: Vec<usize>,
    /// Whether the lines draw Nerd Font glyphs, which a copy turns back into text.
    pub nerd_icons: bool,
    /// The lines that hold the Nerd Font tip, if any, so the TUI can remember it was shown
    /// once they are on screen.
    pub nerd_tip: Option<Range<usize>>,
    /// Every row part of every link, so a link that wraps is clickable on each of its rows.
    pub links: Vec<LinkHit>,
    pub hits: Vec<Hit>,
    /// Code blocks of the most recent assistant turn (durable or live) that had any.
    pub last_code_blocks: Vec<String>,
    /// The first row of each durable message's own lines, past the blank row that separates
    /// it from the one before, by message id. A message with no lines of its own maps to the
    /// row after the lines before it.
    pub message_rows: BTreeMap<i64, usize>,
    /// The rows of each rendered block's own lines, in row order, without the blank and rule
    /// rows around them. A hidden block has none.
    pub blocks: Vec<(BlockId, Range<usize>)>,
}

/// A transcript row described by the block it belongs to or the block below it, so a rebuilt
/// view finds the same text again although rows were added or removed above it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    block: BlockId,
    /// The row's distance below the block's first row; negative for a row above the block.
    rows: isize,
    /// Whether the row is one of the block's own lines.
    inside: bool,
}

impl View {
    /// What row `row` shows, as an anchor `relocate` resolves in a later view.
    pub fn anchor(&self, row: usize) -> Option<Anchor> {
        let below = self.blocks.partition_point(|(_, rows)| rows.end <= row);
        let (block, rows) = match self.blocks.get(below) {
            Some((block, rows)) => (block, rows),
            // Past the last block, as on the queued and error lines: count from that block.
            None => {
                let (block, rows) = self.blocks.last()?;
                return Some(Anchor {
                    block: *block,
                    rows: (row - rows.start) as isize,
                    inside: false,
                });
            }
        };
        Some(Anchor {
            block: *block,
            rows: row as isize - rows.start as isize,
            inside: rows.contains(&row),
        })
    }

    /// The row `anchor` names in this view, or `None` when its block is gone.
    #[cfg(test)]
    pub fn find(&self, anchor: &Anchor) -> Option<usize> {
        self.block_rows(anchor.block)
            .map(|rows| anchor.place(&rows))
    }

    fn block_rows(&self, block: BlockId) -> Option<Range<usize>> {
        self.blocks
            .iter()
            .find(|(id, _)| *id == block)
            .map(|(_, rows)| rows.clone())
    }
}

impl Anchor {
    /// The row this anchor names in a block now on `rows`.
    fn place(&self, rows: &Range<usize>) -> usize {
        let offset = if self.inside {
            // A block that shrank, such as a collapsed one, keeps the row on its last line.
            self.rows.min(rows.len().saturating_sub(1) as isize)
        } else {
            self.rows
        };
        rows.start.saturating_add_signed(offset)
    }
}

/// Finds `old`'s blocks in `new`, a rebuild of it, with each block's rows indexed once.
struct Mover<'v> {
    old: &'v View,
    new: &'v View,
    rows: HashMap<BlockId, Range<usize>>,
    /// The durable messages `new` has that are newer than every message of `old`. When there
    /// are any, the live turn may have been persisted as one of them.
    persisted: Vec<i64>,
}

impl<'v> Mover<'v> {
    fn new(old: &'v View, new: &'v View) -> Self {
        let newest = old.message_rows.last_key_value().map(|(&id, _)| id);
        let persisted = match newest {
            Some(newest) => new
                .message_rows
                .range(newest + 1..)
                .map(|(&id, _)| id)
                .collect(),
            None => new.message_rows.keys().copied().collect(),
        };
        let rows = new.blocks.iter().cloned().collect();
        Mover {
            old,
            new,
            rows,
            persisted,
        }
    }

    /// Where `old`'s block `block`, drawn on `old_rows`, is in `new`. A durable block keeps
    /// its id. A live block keeps its id while no newer durable message appears. Once one
    /// does, the live turn may have been persisted as it and the next turn may have started,
    /// so the block is looked for only as the same block of a newer message that starts with
    /// the same text, never in the new live turn.
    fn moved(&self, block: BlockId, old_rows: &Range<usize>) -> Option<Range<usize>> {
        if block.0.is_some() || self.persisted.is_empty() {
            return self.rows.get(&block).cloned();
        }
        let start = self.old.lines.get(old_rows.start)?;
        self.persisted
            .iter()
            .filter_map(|&id| self.rows.get(&(Some(id), block.1)))
            .find(|rows| {
                self.new
                    .lines
                    .get(rows.start)
                    .is_some_and(|line| same_text(line, start))
            })
            .cloned()
    }
}

/// Whether two lines show the same text, whatever their styles.
fn same_text(a: &Line, b: &Line) -> bool {
    a.spans
        .iter()
        .flat_map(|s| s.content.chars())
        .eq(b.spans.iter().flat_map(|s| s.content.chars()))
}

/// Where `old`'s row `row` is in `new`, a rebuild of it, so text the reader saw stays on its
/// screen row. When the row's block is gone, as a result folded into a call an older page
/// brought in, the nearest block below that survived keeps its distance from the row.
/// `None` when no block at or below the row survived.
pub fn relocate(old: &View, new: &View, row: usize) -> Option<usize> {
    let anchor = old.anchor(row)?;
    let mover = Mover::new(old, new);
    if let Some(old_rows) = old.block_rows(anchor.block)
        && let Some(rows) = mover.moved(anchor.block, &old_rows)
    {
        return Some(anchor.place(&rows));
    }
    let below = old.blocks.partition_point(|(_, rows)| rows.start <= row);
    old.blocks[below..].iter().find_map(|(block, rows)| {
        let moved = mover.moved(*block, rows)?;
        Some(moved.start.saturating_sub(rows.start - row))
    })
}

#[derive(Debug, Clone)]
pub struct Welcome {
    pub url: String,
    pub user: String,
    pub art: Vec<String>,
    /// Whether a user's own `art` draws in the brand accent; the bundled wordmark always does.
    pub art_accent: bool,
    pub show: bool,
    /// Whether the welcome may suggest a Nerd Font, which it does only with text icons.
    pub tip: bool,
}

/// What an `attached` line adds when a click saves its file.
const SAVE_HINT: &str = " · click to save";

/// A renderable unit before wrapping.
enum Item<'a> {
    UserText(Cow<'a, str>),
    AssistantText(Cow<'a, str>),
    Reasoning(Cow<'a, str>),
    /// A file sent with a message, by name, or by type when it has none, with the id a
    /// click saves it by.
    File {
        label: String,
        file: Option<uuid::Uuid>,
    },
    Tool {
        name: &'a str,
        /// The call's ID, which matches a durable call against the core's unresolved tools.
        call_id: Option<&'a str>,
        /// What goes in the parentheses after the name; `None` leaves them out, as for a
        /// result whose call was never loaded and that names nothing.
        args: Option<String>,
        result: String,
        is_error: bool,
        done: bool,
        /// What a subagent tool's head says in place of its name and arguments.
        subagent: Option<SubagentHead>,
        /// What a `start_workspace` or `stop_workspace` head says in place of its name and
        /// arguments.
        /// Boxed to keep `Item` small.
        lifecycle: Option<Box<LifecycleHead>>,
    },
}

/// The head of a subagent tool, worded as the web UI words it.
struct SubagentHead {
    verb: &'static str,
    /// The subagent's title, or `None` when nothing names it.
    title: Option<String>,
    /// The tool name and its arguments, shown when the call is expanded.
    raw: String,
    /// Whether the parent stopped waiting on a subagent that may still run, which is not a
    /// failure and gets a neutral marker.
    stopped: bool,
}

/// What names a subagent that a call names only by ID.
struct Names<'n> {
    titles: &'n Titles,
    chats: Option<&'n ChatList>,
}

/// The head of subagent tool `name`, or `None` for any other tool. `raw_args` gives its
/// arguments as text, for the expanded call.
fn subagent_head(
    name: &str,
    args: Option<&serde_json::Value>,
    raw_args: impl FnOnce() -> String,
    result: Option<&ToolResultInfo<'_>>,
    names: &Names,
) -> Option<SubagentHead> {
    subagent::action(name)?;
    let phase = match result {
        Some(r) if r.done && r.is_error => Phase::Failed,
        Some(r) if r.done => Phase::Done,
        _ => Phase::Running,
    };
    let value = result.and_then(|r| r.value);
    let (verb, title) = subagent::label(name, args, value, phase, names.titles, names.chats)?;
    Some(SubagentHead {
        verb,
        title,
        raw: format!("{name}({})", raw_args()),
        stopped: subagent::stopped_waiting(name, value, phase),
    })
}

/// The head of a subagent tool result whose call is not loaded.
fn orphan_head(
    name: &str,
    result: Option<&serde_json::Value>,
    phase: Phase,
    names: &Names,
) -> Option<SubagentHead> {
    let (verb, title) = subagent::label(name, None, result, phase, names.titles, names.chats)?;
    Some(SubagentHead {
        verb,
        title,
        raw: format!("{name}()"),
        stopped: subagent::stopped_waiting(name, result, phase),
    })
}

/// What a workspace lifecycle tool does to the chat's workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
    Start,
    Stop,
}

impl Lifecycle {
    fn of(name: &str) -> Option<Lifecycle> {
        match name {
            "start_workspace" => Some(Lifecycle::Start),
            "stop_workspace" => Some(Lifecycle::Stop),
            _ => None,
        }
    }

    fn verb(self, phase: Phase) -> &'static str {
        match (self, phase) {
            (Lifecycle::Start, Phase::Running) => "Starting workspace…",
            (Lifecycle::Start, Phase::Done) => "Started",
            (Lifecycle::Start, Phase::Failed) => "Failed to start",
            (Lifecycle::Stop, Phase::Running) => "Stopping workspace…",
            (Lifecycle::Stop, Phase::Done) => "Stopped",
            (Lifecycle::Stop, Phase::Failed) => "Failed to stop",
        }
    }
}

/// The head of a `start_workspace` or `stop_workspace` call, worded as the web UI's
/// `WorkspaceLifecycleTool` words it.
struct LifecycleHead {
    /// The label in the accent color: the verb, or the title of a quota failure.
    verb: String,
    /// The workspace after the verb, else "workspace"; `None` while the call runs and after
    /// a quota title.
    object: Option<String>,
    /// The tool name and its arguments, shown when the call is expanded.
    raw: String,
    /// Whether the call failed, which a build failure reports only in the result's `error`
    /// field, with `is_error` false so the result keeps its `build_id`.
    failed: bool,
}

/// The `error_code` of a build refused because the workspace quota is full.
const INSUFFICIENT_QUOTA: &str = "INSUFFICIENT_QUOTA";

/// The head of `start_workspace` or `stop_workspace`, or `None` for any other tool. The
/// `parameters` a start may carry stay out of the label and show in the expanded call.
fn lifecycle_head(
    name: &str,
    args: Option<&serde_json::Value>,
    result: Option<&serde_json::Value>,
    done: bool,
    is_error: bool,
) -> Option<LifecycleHead> {
    let action = Lifecycle::of(name)?;
    let record = result_object(result);
    let field = |key: &str| {
        record
            .as_ref()?
            .get(key)?
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| markdown::drawable(s).into_owned())
    };
    let failed = done && (is_error || field("error").is_some());
    let phase = match (done, failed) {
        (false, _) => Phase::Running,
        (true, true) => Phase::Failed,
        (true, false) => Phase::Done,
    };
    let quota = done && field("error_code").as_deref() == Some(INSUFFICIENT_QUOTA);
    let (verb, object) = if quota {
        let title = field("title").unwrap_or_else(|| "Workspace quota reached".to_owned());
        (title, None)
    } else if phase == Phase::Running {
        (action.verb(phase).to_owned(), None)
    } else {
        let workspace = field("workspace_name").unwrap_or_else(|| "workspace".to_owned());
        (action.verb(phase).to_owned(), Some(workspace))
    };
    let shown_args = args
        .filter(|a| args_summary(a).is_some())
        .map(|a| a.to_string())
        .unwrap_or_default();
    Some(LifecycleHead {
        verb,
        object,
        raw: format!("{name}({shown_args})"),
        failed,
    })
}

/// `result` as an object, parsing a result that arrived as a JSON string first. A result
/// that is already an object is borrowed rather than copied.
fn result_object(
    result: Option<&serde_json::Value>,
) -> Option<Cow<'_, serde_json::Map<String, serde_json::Value>>> {
    match result? {
        serde_json::Value::Object(map) => Some(Cow::Borrowed(map)),
        serde_json::Value::String(s) => match serde_json::from_str::<serde_json::Value>(s).ok()? {
            serde_json::Value::Object(map) => Some(Cow::Owned(map)),
            _ => None,
        },
        _ => None,
    }
}

/// A tool's result, gathered from wherever Coder recorded it: a durable `tool-result` part
/// (possibly in a later message with role "tool", since Coder stores a tool's result
/// separately from its call) or a live `LiveBlock::ToolResult`.
struct ToolResultInfo<'m> {
    result: String,
    /// What the result names, for a call whose arguments say nothing.
    summary: Option<String>,
    /// The result as the server sent it, which names a subagent's chat and title.
    value: Option<&'m serde_json::Value>,
    is_error: bool,
    done: bool,
}

struct Out<'t> {
    view: View,
    width: u16,
    theme: &'t Theme,
    hidden: usize,
    /// How many links the view has numbered so far, which numbers the next block's links.
    links_numbered: usize,
}

/// What `render_items` reads besides the items themselves.
struct Ctx<'c> {
    prefs: &'c DisplayPrefs,
    overrides: &'c BTreeMap<String, Density>,
    toggles: &'c HashSet<BlockId>,
    /// Assistant text blocks that start a turn's answer and get a rule above them.
    answers: &'c HashSet<BlockId>,
    /// What the agent is doing, which decides whether a block's marker animates.
    activity: Option<Activity>,
    /// Tool calls after the last user message. Only these can be running: a call the provider
    /// interrupted never gets a result, and must not animate on every later turn.
    latest_tools: &'c HashSet<BlockId>,
    /// The files a click on their `attached` line saves.
    saveable: Option<&'c HashSet<uuid::Uuid>>,
}

impl Out<'_> {
    /// Appends wrapped rows with their metadata and returns their range.
    fn extend_rows(&mut self, rows: Vec<(Line<'static>, bool)>, user: bool) -> Range<usize> {
        self.extend_marked(
            rows,
            LineMeta {
                user,
                ..LineMeta::default()
            },
        )
    }

    /// Appends wrapped rows that all share `meta` apart from their continuation flag.
    fn extend_marked(&mut self, rows: Vec<(Line<'static>, bool)>, meta: LineMeta) -> Range<usize> {
        let start = self.view.lines.len();
        for (line, continuation) in rows {
            self.view.lines.push(line);
            self.view.meta.push(LineMeta {
                continuation,
                ..meta
            });
        }
        start..self.view.lines.len()
    }

    /// Wraps and appends `lines`; `user` marks them as the user's own message.
    fn push(&mut self, lines: Vec<Line<'static>>, user: bool) -> Range<usize> {
        self.flush_hidden();
        let rows = wrap_rows(&lines, self.width);
        self.extend_rows(rows, user)
    }

    /// Records on the rows `range` the cells at their end that `hint` took, however it wrapped,
    /// so a copy leaves them out.
    fn mark_hint(&mut self, range: Range<usize>, hint: &str) {
        let mut rest = hint;
        for row in range.rev() {
            let text = self.view.lines[row].to_string();
            let text = text.trim_end();
            let rest_trimmed = rest.trim_end();
            let taken = (0..rest_trimmed.len())
                .filter(|&i| rest_trimmed.is_char_boundary(i))
                .find(|&i| text.ends_with(&rest_trimmed[i..]))
                .map_or(0, |i| rest_trimmed.len() - i);
            if taken == 0 {
                break;
            }
            self.view.meta[row].hint =
                cells_width(&rest_trimmed[rest_trimmed.len() - taken..]) as u16;
            rest = &rest_trimmed[..rest_trimmed.len() - taken];
        }
    }

    /// `push` for lines already wrapped to the view's width, each as its rows.
    fn push_wrapped(&mut self, wrapped: Vec<Vec<Line<'static>>>, user: bool) -> Range<usize> {
        self.flush_hidden();
        let rows = wrapped
            .into_iter()
            .flat_map(|rows| rows.into_iter().enumerate().map(|(i, row)| (row, i > 0)))
            .collect();
        self.extend_rows(rows, user)
    }

    fn flush_hidden(&mut self) {
        if self.hidden == 0 {
            return;
        }
        let n = std::mem::take(&mut self.hidden);
        let label = if n == 1 {
            "1 hidden tool call".to_string()
        } else {
            format!("{n} hidden tool calls")
        };
        let line = Line::from(Span::styled(format!("  ({label})"), self.theme.dim));
        let rows = wrap_rows(&[line], self.width);
        self.extend_rows(rows, false);
    }

    fn gap(&mut self) {
        self.flush_hidden();
        if self.view.lines.last().is_some_and(|l| !l.spans.is_empty()) {
            self.extend_rows(vec![(Line::default(), false)], false);
        }
    }

    /// Records the rows of block `id`'s own lines, which anchors find it by.
    fn block(&mut self, id: BlockId, rows: Range<usize>) {
        if !rows.is_empty() {
            self.view.blocks.push((id, rows));
        }
    }

    /// A full-width rule between a turn's work and its answer.
    fn rule(&mut self) {
        self.gap();
        let line = Line::from(Span::styled(
            "─".repeat(self.width as usize),
            self.theme.rule,
        ));
        self.extend_marked(
            vec![(line, false)],
            LineMeta {
                rule: true,
                ..LineMeta::default()
            },
        );
    }
}

fn one_line(text: &str, max: usize) -> String {
    let first = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim();
    let mut out: String = first.chars().take(max).collect();
    if first.chars().count() > max || text.lines().filter(|l| !l.trim().is_empty()).count() > 1 {
        out.push('…');
    }
    out
}

/// The call's arguments on one line: the first non-empty string among them, else all of them
/// as JSON. `None` when they say nothing, such as `{}` or only empty values, so the summary can
/// fall back to the result.
fn args_summary(args: &serde_json::Value) -> Option<String> {
    match args {
        serde_json::Value::Object(map) => map
            .values()
            .filter_map(|v| v.as_str())
            .find(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                map.values()
                    .any(|v| !is_empty_value(v))
                    .then(|| args.to_string())
            }),
        serde_json::Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// Whether `v` holds nothing worth showing: null, an empty string or array, or an object of
/// such values.
fn is_empty_value(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Null => true,
        serde_json::Value::String(s) => s.is_empty(),
        serde_json::Value::Array(a) => a.is_empty(),
        serde_json::Value::Object(m) => m.values().all(is_empty_value),
        _ => false,
    }
}

/// Result fields that name what a tool acted on, for a call whose arguments say nothing.
/// Workspace tools report the workspace as `workspace_name`
/// (`coderd/x/chatd/chattool/startworkspace.go:214-269`); `start_workspace` and
/// `stop_workspace` draw it through `lifecycle_head` instead.
const RESULT_NAME_FIELDS: &[&str] = &["workspace_name"];

/// The first non-empty `RESULT_NAME_FIELDS` value of a result object, parsing a result that
/// arrived as a JSON string first.
fn result_summary(result: Option<&serde_json::Value>) -> Option<String> {
    let map = result_object(result)?;
    RESULT_NAME_FIELDS
        .iter()
        .find_map(|k| map.get(*k)?.as_str().filter(|s| !s.trim().is_empty()))
        .map(str::to_owned)
}

/// Strips ANSI CSI escape sequences (`ESC '[' ...` up to a final byte in `0x40..=0x7E`) from
/// tool output, since some tools (e.g. a colorized `ls`) include them in their result text and
/// the transcript has no terminal emulator to interpret them.
fn strip_ansi_csi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c2 in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c2) {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn result_text(result: Option<&serde_json::Value>, raw: &str) -> String {
    let text = match result {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Object(map)) => map
            .values()
            .filter_map(|v| v.as_str())
            .next()
            .map(str::to_owned)
            .unwrap_or_else(|| serde_json::to_string_pretty(result.unwrap()).unwrap_or_default()),
        Some(other) => other.to_string(),
        None => raw.to_owned(),
    };
    markdown::drawable(&carriage_returns(&strip_ansi_csi(&text))).into_owned()
}

/// `text` as a terminal leaves it after carriage returns: on each line, what follows the
/// last lone CR replaces what came before it, so a progress bar shows its last state. A
/// CRLF stays one line break.
fn carriage_returns(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_owned();
    }
    text.replace("\r\n", "\n")
        .split('\n')
        .map(|line| line.rsplit('\r').next().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Collects every tool-result, and every id that has a matching tool-call, from durable
/// messages and the live turn. Coder persists a tool's result in a separate message with role
/// "tool" (see `coderd/x/chatd/message_conversion.go`), not alongside its call, so pairing a
/// call with its result requires looking across the whole transcript rather than one message.
fn collect_tool_results<'m>(
    messages: &[&'m types::CodersdkChatMessage],
    live: &'m [LiveBlock],
) -> (BTreeMap<String, ToolResultInfo<'m>>, HashSet<String>) {
    let mut results = BTreeMap::new();
    let mut calls = HashSet::new();
    for m in messages {
        for p in &m.content {
            match p.type_.as_ref().map(|t| t.as_str()) {
                Some("tool-call") => {
                    calls.insert(p.tool_call_id.clone().unwrap_or_default());
                }
                Some("tool-result") => {
                    results.insert(
                        p.tool_call_id.clone().unwrap_or_default(),
                        ToolResultInfo {
                            result: result_text(p.result.as_ref(), ""),
                            summary: result_summary(p.result.as_ref()),
                            value: p.result.as_ref(),
                            is_error: p.is_error.unwrap_or(false),
                            done: true,
                        },
                    );
                }
                _ => {}
            }
        }
    }
    for b in live {
        match b {
            LiveBlock::ToolCall { id, .. } => {
                calls.insert(id.clone());
            }
            LiveBlock::ToolResult {
                id,
                result,
                result_raw,
                is_error,
                done,
                ..
            } => {
                results.insert(
                    id.clone(),
                    ToolResultInfo {
                        result: result_text(result.as_ref(), result_raw),
                        summary: result_summary(result.as_ref()),
                        value: result.as_ref(),
                        is_error: *is_error,
                        done: *done,
                    },
                );
            }
            _ => {}
        }
    }
    (results, calls)
}

/// What names a sent file after its icon: its name, else its type.
fn file_label(p: &types::CodersdkChatMessagePart) -> String {
    let name = scuttle_core::files::display_name(scuttle_core::files::part_name(p));
    match (name, p.media_type.as_deref().filter(|t| !t.is_empty())) {
        (name, _) if !name.is_empty() => name,
        (_, Some(kind)) => format!("a {kind} file"),
        (_, None) => "a file".to_owned(),
    }
}

/// The kind of text a part adds to the item before it.
#[derive(Clone, Copy)]
enum Run {
    User,
    Assistant,
    Reasoning,
}

/// Adds a text part the way the web UI does: a whitespace-only part is dropped, and a part
/// directly after text of the same kind continues that item with no separator, so one reply
/// the server split into several parts renders as one Markdown document.
fn push_text<'a>(items: &mut Vec<Item<'a>>, run: Run, text: Option<&'a str>, join: bool) {
    let text = text.unwrap_or_default();
    if text.trim().is_empty() {
        return;
    }
    let last = match (items.last_mut().filter(|_| join), run) {
        (Some(Item::UserText(t)), Run::User)
        | (Some(Item::AssistantText(t)), Run::Assistant)
        | (Some(Item::Reasoning(t)), Run::Reasoning) => Some(t),
        _ => None,
    };
    match last {
        Some(t) => t.to_mut().push_str(text),
        None => {
            let text = Cow::Borrowed(text);
            items.push(match run {
                Run::User => Item::UserText(text),
                Run::Assistant => Item::AssistantText(text),
                Run::Reasoning => Item::Reasoning(text),
            });
        }
    }
}

fn items_for_message<'a>(
    m: &'a types::CodersdkChatMessage,
    results: &BTreeMap<String, ToolResultInfo<'_>>,
    calls: &HashSet<String>,
    names: &Names,
) -> Vec<Item<'a>> {
    let role = m.role.as_ref().map(|r| r.as_str());
    // A "tool" message carries tool-result parts, already folded into `results` for the calls
    // they answer; a result whose call is outside the loaded history gets its own line.
    if role == Some("tool") {
        return m
            .content
            .iter()
            .filter(|p| p.type_.as_ref().map(|t| t.as_str()) == Some("tool-result"))
            .filter(|p| !calls.contains(p.tool_call_id.as_deref().unwrap_or_default()))
            .map(|p| {
                let name = p.tool_name.as_deref().unwrap_or("tool");
                let failed = p.is_error.unwrap_or(false);
                let phase = if failed { Phase::Failed } else { Phase::Done };
                Item::Tool {
                    name,
                    call_id: p.tool_call_id.as_deref(),
                    args: result_summary(p.result.as_ref()),
                    result: result_text(p.result.as_ref(), ""),
                    is_error: failed,
                    done: true,
                    subagent: orphan_head(name, p.result.as_ref(), phase, names),
                    lifecycle: lifecycle_head(name, None, p.result.as_ref(), true, failed)
                        .map(Box::new),
                }
            })
            .collect();
    }
    let user = role == Some("user");
    let mut items = Vec::new();
    let mut split = false;
    for p in &m.content {
        match p.type_.as_ref().map(|t| t.as_str()).unwrap_or_default() {
            "text" if user => push_text(&mut items, Run::User, p.text.as_deref(), !split),
            "text" => push_text(&mut items, Run::Assistant, p.text.as_deref(), !split),
            "reasoning" => push_text(&mut items, Run::Reasoning, p.text.as_deref(), !split),
            // A source draws nothing, but like the live stream's source block it ends a run.
            "source" => {
                split = true;
                continue;
            }
            // The provider runs these itself, so they neither draw nor end a run.
            "tool-call" if p.provider_executed == Some(true) => continue,
            "file" => items.push(Item::File {
                label: file_label(p),
                file: p.file_id,
            }),
            "tool-call" => {
                let name = p.tool_name.as_deref().unwrap_or("tool");
                let result = results.get(p.tool_call_id.as_deref().unwrap_or_default());
                items.push(Item::Tool {
                    name,
                    call_id: p.tool_call_id.as_deref(),
                    args: Some(
                        p.args
                            .as_ref()
                            .and_then(args_summary)
                            .or_else(|| result.and_then(|r| r.summary.clone()))
                            .unwrap_or_default(),
                    ),
                    result: result.map(|r| r.result.clone()).unwrap_or_default(),
                    is_error: result.map(|r| r.is_error).unwrap_or(false),
                    done: result.map(|r| r.done).unwrap_or(false),
                    subagent: subagent_head(
                        name,
                        p.args.as_ref(),
                        || p.args.as_ref().map(|a| a.to_string()).unwrap_or_default(),
                        result,
                        names,
                    ),
                    lifecycle: lifecycle_head(
                        name,
                        p.args.as_ref(),
                        result.and_then(|r| r.value),
                        result.is_some_and(|r| r.done),
                        result.is_some_and(|r| r.is_error),
                    )
                    .map(Box::new),
                });
            }
            _ => {}
        }
        split = false;
    }
    items
}

fn items_for_live<'a>(
    blocks: &'a [LiveBlock],
    results: &BTreeMap<String, ToolResultInfo<'_>>,
    calls: &HashSet<String>,
    names: &Names,
) -> Vec<Item<'a>> {
    let mut items = Vec::new();
    for b in blocks {
        match b {
            LiveBlock::Text(t) => items.push(Item::AssistantText(Cow::Borrowed(t))),
            LiveBlock::Reasoning(t) => items.push(Item::Reasoning(Cow::Borrowed(t))),
            LiveBlock::ToolCall {
                id,
                name,
                args,
                args_raw,
            } => {
                let result = results.get(id);
                items.push(Item::Tool {
                    name,
                    call_id: Some(id),
                    args: Some(
                        match args {
                            Some(args) => args_summary(args),
                            None => Some(args_raw.clone()).filter(|raw| !raw.is_empty()),
                        }
                        .or_else(|| result.and_then(|r| r.summary.clone()))
                        .unwrap_or_default(),
                    ),
                    result: result.map(|r| r.result.clone()).unwrap_or_default(),
                    is_error: result.map(|r| r.is_error).unwrap_or(false),
                    done: result.map(|r| r.done).unwrap_or(false),
                    subagent: subagent_head(
                        name,
                        args.as_ref(),
                        || match args {
                            Some(args) => args.to_string(),
                            None => args_raw.clone(),
                        },
                        result,
                        names,
                    ),
                    lifecycle: lifecycle_head(
                        name,
                        args.as_ref(),
                        result.and_then(|r| r.value),
                        result.is_some_and(|r| r.done),
                        result.is_some_and(|r| r.is_error),
                    )
                    .map(Box::new),
                });
            }
            // A result whose call never showed up live or durably (e.g. the call was in an
            // earlier, already-flushed generation) still gets its own tool line.
            LiveBlock::ToolResult {
                id,
                name,
                result,
                result_raw,
                is_error,
                done,
                ..
            } if !calls.contains(id) => {
                items.push(Item::Tool {
                    name,
                    call_id: Some(id),
                    args: result_summary(result.as_ref()),
                    result: result_text(result.as_ref(), result_raw),
                    is_error: *is_error,
                    done: *done,
                    subagent: orphan_head(
                        name,
                        result.as_ref(),
                        match (*done, *is_error) {
                            (false, _) => Phase::Running,
                            (true, true) => Phase::Failed,
                            (true, false) => Phase::Done,
                        },
                        names,
                    ),
                    lifecycle: lifecycle_head(name, None, result.as_ref(), *done, *is_error)
                        .map(Box::new),
                });
            }
            LiveBlock::File {
                file_id,
                name,
                media_type,
            } => items.push(Item::File {
                label: file_label(&types::CodersdkChatMessagePart {
                    name: name.clone(),
                    media_type: media_type.clone(),
                    ..Default::default()
                }),
                file: *file_id,
            }),
            _ => {}
        }
    }
    items
}

/// The first assistant text after the last reasoning or tool block of each turn. A turn runs
/// from one user message to the next, across every assistant and tool message between them,
/// since Coder stores each step of a turn as its own message.
fn answer_starts(groups: &[(Option<i64>, Vec<Item>)]) -> HashSet<BlockId> {
    let mut starts = HashSet::new();
    let mut saw_work = false;
    let mut pending: Option<BlockId> = None;
    for (owner, items) in groups {
        for (index, item) in items.iter().enumerate() {
            match item {
                Item::UserText(_) => {
                    starts.extend(pending.take());
                    saw_work = false;
                }
                Item::Reasoning(_) | Item::Tool { .. } => {
                    saw_work = true;
                    pending = None;
                }
                Item::File { .. } => {}
                Item::AssistantText(text)
                    if saw_work && pending.is_none() && !text.trim().is_empty() =>
                {
                    pending = Some((*owner, index));
                }
                Item::AssistantText(_) => {}
            }
        }
    }
    starts.extend(pending);
    starts
}

/// The tool blocks that can be running: the durable calls the core counts as this turn's
/// unresolved tools, and every call of the live turn, whose own `done` says whether it ended.
fn latest_tools(groups: &[(Option<i64>, Vec<Item>)], transcript: &Transcript) -> HashSet<BlockId> {
    let unresolved = transcript.unresolved_tools();
    let keys: HashSet<(i64, Option<&str>)> = unresolved.iter().map(|t| t.key()).collect();
    let mut tools = HashSet::new();
    for (owner, items) in groups {
        for (index, item) in items.iter().enumerate() {
            let Item::Tool { call_id, .. } = item else {
                continue;
            };
            let running = match owner {
                Some(message) => keys.contains(&(*message, *call_id)),
                None => true,
            };
            if running {
                tools.insert((*owner, index));
            }
        }
    }
    tools
}

/// Renders `items` into `out`, returning the code blocks of any assistant text among them.
/// `live` selects `markdown::render` (uncached, since live text keeps changing) over
/// `markdown::render_cached` (for durable, unchanging text); both take the pane width, which
/// lays out any table in the text.
fn render_items(
    out: &mut Out,
    owner: Option<i64>,
    items: Vec<Item>,
    ctx: &Ctx,
    live: bool,
) -> Vec<String> {
    let width = out.width as usize;
    let mut code_blocks = Vec::new();
    let count = items.len();
    for (index, item) in items.into_iter().enumerate() {
        let id: BlockId = (owner, index);
        match item {
            Item::UserText(text) => {
                out.gap();
                // A message of only files has no text to show, so it draws no empty `›` line.
                let text = if text.trim().is_empty() { "" } else { &text };
                let lines = text
                    .lines()
                    .map(|l| {
                        Line::from(vec![
                            Span::styled("› ", out.theme.user),
                            Span::raw(l.to_owned()),
                        ])
                    })
                    .collect();
                let range = out.push(lines, true);
                out.block(id, range);
            }
            Item::AssistantText(text) => {
                if ctx.answers.contains(&id) {
                    out.rule();
                }
                out.gap();
                let rendered = if live {
                    markdown::render(&text, out.width)
                } else {
                    markdown::render_cached(&text, out.width)
                };
                // Each rendered line is wrapped once; its rows place the code blocks and links
                // and are then pushed as they are.
                let wrapped: Vec<Vec<Line<'static>>> = rendered
                    .lines
                    .iter()
                    .map(|line| wrap_line(line, out.width))
                    .collect();
                // The first transcript row of each rendered line, and the row after the last.
                let mut starts = Vec::with_capacity(wrapped.len() + 1);
                let mut at = out.view.lines.len();
                for rows in &wrapped {
                    starts.push(at);
                    at += rows.len();
                }
                starts.push(at);
                for block in &rendered.code_blocks {
                    out.view.hits.push(Hit {
                        lines: starts[block.start]..starts[block.end],
                        target: HitTarget::CopyCode(block.code.clone()),
                    });
                }
                if !rendered.links.is_empty() {
                    // Numbered after the links of earlier blocks, so ids stay unique in the view.
                    let first_id = out.links_numbered;
                    out.links_numbered +=
                        rendered.links.iter().map(|l| l.id + 1).max().unwrap_or(0);
                    for link in &rendered.links {
                        for (row, cols) in cols_on_rows(&wrapped[link.line], &link.cols) {
                            out.view.links.push(LinkHit {
                                line: starts[link.line] + row,
                                cols: cols.start as u16..cols.end as u16,
                                url: link.url.clone(),
                                link: first_id + link.id,
                            });
                        }
                    }
                }
                code_blocks.extend(rendered.code_blocks.iter().map(|b| b.code.clone()));
                let range = out.push_wrapped(wrapped, false);
                out.block(id, range);
            }
            Item::File { label, file } => {
                // A message that starts with a file, such as a paste sent alone, still stands
                // apart from the one before it.
                if index == 0 {
                    out.gap();
                }
                // Only a file a click can save offers the click.
                let saves = file.filter(|f| ctx.saveable.is_some_and(|s| s.contains(f)));
                let mut spans =
                    icons::line_with(out.theme, "  ", Icon::Attached, &label, out.theme.dim);
                if saves.is_some() {
                    spans.push(Span::styled(SAVE_HINT, out.theme.dim));
                }
                let range = out.push(vec![Line::from(spans)], true);
                if let Some(file) = saves {
                    out.mark_hint(range.clone(), SAVE_HINT);
                    out.view.hits.push(Hit {
                        lines: range.clone(),
                        target: HitTarget::SaveFile(file),
                    });
                }
                out.block(id, range);
            }
            Item::Reasoning(text) => {
                out.gap();
                let mut density = density_for(BlockKind::Reasoning, ctx.prefs, ctx.overrides);
                if ctx.toggles.contains(&id) {
                    density = if density == Density::Expanded {
                        Density::Summary
                    } else {
                        Density::Expanded
                    };
                }
                let lines = match density {
                    Density::Expanded => markdown::drawable(&text)
                        .lines()
                        .map(|l| Line::from(Span::styled(l.to_owned(), out.theme.dim)))
                        .collect(),
                    _ => vec![Line::from(Span::styled("∴ Thinking", out.theme.dim))],
                };
                let range = out.push(lines, false);
                out.block(id, range.clone());
                // Only the block the agent is thinking in right now animates.
                if density != Density::Expanded
                    && live
                    && index + 1 == count
                    && matches!(ctx.activity, Some(Activity::Thinking))
                {
                    out.view.spinners.push(range.start);
                    out.view.thinking.push(range.start);
                }
                out.view.hits.push(Hit {
                    lines: range,
                    target: HitTarget::Toggle(id),
                });
            }
            Item::Tool {
                name,
                args,
                result,
                is_error,
                done,
                subagent,
                lifecycle,
                ..
            } => {
                let mut density = density_for(BlockKind::Tool(name), ctx.prefs, ctx.overrides);
                if ctx.toggles.contains(&id) {
                    density = if density == Density::Expanded {
                        Density::Summary
                    } else {
                        Density::Expanded
                    };
                }
                if density == Density::Hidden {
                    out.hidden += 1;
                    continue;
                }
                out.gap();
                let stopped = subagent.as_ref().is_some_and(|s| s.stopped);
                let failed = is_error || lifecycle.as_ref().is_some_and(|l| l.failed);
                let marker = match (done, failed) {
                    _ if stopped => Span::styled("◼ ", out.theme.dim),
                    (false, _) => Span::styled("◌ ", out.theme.warn),
                    (true, true) => Span::styled("✗ ", out.theme.error),
                    (true, false) => Span::styled("⏺ ", out.theme.ok),
                };
                // The kind's icon gets its own slot after the state marker, which the spinner
                // paints over, so the marker stays one cell.
                let kind = icons::lead(out.theme, icons::tool_icon(name), out.theme.accent);
                let kind_width = kind.as_ref().map_or(0, |k| cells_width(&k.content));
                let head_budget = width
                    .saturating_sub(name.chars().count() + 6 + kind_width)
                    .max(8);
                let mut head = vec![marker];
                head.extend(kind);
                match (&subagent, &lifecycle) {
                    (Some(s), _) => {
                        head.push(Span::styled(s.verb, out.theme.accent));
                        head.push(Span::raw(match &s.title {
                            Some(title) => format!(" '{}'", one_line(title, head_budget)),
                            None => " a subagent".to_owned(),
                        }));
                    }
                    (None, Some(l)) => {
                        // A quota title comes from the server, so it is held to one line.
                        head.push(Span::styled(
                            one_line(&l.verb, head_budget),
                            out.theme.accent,
                        ));
                        if let Some(object) = &l.object {
                            head.push(Span::raw(format!(" {}", one_line(object, head_budget))));
                        }
                    }
                    (None, None) => {
                        head.push(Span::styled(name.to_owned(), out.theme.accent));
                        if let Some(args) = args {
                            head.push(Span::raw(format!("({})", one_line(&args, head_budget))));
                        }
                    }
                }
                let head = Line::from(head);
                let mut lines = vec![head];
                match density {
                    Density::Expanded => {
                        // The label hides the raw call, so the expanded call shows it.
                        let raw = match (&subagent, &lifecycle) {
                            (Some(s), _) => Some(&s.raw),
                            (None, Some(l)) => Some(&l.raw),
                            (None, None) => None,
                        };
                        if let Some(raw) = raw {
                            lines.push(Line::from(Span::styled(format!("  {raw}"), out.theme.dim)));
                        }
                        lines.extend(
                            result
                                .lines()
                                .map(|l| Line::from(Span::styled(format!("  {l}"), out.theme.dim))),
                        );
                    }
                    _ if !result.is_empty() => lines.push(Line::from(Span::styled(
                        format!("  ⎿ {}", one_line(&result, width.saturating_sub(6).max(8))),
                        out.theme.dim,
                    ))),
                    _ => {}
                }
                let mut wrapped: Vec<(Line<'static>, bool)> = Vec::new();
                for (i, l) in lines.into_iter().enumerate() {
                    let w = wrap_rows(&[l], out.width);
                    if density == Density::Expanded || i > 0 || w.len() == 1 {
                        wrapped.extend(w);
                    } else {
                        wrapped.push(w.into_iter().next().unwrap_or_default());
                    }
                }
                if density != Density::Expanded {
                    wrapped.truncate(2);
                }
                let range = out.extend_rows(wrapped, false);
                out.block(id, range.clone());
                // A sent message the agent has not picked up yet ends the turn before it.
                if !done
                    && ctx.latest_tools.contains(&id)
                    && ctx
                        .activity
                        .as_ref()
                        .is_some_and(|a| *a != Activity::Waiting)
                {
                    out.view.spinners.push(range.start);
                }
                out.view.hits.push(Hit {
                    lines: range,
                    target: HitTarget::Toggle(id),
                });
            }
        }
    }
    code_blocks
}

/// The welcome block. Art shows above the name with the credit under it, unless a line of it
/// is wider than `width`, where it would wrap into noise; a blank `art` shows the name alone.
fn welcome_lines(w: &Welcome, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![Line::default()];
    let fits = w.art.iter().all(|l| cells_width(l) <= usize::from(width));
    if !w.art.is_empty() && fits {
        // A user's own art takes the accent unless `welcome.art_color` says plain; the credit
        // shows under any art, and `theme.brand` is already plain under `NO_COLOR`.
        let style = if w.art_accent || crate::art::is_wordmark(&w.art) {
            theme.brand
        } else {
            Style::default()
        };
        lines.extend(
            w.art
                .iter()
                .map(|l| Line::from(Span::styled(l.clone(), style))),
        );
        lines.push(Line::from(Span::styled(crate::art::CREDIT, theme.dim)));
        lines.push(Line::default());
    }
    lines.push(Line::from(Span::styled("scuttle", theme.accent)));
    lines.push(Line::from(Span::styled(
        "An unofficial terminal client for Coder Agents",
        theme.dim,
    )));
    lines.push(Line::default());
    if !w.user.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Signed in as ", theme.dim),
            Span::raw(w.user.clone()),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled("Deployment ", theme.dim),
        Span::raw(w.url.clone()),
    ]));
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Type a message to start. /help lists commands.",
        theme.dim,
    )));
    lines
}

/// The tip text mode shows under the welcome: the words before the link, the link's text,
/// and what follows it.
const TIP: [&str; 3] = ["scuttle works better with a ", "Nerd Font", "!"];

/// Appends a blank row and the Nerd Font tip, with "Nerd Font" registered as a link to
/// nerdfonts.com on every row it wraps onto, so it lights up and opens as an assistant's
/// link does.
fn push_nerd_font_tip(out: &mut Out) {
    out.flush_hidden();
    out.extend_rows(vec![(Line::default(), false)], false);
    let [before, link, after] = TIP;
    let line = Line::from(vec![
        Span::styled(before, out.theme.dim),
        Span::styled(link, Style::new().add_modifier(Modifier::UNDERLINED)),
        Span::styled(after, out.theme.dim),
    ]);
    let rows = wrap_line(&line, out.width);
    let first = out.view.lines.len();
    let start = cells_width(before);
    let id = out.links_numbered;
    out.links_numbered += 1;
    for (row, cols) in cols_on_rows(&rows, &(start..start + cells_width(link))) {
        out.view.links.push(LinkHit {
            line: first + row,
            cols: cols.start as u16..cols.end as u16,
            url: icons::NERD_FONTS_URL.to_owned(),
            link: id,
        });
    }
    out.push_wrapped(vec![rows], false);
    out.view.nerd_tip = Some(first..out.view.lines.len());
}

/// What a transcript is built from, so the subagent preview can build one without the app.
pub struct TranscriptSource<'a> {
    pub transcript: &'a Transcript,
    pub prefs: &'a DisplayPrefs,
    /// What the agent is doing, which decides whether markers animate; `None` keeps them still.
    pub activity: Option<Activity>,
    /// What the top line says about older history; `None` shows no line.
    pub history: Option<HistoryEdge>,
    /// The chat list, which titles a subagent a call names only by ID; `None` skips that lookup.
    pub chats: Option<&'a ChatList>,
    /// The files a click on their `attached` line saves; `None`, as in a subagent preview,
    /// offers no click.
    pub saveable: Option<&'a HashSet<uuid::Uuid>>,
}

pub fn build(
    app: &App,
    overrides: &BTreeMap<String, Density>,
    toggles: &HashSet<BlockId>,
    welcome: &Welcome,
    theme: &Theme,
    width: u16,
) -> View {
    // A click saves an attached file only while scuttle takes the mouse, and only a file the
    // chat still stores, so only those lines offer one.
    let saveable: HashSet<uuid::Uuid> = if app.mouse {
        scuttle_core::files::file_rows(app.chat.as_deref(), &app.transcript)
            .into_iter()
            .filter(|row| !row.expired)
            .map(|row| row.id)
            .collect()
    } else {
        HashSet::new()
    };
    let source = TranscriptSource {
        transcript: &app.transcript,
        prefs: &app.prefs,
        activity: app.activity(),
        history: app.history_edge(),
        chats: Some(&app.chats),
        saveable: Some(&saveable),
    };
    build_transcript(
        &source,
        overrides,
        toggles,
        welcome.show.then_some(welcome),
        theme,
        width,
    )
}

/// The rows an older page added above `old`'s first message, when `new` is `old` with
/// older messages loaded before it: how far that message, and the reader's place, moved down.
/// `None` when `new` holds nothing older than `old`'s first message, or no longer holds it.
pub fn prepended_rows(old: &View, new: &View) -> Option<usize> {
    let (&first, &before) = old.message_rows.first_key_value()?;
    let (&new_first, _) = new.message_rows.first_key_value()?;
    if new_first >= first {
        return None;
    }
    new.message_rows.get(&first)?.checked_sub(before)
}

/// A queued message's text parts on one line.
pub fn queued_text(queued: &types::CodersdkChatQueuedMessage) -> String {
    queued
        .content
        .iter()
        .filter_map(|p| p.text.as_deref())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The lines of a transcript at `width`, with `welcome` on a blank transcript.
pub fn build_transcript(
    source: &TranscriptSource,
    overrides: &BTreeMap<String, Density>,
    toggles: &HashSet<BlockId>,
    welcome: Option<&Welcome>,
    theme: &Theme,
    width: u16,
) -> View {
    let mut out = Out {
        view: View {
            nerd_icons: theme.icons == IconSet::Nerd,
            ..View::default()
        },
        width: width.max(2),
        theme,
        hidden: 0,
        links_numbered: 0,
    };
    let messages: Vec<_> = source.transcript.messages().collect();
    if messages.is_empty() && source.transcript.live.is_empty() {
        if let Some(welcome) = welcome {
            out.push(welcome_lines(welcome, theme, out.width), false);
            // Only text mode suggests the font the icons need, and only until it was shown.
            if theme.icons == IconSet::Text && welcome.tip {
                push_nerd_font_tip(&mut out);
            }
        }
        return out.view;
    }
    if let Some(edge) = source.history {
        let text = match edge {
            HistoryEdge::More => "PageUp loads older messages",
            HistoryEdge::Loading => "Loading older messages\u{2026}",
            HistoryEdge::Start => "Start of chat",
        };
        out.push(vec![Line::from(Span::styled(text, theme.dim))], false);
    }
    let (results, calls) = collect_tool_results(&messages, &source.transcript.live.blocks);
    let titles = Titles::collect(&messages, &source.transcript.live.blocks);
    let names = Names {
        titles: &titles,
        chats: source.chats,
    };
    let mut groups: Vec<(Option<i64>, Vec<Item>)> = messages
        .iter()
        .map(|m| (m.id, items_for_message(m, &results, &calls, &names)))
        .collect();
    let has_live = !source.transcript.live.is_empty();
    if has_live {
        groups.push((
            None,
            items_for_live(&source.transcript.live.blocks, &results, &calls, &names),
        ));
    }
    let answers = answer_starts(&groups);
    let latest_tools = latest_tools(&groups, source.transcript);
    let ctx = Ctx {
        prefs: source.prefs,
        overrides,
        toggles,
        answers: &answers,
        activity: source.activity.clone(),
        latest_tools: &latest_tools,
        saveable: source.saveable,
    };
    let count = groups.len();
    for (i, (owner, items)) in groups.into_iter().enumerate() {
        let live = has_live && i + 1 == count;
        let start = out.view.lines.len();
        let code = render_items(&mut out, owner, items, &ctx, live);
        if let Some(id) = owner {
            let blank = out.view.lines[start..]
                .iter()
                .take_while(|l| l.spans.is_empty())
                .count();
            out.view.message_rows.insert(id, start + blank);
        }
        if !code.is_empty() {
            out.view.last_code_blocks = code;
        }
    }
    for queued in &source.transcript.queued {
        // The indent, the label's slot, and a cell for the ellipsis: 12 cells for text
        // mode's `queued · `, 5 for a glyph's slot.
        let label = icons::slot(theme.icons, Icon::Queued).width;
        let text = one_line(
            &queued_text(queued),
            (width as usize)
                .saturating_sub(3 + usize::from(label))
                .max(8),
        );
        out.push(
            vec![Line::from(icons::line_with(
                theme,
                "  ",
                Icon::Queued,
                &text,
                theme.dim,
            ))],
            false,
        );
    }
    if let Some(err) = source.transcript.last_error.as_ref() {
        out.gap();
        out.push(
            vec![Line::from(icons::line_with(
                theme,
                "",
                Icon::Error,
                &format!("Error: {err}"),
                theme.error,
            ))],
            false,
        );
    }
    out.flush_hidden();
    out.view
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::Paragraph;
    use scuttle_core::config::BusyBehavior;
    use serde_json::json;
    use unicode_width::UnicodeWidthStr;

    fn app_with(messages: serde_json::Value) -> App {
        let mut app = App::new(BusyBehavior::Queue, true);
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap();
        app.update(scuttle_core::app::Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: serde_json::from_value(messages).unwrap(),
        });
        app
    }

    fn welcome() -> Welcome {
        Welcome {
            url: "https://dogfood.example".into(),
            user: "nick".into(),
            art: vec![],
            art_accent: true,
            show: true,
            tip: true,
        }
    }

    fn draw(view: &View, w: u16, h: u16) -> Terminal<TestBackend> {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| f.render_widget(Paragraph::new(view.lines.clone()), f.area()))
            .unwrap();
        t
    }

    fn texts(view: &View) -> Vec<String> {
        view.lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn the_wordmark_shows_with_its_credit_above_the_name() {
        let app = App::new(BusyBehavior::Queue, true);
        let art = crate::art::wordmark_lines();
        let welcome = Welcome {
            art: art.clone(),
            ..welcome()
        };
        let view = build(
            &app,
            &Default::default(),
            &Default::default(),
            &welcome,
            &Theme::terminal(true),
            80,
        );
        let shown = texts(&view);
        let first = shown
            .iter()
            .position(|l| *l == art[0])
            .unwrap_or_else(|| panic!("the wordmark is missing: {shown:#?}"));
        assert_eq!(&shown[first..first + art.len()], art.as_slice());
        assert_eq!(shown[first + art.len()], crate::art::CREDIT);
        assert!(
            shown[first + art.len()..].iter().any(|l| l == "scuttle"),
            "the name follows: {shown:#?}"
        );
    }

    #[test]
    fn plain_custom_art_draws_in_the_normal_style_and_the_wordmark_in_the_accent() {
        let app = App::new(BusyBehavior::Queue, true);
        let theme = Theme::terminal(true);
        let art_style = |art: Vec<String>| {
            let rows = art.len();
            let welcome = Welcome {
                art: art.clone(),
                art_accent: false,
                ..welcome()
            };
            let view = build(
                &app,
                &Default::default(),
                &Default::default(),
                &welcome,
                &theme,
                80,
            );
            let shown = texts(&view);
            let at = shown.iter().position(|l| *l == art[0]).unwrap();
            assert_eq!(
                shown[at + rows],
                crate::art::CREDIT,
                "the credit shows under any art: {shown:#?}"
            );
            view.lines[at].spans[0].style
        };
        assert_eq!(art_style(crate::art::wordmark_lines()), theme.brand);
        assert_eq!(
            art_style(vec!["~~ my art ~~".into(), "~~~~~~~~~~~~".into()]),
            Style::default(),
            "a user's own plain art draws in the normal style"
        );
    }

    #[test]
    fn a_users_art_draws_in_the_accent_unless_plain_or_no_color() {
        let app = App::new(BusyBehavior::Queue, true);
        let art: Vec<String> = vec!["~~ my art ~~".into(), "~~~~~~~~~~~~".into()];
        let style = |theme: &Theme, art: &[String], art_accent: bool| {
            let welcome = Welcome {
                art: art.to_vec(),
                art_accent,
                ..welcome()
            };
            let view = build(
                &app,
                &Default::default(),
                &Default::default(),
                &welcome,
                theme,
                80,
            );
            let at = texts(&view).iter().position(|l| *l == art[0]).unwrap();
            view.lines[at].spans[0].style
        };
        let theme = Theme::terminal(true);
        assert_ne!(theme.brand, Style::default());
        assert_eq!(
            style(&theme, &art, true),
            theme.brand,
            "custom art takes the accent by default"
        );
        assert_eq!(
            style(&theme, &art, false),
            Style::default(),
            "art_color = plain keeps the normal style"
        );
        let plain = Theme::terminal_with(true, crate::theme::Colors::None);
        assert_eq!(
            style(&plain, &art, true),
            Style::default(),
            "NO_COLOR draws custom art plain"
        );
        assert_eq!(
            style(&theme, &crate::art::wordmark_lines(), false),
            theme.brand,
            "the wordmark is the accent whatever art_color says"
        );
    }

    #[test]
    fn art_wider_than_the_pane_gives_way_to_the_name() {
        let app = App::new(BusyBehavior::Queue, true);
        let welcome = Welcome {
            art: crate::art::wordmark_lines(),
            ..welcome()
        };
        let view = build(
            &app,
            &Default::default(),
            &Default::default(),
            &welcome,
            &Theme::terminal(true),
            20,
        );
        let shown = texts(&view);
        assert!(!shown.iter().any(|l| l.contains("____")), "{shown:#?}");
        assert!(
            !shown.iter().any(|l| l.contains("Coder Technologies")),
            "{shown:#?}"
        );
        assert!(shown.iter().any(|l| l == "scuttle"), "{shown:#?}");
    }

    #[test]
    fn welcome_shows_on_a_blank_chat() {
        let app = App::new(BusyBehavior::Queue, true);
        let view = build(
            &app,
            &Default::default(),
            &Default::default(),
            &welcome(),
            &Theme::terminal(true),
            80,
        );
        insta::assert_snapshot!(draw(&view, 80, 12).backend());
    }

    #[test]
    fn narrow_terminal_welcome() {
        let app = App::new(BusyBehavior::Queue, true);
        let view = build(
            &app,
            &Default::default(),
            &Default::default(),
            &welcome(),
            &Theme::terminal(true),
            40,
        );
        assert!(
            view.lines
                .iter()
                .all(|l| l.spans.iter().map(|s| s.content.width()).sum::<usize>() <= 40)
        );
        insta::assert_snapshot!(draw(&view, 40, 14).backend());
    }

    #[test]
    fn conversation_renders_user_markdown_and_tool_summary() {
        let app = app_with(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "list files"}]},
            {"id": 2, "role": "assistant", "content": [
                {"type": "text", "text": "Here you go:\n\n```sh\nls -la\n```"},
                {"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args": {"command": "ls -la"}},
                {"type": "tool-result", "tool_call_id": "a", "tool_name": "execute", "result": {"output": "total 0\nfile.txt"}}
            ]}
        ]));
        let view = build(
            &app,
            &Default::default(),
            &Default::default(),
            &welcome(),
            &Theme::terminal(true),
            60,
        );
        insta::assert_snapshot!(draw(&view, 60, 14).backend());
        assert_eq!(view.last_code_blocks, vec!["ls -la\n".to_string()]);
        assert!(
            view.hits
                .iter()
                .any(|h| matches!(&h.target, HitTarget::CopyCode(c) if c == "ls -la\n"))
        );
        assert!(
            view.hits
                .iter()
                .any(|h| matches!(h.target, HitTarget::Toggle((Some(2), _))))
        );
    }

    /// The head lines of every tool call in `view`.
    fn tool_heads(view: &View) -> Vec<String> {
        texts(view)
            .into_iter()
            .filter(|l| ['⏺', '◌', '✗', '◼'].iter().any(|m| l.starts_with(*m)))
            .collect()
    }

    #[test]
    fn subagent_tools_read_like_the_web_ui() {
        let child = uuid::Uuid::new_v4();
        let app = app_with(json!([
            {"id": 1, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "s",
                "tool_name": "spawn_agent", "args": {"title": "Fix failing CI", "prompt": "go"}}]},
            {"id": 2, "role": "tool", "content": [{"type": "tool-result", "tool_call_id": "s",
                "tool_name": "spawn_agent", "result": {"chat_id": child, "title": "Fix failing CI", "status": "pending"}}]},
            {"id": 3, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "w",
                "tool_name": "wait_agent", "args": {"chat_id": child}}]},
            {"id": 4, "role": "tool", "content": [{"type": "tool-result", "tool_call_id": "w",
                "tool_name": "wait_agent", "result": {"chat_id": child, "status": "completed"}}]},
            {"id": 5, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "m",
                "tool_name": "message_agent", "args": {"chat_id": child, "message": "and the docs"}}]},
            {"id": 6, "role": "tool", "content": [{"type": "tool-result", "tool_call_id": "m",
                "tool_name": "message_agent", "is_error": true, "result": {"error": "busy"}}]}
        ]));
        let view = build(
            &app,
            &Default::default(),
            &Default::default(),
            &welcome(),
            &Theme::terminal(true),
            80,
        );
        assert_eq!(
            tool_heads(&view),
            [
                "⏺ Spawned 'Fix failing CI'",
                "⏺ Waited for 'Fix failing CI'",
                "✗ Failed to message 'Fix failing CI'",
            ]
        );
    }

    #[test]
    fn a_running_call_finds_its_title_in_the_chat_list_or_shows_the_short_id() {
        let listed = uuid::Uuid::new_v4();
        let unknown = uuid::Uuid::parse_str("1234abcd-0000-4000-8000-000000000000").unwrap();
        let mut app = app_with(json!([{"id": 1, "role": "assistant", "content": [
            {"type": "tool-call", "tool_call_id": "a", "tool_name": "wait_agent", "args": {"chat_id": listed}},
            {"type": "tool-call", "tool_call_id": "b", "tool_name": "interrupt_agent", "args": {"chat_id": unknown}},
            {"type": "tool-call", "tool_call_id": "c", "tool_name": "spawn_explore_agent", "args": {}}
        ]}]));
        app.update(scuttle_core::app::Msg::ChatsLoaded {
            query: scuttle_core::chat_list::ListQuery::Default,
            offset: 0,
            chats: serde_json::from_value(json!([{"id": listed, "title": "Explore the repo",
                "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}]))
            .unwrap(),
        });
        let view = build(
            &app,
            &Default::default(),
            &Default::default(),
            &welcome(),
            &Theme::terminal(true),
            80,
        );
        assert_eq!(
            tool_heads(&view),
            [
                "◌ Waiting for 'Explore the repo'",
                "◌ Interrupting '1234abcd'",
                "◌ Spawning a subagent",
            ]
        );
    }

    #[test]
    fn expanding_a_subagent_call_shows_its_raw_arguments() {
        let child = uuid::Uuid::new_v4();
        let app = app_with(json!([
            {"id": 1, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "w",
                "tool_name": "wait_agent", "args": {"chat_id": child}}]},
            {"id": 2, "role": "tool", "content": [{"type": "tool-result", "tool_call_id": "w",
                "tool_name": "wait_agent", "result": {"chat_id": child, "title": "Fix CI", "status": "completed"}}]}
        ]));
        let toggles: HashSet<BlockId> = [(Some(1), 0)].into();
        let view = build(
            &app,
            &Default::default(),
            &toggles,
            &welcome(),
            &Theme::terminal(true),
            100,
        );
        let shown = texts(&view);
        assert!(
            shown.iter().any(|l| l == "⏺ Waited for 'Fix CI'"),
            "{shown:#?}"
        );
        let raw = format!("  wait_agent({{\"chat_id\":\"{child}\"}})");
        assert!(shown.contains(&raw), "{shown:#?}");
    }

    #[test]
    fn a_wait_the_parent_interrupted_and_a_legacy_close_read_like_the_web_ui() {
        let child = uuid::Uuid::new_v4();
        let app = app_with(json!([
            {"id": 1, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "s",
                "tool_name": "spawn_subagent", "args": {"title": "Fix CI"}}]},
            {"id": 2, "role": "tool", "content": [{"type": "tool-result", "tool_call_id": "s",
                "tool_name": "spawn_subagent", "result": {"chat_id": child}}]},
            {"id": 3, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "w",
                "tool_name": "wait_agent", "args": {"chat_id": child}}]},
            {"id": 4, "role": "tool", "content": [{"type": "tool-result", "tool_call_id": "w",
                "tool_name": "wait_agent", "is_error": true,
                "result": {"error": "tool call was interrupted before it produced a result"}}]},
            {"id": 5, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "c",
                "tool_name": "close_agent", "args": {"chat_id": child}}]},
            {"id": 6, "role": "tool", "content": [{"type": "tool-result", "tool_call_id": "c",
                "tool_name": "close_agent", "result": {"chat_id": child, "interrupted": true}}]}
        ]));
        let view = build(
            &app,
            &Default::default(),
            &Default::default(),
            &welcome(),
            &Theme::terminal(true),
            80,
        );
        assert_eq!(
            tool_heads(&view),
            [
                "⏺ Spawned 'Fix CI'",
                "◼ Stopped waiting for 'Fix CI'",
                "⏺ Interrupted 'Fix CI'",
            ]
        );
    }

    #[test]
    fn a_stopped_wait_has_a_dim_marker_and_a_failed_wait_keeps_the_red_cross() {
        let child = uuid::Uuid::new_v4();
        let app = app_with(json!([
            {"id": 1, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "a", "tool_name": "wait_agent", "args": {"chat_id": child}},
                {"type": "tool-call", "tool_call_id": "b", "tool_name": "wait_agent", "args": {"chat_id": child}}
            ]},
            {"id": 2, "role": "tool", "content": [
                {"type": "tool-result", "tool_call_id": "a", "tool_name": "wait_agent", "is_error": true,
                    "result": {"error": "tool call was interrupted before it produced a result"}},
                {"type": "tool-result", "tool_call_id": "b", "tool_name": "wait_agent", "is_error": true,
                    "result": {"error": "chat not found"}}
            ]}
        ]));
        let theme = Theme::terminal(true);
        let view = build(
            &app,
            &Default::default(),
            &Default::default(),
            &welcome(),
            &theme,
            80,
        );
        let marker = |verb: &str| {
            let line = view
                .lines
                .iter()
                .find(|l| l.spans.get(1).is_some_and(|s| s.content == verb))
                .unwrap_or_else(|| panic!("no {verb} head in {:#?}", texts(&view)));
            (line.spans[0].content.to_string(), line.spans[0].style)
        };
        assert_eq!(marker("Stopped waiting for"), ("◼ ".to_owned(), theme.dim));
        assert_eq!(marker("Failed waiting for"), ("✗ ".to_owned(), theme.error));
    }

    #[test]
    fn summary_mode_bounds_huge_tool_output() {
        let huge: String = (0..10_000).map(|i| format!("line {i}\n")).collect();
        let app = app_with(json!([
            {"id": 1, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "a", "tool_name": "read_file", "args": {"path": "/big"}},
                {"type": "tool-result", "tool_call_id": "a", "tool_name": "read_file", "result": {"content": huge}}
            ]}
        ]));
        let view = build(
            &app,
            &Default::default(),
            &Default::default(),
            &welcome(),
            &Theme::terminal(true),
            40,
        );
        let tool_lines: Vec<_> = texts(&view)
            .into_iter()
            .filter(|l| !l.trim().is_empty())
            .collect();
        assert!(tool_lines.len() <= 2, "{tool_lines:?}");
        assert!(
            view.lines
                .iter()
                .all(|l| l.spans.iter().map(|s| s.content.width()).sum::<usize>() <= 40)
        );
    }

    #[test]
    fn queued_messages_show_dimmed_after_the_conversation() {
        let mut app = app_with(
            json!([{"id": 1, "role": "user", "content": [{"type": "text", "text": "first"}]}]),
        );
        app.transcript.queued = serde_json::from_value(
            json!([{"id": 5, "content": [{"type": "text", "text": "next question"}]}]),
        )
        .unwrap();
        let view = build(
            &app,
            &Default::default(),
            &Default::default(),
            &welcome(),
            &Theme::terminal(true),
            60,
        );
        let last = texts(&view)
            .into_iter()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap();
        assert!(
            last.contains("queued") && last.contains("next question"),
            "{last}"
        );
    }

    #[test]
    fn toggling_expands_and_hidden_tools_are_counted() {
        let app = app_with(json!([
            {"id": 1, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "a", "tool_name": "read_file", "args": {"path": "/x"}},
                {"type": "tool-result", "tool_call_id": "a", "tool_name": "read_file", "result": {"content": "one\ntwo\nthree"}}
            ]}
        ]));
        let collapsed = build(
            &app,
            &Default::default(),
            &Default::default(),
            &welcome(),
            &Theme::terminal(true),
            60,
        );
        let id = collapsed
            .hits
            .iter()
            .find_map(|h| match h.target {
                HitTarget::Toggle(id) => Some(id),
                _ => None,
            })
            .unwrap();
        let mut toggles = HashSet::new();
        toggles.insert(id);
        let expanded = build(
            &app,
            &Default::default(),
            &toggles,
            &welcome(),
            &Theme::terminal(true),
            60,
        );
        assert!(texts(&expanded).iter().any(|l| l.contains("three")));
        let mut overrides = BTreeMap::new();
        overrides.insert("read_file".to_string(), Density::Hidden);
        let hidden = build(
            &app,
            &overrides,
            &Default::default(),
            &welcome(),
            &Theme::terminal(true),
            60,
        );
        assert!(
            texts(&hidden)
                .iter()
                .any(|l| l.contains("1 hidden tool call"))
        );
    }

    #[test]
    fn results_in_a_separate_tool_message_complete_the_call() {
        // Coder stores a tool's result in a later message with role "tool", not alongside its
        // call (coderd/x/chatd/message_conversion.go; codersdk.ChatMessageRoleTool = "tool").
        let app = app_with(json!([
            {"id": 1, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args": {"command": "ls"}}
            ]},
            {"id": 2, "role": "tool", "content": [
                {"type": "tool-result", "tool_call_id": "a", "tool_name": "execute", "result": {"output": "done"}}
            ]}
        ]));
        let view = build(
            &app,
            &Default::default(),
            &Default::default(),
            &welcome(),
            &Theme::terminal(true),
            60,
        );
        let lines = texts(&view);
        assert!(
            lines
                .iter()
                .any(|l| l.contains('⏺') && l.contains("execute"))
        );
        assert!(lines.iter().any(|l| l.contains('⎿') && l.contains("done")));
        assert!(!lines.iter().any(|l| l.contains('◌')));
        // The role "tool" message adds no lines of its own: only the call's own summary shows.
        assert_eq!(lines.iter().filter(|l| !l.trim().is_empty()).count(), 2);
    }

    #[test]
    fn live_result_for_a_durable_call_is_shown() {
        let mut app = app_with(json!([
            {"id": 1, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args": {"command": "ls"}}
            ]}
        ]));
        // The result streamed in live and hasn't become a durable "tool" message yet.
        app.transcript.live.blocks.push(LiveBlock::ToolResult {
            id: "a".into(),
            name: "execute".into(),
            result_raw: String::new(),
            result: Some(json!({"output": "done"})),
            reasoning: String::new(),
            is_error: false,
            done: true,
        });
        let view = build(
            &app,
            &Default::default(),
            &Default::default(),
            &welcome(),
            &Theme::terminal(true),
            60,
        );
        let lines = texts(&view);
        assert!(
            lines
                .iter()
                .any(|l| l.contains('⏺') && l.contains("execute"))
        );
        assert!(lines.iter().any(|l| l.contains('⎿') && l.contains("done")));
        assert!(!lines.iter().any(|l| l.contains('◌')));
    }

    #[test]
    fn code_block_hits_cover_exactly_the_code_lines() {
        let text = "This paragraph is long enough that it definitely wraps across several lines once rendered at a width of only twenty columns, well before the fenced code block that follows it.\n\n```txt\nfoo\nbar\n```";
        let app = app_with(json!([
            {"id": 1, "role": "assistant", "content": [{"type": "text", "text": text}]}
        ]));
        let view = build(
            &app,
            &Default::default(),
            &Default::default(),
            &welcome(),
            &Theme::terminal(true),
            20,
        );
        let hit = view
            .hits
            .iter()
            .find(|h| matches!(h.target, HitTarget::CopyCode(_)))
            .unwrap();
        let lines = texts(&view);
        let code_lines = &lines[hit.lines.clone()];
        assert_eq!(code_lines, &["foo".to_string(), "bar".to_string()]);
    }

    #[test]
    fn control_characters_never_reach_tool_results_reasoning_or_subagent_titles() {
        assert_eq!(
            result_text(Some(&json!({"output": "ding\x07 \x1bdone"})), ""),
            "ding done"
        );
        let child = uuid::Uuid::new_v4();
        let app = app_with(json!([
            {"id": 1, "role": "assistant", "content": [
                {"type": "reasoning", "text": "think\x07ing \x1bhard"},
                {"type": "tool-call", "tool_call_id": "s", "tool_name": "spawn_agent",
                    "args": {"title": "Fix\x1b CI\x07", "prompt": "go"}}]},
            {"id": 2, "role": "tool", "content": [{"type": "tool-result", "tool_call_id": "s",
                "tool_name": "spawn_agent", "result": {"chat_id": child, "status": "pending"}}]}
        ]));
        let mut app = app;
        app.prefs.thinking = "always_expanded".into();
        let shown = texts(&build_at(&app, 80));
        assert!(
            shown.iter().any(|l| l.contains("thinking hard")),
            "{shown:#?}"
        );
        for line in &shown {
            assert!(
                !line.chars().any(|c| c.is_control()),
                "a control character is left in {line:?}"
            );
        }
        assert!(
            shown.iter().any(|l| l.contains("Spawned 'Fix CI'")),
            "{shown:#?}"
        );
    }

    #[test]
    fn a_lone_cr_in_tool_output_keeps_only_what_follows_it_on_the_line() {
        assert_eq!(
            result_text(
                Some(
                    &json!({"output": "Downloading 10%\rDownloading 50%\rDownloading 100%\ndone"})
                ),
                ""
            ),
            "Downloading 100%\ndone"
        );
        assert_eq!(result_text(Some(&json!("10%\r50%\r100%")), ""), "100%");
    }

    #[test]
    fn a_crlf_in_tool_output_stays_one_line_break() {
        assert_eq!(
            result_text(Some(&json!({"output": "one\r\ntwo\r\n\r\nthree"})), ""),
            "one\ntwo\n\nthree"
        );
    }

    #[test]
    fn result_text_strips_ansi_csi_sequences() {
        let colored = "\u{1b}[32mdone\u{1b}[0m";
        assert_eq!(result_text(Some(&json!({"output": colored})), ""), "done");
    }
    #[test]
    fn a_wrapped_link_is_clickable_on_every_row() {
        let text = "Read [the very long documentation title here](https://coder.com/docs) please";
        let app = app_with(json!([
            {"id": 1, "role": "assistant", "content": [{"type": "text", "text": text}]}
        ]));
        let view = build_at(&app, 20);
        let lines = texts(&view);
        assert_eq!(view.links.len(), 3, "{lines:?} {:?}", view.links);
        assert!(
            view.links.windows(2).all(|w| w[1].line == w[0].line + 1),
            "one part per row, on consecutive rows"
        );
        assert!(view.links.iter().all(|l| l.url == "https://coder.com/docs"));
        let covered: String = view
            .links
            .iter()
            .map(|l| {
                lines[l.line]
                    .chars()
                    .skip(l.cols.start as usize)
                    .take((l.cols.end - l.cols.start) as usize)
                    .collect::<String>()
            })
            .collect();
        assert_eq!(covered, "the very long documentation title here");
    }

    #[test]
    fn links_in_separate_messages_have_separate_ids() {
        let app = app_with(json!([
            {"id": 1, "role": "assistant", "content": [{"type": "text",
                "text": "[the very long documentation title here](https://coder.com/docs) and [b](https://b.example)"}]},
            {"id": 2, "role": "assistant", "content": [{"type": "text",
                "text": "[the very long documentation title here](https://coder.com/docs)"}]}
        ]));
        let view = build_at(&app, 20);
        let ids: Vec<(String, usize)> =
            view.links.iter().map(|l| (l.url.clone(), l.link)).collect();
        let docs = "https://coder.com/docs".to_string();
        let b = "https://b.example".to_string();
        assert_eq!(
            ids,
            [
                (docs.clone(), 0),
                (docs.clone(), 0),
                (docs.clone(), 0),
                (b, 1),
                (docs.clone(), 2),
                (docs.clone(), 2),
                (docs, 2),
            ],
            "{:?}",
            texts(&view)
        );
    }

    fn build_at(app: &App, width: u16) -> View {
        build(
            app,
            &Default::default(),
            &Default::default(),
            &welcome(),
            &Theme::terminal(true),
            width,
        )
    }

    fn split_reply(parts: serde_json::Value) -> App {
        app_with(json!([
            {"id": 1, "role": "assistant", "content": parts}
        ]))
    }

    fn live_app(parts: &[serde_json::Value]) -> App {
        let mut app = App::new(BusyBehavior::Queue, true);
        for (i, part) in parts.iter().enumerate() {
            let mp = serde_json::from_value(json!({"seq": i + 1, "part": part})).unwrap();
            app.transcript.live.apply(&mp);
        }
        app
    }

    fn shown(app: &App) -> Vec<String> {
        texts(&build_at(app, 100))
            .into_iter()
            .map(|l| l.trim_end().to_owned())
            .filter(|l| !l.is_empty())
            .collect()
    }

    fn text_part(t: &str) -> serde_json::Value {
        json!({"type": "text", "text": t})
    }

    #[test]
    fn consecutive_text_parts_render_as_one_bullet() {
        let app = split_reply(json!([
            text_part("- You can "),
            text_part("create a Linear issue"),
            text_part(", but someone has to open each response."),
        ]));
        let lines = texts(&build_at(&app, 100));
        let at = lines
            .iter()
            .position(|l| l.contains("create a Linear issue"))
            .unwrap_or_else(|| panic!("the reply is missing: {lines:#?}"));
        assert!(
            lines[at]
                .contains("You can create a Linear issue, but someone has to open each response."),
            "one bullet line: {lines:#?}"
        );
        assert!(lines[at].contains('•'), "a bullet: {lines:#?}");
        assert!(lines[at + 1..].iter().all(|l| !l.contains("someone")));
    }

    #[test]
    fn a_whitespace_only_text_part_is_skipped() {
        let split = split_reply(json!([
            text_part("one"),
            text_part("  \n"),
            text_part(" two")
        ]));
        let whole = split_reply(json!([text_part("one two")]));
        assert_eq!(shown(&split), shown(&whole));
    }

    #[test]
    fn a_tool_call_between_text_parts_still_splits_them() {
        let app = split_reply(json!([
            text_part("before"),
            {"type": "tool-call", "tool_call_id": "c1", "tool_name": "execute", "args": {"command": "ls"}},
            text_part("after"),
        ]));
        let lines = shown(&app);
        let before = lines.iter().position(|l| l.contains("before")).unwrap();
        let after = lines.iter().position(|l| l.contains("after")).unwrap();
        assert!(after > before + 1, "the tool sits between: {lines:#?}");
        assert!(!lines.iter().any(|l| l.contains("beforeafter")));
    }

    #[test]
    fn consecutive_reasoning_parts_render_as_one_block() {
        let app = split_reply(json!([
            {"type": "reasoning", "text": "think"},
            {"type": "reasoning", "text": "ing hard"},
        ]));
        let lines = texts(&build_at(&app, 100));
        let thinking = lines.iter().filter(|l| l.contains("∴ Thinking")).count();
        assert_eq!(thinking, 1, "one reasoning block: {lines:#?}");
    }

    #[test]
    fn consecutive_user_text_parts_render_as_one_line() {
        let app = app_with(json!([
            {"id": 1, "role": "user", "content": [text_part("hello "), text_part("world")]}
        ]));
        let lines = texts(&build_at(&app, 100));
        assert!(
            lines.iter().any(|l| l.contains("› hello world")),
            "one user line: {lines:#?}"
        );
    }

    #[test]
    fn a_streamed_reply_renders_like_the_saved_one() {
        let parts = [
            text_part("- You can "),
            text_part("create a Linear issue"),
            text_part(", but someone has to open each response."),
        ];
        let saved = split_reply(json!(parts));
        let live = live_app(&parts);
        assert_eq!(shown(&live), shown(&saved));
    }

    #[test]
    fn copying_a_merged_reply_gives_what_is_displayed() {
        use crate::selection::{Pos, Selection, selected_text};
        let app = split_reply(json!([
            text_part("- You can "),
            text_part("create a Linear issue"),
            text_part(", but someone."),
        ]));
        let view = build_at(&app, 60);
        let all = Selection {
            anchor: Pos { line: 0, col: 0 },
            head: Pos {
                line: view.lines.len() - 1,
                col: 59,
            },
        };
        let copied = selected_text(&view, &all);
        assert!(
            copied.contains("• You can create a Linear issue, but someone."),
            "{copied:?}"
        );
    }

    #[test]
    fn a_source_between_text_parts_splits_them_like_the_live_stream() {
        let parts = [
            text_part("before"),
            json!({"type": "source", "url": "https://example.com", "title": "Example"}),
            text_part("after"),
        ];
        let saved = split_reply(json!(parts));
        let live = live_app(&parts);
        assert_eq!(shown(&saved), shown(&live));
        assert!(!shown(&saved).iter().any(|l| l.contains("beforeafter")));
    }

    #[test]
    fn a_provider_executed_call_between_text_parts_does_not_split_them() {
        let parts = [
            text_part("before "),
            json!({"type": "tool-call", "tool_call_id": "p1", "tool_name": "web_search", "provider_executed": true}),
            text_part("after"),
        ];
        let saved = split_reply(json!(parts));
        let live = live_app(&parts);
        assert_eq!(shown(&saved), vec!["before after".to_owned()]);
        assert_eq!(shown(&live), shown(&saved));
    }

    fn rule_rows(lines: &[String]) -> Vec<usize> {
        lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.starts_with('─'))
            .map(|(i, _)| i)
            .collect()
    }

    #[test]
    fn a_rule_separates_the_work_from_the_answer() {
        let app = app_with(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "fix it"}]},
            {"id": 2, "role": "assistant", "content": [
                {"type": "reasoning", "text": "think"},
                {"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args": {"command": "ls"}}
            ]},
            {"id": 3, "role": "tool", "content": [
                {"type": "tool-result", "tool_call_id": "a", "tool_name": "execute", "result": {"output": "ok"}}
            ]},
            {"id": 4, "role": "assistant", "content": [{"type": "text", "text": "All fixed."}]},
            {"id": 5, "role": "user", "content": [{"type": "text", "text": "thanks"}]},
            {"id": 6, "role": "assistant", "content": [{"type": "text", "text": "Any time."}]}
        ]));
        let view = build_at(&app, 40);
        let lines = texts(&view);
        let rules: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.starts_with('─'))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            rules.len(),
            1,
            "a turn without work gets no rule: {lines:?}"
        );
        let rule = rules[0];
        assert_eq!(lines[rule].chars().count(), 40, "the rule spans the width");
        let tool = lines.iter().position(|l| l.contains("execute")).unwrap();
        let answer = lines.iter().position(|l| l.contains("All fixed.")).unwrap();
        assert!(tool < rule && rule < answer, "{lines:?}");
        let marked: Vec<usize> = (0..view.meta.len())
            .filter(|&i| view.meta[i].rule)
            .collect();
        assert_eq!(marked, vec![rule], "only the rule row is marked");
    }

    #[test]
    fn a_turn_without_work_gets_no_rule() {
        let app = app_with(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "hi"}]},
            {"id": 2, "role": "assistant", "content": [{"type": "text", "text": "Hello."}]}
        ]));
        let lines = texts(&build_at(&app, 40));
        assert!(rule_rows(&lines).is_empty(), "{lines:?}");
    }

    #[test]
    fn two_text_blocks_after_work_get_one_rule_above_the_first() {
        let app = app_with(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "go"}]},
            {"id": 2, "role": "assistant", "content": [
                {"type": "text", "text": "Let me look."},
                {"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args": {"command": "ls"}},
                {"type": "tool-result", "tool_call_id": "a", "tool_name": "execute", "result": {"output": "ok"}}
            ]},
            {"id": 3, "role": "assistant", "content": [
                {"type": "text", "text": "First part."}
            ]},
            {"id": 4, "role": "assistant", "content": [
                {"type": "text", "text": "Second part."}
            ]}
        ]));
        let lines = texts(&build_at(&app, 40));
        let rules = rule_rows(&lines);
        assert_eq!(rules.len(), 1, "{lines:?}");
        let look = lines
            .iter()
            .position(|l| l.contains("Let me look."))
            .unwrap();
        let first = lines
            .iter()
            .position(|l| l.contains("First part."))
            .unwrap();
        let second = lines
            .iter()
            .position(|l| l.contains("Second part."))
            .unwrap();
        assert!(
            look < rules[0],
            "text before the work gets no rule: {lines:?}"
        );
        assert!(rules[0] < first && first < second, "{lines:?}");
    }

    #[test]
    fn only_the_text_after_the_last_work_gets_the_rule() {
        let app = app_with(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "go"}]},
            {"id": 2, "role": "assistant", "content": [
                {"type": "reasoning", "text": "think"},
                {"type": "text", "text": "Checking."},
                {"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args": {"command": "ls"}},
                {"type": "tool-result", "tool_call_id": "a", "tool_name": "execute", "result": {"output": "ok"}},
                {"type": "text", "text": "Done."}
            ]}
        ]));
        let lines = texts(&build_at(&app, 40));
        let rules = rule_rows(&lines);
        assert_eq!(rules.len(), 1, "{lines:?}");
        let tool = lines.iter().position(|l| l.contains("execute")).unwrap();
        let done = lines.iter().position(|l| l.contains("Done.")).unwrap();
        assert!(tool < rules[0] && rules[0] < done, "{lines:?}");
    }

    #[test]
    fn a_live_turn_gets_no_rule_before_its_answer_starts() {
        let mut app = app_with(
            json!([{"id": 1, "role": "user", "content": [{"type": "text", "text": "go"}]}]),
        );
        app.transcript
            .live
            .blocks
            .push(LiveBlock::Reasoning("hmm".into()));
        app.transcript.live.blocks.push(LiveBlock::ToolCall {
            id: "a".into(),
            name: "execute".into(),
            args_raw: String::new(),
            args: Some(json!({"command": "ls"})),
        });
        let lines = texts(&build_at(&app, 40));
        assert!(rule_rows(&lines).is_empty(), "{lines:?}");
    }

    #[test]
    fn a_streaming_answer_after_a_tool_gets_the_rule() {
        let mut app = app_with(
            json!([{"id": 1, "role": "user", "content": [{"type": "text", "text": "go"}]}]),
        );
        app.transcript.live.blocks.push(LiveBlock::ToolCall {
            id: "a".into(),
            name: "execute".into(),
            args_raw: String::new(),
            args: Some(json!({"command": "ls"})),
        });
        app.transcript
            .live
            .blocks
            .push(LiveBlock::Text("Here is the answer".into()));
        let lines = texts(&build_at(&app, 40));
        let rule = lines
            .iter()
            .position(|l| l.starts_with('─'))
            .expect("a rule");
        let answer = lines
            .iter()
            .position(|l| l.contains("Here is the answer"))
            .unwrap();
        assert!(rule < answer);
    }

    #[test]
    fn user_rows_are_marked_and_every_row_has_metadata() {
        let long = "please look at this long request that wraps across more than one row";
        let app = app_with(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": long}]},
            {"id": 2, "role": "assistant", "content": [{"type": "text", "text": "Sure, looking now."}]}
        ]));
        let view = build_at(&app, 30);
        assert_eq!(view.lines.len(), view.meta.len());
        let lines = texts(&view);
        let user_rows: Vec<usize> = view
            .meta
            .iter()
            .enumerate()
            .filter(|(_, m)| m.user)
            .map(|(i, _)| i)
            .collect();
        assert!(user_rows.len() >= 2, "{lines:?}");
        assert!(lines[user_rows[0]].starts_with("› "));
        assert!(!view.meta[user_rows[0]].continuation);
        assert!(view.meta[user_rows[1]].continuation);
        let answer = lines.iter().position(|l| l.contains("Sure")).unwrap();
        assert!(!view.meta[answer].user);
    }

    #[test]
    fn every_fixture_keeps_lines_and_metadata_in_step() {
        let huge: String = (0..50).map(|i| format!("line {i}\n")).collect();
        let app = app_with(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "a\nb"}]},
            {"id": 2, "role": "assistant", "content": [
                {"type": "text", "text": "text\n\n```sh\nls\n```"},
                {"type": "tool-call", "tool_call_id": "a", "tool_name": "read_file", "args": {"path": "/x"}},
                {"type": "tool-result", "tool_call_id": "a", "tool_name": "read_file", "result": {"content": huge}}
            ]}
        ]));
        for width in [12u16, 40, 120] {
            let view = build_at(&app, width);
            assert_eq!(view.lines.len(), view.meta.len(), "width {width}");
        }
    }

    #[test]
    fn arguments_that_say_nothing_have_no_summary() {
        assert_eq!(args_summary(&json!({})), None);
        assert_eq!(args_summary(&json!({"parameters": {}})), None);
        assert_eq!(args_summary(&json!({"path": "", "tags": []})), None);
        assert_eq!(args_summary(&serde_json::Value::Null), None);
        assert_eq!(
            args_summary(&json!({"command": "ls -la"})).as_deref(),
            Some("ls -la")
        );
        assert_eq!(
            args_summary(&json!({"count": 5})).as_deref(),
            Some(r#"{"count":5}"#)
        );
        assert_eq!(
            result_summary(Some(&json!({"started": true, "workspace_name": "dev"}))).as_deref(),
            Some("dev")
        );
        assert_eq!(
            result_summary(Some(&json!(r#"{"workspace_name":"dev"}"#))).as_deref(),
            Some("dev"),
            "a result stored as a JSON string"
        );
        assert_eq!(result_summary(Some(&json!({"output": "ok"}))), None);
        assert_eq!(result_summary(None), None);
    }

    #[test]
    fn result_summary_ignores_malformed_results() {
        assert_eq!(result_summary(Some(&json!("not json"))), None);
        assert_eq!(result_summary(Some(&json!(42))), None);
        assert_eq!(result_summary(Some(&json!([1, 2]))), None);
        assert_eq!(
            result_summary(Some(&json!("[1,2]"))),
            None,
            "a JSON string holding a non-object"
        );
    }

    #[test]
    fn a_quota_title_keeps_to_one_line() {
        let app = app_with(json!([
            {"id": 1, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "a", "tool_name": "start_workspace", "args": {}}
            ]},
            {"id": 2, "role": "tool", "content": [
                {"type": "tool-result", "tool_call_id": "a", "tool_name": "start_workspace",
                    "result": {"error_code": "INSUFFICIENT_QUOTA", "error": "insufficient quota",
                        "title": "Quota full\nDelete a workspace first"}}
            ]}
        ]));
        let view = build_at(&app, 80);
        assert_eq!(tool_heads(&view), ["✗ Quota full…"]);
        assert!(
            view.lines
                .iter()
                .all(|l| l.spans.iter().all(|s| !s.content.contains('\n'))),
            "no span holds a raw newline"
        );
    }

    #[test]
    fn workspace_start_and_stop_read_like_the_web_ui() {
        let build_id = "0c6f6f7e-1111-4222-8333-444455556666";
        let app = app_with(json!([
            {"id": 1, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "a", "tool_name": "start_workspace", "args": {}},
                {"type": "tool-call", "tool_call_id": "b", "tool_name": "stop_workspace", "args": {}},
                {"type": "tool-call", "tool_call_id": "c", "tool_name": "start_workspace", "args": {"parameters": {}}},
                {"type": "tool-call", "tool_call_id": "d", "tool_name": "stop_workspace", "args": {}},
                {"type": "tool-call", "tool_call_id": "e", "tool_name": "start_workspace", "args": {}},
                {"type": "tool-call", "tool_call_id": "f", "tool_name": "start_workspace", "args": {}},
                {"type": "tool-call", "tool_call_id": "g", "tool_name": "start_workspace", "args": {}},
                {"type": "tool-call", "tool_call_id": "h", "tool_name": "start_workspace", "args": {}},
                {"type": "tool-call", "tool_call_id": "i", "tool_name": "stop_workspace", "args": {}}
            ]},
            {"id": 2, "role": "tool", "content": [
                {"type": "tool-result", "tool_call_id": "a", "tool_name": "start_workspace",
                    "result": {"started": true, "workspace_name": "dev", "build_id": build_id}},
                {"type": "tool-result", "tool_call_id": "b", "tool_name": "stop_workspace",
                    "result": {"stopped": true, "workspace_name": "dev", "no_build": true}},
                {"type": "tool-result", "tool_call_id": "c", "tool_name": "start_workspace",
                    "result": {"error": "waiting for in-progress build: build failed", "build_id": build_id}},
                {"type": "tool-result", "tool_call_id": "d", "tool_name": "stop_workspace", "is_error": true,
                    "result": "chat has no workspace; use create_workspace first"},
                {"type": "tool-result", "tool_call_id": "e", "tool_name": "start_workspace",
                    "result": r#"{"workspace_name":"dev","error":"boom"}"#},
                {"type": "tool-result", "tool_call_id": "f", "tool_name": "start_workspace",
                    "result": {"error_code": "INSUFFICIENT_QUOTA", "error": "insufficient quota",
                        "title": "Quota full", "message": "Delete a workspace.", "build_id": build_id}},
                {"type": "tool-result", "tool_call_id": "g", "tool_name": "start_workspace",
                    "result": {"error_code": "INSUFFICIENT_QUOTA", "error": "insufficient quota"}}
            ]}
        ]));
        let view = build_at(&app, 80);
        assert_eq!(
            tool_heads(&view),
            [
                "⏺ Started dev",
                "⏺ Stopped dev",
                "✗ Failed to start workspace",
                "✗ Failed to stop workspace",
                "✗ Failed to start dev",
                "✗ Quota full",
                "✗ Workspace quota reached",
                "◌ Starting workspace…",
                "◌ Stopping workspace…",
            ]
        );
        let lines = texts(&view);
        assert!(
            !lines.iter().any(|l| l.contains("_workspace(")),
            "no row shows the raw call: {lines:?}"
        );
    }

    #[test]
    fn a_live_workspace_start_reads_starting_then_started() {
        let mut app = app_with(json!([]));
        app.transcript.live.blocks.push(LiveBlock::ToolCall {
            id: "a".into(),
            name: "start_workspace".into(),
            args_raw: "{}".into(),
            args: Some(json!({})),
        });
        assert_eq!(tool_heads(&build_at(&app, 60)), ["◌ Starting workspace…"]);
        app.transcript.live.blocks.push(LiveBlock::ToolResult {
            id: "a".into(),
            name: "start_workspace".into(),
            result_raw: String::new(),
            result: Some(json!({"started": true, "workspace_name": "dev"})),
            reasoning: String::new(),
            is_error: false,
            done: true,
        });
        app.transcript.live.blocks.push(LiveBlock::ToolResult {
            id: "z".into(),
            name: "stop_workspace".into(),
            result_raw: String::new(),
            result: Some(json!({"stopped": true, "workspace_name": "dev"})),
            reasoning: String::new(),
            is_error: false,
            done: true,
        });
        assert_eq!(
            tool_heads(&build_at(&app, 60)),
            ["⏺ Started dev", "⏺ Stopped dev"],
            "a live result whose call never showed up is labeled too"
        );
    }

    #[test]
    fn a_workspace_result_whose_call_is_not_loaded_still_reads_like_the_web_ui() {
        let app = app_with(json!([
            {"id": 5, "role": "tool", "content": [
                {"type": "tool-result", "tool_call_id": "c9", "tool_name": "stop_workspace",
                    "result": {"stopped": true, "workspace_name": "dev"}},
                {"type": "tool-result", "tool_call_id": "c10", "tool_name": "start_workspace",
                    "is_error": true, "result": "workspace was deleted; use create_workspace to make a new one"}
            ]}
        ]));
        assert_eq!(
            tool_heads(&build_at(&app, 60)),
            ["⏺ Stopped dev", "✗ Failed to start workspace"]
        );
    }

    #[test]
    fn expanding_a_workspace_start_shows_the_parameters_the_head_leaves_out() {
        let app = app_with(json!([
            {"id": 1, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "a", "tool_name": "start_workspace",
                    "args": {"parameters": {"region": "us-east"}}},
                {"type": "tool-call", "tool_call_id": "b", "tool_name": "stop_workspace", "args": {}}
            ]},
            {"id": 2, "role": "tool", "content": [
                {"type": "tool-result", "tool_call_id": "a", "tool_name": "start_workspace",
                    "result": {"started": true, "workspace_name": "dev"}},
                {"type": "tool-result", "tool_call_id": "b", "tool_name": "stop_workspace",
                    "result": {"stopped": true, "workspace_name": "dev"}}
            ]}
        ]));
        let collapsed = texts(&build_at(&app, 100));
        assert!(
            collapsed.iter().any(|l| l == "⏺ Started dev"),
            "{collapsed:#?}"
        );
        assert!(
            !collapsed.iter().any(|l| l.contains("us-east")),
            "the head leaves the parameters out: {collapsed:#?}"
        );
        let toggles: HashSet<BlockId> = [(Some(1), 0), (Some(1), 1)].into();
        let shown = texts(&build(
            &app,
            &Default::default(),
            &toggles,
            &welcome(),
            &Theme::terminal(true),
            100,
        ));
        assert!(shown.iter().any(|l| l == "⏺ Started dev"), "{shown:#?}");
        assert!(
            shown.contains(&r#"  start_workspace({"parameters":{"region":"us-east"}})"#.to_owned()),
            "{shown:#?}"
        );
        assert!(
            shown.contains(&"  stop_workspace()".to_owned()),
            "a call without arguments shows empty parentheses only when expanded: {shown:#?}"
        );
    }

    #[test]
    fn a_workspace_lifecycle_row_keeps_the_kind_slot_in_both_icon_modes() {
        let app = app_with(json!([{"id": 1, "role": "assistant", "content": [
            {"type": "tool-call", "tool_call_id": "a", "tool_name": "stop_workspace", "args": {}}
        ]}]));
        let heads = |theme: &Theme| {
            tool_heads(&build(
                &app,
                &Default::default(),
                &Default::default(),
                &welcome(),
                theme,
                80,
            ))
        };
        assert_eq!(
            heads(&Theme::terminal(true)),
            ["◌ Stopping workspace…"],
            "text mode adds no glyph"
        );
        let kind = icons::slot(IconSet::Nerd, icons::tool_icon("stop_workspace")).text;
        assert_eq!(heads(&nerd()), [format!("◌ {kind}Stopping workspace…")]);
    }

    #[test]
    fn only_the_block_in_progress_is_marked_to_animate() {
        let mut app = app_with(json!([
            {"id": 1, "role": "assistant", "content": [
                {"type": "reasoning", "text": "old thought"},
                {"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args": {"command": "ls"}},
                {"type": "tool-result", "tool_call_id": "a", "tool_name": "execute", "result": {"output": "ok"}}
            ]}
        ]));
        app.transcript.status = Some(coder_sdk::ChatStatus::Running);
        app.transcript
            .live
            .blocks
            .push(LiveBlock::Reasoning("hmm".into()));
        let view = build_at(&app, 40);
        let lines = texts(&view);
        let thinking: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.starts_with("∴ Thinking"))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(thinking.len(), 2, "{lines:?}");
        assert_eq!(
            view.spinners,
            vec![thinking[1]],
            "only the live thought animates"
        );

        app.transcript.live.blocks.push(LiveBlock::ToolCall {
            id: "b".into(),
            name: "execute".into(),
            args_raw: String::new(),
            args: Some(json!({"command": "make"})),
        });
        let view = build_at(&app, 40);
        let running = texts(&view)
            .iter()
            .position(|l| l.contains("execute(make)"))
            .unwrap();
        assert_eq!(
            view.spinners,
            vec![running],
            "the thought is over once the tool starts"
        );

        app.transcript.status = Some(coder_sdk::ChatStatus::Waiting);
        assert!(
            build_at(&app, 40).spinners.is_empty(),
            "nothing animates once the agent stops"
        );
    }

    #[test]
    fn a_saved_call_whose_result_still_streams_animates() {
        let mut app = app_with(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "build it"}]},
            {"id": 2, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args": {"command": "make"}}
            ]}
        ]));
        app.transcript.status = Some(coder_sdk::ChatStatus::Running);
        app.transcript.live.blocks.push(LiveBlock::ToolResult {
            id: "a".into(),
            name: "execute".into(),
            result_raw: "compiling".into(),
            result: None,
            reasoning: String::new(),
            is_error: false,
            done: false,
        });
        let view = build_at(&app, 40);
        let head = texts(&view)
            .iter()
            .position(|l| l.contains("execute(make)"))
            .unwrap();
        assert_eq!(view.spinners, vec![head], "{:?}", texts(&view));
    }

    #[test]
    fn a_call_left_without_a_result_in_an_earlier_turn_stays_still() {
        let mut app = app_with(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "build it"}]},
            {"id": 2, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args": {"command": "make"}}
            ]},
            {"id": 3, "role": "user", "content": [{"type": "text", "text": "try again"}]},
            {"id": 4, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "b", "tool_name": "execute", "args": {"command": "make test"}}
            ]}
        ]));
        app.transcript.status = Some(coder_sdk::ChatStatus::Running);
        let view = build_at(&app, 40);
        let lines = texts(&view);
        let current = lines
            .iter()
            .position(|l| l.contains("execute(make test)"))
            .unwrap();
        assert_eq!(
            view.spinners,
            vec![current],
            "only the call of the latest turn animates: {lines:?}"
        );
    }

    #[test]
    fn a_result_whose_call_was_not_loaded_still_shows() {
        let app = app_with(json!([
            {"id": 5, "role": "tool", "content": [{"type": "tool-result", "tool_call_id": "c9",
                "tool_name": "execute", "result": {"output": "done"}}]}
        ]));
        let text: String = build_at(&app, 60)
            .lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("execute"), "{text}");
    }

    #[test]
    fn each_message_records_the_row_it_starts_on() {
        let app = app_with(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "one"}]},
            {"id": 2, "role": "assistant", "content": [{"type": "text", "text": "two"}]}
        ]));
        let view = build_at(&app, 60);
        let second = view.message_rows[&2];
        assert_eq!(view.message_rows[&1], 0);
        assert!(
            view.lines[second..]
                .iter()
                .any(|l| l.to_string().contains("two"))
        );
        assert!(
            !view.lines[..second]
                .iter()
                .any(|l| l.to_string().contains("two"))
        );
    }

    #[test]
    fn rows_an_older_page_added_are_counted_from_the_old_first_message() {
        let old = app_with(json!([
            {"id": 5, "role": "user", "content": [{"type": "text", "text": "five"}]}
        ]));
        let mut new = app_with(json!([
            {"id": 5, "role": "user", "content": [{"type": "text", "text": "five"}]}
        ]));
        new.transcript.prepend(
            serde_json::from_value(json!([
                {"id": 4, "role": "assistant", "content": [{"type": "text", "text": "four"}]}
            ]))
            .unwrap(),
        );
        let (old, new) = (build_at(&old, 60), build_at(&new, 60));
        assert_eq!(prepended_rows(&old, &old), None, "nothing was added");
        let rows = prepended_rows(&old, &new).unwrap();
        assert_eq!(rows, new.message_rows[&5]);
        assert_eq!(new.lines[rows..], old.lines[..]);
    }

    #[test]
    fn each_durable_message_records_its_first_line() {
        let app = app_with(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "first"}]},
            {"id": 2, "role": "assistant", "content": [{"type": "text", "text": "second"}]}
        ]));
        let view = build_at(&app, 60);
        let one = view.message_rows[&1];
        let two = view.message_rows[&2];
        assert!(one < two);
        assert!(
            view.lines[two..]
                .iter()
                .any(|l| l.to_string().contains("second"))
        );
    }

    fn row_of(view: &View, text: &str) -> usize {
        view.lines
            .iter()
            .position(|l| l.to_string().contains(text))
            .unwrap_or_else(|| panic!("no row shows {text:?}"))
    }

    #[test]
    fn an_anchor_finds_its_row_after_a_rule_appears_inside_its_message() {
        let answer = json!({"id": 2, "role": "assistant", "content": [
            {"type": "text", "text": "intro"},
            {"type": "text", "text": "answer\n\nsecond paragraph"}
        ]});
        let old = build_at(&app_with(json!([answer.clone()])), 60);
        let new = build_at(
            &app_with(json!([
                {"id": 1, "role": "assistant", "content": [{"type": "reasoning", "text": "hmm"}]},
                answer
            ])),
            60,
        );
        assert!(
            new.meta.iter().any(|m| m.rule),
            "the older work puts a rule above the answer"
        );
        for text in ["intro", "second paragraph"] {
            let anchor = old.anchor(row_of(&old, text)).unwrap();
            assert_eq!(new.find(&anchor), Some(row_of(&new, text)), "{text}");
        }
        let blank = row_of(&old, "second paragraph") - 1;
        assert!(old.lines[blank].spans.is_empty());
        let anchor = old.anchor(blank).unwrap();
        assert_eq!(
            new.find(&anchor),
            Some(row_of(&new, "second paragraph") - 1),
            "a blank row keeps its place above the text after it"
        );
        assert_eq!(old.find(&old.anchor(0).unwrap()), Some(0));
    }

    #[test]
    fn an_anchor_keeps_its_row_when_a_message_above_grows() {
        let call = json!({"id": 1, "role": "assistant", "content": [{"type": "tool-call",
            "tool_call_id": "c1", "tool_name": "execute", "args": {"command": "make"}}]});
        let below =
            json!({"id": 3, "role": "user", "content": [{"type": "text", "text": "below"}]});
        let old = build_at(&app_with(json!([call.clone(), below.clone()])), 60);
        let new = build_at(
            &app_with(json!([
                call,
                {"id": 2, "role": "tool", "content": [{"type": "tool-result", "tool_call_id": "c1",
                    "tool_name": "execute", "result": {"output": "built"}}]},
                below
            ])),
            60,
        );
        assert!(row_of(&new, "below") > row_of(&old, "below"));
        let anchor = old.anchor(row_of(&old, "below")).unwrap();
        assert_eq!(new.find(&anchor), Some(row_of(&new, "below")));
    }

    #[test]
    fn the_top_line_says_what_history_is_left() {
        let theme = Theme::terminal(true);
        let long: Vec<serde_json::Value> = (301..=500)
            .map(|i| json!({"id": i, "role": "user", "content": [{"type": "text", "text": format!("m{i}")}]}))
            .collect();
        let mut app = app_with(serde_json::Value::Array(long));
        let top = |app: &App| {
            let view = build_at(app, 60);
            let line = view.lines[0].clone();
            assert!(
                line.spans.iter().all(|s| s.style == theme.dim),
                "the line is dim: {line:?}"
            );
            line.to_string()
        };
        assert_eq!(top(&app), "PageUp loads older messages");
        let effects = app.update(scuttle_core::app::Msg::LoadOlder);
        let [scuttle_core::app::Effect::LoadOlder { generation, .. }] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        assert_eq!(top(&app), "Loading older messages\u{2026}");
        let chat = app.chat_id.unwrap();
        app.update(scuttle_core::app::Msg::ForChat {
            chat,
            msg: Box::new(scuttle_core::app::Msg::OlderLoaded {
                messages: serde_json::from_value(json!([
                    {"id": 1, "role": "user", "content": [{"type": "text", "text": "m1"}]}
                ]))
                .unwrap(),
                has_more: false,
                generation: *generation,
            }),
        });
        let view = build_at(&app, 60);
        assert_eq!(view.lines[0].to_string(), "Start of chat");
        assert!(
            view.message_rows[&1] > 0,
            "the line is above the first message"
        );
        let short = build_at(
            &app_with(json!([
                {"id": 1, "role": "user", "content": [{"type": "text", "text": "only"}]}
            ])),
            60,
        );
        assert!(
            short.lines[0].to_string().contains("only"),
            "a chat that fits in one page shows no line"
        );
    }

    #[test]
    fn an_attached_file_line_offers_a_click_that_saves_it() {
        let file: uuid::Uuid = "5a1e0c3b-7d2f-4e61-9b8a-0c1d2e3f4a5b".parse().unwrap();
        let app = app_with(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "bundle the logs"}]},
            {"id": 2, "role": "assistant", "content": [{"type": "file", "file_id": file,
                "media_type": "application/zip", "name": "build-logs.zip", "file_name": ""}]}
        ]));
        let view = build_at(&app, 60);
        let row = row_of(&view, "attached build-logs.zip · click to save");
        assert!(
            view.hits
                .iter()
                .any(|h| h.lines.contains(&row) && h.target == HitTarget::SaveFile(file)),
            "{:?}",
            view.hits
        );
    }

    #[test]
    fn a_file_the_agent_attaches_shows_while_the_turn_streams() {
        let file: uuid::Uuid = "5a1e0c3b-7d2f-4e61-9b8a-0c1d2e3f4a5b".parse().unwrap();
        let mut app = app_with(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "bundle the logs"}]}
        ]));
        app.transcript.live.blocks.push(LiveBlock::File {
            file_id: Some(file),
            name: Some("build-logs.zip".into()),
            media_type: Some("application/zip".into()),
        });
        let view = build_at(&app, 60);
        let row = row_of(&view, "attached build-logs.zip · click to save");
        assert!(
            view.hits
                .iter()
                .any(|h| h.lines.contains(&row) && h.target == HitTarget::SaveFile(file))
        );
    }

    /// A chat whose agent attached `build-logs.zip`, and whether a click offer shows.
    fn offers_a_save(app: &App) -> bool {
        let view = build_at(app, 60);
        row_of(&view, "attached build-logs.zip");
        texts(&view).iter().any(|l| l.contains("click to save"))
            || view
                .hits
                .iter()
                .any(|h| matches!(h.target, HitTarget::SaveFile(_)))
    }

    fn app_with_a_zip() -> App {
        let file: uuid::Uuid = "5a1e0c3b-7d2f-4e61-9b8a-0c1d2e3f4a5b".parse().unwrap();
        app_with(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "bundle the logs"}]},
            {"id": 2, "role": "assistant", "created_at": "2026-10-05T10:00:00Z", "content": [
                {"type": "file", "file_id": file, "media_type": "application/zip",
                    "name": "build-logs.zip"}]}
        ]))
    }

    #[test]
    fn without_the_mouse_an_attached_line_offers_no_click() {
        let mut app = app_with_a_zip();
        app.mouse = false;
        assert!(!offers_a_save(&app));
    }

    #[test]
    fn a_file_the_chat_no_longer_stores_offers_no_click() {
        let mut app = app_with_a_zip();
        app.chat.as_mut().unwrap().files = serde_json::from_value(json!([{
            "id": uuid::Uuid::new_v4(), "name": "new.png", "mime_type": "image/png",
            "size_bytes": 1, "created_at": "2026-10-05T12:00:00Z"}]))
        .unwrap();
        assert!(!offers_a_save(&app));
    }

    #[test]
    fn an_attached_line_shows_the_name_without_invisible_characters() {
        let app = app_with(json!([
            {"id": 1, "role": "assistant", "content": [{"type": "file",
                "file_id": uuid::Uuid::new_v4(), "name": "invoice\u{202e}gpj.zip"}]}
        ]));
        let view = build_at(&app, 60);
        row_of(&view, "attached invoicegpj.zip");
        assert!(!texts(&view).iter().any(|l| l.contains('\u{202e}')));
    }

    #[test]
    fn an_attached_line_prefers_name_over_the_legacy_file_name() {
        let app = app_with(json!([
            {"id": 1, "role": "assistant", "content": [{"type": "file",
                "file_id": uuid::Uuid::new_v4(), "name": "new.zip", "file_name": "old.zip"}]}
        ]));
        row_of(&build_at(&app, 60), "attached new.zip");
    }

    #[test]
    fn a_sent_file_part_shows_a_dim_line_naming_the_attachment() {
        let app = app_with(json!([
            {"id": 1, "role": "user", "content": [
                {"type": "text", "text": "what is wrong here?"},
                {"type": "file", "file_id": "6f1c1b6e-8d4b-4c55-9a7e-1d2b3c4d5e6f", "name": "shot.png"},
                {"type": "file", "file_id": "7f1c1b6e-8d4b-4c55-9a7e-1d2b3c4d5e6f", "media_type": "text/plain"}
            ]}
        ]));
        let theme = Theme::terminal(true);
        let view = build_at(&app, 60);
        let lines = texts(&view);
        let named = row_of(&view, "attached shot.png");
        assert!(named > row_of(&view, "what is wrong here?"), "{lines:?}");
        assert!(
            view.lines[named]
                .spans
                .iter()
                .filter(|s| !s.content.trim().is_empty())
                .all(|s| s.style == theme.dim),
            "{:?}",
            view.lines[named]
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("attached a text/plain file")),
            "a file without a name is named by its type: {lines:?}"
        );
    }

    #[test]
    fn a_message_of_only_a_paste_shows_its_file_and_no_empty_text_line() {
        for text in [
            json!([]),
            json!([{"type": "text", "text": ""}]),
            json!([{"type": "text", "text": "\n"}]),
        ] {
            let mut content = text.as_array().unwrap().clone();
            content.push(
                json!({"type": "file", "file_id": "6f1c1b6e-8d4b-4c55-9a7e-1d2b3c4d5e6f",
                "name": "pasted-text-2026-10-05-09-30-00.txt"}),
            );
            let app = app_with(json!([
                {"id": 1, "role": "assistant", "content": [{"type": "text", "text": "Send the log."}]},
                {"id": 2, "role": "user", "content": content}
            ]));
            let view = build_at(&app, 60);
            let lines = texts(&view);
            assert!(
                !lines.iter().any(|l| l.trim() == "\u{203a}"),
                "no empty text line: {lines:?}"
            );
            let named = row_of(&view, "attached pasted-text-2026-10-05-09-30-00.txt");
            assert_eq!(
                lines[named - 1].trim(),
                "",
                "a blank line sets the message apart: {lines:?}"
            );
        }
    }

    #[test]
    fn an_orphan_result_without_a_workspace_name_shows_only_the_tool_name() {
        let app = app_with(json!([
            {"id": 5, "role": "tool", "content": [{"type": "tool-result", "tool_call_id": "c9",
                "tool_name": "execute", "result": {"output": "done"}}]}
        ]));
        let lines = texts(&build_at(&app, 60));
        assert!(
            lines.iter().any(|l| l.trim_end() == "⏺ execute"),
            "{lines:?}"
        );
        assert!(!lines.iter().any(|l| l.contains("execute()")), "{lines:?}");
    }

    #[test]
    fn an_unanswered_call_from_an_earlier_step_of_the_turn_stays_still() {
        let mut app = app_with(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "build it"}]},
            {"id": 2, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args": {"command": "make"}}
            ]},
            {"id": 3, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "b", "tool_name": "execute", "args": {"command": "make test"}}
            ]}
        ]));
        app.transcript.status = Some(coder_sdk::ChatStatus::Running);
        let view = build_at(&app, 40);
        let lines = texts(&view);
        let current = lines
            .iter()
            .position(|l| l.contains("execute(make test)"))
            .unwrap();
        assert_eq!(
            view.spinners,
            vec![current],
            "only the core's unresolved call animates: {lines:?}"
        );
    }

    #[test]
    fn after_a_retry_a_live_row_resolves_to_the_same_block_of_the_new_attempt() {
        let mut app = app_with(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "go"}]}
        ]));
        app.transcript
            .live
            .blocks
            .push(LiveBlock::Text("first attempt".into()));
        app.transcript
            .live
            .blocks
            .push(LiveBlock::Text("one\ntwo\nthree".into()));
        let old = build_at(&app, 60);
        let row = row_of(&old, "three");
        let retry = coder_sdk::StreamEvent {
            kind: coder_sdk::StreamEventType::Retry,
            event: serde_json::from_value(
                json!({"type": "retry", "retry": {"attempt": 2, "delay_ms": 10, "error": "busy"}}),
            )
            .ok(),
            raw: json!({"type": "retry"}),
        };
        app.transcript.apply(&retry);
        assert!(
            app.transcript.live.is_empty(),
            "a retry clears the live turn"
        );
        app.transcript
            .live
            .blocks
            .push(LiveBlock::Text("second".into()));
        app.transcript
            .live
            .blocks
            .push(LiveBlock::Text("solo".into()));
        let new = build_at(&app, 60);
        assert_eq!(
            relocate(&old, &new, row),
            Some(row_of(&new, "solo")),
            "block 1 of the new attempt, clamped to its one line"
        );
    }

    #[test]
    fn relocating_past_many_vanished_blocks_finds_the_survivor() {
        let mut old_msgs: Vec<serde_json::Value> = (1..=400)
            .map(|i| json!({"id": i, "role": "user", "content": [{"type": "text", "text": format!("m{i}")}]}))
            .collect();
        old_msgs.push(
            json!({"id": 1000, "role": "user", "content": [{"type": "text", "text": "kept"}]}),
        );
        let old = build_at(&app_with(serde_json::Value::Array(old_msgs)), 60);
        let new = build_at(
            &app_with(json!([
                {"id": 1000, "role": "user", "content": [{"type": "text", "text": "kept"}]}
            ])),
            60,
        );
        let row = row_of(&old, "m1");
        let kept = row_of(&old, "kept");
        assert_eq!(
            relocate(&old, &new, row),
            Some(row_of(&new, "kept").saturating_sub(kept - row))
        );
    }

    #[test]
    fn a_table_rewraps_to_the_pane_on_every_width() {
        let app = app_with(
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text",
            "text": "| name | note |\n| --- | --- |\n| a | the quick brown fox jumps over the lazy dog |"}]}]),
        );
        let at = |width| {
            build(
                &app,
                &Default::default(),
                &Default::default(),
                &welcome(),
                &Theme::terminal(true),
                width,
            )
        };
        let (wide, narrow) = (at(60), at(30));
        for (view, width) in [(&wide, 60), (&narrow, 30)] {
            assert!(
                view.lines.iter().all(|l| l.width() <= width),
                "{:#?}",
                texts(view)
            );
        }
        assert!(narrow.lines.len() > wide.lines.len());
        assert!(texts(&wide).iter().any(|l| l.starts_with('┌')));
    }

    /// The theme the tests draw with, with Nerd Font icons.
    fn nerd() -> Theme {
        Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal(true)
        }
    }

    #[test]
    fn nerd_icons_put_each_tool_kind_in_its_own_slot_after_the_state_marker() {
        let names = [
            "execute",
            "read_file",
            "edit_files",
            "find_tools",
            "web_search",
            "github__create_issue",
            "propose_plan",
            "ask_user_question",
            "create_workspace",
            "attach_file",
        ];
        let calls: Vec<serde_json::Value> = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                json!({"type": "tool-call", "tool_call_id": format!("c{i}"),
                    "tool_name": name, "args": {"path": "a"}})
            })
            .collect();
        let app = app_with(json!([{"id": 1, "role": "assistant", "content": calls}]));
        let view_with = |theme: &Theme| {
            build(
                &app,
                &Default::default(),
                &Default::default(),
                &welcome(),
                theme,
                80,
            )
        };
        let text = tool_heads(&view_with(&Theme::terminal(true)));
        let glyphs = tool_heads(&view_with(&nerd()));
        assert_eq!(text.len(), names.len(), "{text:#?}");
        assert_eq!(glyphs.len(), names.len(), "{glyphs:#?}");
        for ((name, plain), head) in names.iter().zip(&text).zip(&glyphs) {
            assert_eq!(*plain, format!("◌ {name}(a)"), "text mode adds no glyph");
            let kind = icons::slot(IconSet::Nerd, icons::tool_icon(name)).text;
            assert_eq!(
                *head,
                format!("◌ {kind}{name}(a)"),
                "the kind's slot follows the one-cell marker"
            );
        }
        let theme = nerd();
        let view = view_with(&theme);
        let head = &view.lines[row_of(&view, "execute(a)")];
        assert_eq!(head.spans[0].content, "◌ ");
        // The wrap joins cells of one style, so the kind shares a span with the name.
        assert!(
            head.spans[1].content.starts_with("\u{ea85} execute"),
            "{head:?}"
        );
        assert_eq!(
            head.spans[1].style, theme.accent,
            "the kind takes the name's accent"
        );
    }

    #[test]
    fn nerd_icons_lead_the_error_the_queued_messages_and_the_sent_files() {
        let mut app = app_with(json!([
            {"id": 1, "role": "user", "content": [
                {"type": "text", "text": "look"},
                {"type": "file", "file_id": "6f1c1b6e-8d4b-4c55-9a7e-1d2b3c4d5e6f", "name": "shot.png"},
                {"type": "file", "file_id": "7f1c1b6e-8d4b-4c55-9a7e-1d2b3c4d5e6f", "media_type": "text/plain"}
            ]}
        ]));
        app.transcript.queued = serde_json::from_value(
            json!([{"id": 5, "content": [{"type": "text", "text": "next question"}]}]),
        )
        .unwrap();
        app.transcript.last_error = Some("boom".into());
        let lines = |theme: &Theme| {
            texts(&build(
                &app,
                &Default::default(),
                &Default::default(),
                &welcome(),
                theme,
                60,
            ))
        };
        let text = lines(&Theme::terminal(true));
        for row in [
            "  attached shot.png · click to save",
            "  attached a text/plain file · click to save",
            "  queued · next question",
            "Error: boom",
        ] {
            assert!(text.iter().any(|l| l == row), "{row:?} is not in {text:#?}");
        }
        let glyphs = lines(&nerd());
        for row in [
            "  \u{ec34} shot.png · click to save",
            "  \u{ec34} a text/plain file · click to save",
            "  \u{ea82} next question",
            "\u{ea87} Error: boom",
        ] {
            assert!(
                glyphs.iter().any(|l| l == row),
                "{row:?} is not in {glyphs:#?}"
            );
        }
    }

    /// Whether `c` is a private-use character, where Nerd Fonts put their glyphs.
    fn private_use(c: char) -> bool {
        matches!(c, '\u{e000}'..='\u{f8ff}' | '\u{f0000}'..='\u{10ffff}')
    }

    #[test]
    fn copying_a_nerd_transcript_gives_the_text_icons_words_and_no_glyphs() {
        use crate::selection::{Pos, Selection, selected_text};
        let mut app = app_with(json!([
            {"id": 1, "role": "user", "content": [
                {"type": "text", "text": "look"},
                {"type": "file", "file_id": "6f1c1b6e-8d4b-4c55-9a7e-1d2b3c4d5e6f", "name": "shot.png"}
            ]},
            {"id": 2, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "c1", "tool_name": "execute", "args": {"command": "make"}},
                {"type": "tool-call", "tool_call_id": "c2", "tool_name": "github__create_issue", "args": {"path": "a"}}
            ]}
        ]));
        app.transcript.queued = serde_json::from_value(
            json!([{"id": 5, "content": [{"type": "text", "text": "next question"}]}]),
        )
        .unwrap();
        app.transcript.last_error = Some("boom".into());
        let copy = |theme: &Theme| {
            let view = build(
                &app,
                &Default::default(),
                &Default::default(),
                &welcome(),
                theme,
                60,
            );
            let all = Selection {
                anchor: Pos { line: 0, col: 0 },
                head: Pos {
                    line: view.lines.len() - 1,
                    col: 59,
                },
            };
            selected_text(&view, &all)
        };
        let nerd_copy = copy(&nerd());
        assert!(
            !nerd_copy.chars().any(private_use),
            "a glyph was copied: {nerd_copy:?}"
        );
        assert_eq!(
            nerd_copy,
            copy(&Theme::terminal(true)),
            "as text mode copies"
        );
        for words in ["attached shot.png", "queued · next question", "Error: boom"] {
            assert!(
                nerd_copy.contains(words),
                "{words:?} is not in {nerd_copy:?}"
            );
        }
    }

    #[test]
    fn a_nerd_queued_preview_gets_the_columns_its_glyph_frees() {
        let mut app = app_with(json!([
            {"id": 1, "role": "user", "content": [{"type": "text", "text": "hi"}]}
        ]));
        app.transcript.queued = serde_json::from_value(
            json!([{"id": 5, "content": [{"type": "text", "text": "word ".repeat(40)}]}]),
        )
        .unwrap();
        let last_width = |theme: &Theme| {
            let view = build(
                &app,
                &Default::default(),
                &Default::default(),
                &welcome(),
                theme,
                60,
            );
            view.lines.last().unwrap().width()
        };
        assert_eq!(last_width(&nerd()), last_width(&Theme::terminal(true)));
    }

    #[test]
    fn text_icons_suggest_a_nerd_font_with_a_link_over_its_name() {
        let app = App::new(BusyBehavior::Queue, true);
        let view_at = |theme: &Theme, welcome: &Welcome, width| {
            build(
                &app,
                &Default::default(),
                &Default::default(),
                welcome,
                theme,
                width,
            )
        };
        let theme = Theme::terminal(true);
        let wide = view_at(&theme, &welcome(), 80);
        let shown = texts(&wide);
        assert_eq!(shown[8], "", "a blank row sets the tip off");
        assert_eq!(shown[9], "scuttle works better with a Nerd Font!");
        assert_eq!(wide.nerd_tip, Some(9..10), "the view says where the tip is");
        assert_eq!(
            wide.links,
            vec![LinkHit {
                line: 9,
                cols: 28..37,
                url: icons::NERD_FONTS_URL.to_owned(),
                link: 0,
            }],
            "the link covers exactly the words Nerd Font"
        );
        let tip = &wide.lines[9];
        assert_eq!(tip.spans[1].content, "Nerd Font");
        assert!(
            tip.spans[1]
                .style
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );
        assert_eq!(tip.spans[0].style, theme.dim);
        let narrow = view_at(&theme, &welcome(), 30);
        let shown = texts(&narrow);
        let row = shown
            .iter()
            .position(|l| l.starts_with("scuttle works better"))
            .unwrap_or_else(|| panic!("no tip in {shown:#?}"));
        assert_eq!(shown[row], "scuttle works better with a ");
        assert_eq!(shown[row + 1], "Nerd Font!");
        assert_eq!(
            narrow.links,
            vec![LinkHit {
                line: row + 1,
                cols: 0..9,
                url: icons::NERD_FONTS_URL.to_owned(),
                link: 0,
            }],
            "the link follows the wrap onto the next row"
        );
        let nerd_view = view_at(&nerd(), &welcome(), 80);
        assert!(nerd_view.links.is_empty(), "nerd mode shows no tip");
        assert_eq!(nerd_view.nerd_tip, None);
        assert!(
            !texts(&nerd_view).iter().any(|l| l.contains("Nerd Font")),
            "{:#?}",
            texts(&nerd_view)
        );
        let shown_before = Welcome {
            tip: false,
            ..welcome()
        };
        let declined = view_at(&theme, &shown_before, 80);
        assert!(
            declined.links.is_empty(),
            "a tip shown before is not offered"
        );
        assert_eq!(declined.nerd_tip, None);
        assert!(
            !texts(&declined).iter().any(|l| l.contains("Nerd Font")),
            "{:#?}",
            texts(&declined)
        );
    }
}
