//! The one-line status footer: model, context, organization, plan mode, and status, or the
//! given notice.

use coder_sdk::ChatStatus;
use ratatui::text::{Line, Span};
use scuttle_core::app::{App, Connection, Notice};
use scuttle_core::transcript::RetryInfo;
use scuttle_core::usage::{context_usage, format_tokens};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;

/// Cuts `text` to `width` columns on one line, at grapheme cluster boundaries measured the way
/// `wrap.rs` measures them. Line breaks become spaces so a notice never spills over.
fn fit(text: String, width: usize) -> String {
    let text = text.replace(['\n', '\r'], " ");
    let mut used = 0;
    let mut out = String::new();
    for g in text.graphemes(true) {
        let w = g.width();
        if used + w > width {
            break;
        }
        used += w;
        out.push_str(g);
    }
    out
}

/// "retrying (attempt N, in Xs): reason", leaving out whatever the server did not report.
fn retry_status(retry: &RetryInfo) -> String {
    let mut out = format!("retrying (attempt {}", retry.attempt);
    if retry.delay_ms > 0 {
        out.push_str(&format!(", in {:.1}s", retry.delay_ms as f64 / 1000.0));
    }
    out.push(')');
    if !retry.error.is_empty() {
        out.push_str(": ");
        out.push_str(&retry.error);
    }
    out
}

/// Renders the footer. When `notice` is `Some`, it replaces the status line; the
/// caller decides which notice (if any) is active, since `App::notices` only grows.
pub fn footer_line(app: &App, notice: Option<&Notice>, theme: &Theme, width: u16) -> Line<'static> {
    let width = width as usize;
    if let Some(notice) = notice {
        let (text, style) = match notice {
            Notice::Info(t) => (t.clone(), theme.dim),
            Notice::Error(t) => (t.clone(), theme.error),
        };
        return Line::from(Span::styled(fit(text, width), style));
    }
    let mut parts = Vec::new();
    if let Some(name) = app.model_name() {
        parts.push(match app.effort() {
            Some(effort) => format!("{name} ({effort})"),
            None => name,
        });
    }
    if let Some(u) = context_usage(app.transcript.messages()) {
        match u.limit {
            Some(limit) if limit > 0 => {
                let pct = i128::from(u.used) * 100 / i128::from(limit);
                parts.push(format!(
                    "{}/{} ({pct}%)",
                    format_tokens(u.used),
                    format_tokens(limit)
                ));
            }
            _ => parts.push(format_tokens(u.used)),
        }
    }
    let org_at = (app.organizations.len() > 1).then(|| {
        parts.push(app.org_label(app.current_org()));
        parts.len() - 1
    });
    if app.plan_mode {
        parts.push("plan mode".into());
    }
    let status = match app.connection {
        Connection::Reconnecting { attempt } => match app.last_stream_error.as_deref() {
            Some(error) => format!("reconnecting (attempt {attempt}): {error}"),
            None => format!("reconnecting (attempt {attempt})"),
        },
        Connection::Connecting => "connecting".into(),
        Connection::Idle => "new chat".into(),
        Connection::Live => match (&app.transcript.retry, &app.transcript.status) {
            (Some(retry), _) => retry_status(retry),
            (None, Some(ChatStatus::RequiresAction)) => "action required".into(),
            (None, Some(s)) => s.as_str().replace('_', " "),
            (None, None) => "ready".into(),
        },
    };
    parts.push(status);
    // The organization is the least urgent part, so it goes first when the line is too long.
    if let Some(i) = org_at
        && UnicodeWidthStr::width(parts.join(" · ").as_str()) > width
    {
        parts.remove(i);
    }
    Line::from(Span::styled(fit(parts.join(" · "), width), theme.dim))
}

#[cfg(test)]
mod tests {
    use super::*;
    use scuttle_core::app::{App, Msg};
    use scuttle_core::config::BusyBehavior;
    use serde_json::json;

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn shows_model_context_and_status() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let id = uuid::Uuid::new_v4();
        app.update(scuttle_core::app::Msg::ModelsLoaded(vec![serde_json::from_value(json!({"id": id, "display_name": "Big", "is_default": true, "enabled": true, "reasoning_efforts": []})).unwrap()]));
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap();
        app.update(scuttle_core::app::Msg::ChatLoaded { chat: Box::new(chat), messages: serde_json::from_value(json!([{"id": 1, "role": "assistant", "content": [], "usage": {"input_tokens": 12000, "context_limit": 200000}}])).unwrap() });
        let line = footer_line(&app, app.notices.last(), &Theme::terminal(true), 80);
        let t = text(&line);
        assert!(t.contains("Big"), "{t}");
        assert!(t.contains("12.0k/200.0k"), "{t}");
        assert!(t.contains("connecting"), "{t}");
    }

    #[test]
    fn latest_notice_replaces_the_status_and_reconnecting_is_visible() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.connection = Connection::Reconnecting { attempt: 2 };
        assert!(
            text(&footer_line(
                &app,
                app.notices.last(),
                &Theme::terminal(true),
                80
            ))
            .contains("reconnecting")
        );
        app.notices
            .push(Notice::Error("Could not send message: HTTP 409".into()));
        assert!(
            text(&footer_line(
                &app,
                app.notices.last(),
                &Theme::terminal(true),
                80
            ))
            .contains("HTTP 409")
        );
    }

    #[test]
    fn notice_none_with_notices_present_shows_the_status_line_instead() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.connection = Connection::Reconnecting { attempt: 2 };
        app.notices
            .push(Notice::Error("Could not send message: HTTP 409".into()));
        let t = text(&footer_line(&app, None, &Theme::terminal(true), 80));
        assert!(t.contains("reconnecting"), "{t}");
        assert!(!t.contains("HTTP 409"), "{t}");
    }

    fn live_app(status: &str) -> App {
        let mut app = App::new(BusyBehavior::Queue, true);
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap();
        app.update(Msg::ChatLoaded {
            chat: Box::new(chat),
            messages: vec![],
        });
        app.update(stream(
            json!({"type": "status", "status": {"status": status}}),
        ));
        app
    }

    fn stream(v: serde_json::Value) -> Msg {
        Msg::Stream(coder_sdk::StreamEvent {
            kind: coder_sdk::StreamEventType::parse(v["type"].as_str().unwrap_or_default()),
            event: serde_json::from_value(v.clone()).ok(),
            raw: v,
        })
    }

    fn status_text(app: &App) -> String {
        text(&footer_line(app, None, &Theme::terminal(true), 120))
    }

    #[test]
    fn a_provider_retry_shows_the_attempt_delay_and_reason() {
        let mut app = live_app("running");
        app.update(stream(json!({"type": "retry", "retry": {"attempt": 2, "delay_ms": 1500, "error": "rate limited"}})));
        let t = status_text(&app);
        assert!(
            t.contains("retrying (attempt 2, in 1.5s): rate limited"),
            "{t}"
        );
    }

    #[test]
    fn requires_action_reads_action_required() {
        let t = status_text(&live_app("requires_action"));
        assert!(t.contains("action required"), "{t}");
    }

    #[test]
    fn reconnecting_shows_the_last_stream_error() {
        let mut app = live_app("waiting");
        app.update(Msg::StreamEnded {
            error: Some("HTTP 404".into()),
        });
        app.update(Msg::StreamEnded {
            error: Some("chat not found".into()),
        });
        let t = status_text(&app);
        assert!(
            t.contains("reconnecting (attempt 2): chat not found"),
            "{t}"
        );
    }

    #[test]
    fn notice_line_breaks_become_spaces() {
        let app = App::new(BusyBehavior::Queue, true);
        let notice = Notice::Error("first\nsecond\r\nthird".into());
        let t = text(&footer_line(
            &app,
            Some(&notice),
            &Theme::terminal(true),
            80,
        ));
        assert_eq!(t, "first second  third");
    }

    #[test]
    fn fit_never_splits_a_grapheme_cluster() {
        // A ZWJ sequence is one two-column cluster, though its codepoints measure four.
        assert_eq!(
            fit("a\u{1F469}\u{200D}\u{1F4BB}b".into(), 3),
            "a\u{1F469}\u{200D}\u{1F4BB}"
        );
        assert_eq!(fit("ae\u{301}x".into(), 2), "ae\u{301}");
    }

    #[test]
    fn a_huge_token_count_does_not_overflow_the_percentage() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap();
        app.update(Msg::ChatLoaded { chat: Box::new(chat), messages: serde_json::from_value(json!([{"id": 1, "role": "assistant", "content": [], "usage": {"input_tokens": 9_000_000_000_000_000_000_i64, "context_limit": 9_100_000_000_000_000_000_i64}}])).unwrap() });
        let t = status_text(&app);
        assert!(t.contains("(98%)"), "{t}");
    }

    #[test]
    fn fits_narrow_widths() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.notices.push(Notice::Info(
            "a very long notice that will not fit in a narrow terminal at all".into(),
        ));
        let line = footer_line(&app, app.notices.last(), &Theme::terminal(true), 20);
        assert!(unicode_width::UnicodeWidthStr::width(text(&line).as_str()) <= 20);
    }

    #[test]
    fn context_without_a_limit_shows_used_tokens_alone() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap();
        app.update(scuttle_core::app::Msg::ChatLoaded { chat: Box::new(chat), messages: serde_json::from_value(json!([{"id": 1, "role": "assistant", "content": [], "usage": {"input_tokens": 12000}}])).unwrap() });
        let t = text(&footer_line(
            &app,
            app.notices.last(),
            &Theme::terminal(true),
            80,
        ));
        assert!(t.contains("12.0k"), "{t}");
        assert!(!t.contains("12.0k/"), "{t}");
    }

    #[test]
    fn the_organization_shows_when_there_are_several() {
        use scuttle_core::app::OrgRef;
        let org = |name: &str, is_default| OrgRef {
            id: uuid::Uuid::new_v4(),
            name: name.to_lowercase(),
            display_name: name.into(),
            is_default,
        };
        let coder = org("Coder", true);
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::OrganizationsLoaded(vec![coder.clone()]));
        app.update(Msg::Started {
            org_id: coder.id,
            open_chat: None,
        });
        assert!(!status_text(&app).contains("Coder"));
        app.update(Msg::OrganizationsLoaded(vec![coder, org("Product", false)]));
        let t = status_text(&app);
        assert!(t.contains("Coder · new chat"), "{t}");
    }

    /// A chat with a model, context usage, plan mode, and a status, in one of two organizations.
    fn busy_footer_app() -> App {
        use scuttle_core::app::OrgRef;
        let mut app = App::new(BusyBehavior::Queue, true);
        let orgs: Vec<OrgRef> = ["Engineering Platform", "Product"]
            .iter()
            .map(|name| OrgRef {
                id: uuid::Uuid::new_v4(),
                name: name.to_lowercase(),
                display_name: (*name).into(),
                is_default: false,
            })
            .collect();
        app.update(Msg::OrganizationsLoaded(orgs.clone()));
        app.update(Msg::Started {
            org_id: orgs[0].id,
            open_chat: None,
        });
        app.update(Msg::ModelsLoaded(vec![serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "display_name": "Big", "is_default": true, "enabled": true, "reasoning_efforts": []})).unwrap()]));
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}, "plan_mode": "plan"})).unwrap();
        app.update(Msg::ChatLoaded { chat: Box::new(chat), messages: serde_json::from_value(json!([{"id": 1, "role": "assistant", "content": [], "usage": {"input_tokens": 12000, "context_limit": 200000}}])).unwrap() });
        app.update(stream(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        app
    }

    #[test]
    fn the_footer_with_an_organization_never_exceeds_its_width() {
        let app = busy_footer_app();
        let full = status_text(&app);
        assert_eq!(
            full,
            "Big · 12.0k/200.0k (6%) · Engineering Platform · plan mode · running"
        );
        for width in 0..=120u16 {
            let t = text(&footer_line(&app, None, &Theme::terminal(true), width));
            assert!(
                UnicodeWidthStr::width(t.as_str()) <= width as usize,
                "{width}: {t:?}"
            );
        }
    }

    #[test]
    fn the_organization_gives_way_to_the_status_when_space_is_short() {
        let app = busy_footer_app();
        let without_org = "Big · 12.0k/200.0k (6%) · plan mode · running";
        let width = UnicodeWidthStr::width(without_org) as u16;
        assert_eq!(
            text(&footer_line(&app, None, &Theme::terminal(true), width)),
            without_org
        );
    }

    #[test]
    fn plan_mode_shows_in_the_footer() {
        let mut app = live_app("waiting");
        assert!(!status_text(&app).contains("plan mode"));
        app.update(Msg::Command(scuttle_core::commands::Command::PlanMode(
            Some(true),
        )));
        let t = status_text(&app);
        assert!(t.contains("plan mode · waiting"), "{t}");
    }

    #[test]
    fn the_effort_shows_next_to_the_model() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::ModelsLoaded(vec![serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "display_name": "Thinker", "is_default": true, "enabled": true, "reasoning_efforts": ["low", "high"]})).unwrap()]));
        assert_eq!(status_text(&app), "Thinker (high) · new chat");
        app.update(Msg::EffortChosen("low".into()));
        assert_eq!(status_text(&app), "Thinker (low) · new chat");
    }
}
