//! Durable messages plus stream state, updated by the chat stream reducer.

use std::collections::BTreeMap;

use coder_sdk::{ChatStatus, StreamEvent, StreamEventType, types};

pub use crate::live::Applied;
use crate::live::LiveTurn;

/// A provider retry the server reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryInfo {
    pub attempt: i64,
    pub delay_ms: i64,
    pub error: String,
}

/// Everything shown for one chat: durable history and the in-progress turn.
#[derive(Debug, Default)]
pub struct Transcript {
    messages: BTreeMap<i64, types::CodersdkChatMessage>,
    pub live: LiveTurn,
    pub status: Option<ChatStatus>,
    pub last_error: Option<String>,
    pub retry: Option<RetryInfo>,
    pub queued: Vec<types::CodersdkChatQueuedMessage>,
    pub action_required: Vec<String>,
    pending_history: Option<Vec<types::CodersdkChatMessage>>,
}

impl Transcript {
    pub fn messages(&self) -> impl DoubleEndedIterator<Item = &types::CodersdkChatMessage> {
        self.messages.values()
    }

    pub fn last_message_id(&self) -> Option<i64> {
        self.messages.keys().next_back().copied()
    }

    pub fn load(&mut self, messages: Vec<types::CodersdkChatMessage>) {
        for m in messages {
            self.upsert(m);
        }
    }

    fn upsert(&mut self, m: types::CodersdkChatMessage) {
        if let Some(id) = m.id {
            self.messages.insert(id, m);
        }
    }

    fn flush_history(&mut self) {
        let Some(buffer) = self.pending_history.take() else {
            return;
        };
        self.messages.clear();
        for m in buffer {
            self.upsert(m);
        }
    }

    /// Applies one stream event. Returns `Applied::Reconnect` when the stream must be reopened.
    pub fn apply(&mut self, ev: &StreamEvent) -> Applied {
        if self.pending_history.is_some() && ev.kind != StreamEventType::Message {
            if ev.kind == StreamEventType::HistoryReset {
                self.pending_history = None;
            } else {
                self.flush_history();
            }
        }
        let Some(e) = ev.event.as_ref() else {
            return match ev.kind {
                StreamEventType::PreviewReset => {
                    self.live.reset_preview();
                    Applied::Changed
                }
                StreamEventType::HistoryReset => {
                    self.pending_history = Some(Vec::new());
                    Applied::Changed
                }
                _ => Applied::Unchanged,
            };
        };
        match &ev.kind {
            StreamEventType::Message => {
                let Some(m) = e.message.clone() else {
                    return Applied::Unchanged;
                };
                if let Some(buffer) = self.pending_history.as_mut() {
                    buffer.push(m);
                    return Applied::Changed;
                }
                let assistant = m.role.as_ref().map(|r| r.as_str()) == Some("assistant");
                self.upsert(m);
                self.last_error = None;
                self.retry = None;
                if assistant {
                    self.live.clear();
                }
                Applied::Changed
            }
            StreamEventType::MessagePart => match e.message_part.as_ref() {
                Some(mp) => self.live.apply(mp),
                None => Applied::Unchanged,
            },
            StreamEventType::Status => {
                let status = e
                    .status
                    .as_ref()
                    .and_then(|s| s.status.as_ref())
                    .map(|s| ChatStatus::parse(s.as_str()));
                match status {
                    Some(ChatStatus::Waiting) => {
                        self.live.set_idle(true);
                        self.last_error = None;
                        self.retry = None;
                    }
                    Some(ChatStatus::Running) => {
                        self.live.set_idle(false);
                        self.last_error = None;
                        self.retry = None;
                    }
                    Some(ChatStatus::Error) => {}
                    _ => {
                        self.last_error = None;
                        self.retry = None;
                    }
                }
                self.status = status;
                Applied::Changed
            }
            StreamEventType::Error => {
                let message = e
                    .error
                    .as_ref()
                    .and_then(|x| x.message.clone())
                    .unwrap_or_else(|| "unknown error".into());
                self.last_error = Some(message);
                self.retry = None;
                self.live.clear();
                Applied::Changed
            }
            StreamEventType::Retry => {
                if let Some(r) = e.retry.as_ref() {
                    self.retry = Some(RetryInfo {
                        attempt: r.attempt.unwrap_or(0),
                        delay_ms: r.delay_ms.unwrap_or(0),
                        error: r.error.clone().unwrap_or_default(),
                    });
                }
                self.live.clear();
                Applied::Changed
            }
            StreamEventType::QueueUpdate => {
                self.queued = e.queued_messages.clone();
                Applied::Changed
            }
            StreamEventType::ActionRequired => {
                self.action_required = e
                    .action_required
                    .as_ref()
                    .map(|a| {
                        a.tool_calls
                            .iter()
                            .filter_map(|c| c.tool_name.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                Applied::Changed
            }
            StreamEventType::PreviewReset => {
                self.live.reset_preview();
                Applied::Changed
            }
            StreamEventType::HistoryReset => {
                self.pending_history = Some(Vec::new());
                Applied::Changed
            }
            StreamEventType::Unknown(_) => Applied::Unchanged,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::LiveBlock;
    use serde_json::json;

    fn ev(v: serde_json::Value) -> StreamEvent {
        StreamEvent {
            kind: StreamEventType::parse(v["type"].as_str().unwrap_or_default()),
            event: serde_json::from_value(v.clone()).ok(),
            raw: v,
        }
    }

    fn message(id: i64, role: &str, text: &str) -> serde_json::Value {
        json!({"type": "message", "message": {"id": id, "role": role, "content": [{"type": "text", "text": text}]}})
    }

    fn part(seq: i64, text: &str) -> serde_json::Value {
        json!({"type": "message_part", "message_part": {"history_version": 1, "generation_attempt": 1, "seq": seq, "role": "assistant", "part": {"type": "text", "text": text}}})
    }

    fn ids(t: &Transcript) -> Vec<i64> {
        t.messages().filter_map(|m| m.id).collect()
    }

    #[test]
    fn messages_are_upserted_in_id_order() {
        let mut t = Transcript::default();
        t.apply(&ev(message(2, "assistant", "b")));
        t.apply(&ev(message(1, "user", "a")));
        t.apply(&ev(message(2, "assistant", "b2")));
        assert_eq!(ids(&t), vec![1, 2]);
        assert_eq!(t.last_message_id(), Some(2));
    }

    #[test]
    fn snapshot_replay_does_not_duplicate_messages() {
        let mut t = Transcript::default();
        for id in 1..=3 {
            t.apply(&ev(message(id, "user", "x")));
        }
        for id in 1..=3 {
            t.apply(&ev(message(id, "user", "x")));
        }
        assert_eq!(ids(&t), vec![1, 2, 3]);
    }

    #[test]
    fn assistant_message_clears_live_but_user_message_does_not() {
        let mut t = Transcript::default();
        t.apply(&ev(part(1, "streaming")));
        t.apply(&ev(message(1, "user", "q")));
        assert!(!t.live.is_empty());
        t.apply(&ev(message(2, "assistant", "final")));
        assert!(t.live.is_empty());
    }

    #[test]
    fn history_reset_replaces_messages_from_the_first_replacement_onward() {
        let mut t = Transcript::default();
        for id in 1..=4 {
            t.apply(&ev(message(id, "user", "old")));
        }
        t.apply(&ev(json!({"type": "history_reset"})));
        t.apply(&ev(message(3, "user", "edited")));
        assert_eq!(
            ids(&t),
            vec![1, 2, 3, 4],
            "buffered until the next non-message event"
        );
        t.apply(&ev(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        assert_eq!(ids(&t), vec![3]);
        let edited = t.messages().last().unwrap();
        assert_eq!(edited.content[0].text.as_deref(), Some("edited"));
    }

    #[test]
    fn waiting_status_makes_live_idle() {
        let mut t = Transcript::default();
        t.apply(&ev(part(1, "a")));
        t.apply(&ev(
            json!({"type": "status", "status": {"status": "waiting"}}),
        ));
        assert_eq!(t.status, Some(ChatStatus::Waiting));
        assert_eq!(t.apply(&ev(part(2, "late"))), Applied::Unchanged);
    }

    #[test]
    fn error_and_retry_clear_live_and_record_details() {
        let mut t = Transcript::default();
        t.apply(&ev(part(1, "a")));
        t.apply(&ev(json!({"type": "retry", "retry": {"attempt": 2, "delay_ms": 1500, "error": "rate limited"}})));
        assert!(t.live.is_empty());
        assert_eq!(
            t.retry,
            Some(RetryInfo {
                attempt: 2,
                delay_ms: 1500,
                error: "rate limited".into()
            })
        );
        t.apply(&ev(json!({"type": "error", "error": {"message": "boom"}})));
        assert_eq!(t.last_error.as_deref(), Some("boom"));
    }

    #[test]
    fn queue_update_replaces_the_queue() {
        let mut t = Transcript::default();
        t.apply(&ev(json!({"type": "queue_update", "queued_messages": [{"id": 1, "content": []}, {"id": 2, "content": []}]})));
        assert_eq!(t.queued.len(), 2);
        t.apply(&ev(json!({"type": "queue_update", "queued_messages": []})));
        assert!(t.queued.is_empty());
    }

    #[test]
    fn unknown_event_is_unchanged_and_gap_propagates_reconnect() {
        let mut t = Transcript::default();
        assert_eq!(
            t.apply(&ev(json!({"type": "from_the_future"}))),
            Applied::Unchanged
        );
        t.apply(&ev(part(1, "a")));
        assert!(matches!(t.apply(&ev(part(5, "e"))), Applied::Reconnect(_)));
    }

    #[test]
    fn load_then_live_part_keeps_both() {
        let mut t = Transcript::default();
        let m: types::CodersdkChatMessage =
            serde_json::from_value(json!({"id": 7, "role": "user", "content": []})).unwrap();
        t.load(vec![m]);
        t.apply(&ev(part(1, "hi")));
        assert_eq!(ids(&t), vec![7]);
        assert_eq!(t.live.blocks, vec![LiveBlock::Text("hi".into())]);
    }

    #[test]
    fn history_reset_with_no_replacements_clears_history() {
        let mut t = Transcript::default();
        for id in 1..=3 {
            t.apply(&ev(message(id, "user", "msg")));
        }
        assert_eq!(ids(&t), vec![1, 2, 3]);
        t.apply(&ev(json!({"type": "history_reset"})));
        t.apply(&ev(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        assert_eq!(ids(&t), Vec::<i64>::new());
    }

    #[test]
    fn second_history_reset_restarts_buffering() {
        let mut t = Transcript::default();
        t.apply(&ev(json!({"type": "history_reset"})));
        t.apply(&ev(message(5, "user", "msg5")));
        t.apply(&ev(json!({"type": "history_reset"})));
        assert_eq!(
            ids(&t),
            Vec::<i64>::new(),
            "first buffer discarded, not flushed"
        );
        t.apply(&ev(message(7, "user", "msg7")));
        t.apply(&ev(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        assert_eq!(ids(&t), vec![7]);
    }

    #[test]
    fn last_error_clears_when_the_chat_recovers() {
        let mut t = Transcript::default();
        t.apply(&ev(json!({"type": "error", "error": {"message": "boom"}})));
        assert_eq!(t.last_error.as_deref(), Some("boom"));
        assert_eq!(t.retry, None);
        t.apply(&ev(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        assert_eq!(t.last_error, None);
        t.apply(&ev(json!({"type": "error", "error": {"message": "oops"}})));
        assert_eq!(t.last_error.as_deref(), Some("oops"));
        t.apply(&ev(message(1, "user", "recovery")));
        assert_eq!(t.last_error, None);
        assert_eq!(t.retry, None);
    }
}
