//! Pickers for the model and the workspace, drawn above the composer.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};
use scuttle_core::app::{App, Picker};
use uuid::Uuid;

use crate::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerChoice {
    Model(Uuid),
    Workspace(Option<Uuid>),
    Cancel,
}

pub struct PickerState {
    kind: Picker,
    items: Vec<(String, Option<Uuid>)>,
    selected: usize,
}

impl PickerState {
    pub fn open(kind: Picker, app: &App) -> PickerState {
        let items = match kind {
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
        };
        PickerState {
            kind,
            items,
            selected: 0,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<PickerChoice> {
        match key.code {
            KeyCode::Esc => Some(PickerChoice::Cancel),
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                None
            }
            KeyCode::Down => {
                if self.selected + 1 < self.items.len() {
                    self.selected += 1;
                }
                None
            }
            KeyCode::Enter => {
                let (_, id) = self.items.get(self.selected)?;
                Some(match self.kind {
                    Picker::Model => PickerChoice::Model((*id)?),
                    Picker::Workspace => PickerChoice::Workspace(*id),
                })
            }
            _ => None,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let title = match self.kind {
            Picker::Model => " Model ",
            Picker::Workspace => " Workspace ",
        };
        let items: Vec<ListItem> = self
            .items
            .iter()
            .map(|(name, _)| ListItem::new(name.clone()))
            .collect();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(theme.accent)
            .highlight_symbol("› ");
        let mut state = ListState::default().with_selected(Some(self.selected));
        frame.render_widget(Clear, area);
        frame.render_stateful_widget(list, area, &mut state);
    }
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
