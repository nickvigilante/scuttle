//! Pickers for the model, the effort, the workspace, and the organization, drawn above the
//! composer.

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
    Effort(String),
    Organization(Uuid),
    Cancel,
}

pub struct PickerState {
    kind: Picker,
    items: Vec<(String, Option<Uuid>)>,
    selected: usize,
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
                .effort_label()
                .and_then(|current| items.iter().position(|(name, _)| *name == current))
                .unwrap_or(0),
            Picker::Organization => app
                .org_id
                .and_then(|current| items.iter().position(|(_, id)| *id == Some(current)))
                .unwrap_or(0),
            Picker::Model | Picker::Workspace => 0,
        };
        PickerState {
            kind,
            items,
            selected,
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

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let title = match self.kind {
            Picker::Model => " Model ",
            Picker::Workspace => " Workspace ",
            Picker::Effort => " Reasoning effort ",
            Picker::Organization => " Organization ",
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
    fn effort_picker_lists_the_efforts_and_starts_on_the_current_one() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::ModelsLoaded(
            serde_json::from_value(json!([
                {"id": uuid::Uuid::new_v4(), "display_name": "Thinker", "enabled": true, "is_default": true, "reasoning_efforts": ["low", "medium", "high"]}
            ]))
            .unwrap(),
        ));
        app.update(Msg::EffortChosen("medium".into()));
        let mut p = PickerState::open(Picker::Effort, &app);
        let names: Vec<&str> = p.items.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["low", "medium", "high"]);
        press(&mut p, KeyCode::Down);
        assert_eq!(
            press(&mut p, KeyCode::Enter),
            Some(PickerChoice::Effort("high".into()))
        );
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
            },
            OrgRef {
                id: coder,
                name: "coder".into(),
                display_name: "Coder".into(),
                is_default: true,
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
