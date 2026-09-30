//! Turns core state into transcript lines, the welcome block, and click targets.

use std::collections::{BTreeMap, HashSet};
use std::ops::Range;

use coder_sdk::types;
use ratatui::text::{Line, Span};
use scuttle_core::app::App;
use scuttle_core::density::{BlockKind, Density, density_for};
use scuttle_core::live::LiveBlock;

use crate::markdown;
use crate::theme::Theme;
use crate::wrap::{wrap_lines, wrap_rows};

/// A block: (message ID, or `None` for the live turn; index of the block within it).
pub type BlockId = (Option<i64>, usize);

#[derive(Debug, Clone, PartialEq)]
pub enum HitTarget {
    Toggle(BlockId),
    CopyCode(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub lines: Range<usize>,
    pub target: HitTarget,
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
}

#[derive(Debug, Clone, Default)]
pub struct View {
    pub lines: Vec<Line<'static>>,
    /// One entry per row of `lines`.
    pub meta: Vec<LineMeta>,
    pub hits: Vec<Hit>,
    /// Code blocks of the most recent assistant turn (durable or live) that had any.
    pub last_code_blocks: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Welcome {
    pub url: String,
    pub user: String,
    pub art: Vec<String>,
    pub show: bool,
}

/// A renderable unit before wrapping.
enum Item<'a> {
    UserText(&'a str),
    AssistantText(&'a str),
    Reasoning(&'a str),
    Tool {
        name: &'a str,
        args: String,
        result: String,
        is_error: bool,
        done: bool,
    },
}

/// A tool's result, gathered from wherever Coder recorded it: a durable `tool-result` part
/// (possibly in a later message with role "tool", since Coder stores a tool's result
/// separately from its call) or a live `LiveBlock::ToolResult`.
struct ToolResultInfo {
    result: String,
    is_error: bool,
    done: bool,
}

struct Out<'t> {
    view: View,
    width: u16,
    theme: &'t Theme,
    hidden: usize,
}

/// What `render_items` reads besides the items themselves.
struct Ctx<'c> {
    app: &'c App,
    overrides: &'c BTreeMap<String, Density>,
    toggles: &'c HashSet<BlockId>,
    /// Assistant text blocks that start a turn's answer and get a rule above them.
    answers: &'c HashSet<BlockId>,
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

fn args_summary(args: &serde_json::Value) -> String {
    match args {
        serde_json::Value::Object(map) => map
            .values()
            .filter_map(|v| v.as_str())
            .next()
            .map(str::to_owned)
            .unwrap_or_else(|| args.to_string()),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
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
    strip_ansi_csi(&text)
}

/// Collects every tool-result, and every id that has a matching tool-call, from durable
/// messages and the live turn. Coder persists a tool's result in a separate message with role
/// "tool" (see `coderd/x/chatd/message_conversion.go`), not alongside its call, so pairing a
/// call with its result requires looking across the whole transcript rather than one message.
fn collect_tool_results(
    messages: &[&types::CodersdkChatMessage],
    live: &[LiveBlock],
) -> (BTreeMap<String, ToolResultInfo>, HashSet<String>) {
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

fn items_for_message<'a>(
    m: &'a types::CodersdkChatMessage,
    results: &BTreeMap<String, ToolResultInfo>,
) -> Vec<Item<'a>> {
    let role = m.role.as_ref().map(|r| r.as_str());
    // A "tool" message carries only tool-result parts, already folded into `results`; it adds
    // no lines of its own.
    if role == Some("tool") {
        return Vec::new();
    }
    let user = role == Some("user");
    let mut items = Vec::new();
    for p in &m.content {
        match p.type_.as_ref().map(|t| t.as_str()).unwrap_or_default() {
            "text" if user => items.push(Item::UserText(p.text.as_deref().unwrap_or_default())),
            "text" => items.push(Item::AssistantText(p.text.as_deref().unwrap_or_default())),
            "reasoning" => items.push(Item::Reasoning(p.text.as_deref().unwrap_or_default())),
            "tool-call" => {
                let id = p.tool_call_id.clone().unwrap_or_default();
                let result = results.get(&id);
                items.push(Item::Tool {
                    name: p.tool_name.as_deref().unwrap_or("tool"),
                    args: p.args.as_ref().map(args_summary).unwrap_or_default(),
                    result: result.map(|r| r.result.clone()).unwrap_or_default(),
                    is_error: result.map(|r| r.is_error).unwrap_or(false),
                    done: result.map(|r| r.done).unwrap_or(false),
                });
            }
            _ => {}
        }
    }
    items
}

fn items_for_live<'a>(
    blocks: &'a [LiveBlock],
    results: &BTreeMap<String, ToolResultInfo>,
    calls: &HashSet<String>,
) -> Vec<Item<'a>> {
    let mut items = Vec::new();
    for b in blocks {
        match b {
            LiveBlock::Text(t) => items.push(Item::AssistantText(t)),
            LiveBlock::Reasoning(t) => items.push(Item::Reasoning(t)),
            LiveBlock::ToolCall {
                id,
                name,
                args,
                args_raw,
            } => {
                let result = results.get(id);
                items.push(Item::Tool {
                    name,
                    args: args
                        .as_ref()
                        .map(args_summary)
                        .unwrap_or_else(|| args_raw.clone()),
                    result: result.map(|r| r.result.clone()).unwrap_or_default(),
                    is_error: result.map(|r| r.is_error).unwrap_or(false),
                    done: result.map(|r| r.done).unwrap_or(false),
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
                    args: String::new(),
                    result: result_text(result.as_ref(), result_raw),
                    is_error: *is_error,
                    done: *done,
                });
            }
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

/// Renders `items` into `out`, returning the code blocks of any assistant text among them.
/// `live` selects `markdown::render` (uncached, since live text keeps changing) over
/// `markdown::render_cached` (for durable, unchanging text).
fn render_items(
    out: &mut Out,
    owner: Option<i64>,
    items: Vec<Item>,
    ctx: &Ctx,
    live: bool,
) -> Vec<String> {
    let width = out.width as usize;
    let mut code_blocks = Vec::new();
    for (index, item) in items.into_iter().enumerate() {
        let id: BlockId = (owner, index);
        match item {
            Item::UserText(text) => {
                out.gap();
                let lines = text
                    .lines()
                    .map(|l| {
                        Line::from(vec![
                            Span::styled("› ", out.theme.user),
                            Span::raw(l.to_owned()),
                        ])
                    })
                    .collect();
                out.push(lines, true);
            }
            Item::AssistantText(text) => {
                if ctx.answers.contains(&id) {
                    out.rule();
                }
                out.gap();
                let rendered = if live {
                    markdown::render(text)
                } else {
                    markdown::render_cached(text)
                };
                let base = out.view.lines.len();
                for block in &rendered.code_blocks {
                    let start = base + wrap_lines(&rendered.lines[..block.start], out.width).len();
                    let end = base + wrap_lines(&rendered.lines[..block.end], out.width).len();
                    out.view.hits.push(Hit {
                        lines: start..end,
                        target: HitTarget::CopyCode(block.code.clone()),
                    });
                }
                code_blocks.extend(rendered.code_blocks.iter().map(|b| b.code.clone()));
                out.push(rendered.lines, false);
            }
            Item::Reasoning(text) => {
                out.gap();
                let mut density = density_for(BlockKind::Reasoning, &ctx.app.prefs, ctx.overrides);
                if ctx.toggles.contains(&id) {
                    density = if density == Density::Expanded {
                        Density::Summary
                    } else {
                        Density::Expanded
                    };
                }
                let lines = match density {
                    Density::Expanded => text
                        .lines()
                        .map(|l| Line::from(Span::styled(l.to_owned(), out.theme.dim)))
                        .collect(),
                    _ => vec![Line::from(Span::styled("∴ Thinking", out.theme.dim))],
                };
                let range = out.push(lines, false);
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
            } => {
                let mut density = density_for(BlockKind::Tool(name), &ctx.app.prefs, ctx.overrides);
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
                let marker = match (done, is_error) {
                    (false, _) => Span::styled("◌ ", out.theme.warn),
                    (true, true) => Span::styled("✗ ", out.theme.error),
                    (true, false) => Span::styled("⏺ ", out.theme.ok),
                };
                let head_budget = width.saturating_sub(name.chars().count() + 6).max(8);
                let head = Line::from(vec![
                    marker,
                    Span::styled(name.to_owned(), out.theme.accent),
                    Span::raw(format!("({})", one_line(&args, head_budget))),
                ]);
                let mut lines = vec![head];
                match density {
                    Density::Expanded => lines.extend(
                        result
                            .lines()
                            .map(|l| Line::from(Span::styled(format!("  {l}"), out.theme.dim))),
                    ),
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
                out.view.hits.push(Hit {
                    lines: range,
                    target: HitTarget::Toggle(id),
                });
            }
        }
    }
    code_blocks
}

fn welcome_lines(w: &Welcome, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![Line::default()];
    if w.art.is_empty() {
        lines.push(Line::from(Span::styled("scuttle", theme.accent)));
    } else {
        lines.extend(
            w.art
                .iter()
                .map(|l| Line::from(Span::styled(l.clone(), theme.accent))),
        );
    }
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

pub fn build(
    app: &App,
    overrides: &BTreeMap<String, Density>,
    toggles: &HashSet<BlockId>,
    welcome: &Welcome,
    theme: &Theme,
    width: u16,
) -> View {
    let mut out = Out {
        view: View::default(),
        width: width.max(2),
        theme,
        hidden: 0,
    };
    let messages: Vec<_> = app.transcript.messages().collect();
    if messages.is_empty() && app.transcript.live.is_empty() {
        if welcome.show {
            out.push(welcome_lines(welcome, theme), false);
        }
        return out.view;
    }
    let (results, calls) = collect_tool_results(&messages, &app.transcript.live.blocks);
    let mut groups: Vec<(Option<i64>, Vec<Item>)> = messages
        .iter()
        .map(|m| (m.id, items_for_message(m, &results)))
        .collect();
    let has_live = !app.transcript.live.is_empty();
    if has_live {
        groups.push((
            None,
            items_for_live(&app.transcript.live.blocks, &results, &calls),
        ));
    }
    let answers = answer_starts(&groups);
    let ctx = Ctx {
        app,
        overrides,
        toggles,
        answers: &answers,
    };
    let count = groups.len();
    for (i, (owner, items)) in groups.into_iter().enumerate() {
        let live = has_live && i + 1 == count;
        let code = render_items(&mut out, owner, items, &ctx, live);
        if !code.is_empty() {
            out.view.last_code_blocks = code;
        }
    }
    for queued in &app.transcript.queued {
        let text = queued
            .content
            .iter()
            .filter_map(|p| p.text.as_deref())
            .collect::<Vec<_>>()
            .join(" ");
        out.push(
            vec![Line::from(Span::styled(
                format!(
                    "  queued · {}",
                    one_line(&text, (width as usize).saturating_sub(12).max(8))
                ),
                theme.dim,
            ))],
            false,
        );
    }
    if let Some(err) = app.transcript.last_error.as_ref() {
        out.gap();
        out.push(
            vec![Line::from(Span::styled(
                format!("Error: {err}"),
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
            show: true,
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
        insta::assert_snapshot!(draw(&view, 40, 12).backend());
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
    fn result_text_strips_ansi_csi_sequences() {
        let colored = "\u{1b}[32mdone\u{1b}[0m";
        assert_eq!(result_text(Some(&json!({"output": colored})), ""), "done");
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
                {"type": "text", "text": "First part."},
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
}
