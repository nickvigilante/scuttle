//! The reasoning effort slider, drawn above the composer. The model, workspace, and
//! organization pickers are table overlays in overlay.rs.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use scuttle_core::app::App;
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerChoice {
    Effort(String),
    Cancel,
}

pub struct PickerState {
    /// The model's effort levels, lowest first.
    items: Vec<String>,
    selected: usize,
}

impl PickerState {
    /// The slider for the current model's efforts, starting on the effort being sent.
    pub fn open(app: &App) -> PickerState {
        let items = app.efforts().to_vec();
        let selected = app
            .effort()
            .and_then(|current| items.iter().position(|name| *name == current))
            .unwrap_or(0);
        PickerState { items, selected }
    }

    /// Left and Right move the slider; Up and Down do nothing.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<PickerChoice> {
        match key.code {
            KeyCode::Esc => Some(PickerChoice::Cancel),
            KeyCode::Left => {
                self.selected = self.selected.saturating_sub(1);
                None
            }
            KeyCode::Right => {
                if self.selected + 1 < self.items.len() {
                    self.selected += 1;
                }
                None
            }
            KeyCode::Enter => Some(PickerChoice::Effort(self.items.get(self.selected)?.clone())),
            _ => None,
        }
    }

    /// The rows the slider takes, borders included.
    pub fn height(&self) -> u16 {
        4
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let levels: Vec<&str> = self.items.iter().map(String::as_str).collect();
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Reasoning effort ")
            .title_bottom(Line::from(Span::styled(
                " Left/Right, Enter saves, Esc cancels ",
                theme.dim,
            )));
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(slider_rows(&levels, self.selected, theme)).block(block),
            area,
        );
    }
}

/// The slider's two rows: the effort names, and under them a track with a stop centered under
/// each name, filled at `selected`.
pub fn slider_rows(levels: &[&str], selected: usize, theme: &Theme) -> Vec<Line<'static>> {
    const GAP: usize = 3;
    let mut names = Vec::new();
    let mut track = Vec::new();
    for (i, level) in levels.iter().enumerate() {
        if i > 0 {
            names.push(Span::raw(" ".repeat(GAP)));
            track.push(Span::styled("─".repeat(GAP), theme.dim));
        }
        let width = level.width().max(1);
        let left = (width - 1) / 2;
        let (name_style, stop) = if i == selected {
            (theme.accent, Span::styled("●", theme.accent))
        } else {
            (theme.dim, Span::styled("○", theme.dim))
        };
        names.push(Span::styled((*level).to_owned(), name_style));
        track.push(Span::styled("─".repeat(left), theme.dim));
        track.push(stop);
        track.push(Span::styled("─".repeat(width - 1 - left), theme.dim));
    }
    vec![Line::from(names), Line::from(track)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use scuttle_core::app::Msg;
    use scuttle_core::config::BusyBehavior;
    use serde_json::json;

    fn press(p: &mut PickerState, code: KeyCode) -> Option<PickerChoice> {
        p.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn with_models(efforts: serde_json::Value) -> App {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::ModelsLoaded(
            serde_json::from_value(json!([
                {"id": uuid::Uuid::new_v4(), "display_name": "Thinker", "enabled": true, "is_default": true, "reasoning_efforts": efforts, "model_config": {"reasoning_effort": {"default": "medium"}}}
            ]))
            .unwrap(),
        ));
        app
    }

    fn row_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn the_effort_slider_moves_with_left_and_right_and_ignores_up_and_down() {
        let app = with_models(json!(["low", "medium", "high"]));
        let mut p = PickerState::open(&app);
        assert_eq!(p.selected, 1, "the slider starts on the effort being sent");
        assert_eq!(press(&mut p, KeyCode::Up), None);
        assert_eq!(press(&mut p, KeyCode::Down), None);
        assert_eq!(p.selected, 1, "Up and Down do not move the slider");
        press(&mut p, KeyCode::Right);
        press(&mut p, KeyCode::Right);
        assert_eq!(p.selected, 2, "the slider stops at the highest effort");
        press(&mut p, KeyCode::Left);
        press(&mut p, KeyCode::Left);
        press(&mut p, KeyCode::Left);
        assert_eq!(p.selected, 0, "and at the lowest");
        assert_eq!(
            press(&mut p, KeyCode::Enter),
            Some(PickerChoice::Effort("low".into()))
        );
        assert_eq!(press(&mut p, KeyCode::Esc), Some(PickerChoice::Cancel));
    }

    #[test]
    fn the_slider_marks_the_chosen_level_under_its_name() {
        let theme = Theme::terminal(true);
        let rows = slider_rows(&["low", "medium", "high"], 1, &theme);
        let (names, track) = (row_text(&rows[0]), row_text(&rows[1]));
        assert_eq!(names, "low   medium   high");
        let column = |s: &str, needle: &str| s[..s.find(needle).unwrap()].width();
        let dot = column(&track, "●");
        let medium = column(&names, "medium");
        assert!(
            (medium..medium + "medium".len()).contains(&dot),
            "{names}\n{track}"
        );
        assert_eq!(track.matches('○').count(), 2, "{track}");
        assert_eq!(
            track.width(),
            names.width(),
            "the track runs under every name"
        );
    }

    #[test]
    fn a_one_level_slider_stays_put_and_saves_it() {
        let app = with_models(json!(["high"]));
        let mut p = PickerState::open(&app);
        assert_eq!(press(&mut p, KeyCode::Left), None);
        assert_eq!(press(&mut p, KeyCode::Right), None);
        assert_eq!(p.selected, 0);
        let rows = slider_rows(&["high"], 0, &Theme::terminal(true));
        assert_eq!(row_text(&rows[1]), "─●──");
        assert_eq!(
            press(&mut p, KeyCode::Enter),
            Some(PickerChoice::Effort("high".into()))
        );
    }

    #[test]
    fn the_slider_draws_in_a_short_box_with_its_keys() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let app = with_models(json!(["low", "medium", "high"]));
        let p = PickerState::open(&app);
        assert_eq!(p.height(), 4);
        let mut term = Terminal::new(TestBackend::new(50, 4)).unwrap();
        term.draw(|f| p.render(f, f.area(), &Theme::terminal(true)))
            .unwrap();
        let buf = term.backend().buffer();
        let rows: Vec<String> = (0..4)
            .map(|y| (0..50).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        assert!(rows[0].contains("Reasoning effort"), "{rows:?}");
        assert!(rows[1].contains("low   medium   high"), "{rows:?}");
        assert!(rows[2].contains('●'), "{rows:?}");
        assert!(rows[3].contains("Enter saves"), "{rows:?}");
        for width in [8u16, 20] {
            let mut term = Terminal::new(TestBackend::new(width, 4)).unwrap();
            term.draw(|f| p.render(f, f.area(), &Theme::terminal(true)))
                .unwrap();
            let buf = term.backend().buffer();
            let has_corner = (0..width).any(|x| {
                let s = buf[(x, 0)].symbol();
                s == "┌" || s == "┐"
            });
            assert!(has_corner, "no box corner drawn at width {width}");
        }
    }
}
