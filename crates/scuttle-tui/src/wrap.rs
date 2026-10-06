//! Wraps styled lines to a width, counting display columns so wide characters never overflow.
//!
//! Wrapping operates on extended grapheme clusters, not codepoints, so a base character
//! plus its combining marks, or a multi-codepoint ZWJ emoji sequence, is never split across
//! two lines. Tabs are expanded to four columns before measuring, since a bare `'\t'` has no
//! meaningful display width on its own.

use std::ops::Range;

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Wraps at spaces where possible, and splits words longer than the width.
pub fn wrap_line(line: &Line<'static>, width: u16) -> Vec<Line<'static>> {
    let width = width.max(2) as usize;
    let cells: Vec<(String, Style)> = line
        .spans
        .iter()
        .flat_map(|s| {
            let style = s.style;
            let expanded = s.content.replace('\t', "    ");
            expanded
                .graphemes(true)
                .map(str::to_owned)
                .collect::<Vec<_>>()
                .into_iter()
                .map(move |g| (g, style))
        })
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
            let w = cells[end].0.width();
            if used + w > width {
                break;
            }
            if cells[end].0 == " " {
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

/// The narrowest column `hang` wraps a body into; a narrower one wraps under the prefix.
const MIN_HANGING: usize = 30;

/// `prefix` then `body` wrapped to `width`, with every row after the first indented by the
/// prefix's width, so the body reads as a column. When that column would be narrower than
/// `MIN_HANGING`, the two wrap together instead, which costs fewer rows.
pub fn hang(prefix: Vec<Span<'static>>, body: &Line<'static>, width: u16) -> Vec<Line<'static>> {
    let indent: usize = prefix.iter().map(|s| cells_width(&s.content)).sum();
    let Some(rest) = usize::from(width)
        .checked_sub(indent)
        .filter(|rest| *rest >= MIN_HANGING)
    else {
        let mut spans = prefix;
        spans.extend(body.spans.iter().cloned());
        return wrap_line(&Line::from(spans).style(body.style), width);
    };
    wrap_line(body, rest as u16)
        .into_iter()
        .enumerate()
        .map(|(i, row)| {
            let mut spans = if i == 0 {
                prefix.clone()
            } else {
                vec![Span::raw(" ".repeat(indent))]
            };
            spans.extend(row.spans);
            Line::from(spans).style(row.style)
        })
        .collect()
}

/// Wraps each line, pairing every row with whether it continues the row before it, so a
/// selection can rejoin a soft-wrapped line without inserting a line break.
pub fn wrap_rows(lines: &[Line<'static>], width: u16) -> Vec<(Line<'static>, bool)> {
    lines
        .iter()
        .flat_map(|l| {
            wrap_line(l, width)
                .into_iter()
                .enumerate()
                .map(|(i, row)| (row, i > 0))
        })
        .collect()
}

/// The parts of display columns `cols` of an unwrapped line that fall on each of `rows`, the
/// rows `wrap_line` made of that line, as (row index, columns within that row).
pub fn cols_on_rows(rows: &[Line<'static>], cols: &Range<usize>) -> Vec<(usize, Range<usize>)> {
    let mut parts = Vec::new();
    let mut at = 0;
    for (i, row) in rows.iter().enumerate() {
        let width: usize = row.spans.iter().map(|s| cells_width(&s.content)).sum();
        let (from, to) = (cols.start.max(at), cols.end.min(at + width));
        if from < to {
            parts.push((i, from - at..to - at));
        }
        at += width;
    }
    parts
}

/// The display columns `text` takes, measured the way `wrap_line` measures it: tabs as four
/// columns, then one width per grapheme cluster.
pub fn cells_width(text: &str) -> usize {
    text.replace('\t', "    ")
        .graphemes(true)
        .map(|g| g.width())
        .sum()
}

fn to_line(cells: &[(String, Style)], line_style: Style) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (g, style) in cells {
        match spans.last_mut() {
            Some(last) if last.style == *style => last.content.to_mut().push_str(g),
            _ => spans.push(Span::styled(g.clone(), *style)),
        }
    }
    Line::from(spans).style(line_style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;
    use ratatui::text::{Line, Span};
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    /// Byte offsets that fall on an extended-grapheme-cluster boundary in `s`, including 0 and
    /// `s.len()`. A wrap that never splits a cluster only ever cuts at one of these offsets.
    fn cluster_boundaries(s: &str) -> std::collections::BTreeSet<usize> {
        let mut boundaries = std::collections::BTreeSet::new();
        let mut pos = 0;
        boundaries.insert(pos);
        for g in s.graphemes(true) {
            pos += g.len();
            boundaries.insert(pos);
        }
        boundaries
    }

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

    #[test]
    fn never_splits_grapheme_clusters() {
        let family = "👨‍👩‍👧‍👦".repeat(5);
        let accented = "e\u{301}".repeat(5);
        for input in [family, accented] {
            let boundaries = cluster_boundaries(&input);
            for w in 2u16..=6 {
                let out = wrap_line(&Line::from(input.clone()), w);
                let joined: String = out
                    .iter()
                    .map(|l| {
                        l.spans
                            .iter()
                            .map(|s| s.content.as_ref())
                            .collect::<String>()
                    })
                    .collect();
                assert_eq!(joined, input, "width {w}");
                let mut pos = 0;
                for l in &out {
                    assert!(width(l) <= w as usize, "width {w}: {:?}", l);
                    let text: String = l
                        .spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>();
                    if let Some(c) = text.chars().next() {
                        assert_ne!(
                            c, '\u{301}',
                            "line starts with a combining mark at width {w}"
                        );
                        assert_ne!(c, '\u{200D}', "line starts with a ZWJ at width {w}");
                    }
                    pos += text.len();
                    assert!(
                        boundaries.contains(&pos),
                        "line boundary at byte {pos} splits a grapheme cluster at width {w}"
                    );
                }
            }
        }
    }

    #[test]
    fn tabs_count_as_four_columns() {
        let out = wrap_line(&Line::from("\t"), 10);
        assert_eq!(out.len(), 1);
        let text: String = out[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert_eq!(text, "    ");
        assert_eq!(width(&out[0]), 4);
    }
    #[test]
    fn wrap_rows_marks_continuations() {
        let lines = vec![
            Line::from("one two three four five six"),
            Line::from("short"),
        ];
        let rows = wrap_rows(&lines, 10);
        let marks: Vec<bool> = rows.iter().map(|(_, c)| *c).collect();
        assert_eq!(marks, vec![false, true, true, true, false]);
    }

    #[test]
    fn columns_split_across_the_rows_a_line_wraps_into() {
        let rows = wrap_line(&Line::from("alpha beta gamma"), 6);
        assert_eq!(
            cols_on_rows(&rows, &(3..12)),
            vec![(0, 3..6), (1, 0..5), (2, 0..1)]
        );
    }
}
