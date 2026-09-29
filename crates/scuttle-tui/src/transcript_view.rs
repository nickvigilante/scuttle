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
use crate::wrap::wrap_lines;

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

#[derive(Debug, Clone, Default)]
pub struct View {
    pub lines: Vec<Line<'static>>,
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

impl Out<'_> {
    fn push(&mut self, lines: Vec<Line<'static>>) -> Range<usize> {
        self.flush_hidden();
        let start = self.view.lines.len();
        self.view.lines.extend(wrap_lines(&lines, self.width));
        start..self.view.lines.len()
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
        self.view.lines.extend(wrap_lines(&[line], self.width));
    }

    fn gap(&mut self) {
        self.flush_hidden();
        if self.view.lines.last().is_some_and(|l| !l.spans.is_empty()) {
            self.view.lines.push(Line::default());
        }
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

/// Renders `items` into `out`, returning the code blocks of any assistant text among them.
/// `live` selects `markdown::render` (uncached, since live text keeps changing) over
/// `markdown::render_cached` (for durable, unchanging text).
fn render_items(
    out: &mut Out,
    owner: Option<i64>,
    items: Vec<Item>,
    app: &App,
    overrides: &BTreeMap<String, Density>,
    toggles: &HashSet<BlockId>,
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
                out.push(lines);
            }
            Item::AssistantText(text) => {
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
                out.push(rendered.lines);
            }
            Item::Reasoning(text) => {
                out.gap();
                let mut density = density_for(BlockKind::Reasoning, &app.prefs, overrides);
                if toggles.contains(&id) {
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
                let range = out.push(lines);
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
                let mut density = density_for(BlockKind::Tool(name), &app.prefs, overrides);
                if toggles.contains(&id) {
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
                let mut wrapped = Vec::new();
                for (i, l) in lines.into_iter().enumerate() {
                    let w = wrap_lines(&[l], out.width);
                    if density == Density::Expanded || i > 0 || w.len() == 1 {
                        wrapped.extend(w);
                    } else {
                        wrapped.push(w.into_iter().next().unwrap_or_default());
                    }
                }
                if density != Density::Expanded {
                    wrapped.truncate(2);
                }
                let start = out.view.lines.len();
                out.view.lines.extend(wrapped);
                out.view.hits.push(Hit {
                    lines: start..out.view.lines.len(),
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
            out.push(welcome_lines(welcome, theme));
        }
        return out.view;
    }
    let (results, calls) = collect_tool_results(&messages, &app.transcript.live.blocks);
    for m in &messages {
        let code = render_items(
            &mut out,
            m.id,
            items_for_message(m, &results),
            app,
            overrides,
            toggles,
            false,
        );
        if !code.is_empty() {
            out.view.last_code_blocks = code;
        }
    }
    if !app.transcript.live.is_empty() {
        let code = render_items(
            &mut out,
            None,
            items_for_live(&app.transcript.live.blocks, &results, &calls),
            app,
            overrides,
            toggles,
            true,
        );
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
        out.push(vec![Line::from(Span::styled(
            format!(
                "  queued · {}",
                one_line(&text, (width as usize).saturating_sub(12).max(8))
            ),
            theme.dim,
        ))]);
    }
    if let Some(err) = app.transcript.last_error.as_ref() {
        out.gap();
        out.push(vec![Line::from(Span::styled(
            format!("Error: {err}"),
            theme.error,
        ))]);
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
}
