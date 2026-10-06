//! Mouse selection in the transcript. Positions are in transcript-line space, not screen
//! rows, so a selection stays on its text while the view scrolls or streams.

use ratatui::text::Line;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::icons;
use crate::transcript_view::View;
use crate::wrap::cells_width;

/// A cell of the transcript: a row of `View::lines` and a display column in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub line: usize,
    pub col: u16,
}

/// The cells between where a drag started and where the pointer is now, both included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Pos,
    pub head: Pos,
}

impl Selection {
    /// The first and last selected cell, in reading order.
    pub fn bounds(&self) -> (Pos, Pos) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// The columns `[start, end)` selected on row `line` of a transcript `width` wide. Rows
    /// between the first and last are selected edge to edge, as terminals do.
    pub fn columns(&self, line: usize, width: u16) -> Option<(u16, u16)> {
        let (start, end) = self.bounds();
        if line < start.line || line > end.line {
            return None;
        }
        let from = if line == start.line { start.col } else { 0 };
        let to = if line == end.line {
            end.col.saturating_add(1)
        } else {
            width
        };
        let to = to.min(width);
        (from < to).then_some((from, to))
    }
}

/// The selected text. Rows that continue a soft-wrapped line join without a line break,
/// every line loses its trailing spaces, and a rule row is left out along with the blank row
/// after it, so the gaps around it make one paragraph break.
pub fn selected_text(view: &View, selection: &Selection) -> String {
    let (start, end) = selection.bounds();
    let mut out = String::new();
    let Some(last) = view.lines.len().checked_sub(1).map(|l| l.min(end.line)) else {
        return out;
    };
    if start.line > last {
        return out;
    }
    let mut started = false;
    let mut after_rule = false;
    for line in start.line..=last {
        let meta = view.meta.get(line).copied().unwrap_or_default();
        let blank = view.lines[line]
            .spans
            .iter()
            .all(|s| s.content.trim().is_empty());
        if meta.rule || (after_rule && blank) {
            after_rule = meta.rule;
            continue;
        }
        after_rule = false;
        if started && !meta.continuation {
            trim_end(&mut out);
            out.push('\n');
        }
        started = true;
        let from = if line == start.line { start.col } else { 0 };
        let mut to = (line == end.line).then_some(end.col);
        if meta.hint > 0 {
            // A click hint at the row's end shows but is not part of the text.
            let shown = view.lines[line].to_string();
            let content = (cells_width(shown.trim_end()) as u16).saturating_sub(meta.hint);
            let last = content.saturating_sub(1);
            to = Some(to.map_or(last, |t| t.min(last)));
            if content == 0 || from >= content {
                continue;
            }
        }
        out.push_str(&cells(&view.lines[line], from, to, view.nerd_icons));
    }
    trim_end(&mut out);
    out
}

fn trim_end(s: &mut String) {
    let keep = s.trim_end_matches(' ').len();
    s.truncate(keep);
}

/// The graphemes of `line` whose first column lies in `from..=to`, where `None` means the end.
/// As ratatui draws them, a grapheme with a control character or no width takes no cell and
/// is not copied, and no format character is copied, so the copy holds what the cells show. With `nerd_icons`, an icon's
/// slot, its glyph and the space after it, copies as what text mode shows there.
fn cells(line: &Line, from: u16, to: Option<u16>, nerd_icons: bool) -> String {
    let mut col: u16 = 0;
    let mut out = String::new();
    let mut slot_space = false;
    for span in &line.spans {
        let drawn = span
            .content
            .graphemes(true)
            .filter(|g| !g.contains(char::is_control) && g.width() > 0);
        for g in drawn {
            // A format character inside a cluster, such as a joiner, takes no cell of its own.
            let g: String = g
                .chars()
                .filter(|&c| !scuttle_core::text::is_format(c))
                .collect();
            let g = g.as_str();
            let shown = col >= from && to.is_none_or(|to| col <= to);
            col = col.saturating_add(g.width() as u16);
            if std::mem::take(&mut slot_space) && g == " " {
                continue;
            }
            if !shown {
                continue;
            }
            match nerd_icons.then(|| icons::text_for_glyph(g)).flatten() {
                Some(text) => {
                    out.push_str(text);
                    slot_space = true;
                }
                None => out.push_str(g),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript_view::LineMeta;
    use ratatui::text::{Line, Span};

    fn view(rows: &[(&str, bool)]) -> View {
        View {
            lines: rows
                .iter()
                .map(|(t, _)| Line::from(t.to_string()))
                .collect(),
            meta: rows
                .iter()
                .map(|(_, c)| LineMeta {
                    continuation: *c,
                    ..LineMeta::default()
                })
                .collect(),
            ..View::default()
        }
    }

    fn sel(a: (usize, u16), b: (usize, u16)) -> Selection {
        Selection {
            anchor: Pos {
                line: a.0,
                col: a.1,
            },
            head: Pos {
                line: b.0,
                col: b.1,
            },
        }
    }

    #[test]
    fn soft_wrapped_rows_rejoin_and_hard_lines_break() {
        let v = view(&[("one two ", false), ("three", true), ("next   ", false)]);
        assert_eq!(
            selected_text(&v, &sel((0, 0), (2, 3))),
            "one two three\nnext"
        );
        assert_eq!(
            selected_text(&v, &sel((2, 3), (0, 4))),
            "two three\nnext",
            "backwards"
        );
    }

    #[test]
    fn columns_cover_the_selected_cells() {
        let s = sel((1, 4), (3, 2));
        assert_eq!(s.columns(0, 20), None);
        assert_eq!(s.columns(1, 20), Some((4, 20)));
        assert_eq!(s.columns(2, 20), Some((0, 20)));
        assert_eq!(s.columns(3, 20), Some((0, 3)));
        assert_eq!(s.columns(4, 20), None);
    }

    #[test]
    fn wide_characters_are_taken_whole() {
        let v = View {
            lines: vec![Line::from(vec![Span::raw("a中b")])],
            meta: vec![LineMeta::default()],
            ..View::default()
        };
        assert_eq!(selected_text(&v, &sel((0, 1), (0, 2))), "中");
        assert_eq!(
            selected_text(&v, &sel((0, 2), (0, 3))),
            "b",
            "starts inside the wide cell"
        );
    }

    #[test]
    fn a_selection_past_the_last_row_is_clamped() {
        let v = view(&[("only", false)]);
        assert_eq!(selected_text(&v, &sel((0, 0), (9, 9))), "only");
        assert_eq!(selected_text(&v, &sel((5, 0), (9, 9))), "");
    }

    #[test]
    fn rule_rows_are_left_out() {
        let mut v = view(&[
            ("work", false),
            ("", false),
            ("────", false),
            ("answer", false),
        ]);
        v.meta[2].rule = true;
        assert_eq!(selected_text(&v, &sel((0, 0), (3, 9))), "work\n\nanswer");
        assert_eq!(
            selected_text(&v, &sel((2, 1), (3, 9))),
            "answer",
            "a selection starting on the rule"
        );
        assert_eq!(selected_text(&v, &sel((2, 0), (2, 3))), "", "only the rule");
        let mut v = view(&[
            ("work", false),
            ("", false),
            ("────", false),
            ("", false),
            ("answer", false),
        ]);
        v.meta[2].rule = true;
        assert_eq!(
            selected_text(&v, &sel((0, 0), (4, 9))),
            "work\n\nanswer",
            "the rule and its gaps become one paragraph break"
        );
    }
}
