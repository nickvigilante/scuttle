//! Markdown to styled terminal lines, recording where code blocks are for copying.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::highlight;

/// Cap on cached entries; the cache is cleared rather than evicted individually since
/// rendering is cheap and a transcript's distinct messages rarely exceed this.
const CACHE_LIMIT: usize = 512;

thread_local! {
    static CACHE: RefCell<HashMap<u64, (String, Rendered)>> = RefCell::new(HashMap::new());
}

fn cache_key(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    highlight::is_ready().hash(&mut hasher);
    hasher.finish()
}

/// `render`, memoized by the text and whether syntax highlighting is ready, so re-rendering an
/// unchanged message (every redraw, until the assets warm up flips highlighting on) doesn't
/// redo the parse and highlight work. The cached text is stored alongside its hash so a
/// collision between two different texts is detected as a miss rather than returning the
/// wrong `Rendered`.
pub fn render_cached(text: &str) -> Rendered {
    let key = cache_key(text);
    CACHE.with(|cache| {
        if let Some((cached_text, hit)) = cache.borrow().get(&key)
            && cached_text == text
        {
            return hit.clone();
        }
        let rendered = render(text);
        let mut cache = cache.borrow_mut();
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(key, (text.to_owned(), rendered.clone()));
        rendered
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodeBlock {
    pub start: usize,
    pub end: usize,
    pub code: String,
}

#[derive(Debug, Clone, Default)]
pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    pub code_blocks: Vec<CodeBlock>,
}

struct Builder {
    out: Rendered,
    current: Vec<Span<'static>>,
    styles: Vec<Style>,
    list_depth: usize,
    /// Per open list, `Some(next number)` for an ordered list or `None` for an unordered one.
    list_counters: Vec<Option<u64>>,
    quote_depth: usize,
    code: Option<(String, String)>,
    /// Whether the next `TableCell` is the first one in its row (no leading separator).
    first_cell: bool,
}

impl Builder {
    fn style(&self) -> Style {
        self.styles
            .iter()
            .fold(Style::new(), |acc, s| acc.patch(*s))
    }

    fn flush(&mut self) {
        if self.current.is_empty() {
            return;
        }
        let mut spans = Vec::new();
        if self.quote_depth > 0 {
            spans.push(Span::styled(
                "│ ".repeat(self.quote_depth),
                Style::new().fg(Color::DarkGray),
            ));
        }
        spans.append(&mut self.current);
        self.out.lines.push(Line::from(spans));
    }

    fn blank(&mut self) {
        self.flush();
        if self.out.lines.last().is_some_and(|l| !l.spans.is_empty()) {
            self.out.lines.push(Line::default());
        }
    }

    fn finish_code(&mut self) {
        let Some((lang, code)) = self.code.take() else {
            return;
        };
        let start = self.out.lines.len();
        let mut lines = highlight::highlight(&code, &lang).unwrap_or_else(|| {
            code.lines()
                .map(|l| Line::from(Span::styled(l.to_owned(), Style::new().fg(Color::Gray))))
                .collect()
        });
        if self.quote_depth > 0 {
            let marker = "│ ".repeat(self.quote_depth);
            for line in &mut lines {
                line.spans.insert(
                    0,
                    Span::styled(marker.clone(), Style::new().fg(Color::DarkGray)),
                );
            }
        }
        self.out.lines.extend(lines);
        let end = self.out.lines.len();
        self.out.code_blocks.push(CodeBlock { start, end, code });
    }
}

pub fn render(text: &str) -> Rendered {
    let mut b = Builder {
        out: Rendered::default(),
        current: Vec::new(),
        styles: Vec::new(),
        list_depth: 0,
        list_counters: Vec::new(),
        quote_depth: 0,
        code: None,
        first_cell: true,
    };
    for event in Parser::new_ext(text, Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES) {
        if let Some((_, code)) = b.code.as_mut() {
            match event {
                Event::Text(t) => {
                    code.push_str(&t);
                    continue;
                }
                Event::End(TagEnd::CodeBlock) => {
                    b.finish_code();
                    b.blank();
                    continue;
                }
                _ => continue,
            }
        }
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                b.blank();
                let color = if level == HeadingLevel::H1 {
                    Color::Cyan
                } else {
                    Color::Blue
                };
                b.styles
                    .push(Style::new().fg(color).add_modifier(Modifier::BOLD));
            }
            Event::End(TagEnd::Heading(_)) => {
                b.styles.pop();
                b.blank();
            }
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => b.blank(),
            Event::Start(Tag::Strong) => b.styles.push(Style::new().add_modifier(Modifier::BOLD)),
            Event::Start(Tag::Emphasis) => {
                b.styles.push(Style::new().add_modifier(Modifier::ITALIC))
            }
            Event::Start(Tag::Strikethrough) => b
                .styles
                .push(Style::new().add_modifier(Modifier::CROSSED_OUT)),
            Event::End(TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough) => {
                b.styles.pop();
            }
            Event::Start(Tag::BlockQuote(_)) => {
                b.flush();
                b.quote_depth += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                b.flush();
                b.quote_depth -= 1;
            }
            Event::Start(Tag::List(start)) => {
                b.flush();
                b.list_depth += 1;
                b.list_counters.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                b.flush();
                b.list_depth -= 1;
                b.list_counters.pop();
                if b.list_depth == 0 {
                    b.blank();
                }
            }
            Event::Start(Tag::Item) => {
                b.flush();
                let indent = "  ".repeat(b.list_depth.saturating_sub(1));
                let marker = if let Some(Some(n)) = b.list_counters.last_mut() {
                    let marker = format!("{n}. ");
                    *n += 1;
                    marker
                } else {
                    "• ".to_owned()
                };
                b.current.push(Span::raw(format!("{indent}{marker}")));
            }
            Event::End(TagEnd::Item) => b.flush(),
            Event::Start(Tag::Table(_)) => b.flush(),
            Event::End(TagEnd::Table) => b.blank(),
            Event::Start(Tag::TableHead) => {
                b.first_cell = true;
                b.styles.push(Style::new().add_modifier(Modifier::BOLD));
            }
            Event::End(TagEnd::TableHead) => {
                b.styles.pop();
                b.flush();
            }
            Event::Start(Tag::TableRow) => b.first_cell = true,
            Event::End(TagEnd::TableRow) => b.flush(),
            Event::Start(Tag::TableCell) => {
                if b.first_cell {
                    b.first_cell = false;
                } else {
                    b.current.push(Span::raw(" │ "));
                }
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                b.flush();
                let lang = match kind {
                    CodeBlockKind::Fenced(l) => {
                        l.split_whitespace().next().unwrap_or_default().to_owned()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                b.code = Some((lang, String::new()));
            }
            Event::Code(c) => b
                .current
                .push(Span::styled(c.to_string(), Style::new().fg(Color::Yellow))),
            Event::Text(t) => {
                let style = b.style();
                b.current.push(Span::styled(t.to_string(), style));
            }
            Event::SoftBreak => b.current.push(Span::raw(" ")),
            Event::HardBreak => b.flush(),
            Event::Rule => {
                b.flush();
                b.out.lines.push(Line::from(Span::styled(
                    "─".repeat(20),
                    Style::new().fg(Color::DarkGray),
                )));
            }
            _ => {}
        }
    }
    b.finish_code();
    b.flush();
    while b.out.lines.last().is_some_and(|l| l.spans.is_empty()) {
        b.out.lines.pop();
    }
    b.out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;

    fn plain(r: &Rendered) -> Vec<String> {
        r.lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn renders_headings_emphasis_and_lists() {
        let r = render("# Title\n\nSome **bold** and `code`.\n\n- one\n- two\n");
        let text = plain(&r);
        assert_eq!(text[0], "Title");
        assert!(
            r.lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(text.iter().any(|l| l == "Some bold and code."));
        assert!(text.iter().any(|l| l == "• one"));
        assert!(text.iter().any(|l| l == "• two"));
    }

    #[test]
    fn records_code_block_ranges_and_source() {
        let r = render("before\n\n```rust\nfn main() {}\nlet x = 1;\n```\n\nafter\n");
        assert_eq!(r.code_blocks.len(), 1);
        let block = &r.code_blocks[0];
        assert_eq!(block.code, "fn main() {}\nlet x = 1;\n");
        assert_eq!(block.end - block.start, 2);
        assert_eq!(plain(&r)[block.start], "fn main() {}");
    }

    #[test]
    fn unterminated_code_fence_while_streaming_is_still_a_block() {
        let r = render("```\npartial");
        assert_eq!(r.code_blocks.len(), 1);
        assert_eq!(r.code_blocks[0].code.trim_end(), "partial");
    }

    #[test]
    fn renders_tables_as_rows() {
        let r = render("| a | b |\n| --- | --- |\n| 1 | 2 |\n");
        let text = plain(&r);
        assert!(text.iter().any(|l| l == "a │ b"));
        assert!(text.iter().any(|l| l == "1 │ 2"));
        let header_idx = text.iter().position(|l| l == "a │ b").unwrap();
        assert!(
            r.lines[header_idx].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn code_inside_blockquote_keeps_the_marker() {
        let r = render("> ```\n> fn main() {}\n> ```\n");
        let text = plain(&r);
        assert!(text.iter().any(|l| l == "│ fn main() {}"));
    }

    #[test]
    fn numbers_ordered_lists() {
        let r = render("1. one\n2. two\n3. three\n");
        let text = plain(&r);
        assert!(text.iter().any(|l| l == "1. one"));
        assert!(text.iter().any(|l| l == "2. two"));
        assert!(text.iter().any(|l| l == "3. three"));
    }

    #[test]
    fn render_cached_returns_the_same_lines_as_render() {
        let text = "# Cached\n\nSome **bold** text.\n\n```rust\nfn f() {}\n```\n";
        let direct = render(text);
        let cached_first = render_cached(text);
        let cached_second = render_cached(text);
        assert_eq!(plain(&cached_first), plain(&direct));
        assert_eq!(plain(&cached_second), plain(&direct));
        assert_eq!(cached_first.code_blocks, direct.code_blocks);
    }
}
