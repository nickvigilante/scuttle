//! Wraps styled lines to a width, counting display columns so wide characters never overflow.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// Wraps at spaces where possible, and splits words longer than the width.
pub fn wrap_line(line: &Line<'static>, width: u16) -> Vec<Line<'static>> {
    let width = width.max(2) as usize;
    let cells: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
        .collect();
    if cells.is_empty() {
        return vec![Line::default().style(line.style)];
    }
    let mut out = Vec::new();
    let mut start = 0;
    while start < cells.len() {
        let mut used = 0;
        let mut end = start;
        let mut last_space = None;
        while end < cells.len() {
            let w = cells[end].0.width().unwrap_or(0);
            if used + w > width {
                break;
            }
            if cells[end].0 == ' ' {
                last_space = Some(end);
            }
            used += w;
            end += 1;
        }
        if end < cells.len()
            && let Some(space) = last_space.filter(|s| *s > start)
        {
            end = space + 1;
        }
        if end == start {
            end = start + 1;
        }
        out.push(to_line(&cells[start..end], line.style));
        start = end;
    }
    out
}

pub fn wrap_lines(lines: &[Line<'static>], width: u16) -> Vec<Line<'static>> {
    lines.iter().flat_map(|l| wrap_line(l, width)).collect()
}

fn to_line(cells: &[(char, Style)], line_style: Style) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (c, style) in cells {
        match spans.last_mut() {
            Some(last) if last.style == *style => last.content.to_mut().push(*c),
            _ => spans.push(Span::styled(c.to_string(), *style)),
        }
    }
    Line::from(spans).style(line_style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;
    use ratatui::text::{Line, Span};
    use unicode_width::UnicodeWidthStr;

    fn width(line: &Line) -> usize {
        line.spans.iter().map(|s| s.content.width()).sum()
    }

    #[test]
    fn wraps_at_word_boundaries_and_keeps_styles() {
        let line = Line::from(vec![
            Span::raw("hello "),
            Span::styled("bold world", Style::new().bold()),
        ]);
        let out = wrap_line(&line, 8);
        assert!(out.iter().all(|l| width(l) <= 8));
        let text: String = out
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("|");
        assert_eq!(text, "hello |bold |world");
        assert!(out[1].spans.iter().any(|s| s.style == Style::new().bold()));
    }

    #[test]
    fn wraps_wide_characters_within_width() {
        let line = Line::from("日本語のテキストと絵文字🦀🦀🦀が混ざっています");
        for w in [2u16, 3, 5, 10] {
            for l in wrap_line(&line, w) {
                assert!(width(&l) <= w as usize, "width {w}: {:?}", l);
            }
        }
    }

    #[test]
    fn long_words_are_split_and_empty_lines_survive() {
        let out = wrap_line(&Line::from("abcdefghij"), 4);
        assert_eq!(out.len(), 3);
        assert_eq!(wrap_line(&Line::from(""), 10).len(), 1);
    }
}
