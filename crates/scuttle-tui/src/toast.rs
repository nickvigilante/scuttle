//! The toast: one line in the top-right corner of the transcript that names a chat whose turn
//! ended while another chat was open. It covers what is under it and takes no rows.

use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use scuttle_core::alerts::{ChatAlert, Outcome};
use scuttle_core::config::IconSet;

use crate::icons::{self, Icon};
use crate::overlay::ellipsize;
use crate::theme::Theme;
use crate::wrap::cells_width;

/// How long a toast shows, counted from the first draw that shows it.
pub const TOAST_TTL: Duration = Duration::from_secs(5);

/// The toast on screen. A newer alert replaces it and starts its own lifetime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub alert: ChatAlert,
    /// When the toast was first drawn; `None` until then.
    pub since: Option<Instant>,
}

impl Toast {
    pub fn new(alert: ChatAlert) -> Toast {
        Toast { alert, since: None }
    }

    /// Starts the lifetime at the first draw, and returns whether the toast still shows at `now`.
    pub fn live_at(&mut self, now: Instant) -> bool {
        let since = *self.since.get_or_insert(now);
        now.saturating_duration_since(since) < TOAST_TTL
    }

    /// When the toast goes, once it has been drawn.
    pub fn deadline(&self) -> Option<Instant> {
        self.since.map(|since| since + TOAST_TTL)
    }
}

/// The toast's mark, words after the title, and style for `outcome`. The mark keeps a fixed
/// slot: a Nerd Font glyph and its space where `set` draws one, else a character and a space.
fn parts(outcome: Outcome, set: IconSet, theme: &Theme) -> (&'static str, &'static str, Style) {
    let glyph = |icon, text| match set {
        IconSet::Nerd => icons::slot(set, icon).text,
        IconSet::Text => text,
    };
    match outcome {
        Outcome::Finished => ("\u{2713} ", "", theme.ok),
        Outcome::NeedsAnswer => (glyph(Icon::Asking, "? "), " needs an answer", theme.warn),
        Outcome::Failed => (glyph(Icon::Failed, "\u{2717} "), " failed", theme.error),
    }
}

/// The toast's line, at most `max` cells wide: the mark, the title cut with an ellipsis where
/// it does not fit, and the words after it, with a space of padding at each end.
pub fn line(alert: &ChatAlert, theme: &Theme, max: usize) -> Line<'static> {
    let (mark, after, style) = parts(alert.outcome, theme.icons, theme);
    let fixed = 2 + cells_width(mark) + cells_width(after);
    let title = ellipsize(&alert.title, max.saturating_sub(fixed));
    let mark_style = icons::style(theme, style);
    let mut spans = vec![
        Span::styled(" ", style),
        Span::styled(mark, mark_style),
        Span::styled(format!("{title}{after} "), style),
    ];
    // A box too narrow for the mark and the words keeps only what fits.
    let mut used = 0;
    spans.retain_mut(|s| {
        let w = cells_width(&s.content);
        if used + w <= max {
            used += w;
            true
        } else {
            let room = max - used;
            used = max;
            s.content = ellipsize(&s.content, room).into();
            room > 0
        }
    });
    Line::from(spans)
}

/// The widest a toast grows: half of `area`, but never under 24 cells while `area` has them.
fn max_width(area: Rect) -> u16 {
    (area.width / 2).max(24).min(area.width)
}

/// Where a toast of `line` goes: the top row of `area`, flush with its right edge.
pub fn rect(area: Rect, line: &Line) -> Rect {
    let width = (line.width() as u16).min(area.width);
    Rect {
        x: area.right().saturating_sub(width),
        y: area.y,
        width,
        height: area.height.min(1),
    }
}

/// Draws `toast` over the top-right corner of `area`, whatever is there.
pub fn render(f: &mut Frame, area: Rect, toast: &Toast, theme: &Theme) {
    let line = line(&toast.alert, theme, usize::from(max_width(area)));
    let at = rect(area, &line);
    if at.is_empty() {
        return;
    }
    f.render_widget(Clear, at);
    // Reversed, so the toast reads as a box over the transcript in any color scheme.
    f.render_widget(
        Paragraph::new(line).style(Style::default().add_modifier(Modifier::REVERSED)),
        at,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alert(title: &str, outcome: Outcome) -> ChatAlert {
        ChatAlert {
            chat_id: uuid::Uuid::nil(),
            title: title.into(),
            outcome,
            open: false,
        }
    }

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn each_outcome_reads_differently() {
        let theme = Theme::terminal(true);
        let fix = "Fix the swagger annotations";
        assert_eq!(
            text(&line(&alert(fix, Outcome::Finished), &theme, 80)),
            " \u{2713} Fix the swagger annotations "
        );
        assert_eq!(
            text(&line(&alert(fix, Outcome::NeedsAnswer), &theme, 80)),
            " ? Fix the swagger annotations needs an answer "
        );
        assert_eq!(
            text(&line(&alert(fix, Outcome::Failed), &theme, 80)),
            " \u{2717} Fix the swagger annotations failed "
        );
    }

    #[test]
    fn nerd_icons_take_the_same_two_cell_slot() {
        let theme = Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal(true)
        };
        for outcome in [Outcome::Finished, Outcome::NeedsAnswer, Outcome::Failed] {
            let (mark, _, _) = parts(outcome, IconSet::Nerd, &theme);
            assert_eq!(cells_width(mark), 2, "{outcome:?}");
        }
        assert!(text(&line(&alert("x", Outcome::Failed), &theme, 80)).contains('\u{f421}'));
    }

    #[test]
    fn a_long_title_is_cut_by_display_width_and_keeps_its_words() {
        let theme = Theme::terminal(true);
        let wide = "修正".repeat(20);
        let l = line(&alert(&wide, Outcome::NeedsAnswer), &theme, 30);
        assert!(l.width() <= 30, "{} cells: {}", l.width(), text(&l));
        let shown = text(&l);
        assert!(shown.contains('\u{2026}'), "{shown}");
        assert!(shown.ends_with(" needs an answer "), "{shown}");
        let tiny = line(&alert(&wide, Outcome::NeedsAnswer), &theme, 6);
        assert!(tiny.width() <= 6, "{}", text(&tiny));
    }

    #[test]
    fn the_toast_sits_on_the_top_row_flush_right() {
        let area = Rect::new(1, 2, 60, 10);
        let l = Line::from("1234567890");
        assert_eq!(rect(area, &l), Rect::new(51, 2, 10, 1));
        assert_eq!(max_width(area), 30);
        assert_eq!(max_width(Rect::new(0, 0, 20, 5)), 20);
    }

    #[test]
    fn the_lifetime_starts_at_the_first_draw_and_lasts_five_seconds() {
        let mut toast = Toast::new(alert("x", Outcome::Finished));
        assert_eq!(toast.deadline(), None);
        let now = Instant::now();
        assert!(toast.live_at(now));
        assert_eq!(toast.deadline(), Some(now + TOAST_TTL));
        assert!(toast.live_at(now + Duration::from_millis(4999)));
        assert!(!toast.live_at(now + TOAST_TTL));
    }
}
