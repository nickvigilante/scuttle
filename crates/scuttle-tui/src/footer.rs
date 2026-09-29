//! The one-line status footer: model, context, and status, or the given notice.

use ratatui::text::{Line, Span};
use scuttle_core::app::{App, Connection, Notice};
use scuttle_core::usage::{context_usage, format_tokens};
use unicode_width::UnicodeWidthChar;

use crate::theme::Theme;

fn fit(text: String, width: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > width {
            break;
        }
        used += w;
        out.push(c);
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
        parts.push(name);
    }
    if let Some(u) = context_usage(app.transcript.messages()) {
        match u.limit {
            Some(limit) if limit > 0 => {
                let pct = u.used * 100 / limit;
                parts.push(format!(
                    "{}/{} ({pct}%)",
                    format_tokens(u.used),
                    format_tokens(limit)
                ));
            }
            _ => parts.push(format_tokens(u.used)),
        }
    }
    let status = match app.connection {
        Connection::Reconnecting { attempt } => format!("reconnecting (attempt {attempt})"),
        Connection::Connecting => "connecting".into(),
        Connection::Idle => "new chat".into(),
        Connection::Live => app
            .transcript
            .status
            .as_ref()
            .map(|s| s.as_str().replace('_', " "))
            .unwrap_or_else(|| "ready".into()),
    };
    parts.push(status);
    Line::from(Span::styled(fit(parts.join(" · "), width), theme.dim))
}

#[cfg(test)]
mod tests {
    use super::*;
    use scuttle_core::app::App;
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
}
