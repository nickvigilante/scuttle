//! The in-progress assistant turn, assembled from `message_part` stream events.

use coder_sdk::types;
use serde_json::Value;

/// The result of applying one stream event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    Changed,
    Unchanged,
    /// The stream is inconsistent, for example after a `seq` gap, and must be reopened.
    Reconnect(String),
}

/// One piece of the in-progress turn.
#[derive(Debug, Clone, PartialEq)]
pub enum LiveBlock {
    Text(String),
    Reasoning(String),
    ToolCall {
        id: String,
        name: String,
        args_raw: String,
        args: Option<Value>,
    },
    ToolResult {
        id: String,
        name: String,
        result_raw: String,
        result: Option<Value>,
        reasoning: String,
        is_error: bool,
        done: bool,
    },
    Source {
        url: String,
        title: Option<String>,
    },
}

/// Parts of the current generation, applied in `seq` order.
#[derive(Debug, Default)]
pub struct LiveTurn {
    pub blocks: Vec<LiveBlock>,
    cursor: Option<(i64, i64)>,
    last_seq: i64,
    idle: bool,
}

impl LiveTurn {
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
    }

    /// Discards in-progress parts and accepts a replay of the current generation from `seq` 1.
    pub fn reset_preview(&mut self) {
        self.blocks.clear();
        self.last_seq = 0;
    }

    /// While idle, late parts of the current generation are dropped; a newer generation clears it.
    pub fn set_idle(&mut self, idle: bool) {
        self.idle = idle;
    }

    pub fn apply(&mut self, mp: &types::CodersdkChatStreamMessagePart) -> Applied {
        let key = (
            mp.history_version.unwrap_or(0),
            mp.generation_attempt.unwrap_or(0),
        );
        match self.cursor {
            Some(current) if key < current => return Applied::Unchanged,
            Some(current) if key == current => {}
            _ => {
                self.cursor = Some(key);
                self.blocks.clear();
                self.last_seq = 0;
                self.idle = false;
            }
        }
        if self.idle {
            return Applied::Unchanged;
        }
        let seq = mp.seq.unwrap_or(0);
        if seq <= self.last_seq {
            return Applied::Unchanged;
        }
        if seq != self.last_seq + 1 {
            return Applied::Reconnect(format!(
                "stream gap: expected seq {}, got {seq}",
                self.last_seq + 1
            ));
        }
        self.last_seq = seq;
        if let Some(part) = mp.part.as_ref() {
            self.apply_part(part);
        }
        Applied::Changed
    }

    fn apply_part(&mut self, part: &types::CodersdkChatMessagePart) {
        let kind = part.type_.as_ref().map(|t| t.as_str()).unwrap_or_default();
        match kind {
            "text" => append(
                &mut self.blocks,
                false,
                part.text.as_deref().unwrap_or_default(),
            ),
            "reasoning" => append(
                &mut self.blocks,
                true,
                part.text.as_deref().unwrap_or_default(),
            ),
            "tool-call" => self.apply_tool_call(part),
            "tool-result" => self.apply_tool_result(part),
            "source" => {
                let Some(url) = part.url.clone() else { return };
                let seen = self
                    .blocks
                    .iter()
                    .any(|b| matches!(b, LiveBlock::Source { url: u, .. } if *u == url));
                if !seen {
                    self.blocks.push(LiveBlock::Source {
                        url,
                        title: part.title.clone(),
                    });
                }
            }
            _ => {}
        }
    }

    fn apply_tool_call(&mut self, part: &types::CodersdkChatMessagePart) {
        if part.provider_executed == Some(true) {
            return;
        }
        let id = part.tool_call_id.clone().unwrap_or_default();
        let name = part.tool_name.clone().unwrap_or_default();
        let idx = match self
            .blocks
            .iter()
            .position(|b| matches!(b, LiveBlock::ToolCall { id: i, .. } if *i == id))
        {
            Some(i) => i,
            None => {
                self.blocks.push(LiveBlock::ToolCall {
                    id,
                    name: String::new(),
                    args_raw: String::new(),
                    args: None,
                });
                self.blocks.len() - 1
            }
        };
        if let LiveBlock::ToolCall {
            name: block_name,
            args_raw,
            args,
            ..
        } = &mut self.blocks[idx]
        {
            if block_name.is_empty() {
                *block_name = name;
            }
            if let Some(full) = part.args.as_ref() {
                *args_raw = full.to_string();
                *args = Some(full.clone());
            } else if let Some(delta) = part.args_delta.as_deref() {
                args_raw.push_str(delta);
                *args = parse_partial_json(args_raw);
            }
        }
    }

    fn apply_tool_result(&mut self, part: &types::CodersdkChatMessagePart) {
        let id = part.tool_call_id.clone().unwrap_or_default();
        let name = part.tool_name.clone().unwrap_or_default();
        let idx = match self
            .blocks
            .iter()
            .position(|b| matches!(b, LiveBlock::ToolResult { id: i, .. } if *i == id))
        {
            Some(i) => i,
            None => {
                self.blocks.push(LiveBlock::ToolResult {
                    id,
                    name: String::new(),
                    result_raw: String::new(),
                    result: None,
                    reasoning: String::new(),
                    is_error: false,
                    done: false,
                });
                self.blocks.len() - 1
            }
        };
        if let LiveBlock::ToolResult {
            name: block_name,
            result_raw,
            result,
            reasoning,
            is_error,
            done,
            ..
        } = &mut self.blocks[idx]
        {
            if block_name.is_empty() {
                *block_name = name;
            }
            if part.result_reset == Some(true) {
                result_raw.clear();
                *result = None;
                *done = false;
            }
            if part.result.is_some() || part.is_error.is_some() {
                *result = part.result.clone();
                *result_raw = part
                    .result
                    .as_ref()
                    .map(Value::to_string)
                    .unwrap_or_default();
                *is_error = part.is_error.unwrap_or(false);
                *done = true;
                reasoning.clear();
            } else {
                if let Some(delta) = part.result_delta.as_deref() {
                    result_raw.push_str(delta);
                }
                if let Some(delta) = part.reasoning_delta.as_deref() {
                    reasoning.push_str(delta);
                }
            }
        }
    }
}

fn append(blocks: &mut Vec<LiveBlock>, reasoning: bool, delta: &str) {
    if delta.trim().is_empty() {
        return;
    }
    match (blocks.last_mut(), reasoning) {
        (Some(LiveBlock::Reasoning(s)), true) | (Some(LiveBlock::Text(s)), false) => {
            s.push_str(delta)
        }
        (_, true) => blocks.push(LiveBlock::Reasoning(delta.to_owned())),
        (_, false) => blocks.push(LiveBlock::Text(delta.to_owned())),
    }
}

/// Parses JSON that may be cut off mid-stream by closing any open string, array, or object.
pub fn parse_partial_json(raw: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str(raw) {
        return Some(v);
    }
    let mut closers = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for c in raw.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => closers.push('}'),
            '[' => closers.push(']'),
            '}' | ']' => {
                closers.pop();
            }
            _ => {}
        }
    }
    let mut fixed = raw.trim_end().trim_end_matches(',').to_owned();
    if in_string {
        if escaped {
            fixed.pop();
        }
        fixed.push('"');
    }
    while let Some(c) = closers.pop() {
        fixed.push(c);
    }
    serde_json::from_str(&fixed).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use coder_sdk::types::CodersdkChatStreamMessagePart;
    use serde_json::json;

    fn mp(hv: i64, ga: i64, seq: i64, part: serde_json::Value) -> CodersdkChatStreamMessagePart {
        serde_json::from_value(json!({
            "role": "assistant", "history_version": hv, "generation_attempt": ga, "seq": seq, "part": part
        }))
        .unwrap()
    }

    fn text(t: &str) -> serde_json::Value {
        json!({"type": "text", "text": t})
    }

    #[test]
    fn text_deltas_append_to_one_block() {
        let mut live = LiveTurn::default();
        assert_eq!(live.apply(&mp(1, 1, 1, text("Hel"))), Applied::Changed);
        assert_eq!(live.apply(&mp(1, 1, 2, text("lo"))), Applied::Changed);
        assert_eq!(live.blocks, vec![LiveBlock::Text("Hello".into())]);
    }

    #[test]
    fn whitespace_only_delta_is_skipped() {
        let mut live = LiveTurn::default();
        live.apply(&mp(1, 1, 1, text("   ")));
        assert!(live.is_empty());
    }

    #[test]
    fn reasoning_then_text_are_separate_blocks() {
        let mut live = LiveTurn::default();
        live.apply(&mp(1, 1, 1, json!({"type": "reasoning", "text": "think"})));
        live.apply(&mp(1, 1, 2, text("answer")));
        assert_eq!(
            live.blocks,
            vec![
                LiveBlock::Reasoning("think".into()),
                LiveBlock::Text("answer".into())
            ]
        );
    }

    #[test]
    fn newer_generation_resets_and_older_is_ignored() {
        let mut live = LiveTurn::default();
        live.apply(&mp(1, 1, 1, text("old")));
        live.apply(&mp(1, 2, 1, text("new")));
        assert_eq!(live.blocks, vec![LiveBlock::Text("new".into())]);
        assert_eq!(live.apply(&mp(1, 1, 2, text("stale"))), Applied::Unchanged);
        assert_eq!(live.blocks, vec![LiveBlock::Text("new".into())]);
    }

    #[test]
    fn duplicate_seq_is_ignored_and_gap_requests_reconnect() {
        let mut live = LiveTurn::default();
        live.apply(&mp(1, 1, 1, text("a")));
        assert_eq!(live.apply(&mp(1, 1, 1, text("a"))), Applied::Unchanged);
        assert!(matches!(
            live.apply(&mp(1, 1, 3, text("c"))),
            Applied::Reconnect(_)
        ));
    }

    #[test]
    fn tool_call_args_stream_as_partial_json() {
        let mut live = LiveTurn::default();
        let call = |seq, delta: &str| {
            mp(
                1,
                1,
                seq,
                json!({"type": "tool-call", "tool_call_id": "t1", "tool_name": "read_file", "args_delta": delta}),
            )
        };
        live.apply(&call(1, r#"{"path": "/tm"#));
        match &live.blocks[0] {
            LiveBlock::ToolCall { name, args, .. } => {
                assert_eq!(name, "read_file");
                assert_eq!(args.as_ref().unwrap()["path"], "/tm");
            }
            other => panic!("unexpected {other:?}"),
        }
        live.apply(&mp(
            1,
            1,
            2,
            json!({"type": "tool-call", "tool_call_id": "t1", "args": {"path": "/tmp/x"}}),
        ));
        match &live.blocks[0] {
            LiveBlock::ToolCall { args, .. } => {
                assert_eq!(args.as_ref().unwrap()["path"], "/tmp/x")
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(live.blocks.len(), 1);
    }

    #[test]
    fn provider_executed_tool_calls_are_skipped() {
        let mut live = LiveTurn::default();
        live.apply(&mp(1, 1, 1, json!({"type": "tool-call", "tool_call_id": "t1", "tool_name": "web", "provider_executed": true})));
        assert!(live.is_empty());
    }

    #[test]
    fn tool_result_deltas_reset_and_finalize() {
        let mut live = LiveTurn::default();
        let res = |seq, v: serde_json::Value| mp(1, 1, seq, v);
        live.apply(&res(1, json!({"type": "tool-result", "tool_call_id": "t1", "tool_name": "execute", "result_delta": "line1\n", "reasoning_delta": "hmm"})));
        live.apply(&res(
            2,
            json!({"type": "tool-result", "tool_call_id": "t1", "result_reset": true}),
        ));
        live.apply(&res(
            3,
            json!({"type": "tool-result", "tool_call_id": "t1", "result_delta": "again\n"}),
        ));
        live.apply(&res(4, json!({"type": "tool-result", "tool_call_id": "t1", "result": {"output": "done"}, "is_error": false})));
        match &live.blocks[0] {
            LiveBlock::ToolResult {
                result,
                result_raw,
                reasoning,
                done,
                is_error,
                ..
            } => {
                assert!(*done);
                assert!(!*is_error);
                assert_eq!(result.as_ref().unwrap()["output"], "done");
                assert!(result_raw.contains("done"));
                assert!(reasoning.is_empty());
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn sources_are_deduplicated_by_url() {
        let mut live = LiveTurn::default();
        live.apply(&mp(
            1,
            1,
            1,
            json!({"type": "source", "url": "https://a", "title": "A"}),
        ));
        live.apply(&mp(1, 1, 2, json!({"type": "source", "url": "https://a"})));
        assert_eq!(live.blocks.len(), 1);
    }

    #[test]
    fn idle_ignores_same_generation_until_a_new_one_starts() {
        let mut live = LiveTurn::default();
        live.apply(&mp(1, 1, 1, text("a")));
        live.set_idle(true);
        assert_eq!(live.apply(&mp(1, 1, 2, text("late"))), Applied::Unchanged);
        assert_eq!(live.apply(&mp(2, 1, 1, text("next"))), Applied::Changed);
        assert_eq!(live.blocks, vec![LiveBlock::Text("next".into())]);
    }

    #[test]
    fn preview_reset_allows_replay_from_seq_one() {
        let mut live = LiveTurn::default();
        live.apply(&mp(1, 1, 1, text("a")));
        live.apply(&mp(1, 1, 2, text("b")));
        live.reset_preview();
        assert_eq!(live.apply(&mp(1, 1, 1, text("a"))), Applied::Changed);
        assert_eq!(live.blocks, vec![LiveBlock::Text("a".into())]);
    }

    #[test]
    fn partial_json_closes_open_strings_and_brackets() {
        assert_eq!(
            parse_partial_json(r#"{"a": [1, 2"#),
            Some(json!({"a": [1, 2]}))
        );
        assert_eq!(parse_partial_json(r#"{"a": "b"#), Some(json!({"a": "b"})));
        assert_eq!(parse_partial_json("not json"), None);
    }
}
