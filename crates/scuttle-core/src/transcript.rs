//! Durable messages plus stream state, updated by the chat stream reducer.

use std::collections::{BTreeMap, HashSet};

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

/// A tool call in the turn the agent is in that has no result yet, keyed by the message
/// that holds the call and the call's ID.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnresolvedTool {
    pub message_id: i64,
    /// The call's ID; a call without one never matches a result, so it counts as unresolved.
    pub tool_call_id: Option<String>,
    pub name: String,
}

impl UnresolvedTool {
    /// The key a view matches a durable tool block against.
    pub fn key(&self) -> (i64, Option<&str>) {
        (self.message_id, self.tool_call_id.as_deref())
    }
}

/// A message part's type, such as `tool-call`.
fn part_kind(p: &types::CodersdkChatMessagePart) -> Option<&str> {
    p.type_.as_ref().map(|t| t.as_str())
}

/// A message's role, such as `assistant`.
fn message_role(m: &types::CodersdkChatMessage) -> Option<&str> {
    m.role.as_ref().map(|r| r.as_str())
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
    /// How many times a `history_reset` replaced the messages.
    resets: u64,
}

impl Transcript {
    pub fn messages(&self) -> impl DoubleEndedIterator<Item = &types::CodersdkChatMessage> {
        self.messages.values()
    }

    /// Counts the times a `history_reset` replaced the messages, so a view can tell that
    /// lines it drew earlier may now hold different text.
    pub fn history_resets(&self) -> u64 {
        self.resets
    }

    /// The id of the oldest loaded message, which an older page is fetched before.
    pub fn first_message_id(&self) -> Option<i64> {
        self.messages.keys().next().copied()
    }

    pub fn last_message_id(&self) -> Option<i64> {
        self.messages.keys().next_back().copied()
    }

    /// The tool names of this turn's unresolved tools, in call order, for the activity label.
    pub fn unresolved_tool_calls(&self) -> Vec<String> {
        self.unresolved_tools()
            .into_iter()
            .map(|t| t.name)
            .collect()
    }

    /// This turn's tool calls that have no result yet: those in the last assistant message
    /// after the last user message with no result in it or a later message. chatd persists
    /// that message before its tools run, so these are the tools running now. A view matches
    /// its durable tool blocks against them with `UnresolvedTool::key`.
    pub fn unresolved_tools(&self) -> Vec<UnresolvedTool> {
        let Some((&last_id, last)) = self
            .messages
            .iter()
            .rev()
            .find(|(_, m)| matches!(message_role(m), Some("assistant" | "user")))
        else {
            return Vec::new();
        };
        // A user message after the agent's last step starts a turn with no calls yet.
        if message_role(last) != Some("assistant") {
            return Vec::new();
        }
        let resolved: HashSet<&str> = self
            .messages
            .range(last_id..)
            .flat_map(|(_, m)| m.content.iter())
            .filter(|p| part_kind(p) == Some("tool-result"))
            .filter_map(|p| p.tool_call_id.as_deref())
            .collect();
        last.content
            .iter()
            .filter(|p| part_kind(p) == Some("tool-call"))
            .filter(|p| {
                p.tool_call_id
                    .as_deref()
                    .is_none_or(|id| !resolved.contains(id))
            })
            .map(|p| UnresolvedTool {
                message_id: last_id,
                tool_call_id: p.tool_call_id.clone(),
                name: p.tool_name.clone().unwrap_or_default(),
            })
            .collect()
    }

    pub fn load(&mut self, messages: Vec<types::CodersdkChatMessage>) {
        for m in messages {
            self.upsert(m);
        }
    }

    /// Adds a page of older messages. A message already loaded keeps its copy, which the
    /// stream has kept current, so a page that overlaps the loaded ones duplicates nothing.
    pub fn prepend(&mut self, messages: Vec<types::CodersdkChatMessage>) {
        for m in messages {
            if let Some(id) = m.id {
                self.messages.entry(id).or_insert(m);
            }
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
        self.resets += 1;
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
        assert_eq!(t.history_resets(), 0);
        t.apply(&ev(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        assert_eq!(ids(&t), vec![3]);
        assert_eq!(t.history_resets(), 1);
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

    fn stored(id: i64, role: &str, content: serde_json::Value) -> types::CodersdkChatMessage {
        serde_json::from_value(json!({"id": id, "role": role, "content": content})).unwrap()
    }

    #[test]
    fn unresolved_tools_are_this_turns_calls_without_a_result() {
        let mut t = Transcript::default();
        t.load(vec![
            stored(1, "user", json!([{"type": "text", "text": "go"}])),
            stored(
                2,
                "assistant",
                json!([
                    {"type": "tool-call", "tool_call_id": "a", "tool_name": "read", "args": {}},
                    {"type": "tool-call", "tool_call_id": "b", "tool_name": "edit", "args": {}}
                ]),
            ),
            stored(
                3,
                "tool",
                json!([
                    {"type": "tool-result", "tool_call_id": "a", "tool_name": "read", "result": {}}
                ]),
            ),
        ]);
        let tools = t.unresolved_tools();
        assert_eq!(
            tools,
            vec![UnresolvedTool {
                message_id: 2,
                tool_call_id: Some("b".into()),
                name: "edit".into(),
            }]
        );
        assert_eq!(tools[0].key(), (2, Some("b")));
        assert_eq!(t.unresolved_tool_calls(), vec!["edit".to_owned()]);
        t.load(vec![stored(
            4,
            "user",
            json!([{"type": "text", "text": "next"}]),
        )]);
        assert_eq!(
            t.unresolved_tools(),
            vec![],
            "an earlier turn's call is not this turn's"
        );
    }
}
