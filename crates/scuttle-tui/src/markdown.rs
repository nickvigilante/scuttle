//! Markdown to styled terminal lines, recording where code blocks are for copying.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::highlight;

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
    quote_depth: usize,
    code: Option<(String, String)>,
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
        let lines = highlight::highlight(&code, &lang).unwrap_or_else(|| {
            code.lines()
                .map(|l| Line::from(Span::styled(l.to_owned(), Style::new().fg(Color::Gray))))
                .collect()
        });
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
        quote_depth: 0,
        code: None,
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
            Event::Start(Tag::List(_)) => {
                b.flush();
                b.list_depth += 1;
            }
            Event::End(TagEnd::List(_)) => {
                b.flush();
                b.list_depth -= 1;
                if b.list_depth == 0 {
                    b.blank();
                }
            }
            Event::Start(Tag::Item) => {
                b.flush();
                b.current.push(Span::raw(format!(
                    "{}• ",
                    "  ".repeat(b.list_depth.saturating_sub(1))
                )));
            }
            Event::End(TagEnd::Item) => b.flush(),
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
}
