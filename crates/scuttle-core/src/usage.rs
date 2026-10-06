//! Context window usage, computed the way the web UI does, and the AI spend and workspace
//! quota limits as the footer and `/usage` show them.

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
            let is_boundary_type = matches!(
                part.type_.as_ref().map(|t| t.as_str()),
                Some("tool-call") | Some("tool-result")
            );
            let is_result = part.type_.as_ref().map(|t| t.as_str()) == Some("tool-result");
            let is_call = part.type_.as_ref().map(|t| t.as_str()) == Some("tool-call");

            if is_boundary_type {
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
    let abs_n = n.unsigned_abs();
    match abs_n {
        abs_n if abs_n >= 1_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0),
        abs_n if abs_n >= 1_000 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => n.to_string(),
    }
}

/// A cost in micros of a dollar as US dollars, following the web UI's `formatCostMicros`
/// (`site/src/utils/currency.ts`): four decimals for a nonzero amount below one cent, else two,
/// with thousands separators.
pub fn format_cost_micros(micros: i64) -> String {
    // `{:.N}` rounds the exact binary value, as `toFixed` and `Intl.NumberFormat` do, and a
    // decimal tie is never exactly representable, so the digits match the web's.
    let dollars = micros.unsigned_abs() as f64 / 1_000_000.0;
    let sign = if micros < 0 { "-" } else { "" };
    let sub_cent = format!("{dollars:.4}");
    let rounded4: f64 = sub_cent.parse().unwrap_or(0.0);
    if rounded4 > 0.0 && rounded4 < 0.01 {
        return format!("{sign}${sub_cent}");
    }
    let fixed = format!("{dollars:.2}");
    let (whole, cents) = fixed.split_once('.').unwrap_or((&fixed, "00"));
    let mut grouped = String::new();
    for (i, digit) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    format!("{sign}${grouped}.{cents}")
}

/// `1 request` or `3 requests`.
pub fn count(n: i64, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// A token count in short form: `200k`, `1M`, `1.5M`.
pub fn short_tokens(n: i64) -> String {
    // A value that would round to 1000k (999,500..=999,999) rounds up into the millions
    // instead, so it reads "1M".
    if n >= 1_000_000 || (n + 500) / 1_000 >= 1_000 {
        let millions = format!("{:.1}", n as f64 / 1_000_000.0);
        format!("{}M", millions.trim_end_matches(".0"))
    } else if n >= 1_000 {
        format!("{}k", (n + 500) / 1_000)
    } else {
        n.to_string()
    }
}

/// What scuttle knows of one limit: the user's AI spend or their workspace quota.
#[derive(Debug, Clone, Default)]
pub enum LimitState<T> {
    /// Not fetched yet.
    #[default]
    Unknown,
    Loaded(T),
    /// The deployment has no such route (`404`, an open-source build), so it shows nowhere.
    Absent,
    /// The deployment refused it (`403`, unlicensed), with the server's message for `/usage`.
    Refused(String),
    /// The last request failed, and none had loaded before it.
    Failed(String),
}

impl<T> LimitState<T> {
    pub fn loaded(&self) -> Option<&T> {
        match self {
            LimitState::Loaded(v) => Some(v),
            _ => None,
        }
    }

    /// Whether a refresh asks for it again: not after a `404` or `403`, since neither changes
    /// while scuttle runs.
    pub fn refreshes(&self) -> bool {
        !matches!(self, LimitState::Absent | LimitState::Refused(_))
    }

    /// Applies a refused or failed request. A failure keeps a value that already loaded, and a
    /// `404` or `403` already known, since neither changes while scuttle runs.
    /// Returns whether the session token was rejected, which stops every refresh.
    pub fn refuse(&mut self, refusal: Refusal) -> bool {
        match refusal {
            Refusal::Absent => *self = LimitState::Absent,
            Refusal::Unlicensed(message) => *self = LimitState::Refused(message),
            Refusal::Unauthorized => {
                if matches!(self, LimitState::Unknown | LimitState::Failed(_)) {
                    *self = LimitState::Failed(UNAUTHORIZED.into());
                }
                return true;
            }
            Refusal::Failed(message) => {
                if matches!(self, LimitState::Unknown | LimitState::Failed(_)) {
                    *self = LimitState::Failed(message);
                }
            }
        }
        false
    }
}

/// Which limit a request was for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    Spend,
    Quota,
}

/// Why a limit request failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// `404`: the deployment has no such route, as an open-source build does not.
    Absent,
    /// `403`: the deployment is not licensed for it, with the server's message.
    Unlicensed(String),
    /// `401`: the session token was rejected.
    Unauthorized,
    /// Anything else, with its message.
    Failed(String),
}

/// What a limit reads as after the session token was rejected.
pub const UNAUTHORIZED: &str = "the session token was rejected";

/// `used` as a whole percent of `limit`, rounded down: `None` for a negative limit, which means
/// none applies, and 100 for a zero limit, which leaves no room. Clamped, so it never overflows.
pub fn percent(used: i64, limit: i64) -> Option<i64> {
    match limit {
        ..0 => None,
        0 => Some(100),
        _ => {
            let p = i128::from(used) * 100 / i128::from(limit);
            Some(p.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64)
        }
    }
}

/// The context window's use as a percent, when the model reports a limit above zero.
pub fn context_percent(u: &ContextUsage) -> Option<i64> {
    u.limit
        .filter(|limit| *limit > 0)
        .and_then(|limit| percent(u.used, limit))
}

/// The spend as a percent of the effective budget; `None` when no budget applies.
pub fn spend_percent(s: &types::CodersdkUserAiSpendStatus) -> Option<i64> {
    let limit = s.effective_budget.as_ref()?.spend_limit_micros?;
    percent(s.current_spend_micros.unwrap_or(0), limit)
}

/// The credits used as a percent of the quota; `None` when no quota applies (`budget` -1).
pub fn quota_percent(q: &types::CodersdkWorkspaceQuota) -> Option<i64> {
    percent(q.credits_consumed.unwrap_or(0), q.budget?)
}

/// Whether `percent` reached `threshold`. No threshold, or nothing to measure, never warns.
pub fn crossed(percent: Option<i64>, threshold: Option<u8>) -> bool {
    matches!((percent, threshold), (Some(p), Some(t)) if p >= i64::from(t))
}

/// The spend limit, when a budget applies.
fn spend_limit(s: &types::CodersdkUserAiSpendStatus) -> Option<i64> {
    s.effective_budget.as_ref()?.spend_limit_micros
}

/// The footer's spend field: `spend $1.20/$50.00`, or `spend $1.20` when no budget applies.
pub fn spend_field(s: &types::CodersdkUserAiSpendStatus) -> String {
    let spent = format_cost_micros(s.current_spend_micros.unwrap_or(0));
    match spend_limit(s) {
        Some(limit) => format!("spend {spent}/{}", format_cost_micros(limit)),
        None => format!("spend {spent}"),
    }
}

/// When the spend period ends, for the footer: `(resets in 26d)`, or `(resets now)` once it
/// is less than a minute away or past. `None` when the server names no period end.
pub fn spend_resets(s: &types::CodersdkUserAiSpendStatus, now_unix: i64) -> Option<String> {
    let end = s.period_end?;
    Some(format!(
        "(resets {})",
        crate::time::until(end.timestamp(), now_unix)
    ))
}

/// `/usage`'s spend row: the amount against the budget, or that spend is unlimited.
pub fn spend_summary(s: &types::CodersdkUserAiSpendStatus) -> String {
    let used = s.current_spend_micros.unwrap_or(0);
    let spent = format_cost_micros(used);
    let Some(limit) = spend_limit(s) else {
        return format!("{spent} this period; no budget applies, so spend is unlimited");
    };
    // The gateway blocks a request once spend reaches the limit, so reaching it is the end.
    if used >= limit {
        format!("{spent} of {}, limit reached", format_cost_micros(limit))
    } else {
        format!(
            "{spent} of {} ({}%)",
            format_cost_micros(limit),
            percent(used, limit).unwrap_or(0)
        )
    }
}

/// Where the budget comes from, for `/usage`; `None` when no budget applies.
pub fn budget_source(s: &types::CodersdkUserAiSpendStatus) -> Option<String> {
    let budget = s.effective_budget.as_ref()?;
    Some(match budget.limit_source.as_ref().map(|l| l.0.as_str()) {
        Some("user_override") => "Set for you, in place of your group's budget".into(),
        Some("group") => "Your group's budget".into(),
        Some(other) => other.replace('_', " "),
        None => "unknown".into(),
    })
}

/// The quota's budget, when a quota applies: the server sends -1 when none does.
fn quota_budget(q: &types::CodersdkWorkspaceQuota) -> Option<i64> {
    q.budget.filter(|b| *b >= 0)
}

/// The footer's quota field, `quota 3/10`; `None` when no quota applies.
pub fn quota_field(q: &types::CodersdkWorkspaceQuota) -> Option<String> {
    let budget = quota_budget(q)?;
    Some(format!(
        "quota {}/{budget}",
        q.credits_consumed.unwrap_or(0)
    ))
}

/// `/usage`'s quota row.
pub fn quota_summary(q: &types::CodersdkWorkspaceQuota) -> String {
    let used = q.credits_consumed.unwrap_or(0);
    match quota_budget(q) {
        Some(budget) => format!(
            "{used} of {budget} credits used ({}%)",
            percent(used, budget).unwrap_or(0)
        ),
        None => format!(
            "No quota applies; your workspaces use {}",
            count(used, "credit")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn costs_read_like_the_web() {
        assert_eq!(format_cost_micros(0), "$0.00");
        assert_eq!(format_cost_micros(4_200), "$0.0042", "below one cent");
        assert_eq!(format_cost_micros(1_234_560_000), "$1,234.56");
        assert_eq!(format_cost_micros(1_230_000), "$1.23");
        assert_eq!(
            format_cost_micros(10_000),
            "$0.01",
            "one cent takes two decimals"
        );
        assert_eq!(
            format_cost_micros(50),
            "$0.0001",
            "rounds up into a sub-cent amount"
        );
        assert_eq!(
            format_cost_micros(49),
            "$0.00",
            "rounds to zero, so two decimals"
        );
        assert_eq!(format_cost_micros(1_234_567_890_000), "$1,234,567.89");
        assert_eq!(format_cost_micros(-4_200), "-$0.0042");
        assert_eq!(format_cost_micros(-1_234_560_000), "-$1,234.56");
    }

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
        let result = format_tokens(i64::MIN);
        assert!(result.starts_with('-'));
    }

    #[test]
    fn context_windows_read_short() {
        assert_eq!(short_tokens(200_000), "200k");
        assert_eq!(short_tokens(128_000), "128k");
        assert_eq!(short_tokens(32_768), "33k");
        assert_eq!(short_tokens(1_000_000), "1M");
        assert_eq!(short_tokens(1_500_000), "1.5M");
        assert_eq!(short_tokens(999), "999");
        assert_eq!(
            short_tokens(999_900),
            "1M",
            "a value this close to a million rounds up, not to 1000k"
        );
    }

    #[test]
    fn the_spend_period_end_reads_as_when_it_resets() {
        let spend: types::CodersdkUserAiSpendStatus = serde_json::from_value(
            json!({"current_spend_micros": 1, "period_end": "2026-11-01T00:00:00Z"}),
        )
        .unwrap();
        let end = chrono::DateTime::parse_from_rfc3339("2026-11-01T00:00:00Z")
            .unwrap()
            .timestamp();
        assert_eq!(
            spend_resets(&spend, end - 26 * 86_400).as_deref(),
            Some("(resets in 26d)")
        );
        assert_eq!(
            spend_resets(&spend, end + 60).as_deref(),
            Some("(resets now)"),
            "a period already over"
        );
        let open: types::CodersdkUserAiSpendStatus =
            serde_json::from_value(json!({"current_spend_micros": 1})).unwrap();
        assert_eq!(
            spend_resets(&open, end),
            None,
            "no period end, no reset text"
        );
    }

    fn spend(spent: i64, limit: Option<i64>, source: &str) -> types::CodersdkUserAiSpendStatus {
        let budget = limit.map(|l| json!({"spend_limit_micros": l, "limit_source": source}));
        serde_json::from_value(json!({"current_spend_micros": spent, "effective_budget": budget}))
            .unwrap()
    }

    fn quota(used: i64, budget: i64) -> types::CodersdkWorkspaceQuota {
        serde_json::from_value(json!({"credits_consumed": used, "budget": budget})).unwrap()
    }

    #[test]
    fn percents_never_divide_by_zero_or_overflow() {
        assert_eq!(percent(1, 4), Some(25));
        assert_eq!(percent(5, 0), Some(100), "a zero limit leaves no room");
        assert_eq!(percent(0, 0), Some(100));
        assert_eq!(percent(3, -1), None, "a negative limit means none applies");
        assert_eq!(percent(i64::MAX, 1), Some(i64::MAX), "clamped, not wrapped");
        assert_eq!(percent(i64::MAX, i64::MAX), Some(100));
        let u = ContextUsage {
            used: 50,
            limit: Some(200),
        };
        assert_eq!(context_percent(&u), Some(25));
        let no_limit = ContextUsage {
            used: 50,
            limit: Some(0),
        };
        assert_eq!(
            context_percent(&no_limit),
            None,
            "the footer shows no percent then"
        );
    }

    #[test]
    fn a_threshold_is_crossed_at_its_percent() {
        assert!(crossed(Some(80), Some(80)));
        assert!(crossed(Some(120), Some(80)));
        assert!(!crossed(Some(79), Some(80)));
        assert!(!crossed(Some(100), None), "no threshold never warns");
        assert!(!crossed(None, Some(1)), "no limit never warns");
    }

    #[test]
    fn spend_texts_name_the_budget_and_where_it_comes_from() {
        let group = spend(1_200_000, Some(50_000_000), "group");
        assert_eq!(spend_field(&group), "spend $1.20/$50.00");
        assert_eq!(spend_summary(&group), "$1.20 of $50.00 (2%)");
        assert_eq!(spend_percent(&group), Some(2));
        assert_eq!(
            budget_source(&group).as_deref(),
            Some("Your group's budget")
        );
        let mine = spend(1_200_000, Some(50_000_000), "user_override");
        assert_eq!(
            budget_source(&mine).as_deref(),
            Some("Set for you, in place of your group's budget")
        );
        let unlimited = spend(1_200_000, None, "group");
        assert_eq!(spend_field(&unlimited), "spend $1.20");
        assert_eq!(
            spend_summary(&unlimited),
            "$1.20 this period; no budget applies, so spend is unlimited"
        );
        assert_eq!(spend_percent(&unlimited), None);
        assert_eq!(budget_source(&unlimited), None);
        let zero = spend(0, Some(0), "group");
        assert_eq!(
            spend_percent(&zero),
            Some(100),
            "a zero budget is already reached"
        );
        assert_eq!(spend_summary(&zero), "$0.00 of $0.00, limit reached");
        let over = spend(60_000_000, Some(50_000_000), "group");
        assert_eq!(spend_summary(&over), "$60.00 of $50.00, limit reached");
        let empty: types::CodersdkUserAiSpendStatus = serde_json::from_value(json!({})).unwrap();
        assert_eq!(
            spend_field(&empty),
            "spend $0.00",
            "missing fields read as zero"
        );
    }

    #[test]
    fn quota_texts_say_when_no_quota_applies() {
        let some = quota(3, 10);
        assert_eq!(quota_field(&some).as_deref(), Some("quota 3/10"));
        assert_eq!(quota_summary(&some), "3 of 10 credits used (30%)");
        assert_eq!(quota_percent(&some), Some(30));
        let none = quota(3, -1);
        assert_eq!(
            quota_field(&none),
            None,
            "the footer hides a quota that does not apply"
        );
        assert_eq!(
            quota_summary(&none),
            "No quota applies; your workspaces use 3 credits"
        );
        assert_eq!(quota_percent(&none), None);
        let zero = quota(0, 0);
        assert_eq!(quota_field(&zero).as_deref(), Some("quota 0/0"));
        assert_eq!(quota_percent(&zero), Some(100));
    }

    #[test]
    fn a_limit_keeps_what_loaded_through_a_failure_and_stops_asking_after_a_refusal() {
        let mut state = LimitState::Loaded(7u8);
        assert!(!state.refuse(Refusal::Failed("HTTP 502".into())));
        assert_eq!(state.loaded(), Some(&7), "a failed refresh keeps the value");
        assert!(state.refuse(Refusal::Unauthorized), "a 401 says so");
        assert_eq!(state.loaded(), Some(&7));
        let mut unknown = LimitState::<u8>::Unknown;
        assert!(unknown.refreshes());
        unknown.refuse(Refusal::Unauthorized);
        assert!(matches!(&unknown, LimitState::Failed(m) if m == UNAUTHORIZED));
        unknown.refuse(Refusal::Absent);
        assert!(matches!(unknown, LimitState::Absent));
        assert!(
            !unknown.refreshes(),
            "an open-source deployment never grows the route"
        );
        let mut refused = LimitState::<u8>::Unknown;
        refused.refuse(Refusal::Unlicensed(
            "AI Gateway is a Premium feature. Contact sales!".into(),
        ));
        assert!(matches!(&refused, LimitState::Refused(m) if m.starts_with("AI Gateway")));
        assert!(!refused.refreshes());
        refused.refuse(Refusal::Failed("HTTP 502".into()));
        assert!(
            matches!(&refused, LimitState::Refused(_)),
            "a failure does not undo a known refusal"
        );
    }
}
