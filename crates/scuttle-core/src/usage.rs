//! Context window usage, computed the way the web UI does.

use coder_sdk::types;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsage {
    pub used: i64,
    pub limit: Option<i64>,
}

/// Walks messages newest first: a `chat_cleared` tool result or in-flight boundary
/// means no usage. A `chat_summarized` result supplies an estimate, otherwise the
/// latest message with `usage` is used.
pub fn context_usage<'a>(
    messages: impl DoubleEndedIterator<Item = &'a types::CodersdkChatMessage>,
) -> Option<ContextUsage> {
    for m in messages.rev() {
        for part in m.content.iter().rev() {
            let is_result = part.type_.as_ref().map(|t| t.as_str()) == Some("tool-result");
            let is_call = part.type_.as_ref().map(|t| t.as_str()) == Some("tool-call");
            match part.tool_name.as_deref() {
                Some("chat_cleared") => return None,
                Some("chat_summarized") if is_call => return None,
                Some("chat_summarized") if is_result => {
                    if part.is_error == Some(true) {
                        return None;
                    }
                    let result = part.result.as_ref()?;
                    let used = result["estimated_context_tokens"].as_i64()?;
                    let limit = result["context_limit_tokens"].as_i64();
                    return Some(ContextUsage { used, limit });
                }
                _ => {}
            }
        }
        if let Some(u) = m.usage.as_ref() {
            let used = [
                u.input_tokens,
                u.output_tokens,
                u.cache_read_tokens,
                u.cache_creation_tokens,
                u.reasoning_tokens,
            ]
            .iter()
            .map(|v| v.unwrap_or(0))
            .sum();
            return Some(ContextUsage {
                used,
                limit: u.context_limit,
            });
        }
    }
    None
}

pub fn format_tokens(n: i64) -> String {
    match n.abs() {
        abs_n if abs_n >= 1_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0),
        abs_n if abs_n >= 1_000 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => n.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn msgs(v: serde_json::Value) -> Vec<types::CodersdkChatMessage> {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn uses_the_latest_message_with_usage() {
        let m = msgs(json!([
            {"id": 1, "role": "assistant", "content": [], "usage": {"input_tokens": 10, "output_tokens": 5, "context_limit": 1000}},
            {"id": 2, "role": "user", "content": []},
            {"id": 3, "role": "assistant", "content": [], "usage": {"input_tokens": 100, "output_tokens": 20, "cache_read_tokens": 30, "cache_creation_tokens": 4, "reasoning_tokens": 6, "context_limit": 2000}}
        ]));
        assert_eq!(
            context_usage(m.iter()),
            Some(ContextUsage {
                used: 160,
                limit: Some(2000)
            })
        );
    }

    #[test]
    fn cleared_boundary_means_no_usage_and_summarized_uses_the_estimate() {
        let cleared = msgs(json!([
            {"id": 1, "role": "assistant", "content": [], "usage": {"input_tokens": 10, "context_limit": 1000}},
            {"id": 2, "role": "tool", "content": [{"type": "tool-result", "tool_name": "chat_cleared", "result": {}}]}
        ]));
        assert_eq!(context_usage(cleared.iter()), None);
        let summarized = msgs(json!([
            {"id": 1, "role": "assistant", "content": [], "usage": {"input_tokens": 10, "context_limit": 1000}},
            {"id": 2, "role": "tool", "content": [{"type": "tool-result", "tool_name": "chat_summarized", "result": {"estimated_context_tokens": 42, "context_limit_tokens": 1000}}]}
        ]));
        assert_eq!(
            context_usage(summarized.iter()),
            Some(ContextUsage {
                used: 42,
                limit: Some(1000)
            })
        );
    }

    #[test]
    fn formats_tokens_compactly() {
        assert_eq!(format_tokens(950), "950");
        assert_eq!(format_tokens(12_345), "12.3k");
        assert_eq!(format_tokens(1_200_000), "1.2M");
    }

    #[test]
    fn errored_summary_means_no_usage() {
        let m = msgs(json!([
            {"id": 1, "role": "assistant", "content": [], "usage": {"input_tokens": 10, "context_limit": 1000}},
            {"id": 2, "role": "tool", "content": [{"type": "tool-result", "tool_name": "chat_summarized", "result": {"estimated_context_tokens": 42}, "is_error": true}]}
        ]));
        assert_eq!(context_usage(m.iter()), None);
    }

    #[test]
    fn pending_boundary_means_no_usage() {
        let m = msgs(json!([
            {"id": 1, "role": "assistant", "content": [], "usage": {"input_tokens": 10, "context_limit": 1000}},
            {"id": 2, "role": "tool", "content": [
                {"type": "tool-call", "tool_name": "chat_cleared"},
                {"type": "tool-call", "tool_name": "chat_summarized"}
            ]}
        ]));
        assert_eq!(context_usage(m.iter()), None);
    }

    #[test]
    fn missing_context_limit_keeps_the_usage() {
        let m = msgs(json!([
            {"id": 1, "role": "assistant", "content": [], "usage": {"input_tokens": 10}}
        ]));
        assert_eq!(
            context_usage(m.iter()),
            Some(ContextUsage {
                used: 10,
                limit: None
            })
        );
    }

    #[test]
    fn format_tokens_handles_negative_numbers() {
        assert_eq!(format_tokens(-5), "-5");
        assert_eq!(format_tokens(-1_200), "-1.2k");
    }
}
