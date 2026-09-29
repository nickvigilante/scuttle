//! The headless application: every input is a `Msg`, every side effect is an `Effect`.

use std::time::Duration;

use coder_sdk::{ChatStatus, StreamEvent, types};
use uuid::Uuid;

use crate::commands::{self, Command};
use crate::config::BusyBehavior;
use crate::density::DisplayPrefs;
use crate::live::Applied;
use crate::transcript::Transcript;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRef {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    Info(String),
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Connection {
    #[default]
    Idle,
    Connecting,
    Live,
    Reconnecting {
        attempt: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picker {
    Model,
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModelsState {
    #[default]
    Loading,
    Loaded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyTarget {
    LastMessage,
    CodeBlock(usize),
}

#[derive(Debug)]
#[expect(clippy::large_enum_variant)]
pub enum Msg {
    Started {
        org_id: Uuid,
        open_chat: Option<Uuid>,
    },
    ChatLoaded {
        chat: Box<types::CodersdkChat>,
        messages: Vec<types::CodersdkChatMessage>,
    },
    ChatCreated(Box<types::CodersdkChat>),
    /// The runtime sends this when `Effect::CreateChat` fails.
    CreateFailed {
        message: String,
    },
    Stream(StreamEvent),
    StreamEnded {
        error: Option<String>,
    },
    PrefsLoaded(DisplayPrefs),
    ModelsLoaded(Vec<types::CodersdkChatModel>),
    /// The runtime sends this when `Effect::FetchModels` fails.
    ModelsFailed {
        message: String,
    },
    WorkspacesLoaded(Vec<WorkspaceRef>),
    ModelChosen(Uuid),
    WorkspaceChosen(Option<Uuid>),
    ApiFailed {
        action: &'static str,
        message: String,
    },
    Submit(String),
    Command(Command),
    Interrupt,
    /// A background action finished with nothing to update; the next stream event carries the result.
    Refresh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    LoadChat(Uuid),
    OpenStream {
        chat: Uuid,
        after_id: Option<i64>,
    },
    ReconnectAfter {
        chat: Uuid,
        after_id: Option<i64>,
        delay: Duration,
    },
    CreateChat {
        org: Uuid,
        text: String,
        model: Option<Uuid>,
        workspace: Option<Uuid>,
    },
    SendMessage {
        chat: Uuid,
        text: String,
        model: Option<Uuid>,
        busy: BusyBehavior,
    },
    Interrupt(Uuid),
    Compact(Uuid),
    Clear(Uuid),
    SetWorkspace {
        chat: Uuid,
        workspace: Option<Uuid>,
    },
    FetchPrefs,
    FetchModels(Uuid),
    FetchWorkspaces,
    ShowPicker(Picker),
    ShowHelp,
    Copy(CopyTarget),
    SetMouse(bool),
    /// Puts text back in the composer after a failed chat creation.
    RestoreComposer(String),
    Quit,
}

/// Reconnect delay: 500 ms doubling per attempt, capped at 10 s. The runtime adds jitter.
pub fn backoff(attempt: u32) -> Duration {
    let exp = attempt.saturating_sub(1).min(10);
    Duration::from_millis((500u64 << exp).min(10_000))
}

#[derive(Debug, Default)]
pub struct App {
    pub org_id: Option<Uuid>,
    pub chat_id: Option<Uuid>,
    pub chat: Option<Box<types::CodersdkChat>>,
    pub transcript: Transcript,
    pub prefs: DisplayPrefs,
    pub models: Vec<types::CodersdkChatModel>,
    pub workspaces: Vec<WorkspaceRef>,
    pub selected_model: Option<Uuid>,
    pub selected_workspace: Option<Uuid>,
    pub notices: Vec<Notice>,
    pub connection: Connection,
    pub busy: BusyBehavior,
    pub mouse: bool,
    pub models_state: ModelsState,
    /// The text of the message sent with the in-flight `Effect::CreateChat`, if any.
    creating: Option<String>,
    pending_text: Option<String>,
    reconnect_attempt: u32,
}

impl App {
    pub fn new(busy: BusyBehavior, mouse: bool) -> App {
        App {
            busy,
            mouse,
            ..App::default()
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(
            self.transcript.status,
            Some(ChatStatus::Running | ChatStatus::Interrupting | ChatStatus::RequiresAction)
        )
    }

    /// Whether `Msg::Interrupt` should send `Effect::Interrupt`. Unlike `is_running`, this
    /// excludes `Interrupting` since an interrupt is already in flight.
    pub fn can_interrupt(&self) -> bool {
        matches!(
            self.transcript.status,
            Some(ChatStatus::Running | ChatStatus::RequiresAction)
        )
    }

    /// The display name of the model the next message will use.
    pub fn model_name(&self) -> Option<String> {
        let id = self.selected_model.or_else(|| {
            self.models
                .iter()
                .find(|m| m.is_default == Some(true))
                .and_then(|m| m.id)
        })?;
        let model = self.models.iter().find(|m| m.id == Some(id))?;
        model.display_name.clone().or_else(|| model.model.clone())
    }

    fn info(&mut self, text: impl Into<String>) {
        self.notices.push(Notice::Info(text.into()));
    }

    fn error(&mut self, text: impl Into<String>) {
        self.notices.push(Notice::Error(text.into()));
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Started { org_id, open_chat } => {
                self.org_id = Some(org_id);
                let mut effects = vec![
                    Effect::FetchPrefs,
                    Effect::FetchModels(org_id),
                    Effect::FetchWorkspaces,
                ];
                if let Some(id) = open_chat {
                    self.connection = Connection::Connecting;
                    effects.push(Effect::LoadChat(id));
                }
                effects
            }
            Msg::ChatLoaded { chat, messages } => {
                let Some(id) = chat.id else {
                    self.error("The server returned a chat without an id.");
                    self.connection = Connection::Idle;
                    return vec![];
                };
                self.selected_model = self.selected_model.or(chat.last_model_config_id);
                self.selected_workspace = chat.workspace_id;
                self.chat_id = Some(id);
                self.chat = Some(chat);
                self.transcript.load(messages);
                self.connection = Connection::Connecting;
                vec![Effect::OpenStream {
                    chat: id,
                    after_id: self.transcript.last_message_id(),
                }]
            }
            Msg::ChatCreated(chat) => {
                let Some(id) = chat.id else {
                    return self.fail_create("the server returned a chat without an id".into());
                };
                let workspace_mismatch = self.selected_workspace != chat.workspace_id;
                self.creating = None;
                self.chat_id = Some(id);
                self.chat = Some(chat);
                self.connection = Connection::Connecting;
                let mut effects = vec![Effect::OpenStream {
                    chat: id,
                    after_id: None,
                }];
                if workspace_mismatch {
                    effects.push(Effect::SetWorkspace {
                        chat: id,
                        workspace: self.selected_workspace,
                    });
                }
                if let Some(text) = self.pending_text.take() {
                    effects.push(Effect::SendMessage {
                        chat: id,
                        text,
                        model: self.selected_model,
                        busy: self.busy,
                    });
                }
                effects
            }
            Msg::CreateFailed { message } => self.fail_create(message),
            Msg::Stream(ev) => match self.transcript.apply(&ev) {
                Applied::Reconnect(_) => match self.chat_id {
                    Some(chat) => {
                        self.reconnect_attempt += 1;
                        self.transcript.live.clear();
                        self.connection = Connection::Reconnecting {
                            attempt: self.reconnect_attempt,
                        };
                        let delay = if self.reconnect_attempt == 1 {
                            Duration::ZERO
                        } else {
                            backoff(self.reconnect_attempt)
                        };
                        vec![Effect::ReconnectAfter {
                            chat,
                            after_id: self.transcript.last_message_id(),
                            delay,
                        }]
                    }
                    None => vec![],
                },
                _ => {
                    self.connection = Connection::Live;
                    self.reconnect_attempt = 0;
                    vec![]
                }
            },
            Msg::StreamEnded { .. } => {
                let Some(chat) = self.chat_id else {
                    return vec![];
                };
                self.reconnect_attempt += 1;
                self.transcript.live.clear();
                self.connection = Connection::Reconnecting {
                    attempt: self.reconnect_attempt,
                };
                vec![Effect::ReconnectAfter {
                    chat,
                    after_id: self.transcript.last_message_id(),
                    delay: backoff(self.reconnect_attempt),
                }]
            }
            Msg::PrefsLoaded(prefs) => {
                self.prefs = prefs;
                vec![]
            }
            Msg::ModelsLoaded(models) => {
                self.models = models
                    .into_iter()
                    .filter(|m| m.enabled != Some(false))
                    .collect();
                self.models_state = ModelsState::Loaded;
                vec![]
            }
            Msg::ModelsFailed { message } => {
                self.models_state = ModelsState::Failed;
                self.error(format!("Could not load models: {message}"));
                vec![]
            }
            Msg::WorkspacesLoaded(workspaces) => {
                self.workspaces = workspaces;
                vec![]
            }
            Msg::ModelChosen(id) => {
                self.selected_model = Some(id);
                if let Some(name) = self.model_name() {
                    self.info(format!("Model set to {name}"));
                }
                vec![]
            }
            Msg::WorkspaceChosen(ws) => self.set_workspace(ws),
            Msg::ApiFailed { action, message } => {
                self.error(format!("Could not {action}: {message}"));
                vec![]
            }
            Msg::Submit(text) => self.submit(text),
            Msg::Command(cmd) => self.command(cmd),
            Msg::Interrupt => match self.chat_id {
                Some(chat) if self.can_interrupt() => vec![Effect::Interrupt(chat)],
                _ => vec![],
            },
            Msg::Refresh => vec![],
        }
    }

    /// Restores whatever text was typed for the failed `Effect::CreateChat` (and anything
    /// queued behind it) to the composer, and records the failure.
    fn fail_create(&mut self, message: String) -> Vec<Effect> {
        let in_flight = self.creating.take().unwrap_or_default();
        let restored = match self.pending_text.take() {
            Some(pending) => format!("{in_flight}\n\n{pending}"),
            None => in_flight,
        };
        self.error(format!("Could not create the chat: {message}"));
        if restored.is_empty() {
            vec![]
        } else {
            vec![Effect::RestoreComposer(restored)]
        }
    }

    fn submit(&mut self, text: String) -> Vec<Effect> {
        let text = text.trim().to_owned();
        if text.is_empty() {
            return vec![];
        }
        if text.starts_with('/') {
            return match commands::parse(&text) {
                Ok(cmd) => self.command(cmd),
                Err(e) => {
                    self.error(e);
                    vec![]
                }
            };
        }
        if let Some(chat) = self.chat_id {
            return vec![Effect::SendMessage {
                chat,
                text,
                model: self.selected_model,
                busy: self.busy,
            }];
        }
        if self.creating.is_some() {
            self.pending_text = Some(match self.pending_text.take() {
                Some(prev) => format!("{prev}\n\n{text}"),
                None => text,
            });
            self.info("Waiting for the chat to be created; your message will follow.");
            return vec![];
        }
        let Some(org) = self.org_id else {
            self.error("Not connected to Coder yet.");
            return vec![];
        };
        self.creating = Some(text.clone());
        vec![Effect::CreateChat {
            org,
            text,
            model: self.selected_model,
            workspace: self.selected_workspace,
        }]
    }

    fn set_workspace(&mut self, ws: Option<Uuid>) -> Vec<Effect> {
        self.selected_workspace = ws;
        match self.chat_id {
            Some(chat) => vec![Effect::SetWorkspace {
                chat,
                workspace: ws,
            }],
            None => vec![],
        }
    }

    fn command(&mut self, cmd: Command) -> Vec<Effect> {
        match cmd {
            Command::Model(_) if self.models_state == ModelsState::Loading => {
                self.info("Models are still loading.");
                vec![]
            }
            Command::Model(_) if self.models_state == ModelsState::Failed => {
                let Some(org) = self.org_id else {
                    self.info("Models are still loading.");
                    return vec![];
                };
                self.models_state = ModelsState::Loading;
                self.info("Retrying the model list.");
                vec![Effect::FetchModels(org)]
            }
            Command::Model(None) => vec![Effect::ShowPicker(Picker::Model)],
            Command::Model(Some(name)) => {
                let wanted = name.to_lowercase();
                let found = self.models.iter().find(|m| {
                    m.display_name.as_deref().map(str::to_lowercase) == Some(wanted.clone())
                        || m.model.as_deref().map(str::to_lowercase) == Some(wanted.clone())
                });
                match found.and_then(|m| m.id) {
                    Some(id) => self.update(Msg::ModelChosen(id)),
                    None => {
                        self.error(format!("No enabled model named {name:?}"));
                        vec![]
                    }
                }
            }
            Command::Workspace(None) => vec![Effect::ShowPicker(Picker::Workspace)],
            Command::Workspace(Some(name)) if name == "none" => self.set_workspace(None),
            Command::Workspace(Some(name)) => match self
                .workspaces
                .iter()
                .find(|w| w.name == name)
                .map(|w| w.id)
            {
                Some(id) => self.set_workspace(Some(id)),
                None => {
                    self.error(format!("No workspace named {name:?}"));
                    vec![]
                }
            },
            Command::Compact | Command::Clear if self.creating.is_some() => {
                self.info("The chat is still being created.");
                vec![]
            }
            Command::Compact | Command::Clear => match self.chat_id {
                Some(chat) => vec![if cmd == Command::Compact {
                    Effect::Compact(chat)
                } else {
                    Effect::Clear(chat)
                }],
                None => {
                    self.error("Start a chat first.");
                    vec![]
                }
            },
            Command::Copy(None) => vec![Effect::Copy(CopyTarget::LastMessage)],
            Command::Copy(Some(n)) => vec![Effect::Copy(CopyTarget::CodeBlock(n))],
            Command::Mouse => {
                self.mouse = !self.mouse;
                vec![Effect::SetMouse(self.mouse)]
            }
            Command::Help => vec![Effect::ShowHelp],
            Command::Quit => vec![Effect::Quit],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    fn chat(id: Uuid) -> Box<types::CodersdkChat> {
        Box::new(serde_json::from_value(json!({"id": id, "title": "t", "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap())
    }

    fn message(id: i64) -> types::CodersdkChatMessage {
        serde_json::from_value(json!({"id": id, "role": "user", "content": []})).unwrap()
    }

    fn started(app: &mut App) -> Uuid {
        let org = Uuid::new_v4();
        app.update(Msg::Started {
            org_id: org,
            open_chat: None,
        });
        org
    }

    fn ev(v: serde_json::Value) -> Msg {
        Msg::Stream(StreamEvent {
            kind: coder_sdk::StreamEventType::parse(v["type"].as_str().unwrap_or_default()),
            event: serde_json::from_value(v.clone()).ok(),
            raw: v,
        })
    }

    #[test]
    fn start_fetches_settings_and_opens_a_requested_chat() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = Uuid::new_v4();
        let id = Uuid::new_v4();
        let effects = app.update(Msg::Started {
            org_id: org,
            open_chat: Some(id),
        });
        assert!(effects.contains(&Effect::FetchPrefs));
        assert!(effects.contains(&Effect::FetchModels(org)));
        assert!(effects.contains(&Effect::FetchWorkspaces));
        assert!(effects.contains(&Effect::LoadChat(id)));
        assert_eq!(app.connection, Connection::Connecting);
    }

    #[test]
    fn loaded_chat_opens_the_stream_after_the_last_message() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        let effects = app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![message(3), message(9)],
        });
        assert_eq!(
            effects,
            vec![Effect::OpenStream {
                chat: id,
                after_id: Some(9)
            }]
        );
    }

    #[test]
    fn chat_loaded_without_id_shows_an_error() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = Uuid::new_v4();
        let requested = Uuid::new_v4();
        app.update(Msg::Started {
            org_id: org,
            open_chat: Some(requested),
        });
        assert_eq!(app.connection, Connection::Connecting);
        let chat_without_id: Box<types::CodersdkChat> = Box::new(
            serde_json::from_value(json!({"title": "t", "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}))
                .unwrap(),
        );
        let effects = app.update(Msg::ChatLoaded {
            chat: chat_without_id,
            messages: vec![],
        });
        assert!(effects.is_empty());
        assert!(matches!(app.notices.last(), Some(Notice::Error(_))));
        assert_eq!(app.connection, Connection::Idle);
    }

    #[test]
    fn submit_on_blank_chat_creates_one_chat() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        let effects = app.update(Msg::Submit("hello".into()));
        assert_eq!(
            effects,
            vec![Effect::CreateChat {
                org,
                text: "hello".into(),
                model: None,
                workspace: None
            }]
        );
    }

    #[test]
    fn second_submit_while_creating_does_not_create_twice() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("one".into()));
        let effects = app.update(Msg::Submit("two".into()));
        assert!(effects.is_empty());
        assert!(matches!(app.notices.last(), Some(Notice::Info(_))));
        let id = Uuid::new_v4();
        let effects = app.update(Msg::ChatCreated(chat(id)));
        assert!(effects.contains(&Effect::OpenStream {
            chat: id,
            after_id: None
        }));
        assert!(effects.contains(&Effect::SendMessage {
            chat: id,
            text: "two".into(),
            model: None,
            busy: BusyBehavior::Queue
        }));
    }

    #[test]
    fn unrelated_api_failure_does_not_reset_creation() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("one".into()));
        app.update(Msg::Submit("two".into()));
        app.update(Msg::ApiFailed {
            action: "load models",
            message: "HTTP 500".into(),
        });
        let effects = app.update(Msg::Submit("three".into()));
        assert!(effects.is_empty(), "no second CreateChat: {effects:?}");
    }

    #[test]
    fn failed_create_restores_all_typed_text() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("one".into()));
        app.update(Msg::Submit("two".into()));
        let effects = app.update(Msg::CreateFailed {
            message: "HTTP 500".into(),
        });
        assert_eq!(effects, vec![Effect::RestoreComposer("one\n\ntwo".into())]);
        let org = app.org_id.unwrap();
        let effects = app.update(Msg::Submit("three".into()));
        assert_eq!(
            effects,
            vec![Effect::CreateChat {
                org,
                text: "three".into(),
                model: None,
                workspace: None
            }]
        );
        let id = Uuid::new_v4();
        let effects = app.update(Msg::ChatCreated(chat(id)));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::SendMessage { text, .. } if text == "two"))
        );
    }

    #[test]
    fn chat_created_without_id_is_a_failed_create() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("hi".into()));
        let chat_without_id: Box<types::CodersdkChat> = Box::new(
            serde_json::from_value(json!({"title": "t", "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}))
                .unwrap(),
        );
        let effects = app.update(Msg::ChatCreated(chat_without_id));
        assert_eq!(effects, vec![Effect::RestoreComposer("hi".into())]);
        assert!(
            matches!(app.notices.last(), Some(Notice::Error(m)) if m.contains("without an id"))
        );
    }

    #[test]
    fn workspace_chosen_during_create_is_applied_after_creation() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("hello".into()));
        let ws = Uuid::new_v4();
        let effects = app.update(Msg::WorkspaceChosen(Some(ws)));
        assert!(effects.is_empty());
        let id = Uuid::new_v4();
        let effects = app.update(Msg::ChatCreated(chat(id)));
        assert!(effects.contains(&Effect::SetWorkspace {
            chat: id,
            workspace: Some(ws)
        }));
    }

    #[test]
    fn set_workspace_precedes_the_queued_message() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("one".into()));
        app.update(Msg::Submit("two".into()));
        let ws = Uuid::new_v4();
        app.update(Msg::WorkspaceChosen(Some(ws)));
        let id = Uuid::new_v4();
        let effects = app.update(Msg::ChatCreated(chat(id)));
        let set_workspace = effects
            .iter()
            .position(|e| matches!(e, Effect::SetWorkspace { .. }))
            .expect("SetWorkspace effect");
        let send_message = effects
            .iter()
            .position(|e| matches!(e, Effect::SendMessage { .. }))
            .expect("SendMessage effect");
        assert!(set_workspace < send_message);
    }

    #[test]
    fn stray_create_failure_does_not_clear_the_composer() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let effects = app.update(Msg::CreateFailed {
            message: "HTTP 500".into(),
        });
        assert!(effects.is_empty());
        assert!(matches!(app.notices.last(), Some(Notice::Error(_))));
    }

    #[test]
    fn submit_with_a_chat_sends_with_the_configured_busy_behavior() {
        let mut app = App::new(BusyBehavior::Interrupt, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
        });
        let effects = app.update(Msg::Submit("  hi  ".into()));
        assert_eq!(
            effects,
            vec![Effect::SendMessage {
                chat: id,
                text: "hi".into(),
                model: None,
                busy: BusyBehavior::Interrupt
            }]
        );
        assert!(app.update(Msg::Submit("   ".into())).is_empty());
    }

    #[test]
    fn stream_end_schedules_backoff_reconnect_with_after_id() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![message(5)],
        });
        let first = app.update(Msg::StreamEnded { error: None });
        assert_eq!(
            first,
            vec![Effect::ReconnectAfter {
                chat: id,
                after_id: Some(5),
                delay: Duration::from_millis(500)
            }]
        );
        let second = app.update(Msg::StreamEnded {
            error: Some("reset".into()),
        });
        assert_eq!(
            second,
            vec![Effect::ReconnectAfter {
                chat: id,
                after_id: Some(5),
                delay: Duration::from_millis(1000)
            }]
        );
        assert_eq!(app.connection, Connection::Reconnecting { attempt: 2 });
    }

    #[test]
    fn stream_event_after_reconnect_resets_backoff() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
        });
        app.update(Msg::StreamEnded { error: None });
        app.update(ev(
            json!({"type": "status", "status": {"status": "waiting"}}),
        ));
        assert_eq!(app.connection, Connection::Live);
        let effects = app.update(Msg::StreamEnded { error: None });
        assert_eq!(
            effects,
            vec![Effect::ReconnectAfter {
                chat: id,
                after_id: None,
                delay: Duration::from_millis(500)
            }]
        );
    }

    #[test]
    fn stream_gap_reconnects_immediately() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
        });
        let part = |seq| json!({"type": "message_part", "message_part": {"history_version": 1, "generation_attempt": 1, "seq": seq, "part": {"type": "text", "text": "x"}}});
        app.update(ev(part(1)));
        let effects = app.update(ev(part(4)));
        assert_eq!(
            effects,
            vec![Effect::ReconnectAfter {
                chat: id,
                after_id: None,
                delay: Duration::ZERO
            }]
        );
        assert_eq!(app.connection, Connection::Reconnecting { attempt: 1 });
    }

    #[test]
    fn repeated_stream_gaps_back_off() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
        });
        let part = |seq| json!({"type": "message_part", "message_part": {"history_version": 1, "generation_attempt": 1, "seq": seq, "part": {"type": "text", "text": "x"}}});
        app.update(ev(part(1)));
        let first = app.update(ev(part(4)));
        assert_eq!(
            first,
            vec![Effect::ReconnectAfter {
                chat: id,
                after_id: None,
                delay: Duration::ZERO
            }]
        );
        assert_eq!(app.connection, Connection::Reconnecting { attempt: 1 });
        let second = app.update(ev(part(9)));
        assert_eq!(
            second,
            vec![Effect::ReconnectAfter {
                chat: id,
                after_id: None,
                delay: backoff(2)
            }]
        );
        assert_eq!(app.connection, Connection::Reconnecting { attempt: 2 });
    }

    #[test]
    fn stream_gap_clears_the_live_turn() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
        });
        let part = |seq| json!({"type": "message_part", "message_part": {"history_version": 1, "generation_attempt": 1, "seq": seq, "part": {"type": "text", "text": "x"}}});
        app.update(ev(part(1)));
        assert!(!app.transcript.live.is_empty());
        app.update(ev(part(4)));
        assert!(app.transcript.live.is_empty());
    }

    #[test]
    fn commands_need_a_chat_and_resolve_names() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert!(app.update(Msg::Command(Command::Compact)).is_empty());
        assert!(matches!(app.notices.last(), Some(Notice::Error(_))));
        let model_id = Uuid::new_v4();
        app.update(Msg::ModelsLoaded(vec![serde_json::from_value(json!({"id": model_id, "display_name": "Big Model", "model": "big", "enabled": true, "reasoning_efforts": []})).unwrap()]));
        app.update(Msg::Command(Command::Model(Some("big model".into()))));
        assert_eq!(app.selected_model, Some(model_id));
        assert_eq!(
            app.update(Msg::Command(Command::Model(None))),
            vec![Effect::ShowPicker(Picker::Model)]
        );
        let ws = Uuid::new_v4();
        app.update(Msg::WorkspacesLoaded(vec![WorkspaceRef {
            id: ws,
            name: "dev".into(),
        }]));
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
        });
        assert_eq!(
            app.update(Msg::Command(Command::Workspace(Some("dev".into())))),
            vec![Effect::SetWorkspace {
                chat: id,
                workspace: Some(ws)
            }]
        );
        assert_eq!(
            app.update(Msg::Command(Command::Workspace(Some("none".into())))),
            vec![Effect::SetWorkspace {
                chat: id,
                workspace: None
            }]
        );
        assert_eq!(
            app.update(Msg::Command(Command::Compact)),
            vec![Effect::Compact(id)]
        );
        assert_eq!(
            app.update(Msg::Command(Command::Clear)),
            vec![Effect::Clear(id)]
        );
        assert_eq!(
            app.update(Msg::Command(Command::Copy(Some(2)))),
            vec![Effect::Copy(CopyTarget::CodeBlock(2))]
        );
        assert_eq!(
            app.update(Msg::Command(Command::Mouse)),
            vec![Effect::SetMouse(false)]
        );
        assert!(!app.mouse);
        assert_eq!(app.update(Msg::Command(Command::Quit)), vec![Effect::Quit]);
    }

    #[test]
    fn commands_while_creating_explain_the_wait() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        app.update(Msg::Submit("hello".into()));
        assert!(app.update(Msg::Command(Command::Compact)).is_empty());
        assert!(
            matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("still being created"))
        );
        assert!(app.update(Msg::Command(Command::Clear)).is_empty());
        assert!(
            matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("still being created"))
        );
    }

    #[test]
    fn model_commands_before_models_load_explain_the_wait() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert!(app.update(Msg::Command(Command::Model(None))).is_empty());
        assert!(matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("still loading")));
        assert!(
            app.update(Msg::Command(Command::Model(Some("big model".into()))))
                .is_empty()
        );
        assert!(matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("still loading")));
    }

    #[test]
    fn failed_model_load_retries_on_model_command() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let org = started(&mut app);
        app.update(Msg::ModelsFailed {
            message: "HTTP 500".into(),
        });
        assert_eq!(app.models_state, ModelsState::Failed);
        let effects = app.update(Msg::Command(Command::Model(None)));
        assert_eq!(effects, vec![Effect::FetchModels(org)]);
        assert_eq!(app.models_state, ModelsState::Loading);
        assert!(matches!(app.notices.last(), Some(Notice::Info(m)) if m.contains("Retrying")));
    }

    #[test]
    fn submit_parses_slash_commands() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        assert_eq!(
            app.update(Msg::Submit("/help".into())),
            vec![Effect::ShowHelp]
        );
        assert!(app.update(Msg::Submit("/bogus".into())).is_empty());
        assert!(matches!(app.notices.last(), Some(Notice::Error(m)) if m.contains("/help")));
    }

    #[test]
    fn interrupt_only_while_running() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
        });
        assert!(app.update(Msg::Interrupt).is_empty());
        app.update(ev(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        assert_eq!(app.update(Msg::Interrupt), vec![Effect::Interrupt(id)]);
    }

    #[test]
    fn interrupt_is_not_sent_while_interrupting() {
        let mut app = App::new(BusyBehavior::Queue, true);
        started(&mut app);
        let id = Uuid::new_v4();
        app.update(Msg::ChatLoaded {
            chat: chat(id),
            messages: vec![],
        });
        app.update(ev(
            json!({"type": "status", "status": {"status": "interrupting"}}),
        ));
        assert!(app.update(Msg::Interrupt).is_empty());
    }

    #[test]
    fn api_failure_becomes_an_error_notice() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::ApiFailed {
            action: "send message",
            message: "HTTP 409".into(),
        });
        assert!(
            matches!(app.notices.last(), Some(Notice::Error(m)) if m.contains("send message") && m.contains("409"))
        );
    }

    #[test]
    fn refresh_changes_nothing() {
        let mut app = App::new(BusyBehavior::Queue, true);
        assert!(app.update(Msg::Refresh).is_empty());
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff(1), Duration::from_millis(500));
        assert_eq!(backoff(3), Duration::from_millis(2000));
        assert_eq!(backoff(20), Duration::from_secs(10));
    }
}
