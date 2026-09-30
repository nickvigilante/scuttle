//! Pickers for the model, the workspace, and the organization, and the reasoning effort
//! slider, drawn above the composer.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use scuttle_core::app::{App, Picker};
use unicode_width::UnicodeWidthStr;
use uuid::Uuid;

use crate::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerChoice {
    Model(Uuid),
    Workspace(Option<Uuid>),
    Effort(String),
    Organization(Uuid),
    Cancel,
}

pub struct PickerState {
    kind: Picker,
    items: Vec<(String, Option<Uuid>)>,
    selected: usize,
    /// Items shown dimmed, one per item: organizations where the user cannot create chats.
    dimmed: Vec<bool>,
}

impl PickerState {
    pub fn open(kind: Picker, app: &App) -> PickerState {
        let items: Vec<(String, Option<Uuid>)> = match kind {
            // `enabled != Some(false)` matches App's own filter on `Msg::ModelsLoaded`;
            // repeating it here keeps the picker correct even if that changes.
            Picker::Model => app
                .models
                .iter()
                .filter(|m| m.enabled != Some(false))
                .filter_map(|m| {
                    Some((
                        m.display_name.clone().or_else(|| m.model.clone())?,
                        Some(m.id?),
                    ))
                })
                .collect(),
            Picker::Workspace => std::iter::once(("none (no workspace)".to_string(), None))
                .chain(app.workspaces.iter().map(|w| (w.name.clone(), Some(w.id))))
                .collect(),
            Picker::Effort => app.efforts().iter().map(|e| (e.clone(), None)).collect(),
            Picker::Organization => app
                .organizations
                .iter()
                .map(|o| {
                    let mut marks = Vec::new();
                    if o.is_default {
                        marks.push("default");
                    }
                    if !o.can_create_chats {
                        marks.push("no permission to create chats");
                    }
                    if Some(o.id) == app.org_id {
                        marks.push("current");
                    }
                    let label = if marks.is_empty() {
                        o.label().to_owned()
                    } else {
                        format!("{} ({})", o.label(), marks.join(", "))
                    };
                    (label, Some(o.id))
                })
                .collect(),
        };
        let selected = match kind {
            Picker::Effort => app
                .effort()
                .and_then(|current| items.iter().position(|(name, _)| *name == current))
                .unwrap_or(0),
            Picker::Organization => app
                .org_id
                .and_then(|current| items.iter().position(|(_, id)| *id == Some(current)))
                .unwrap_or(0),
            Picker::Model | Picker::Workspace => 0,
        };
        let dimmed = match kind {
            Picker::Organization => app
                .organizations
                .iter()
                .map(|o| !o.can_create_chats)
                .collect(),
            _ => vec![false; items.len()],
        };
        PickerState {
            kind,
            items,
            selected,
            dimmed,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<PickerChoice> {
        // The effort picker is a slider: Left and Right move it, and Up and Down do nothing.
        let slider = self.kind == Picker::Effort;
        match (key.code, slider) {
            (KeyCode::Esc, _) => Some(PickerChoice::Cancel),
            (KeyCode::Left, true) | (KeyCode::Up, false) => {
                self.selected = self.selected.saturating_sub(1);
                None
            }
            (KeyCode::Right, true) | (KeyCode::Down, false) => {
                if self.selected + 1 < self.items.len() {
                    self.selected += 1;
                }
                None
            }
            (KeyCode::Enter, _) => {
                let (name, id) = self.items.get(self.selected)?;
                Some(match self.kind {
                    Picker::Model => PickerChoice::Model((*id)?),
                    Picker::Workspace => PickerChoice::Workspace(*id),
                    Picker::Effort => PickerChoice::Effort(name.clone()),
                    Picker::Organization => PickerChoice::Organization((*id)?),
                })
            }
            _ => None,
        }
    }

    /// Which items are dimmed, one per item.
    #[cfg(test)]
    pub(crate) fn dimmed(&self) -> &[bool] {
        &self.dimmed
    }

    /// The rows the picker takes, borders included: the slider needs two, a list up to eight.
    pub fn height(&self) -> u16 {
        if self.kind == Picker::Effort { 4 } else { 10 }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let title = match self.kind {
            Picker::Effort => return self.render_slider(frame, area, theme),
            Picker::Model => " Model ",
            Picker::Workspace => " Workspace ",
            Picker::Organization => " Organization ",
        };
        let items: Vec<ListItem> = self
            .items
            .iter()
            .zip(&self.dimmed)
            .map(|((name, _), dim)| {
                let item = ListItem::new(name.clone());
                if *dim { item.style(theme.dim) } else { item }
            })
            .collect();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(theme.accent)
            .highlight_symbol("› ");
        let mut state = ListState::default().with_selected(Some(self.selected));
        frame.render_widget(Clear, area);
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn render_slider(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let levels: Vec<&str> = self.items.iter().map(|(name, _)| name.as_str()).collect();
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
    use scuttle_core::app::{Msg, WorkspaceRef};
    use scuttle_core::config::BusyBehavior;
    use serde_json::json;

    fn press(p: &mut PickerState, code: KeyCode) -> Option<PickerChoice> {
        p.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn model_picker_selects_with_arrows_and_enter() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        app.update(Msg::ModelsLoaded(
            serde_json::from_value(json!([
                {"id": a, "display_name": "A", "enabled": true, "reasoning_efforts": []},
                {"id": b, "display_name": "B", "enabled": true, "reasoning_efforts": []}
            ]))
            .unwrap(),
        ));
        let mut p = PickerState::open(Picker::Model, &app);
        press(&mut p, KeyCode::Down);
        assert_eq!(press(&mut p, KeyCode::Enter), Some(PickerChoice::Model(b)));
    }

    #[test]
    fn workspace_picker_offers_none_first_and_escape_cancels() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let ws = uuid::Uuid::new_v4();
        app.update(Msg::WorkspacesLoaded(vec![WorkspaceRef {
            id: ws,
            name: "dev".into(),
        }]));
        let mut p = PickerState::open(Picker::Workspace, &app);
        assert_eq!(
            press(&mut p, KeyCode::Enter),
            Some(PickerChoice::Workspace(None))
        );
        let mut p = PickerState::open(Picker::Workspace, &app);
        press(&mut p, KeyCode::Down);
        assert_eq!(
            press(&mut p, KeyCode::Enter),
            Some(PickerChoice::Workspace(Some(ws)))
        );
        let mut p = PickerState::open(Picker::Workspace, &app);
        assert_eq!(press(&mut p, KeyCode::Esc), Some(PickerChoice::Cancel));
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
        let mut p = PickerState::open(Picker::Effort, &app);
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
        let mut p = PickerState::open(Picker::Effort, &app);
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
        let p = PickerState::open(Picker::Effort, &app);
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

    #[test]
    fn organization_picker_marks_the_default_and_the_current_one() {
        use scuttle_core::app::OrgRef;
        let mut app = App::new(BusyBehavior::Queue, true);
        let (product, coder) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        app.update(Msg::OrganizationsLoaded(vec![
            OrgRef {
                id: product,
                name: "product".into(),
                display_name: "Product".into(),
                is_default: false,
                can_create_chats: true,
            },
            OrgRef {
                id: coder,
                name: "coder".into(),
                display_name: "Coder".into(),
                is_default: true,
                can_create_chats: true,
            },
        ]));
        app.update(Msg::Started {
            org_id: coder,
            open_chat: None,
        });
        let mut p = PickerState::open(Picker::Organization, &app);
        let names: Vec<&str> = p.items.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Product", "Coder (default, current)"]);
        assert_eq!(
            press(&mut p, KeyCode::Enter),
            Some(PickerChoice::Organization(coder)),
            "the picker starts on the current organization"
        );
    }

    #[test]
    fn an_organization_without_chat_permission_is_greyed_out_with_the_reason() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use scuttle_core::app::OrgRef;
        let mut app = App::new(BusyBehavior::Queue, true);
        let (product, coder) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        app.update(Msg::OrganizationsLoaded(vec![
            OrgRef {
                id: product,
                name: "product".into(),
                display_name: "Product".into(),
                is_default: false,
                can_create_chats: false,
            },
            OrgRef {
                id: coder,
                name: "coder".into(),
                display_name: "Coder".into(),
                is_default: true,
                can_create_chats: true,
            },
        ]));
        app.update(Msg::Started {
            org_id: coder,
            open_chat: None,
        });
        let p = PickerState::open(Picker::Organization, &app);
        let names: Vec<&str> = p.items.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "Product (no permission to create chats)",
                "Coder (default, current)"
            ]
        );
        assert_eq!(p.dimmed, [true, false]);
        let theme = Theme::terminal(true);
        let mut term = Terminal::new(TestBackend::new(50, 10)).unwrap();
        term.draw(|f| p.render(f, f.area(), &theme)).unwrap();
        let buf = term.backend().buffer();
        let row = (0..10u16)
            .find(|&y| {
                (0..50u16)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .contains("Product")
            })
            .unwrap();
        let x = (0..50u16).find(|&x| buf[(x, row)].symbol() == "P").unwrap();
        assert_eq!(Some(buf[(x, row)].fg), theme.dim.fg, "the row is dimmed");
    }

    #[test]
    fn model_picker_excludes_disabled_models() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        app.models = serde_json::from_value(json!([
            {"id": a, "display_name": "A", "enabled": true, "reasoning_efforts": []},
            {"id": b, "display_name": "B", "enabled": false, "reasoning_efforts": []}
        ]))
        .unwrap();
        let mut p = PickerState::open(Picker::Model, &app);
        assert_eq!(press(&mut p, KeyCode::Enter), Some(PickerChoice::Model(a)));
        assert_eq!(p.items.len(), 1, "disabled model should not be offered");
    }
}
