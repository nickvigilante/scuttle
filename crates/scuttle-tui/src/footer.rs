//! The one-line status footer: the fields `[statusline]` lists, in its order, or the given
//! notice. A field a command changes reads as `/command: value`, and a field past its warning
//! threshold is highlighted, even when the list leaves it out.

use coder_sdk::ChatStatus;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use scuttle_core::app::{App, Connection, CostState, Notice};
use scuttle_core::config::{StatusField, StatuslineConfig, Thresholds};
use scuttle_core::transcript::RetryInfo;
use scuttle_core::usage::{self, context_usage, format_tokens};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::icons::{self, Icon, IconSet};
use crate::markdown::drawable;
use crate::theme::Theme;
use crate::wrap::cells_width;

/// `text` on one line, cut to `width` columns between grapheme clusters, and whether anything
/// was cut. Control characters are dropped as [`drawable`] drops them; then each line break
/// (`\r\n`, `\n`, or a lone `\r`) becomes one space and a tab four spaces, as `wrap.rs`
/// counts a tab.
fn fit_cut(text: &str, width: usize) -> (String, bool) {
    let flat = drawable(text).replace('\n', " ").replace('\t', "    ");
    let mut used = 0;
    let mut out = String::new();
    for g in flat.graphemes(true) {
        let w = g.width();
        if used + w > width {
            return (out, true);
        }
        used += w;
        out.push_str(g);
    }
    (out, false)
}

/// `text` cut to one line of `width` columns; see [`fit_cut`].
fn fit(text: String, width: usize) -> String {
    fit_cut(&text, width).0
}

/// The columns `text` takes once [`fit_cut`] puts it on one line.
fn flat_width(text: &str) -> usize {
    cells_width(&fit_cut(text, usize::MAX).0)
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

/// One footer field: the slash command that changes it, if any, the icon slot that leads its
/// value, its value, whether the value is a warning, and whether the field reached its
/// threshold.
struct Field {
    command: Option<&'static str>,
    /// The icon's slot text, empty for none, drawn as its own span so `NO_COLOR` leaves the
    /// glyph plain and a cut never splits the glyph from its space.
    icon: &'static str,
    value: String,
    /// Text after the value, such as when the spend resets, which the line drops before it
    /// drops any whole field.
    detail: Option<String>,
    warn: bool,
    alarm: bool,
}

impl Field {
    fn command(command: &'static str, value: String) -> Field {
        Field {
            command: Some(command),
            icon: "",
            value,
            detail: None,
            warn: false,
            alarm: false,
        }
    }

    /// A command field whose value is a warning, such as a model the chat can no longer use.
    fn warning(command: &'static str, value: String) -> Field {
        Field {
            warn: true,
            ..Field::command(command, value)
        }
    }

    fn plain(value: String) -> Field {
        Field {
            command: None,
            icon: "",
            value,
            detail: None,
            warn: false,
            alarm: false,
        }
    }

    /// A plain field whose value `icon`'s slot leads.
    fn with_icon(icon: &'static str, value: String) -> Field {
        Field {
            icon,
            ..Field::plain(value)
        }
    }

    /// The field, highlighted when `crossed` says it reached its threshold.
    fn alarm(mut self, crossed: bool) -> Field {
        self.alarm = crossed;
        self
    }

    /// The field as it reads, such as `/effort: high`.
    fn text(&self) -> String {
        let detail = self
            .detail
            .as_deref()
            .map(|d| format!(" {d}"))
            .unwrap_or_default();
        match self.command {
            Some(command) => format!("{command}: {}{detail}", self.value),
            None => format!("{}{}{detail}", self.icon, self.value),
        }
    }
}

/// The connection, a provider retry, or the chat's status. A stream that connects or
/// reconnects leads with `set`'s icon, the one footer state worth catching at a glance.
fn connection_status(app: &App, set: IconSet) -> Field {
    match app.connection {
        Connection::Reconnecting { attempt } => {
            let icon = icons::slot(set, Icon::Reconnecting).text;
            Field::with_icon(
                icon,
                match app.last_stream_error.as_deref() {
                    Some(error) => format!("reconnecting (attempt {attempt}): {error}"),
                    None => format!("reconnecting (attempt {attempt})"),
                },
            )
        }
        Connection::Connecting => {
            Field::with_icon(icons::slot(set, Icon::Connecting).text, "connecting".into())
        }
        Connection::Idle => Field::plain("new chat".into()),
        Connection::Live => Field::plain(match (&app.transcript.retry, &app.transcript.status) {
            (Some(retry), _) => retry_status(retry),
            (None, Some(ChatStatus::RequiresAction)) => "action required".into(),
            (None, Some(s)) => s.as_str().replace('_', " "),
            (None, None) => "ready".into(),
        }),
    }
}

/// Field `name` as `app` has it now, or `None` while it has nothing to show. A field at or
/// past its threshold in `thresholds` is marked as an alarm.
fn field_for(
    app: &App,
    name: StatusField,
    thresholds: &Thresholds,
    set: IconSet,
    now_unix: i64,
) -> Option<Field> {
    match name {
        // A model the chat can no longer use says so until another is picked.
        StatusField::Model => match app.unavailable_model() {
            Some(gone) => Some(Field::warning(
                "/model",
                match gone.name {
                    Some(name) => format!("{name} (unavailable)"),
                    None => "unavailable".to_owned(),
                },
            )),
            None => app.model_name().map(|name| Field::command("/model", name)),
        },
        StatusField::Effort => app.effort().map(|effort| Field::command("/effort", effort)),
        StatusField::Context => context_usage(app.transcript.messages()).map(|u| {
            let percent = usage::context_percent(&u);
            let text = match (u.limit, percent) {
                (Some(limit), Some(p)) => {
                    format!("{}/{} ({p}%)", format_tokens(u.used), format_tokens(limit))
                }
                _ => format_tokens(u.used),
            };
            Field::plain(text).alarm(usage::crossed(percent, thresholds.context))
        }),
        StatusField::Workspace => app
            .workspace_name()
            .map(|name| Field::command("/workspace", name)),
        StatusField::Organization => (app.organizations.len() > 1)
            .then(|| Field::command("/organization", app.org_label(app.current_org()))),
        StatusField::PlanMode => app
            .plan_mode
            .then(|| Field::command("/plan-mode", "on".into())),
        StatusField::Status => Some(connection_status(app, set)),
        StatusField::Cost => match app.chat_cost.as_ref() {
            Some(CostState::Loaded(cost)) => Some(Field::plain(format!(
                "cost {}",
                usage::format_cost_micros(cost.total_cost_micros.unwrap_or(0))
            ))),
            _ => None,
        },
        StatusField::Spend => app.spend().loaded().map(|spend| {
            let mut field = Field::plain(usage::spend_field(spend)).alarm(usage::crossed(
                usage::spend_percent(spend),
                thresholds.spend,
            ));
            field.detail = usage::spend_resets(spend, now_unix);
            field
        }),
        StatusField::Quota => app.quota().loaded().and_then(|quota| {
            let text = usage::quota_field(quota)?;
            Some(Field::plain(text).alarm(usage::crossed(
                usage::quota_percent(quota),
                thresholds.quota,
            )))
        }),
        StatusField::Queue => {
            let queued = app.transcript.queued.len();
            (queued > 0).then(|| Field::plain(format!("queue {queued}")))
        }
        StatusField::Mcp => app
            .mcp_on_count()
            .map(|n| Field::command("/mcp", format!("{n} on"))),
    }
}

/// The order whole fields give way when the line is too long, first to last. The status is
/// never dropped, only cut once nothing else is left.
const DROP_ORDER: [StatusField; 11] = [
    StatusField::Workspace,
    StatusField::Organization,
    StatusField::Queue,
    StatusField::Mcp,
    StatusField::Cost,
    StatusField::Quota,
    StatusField::Effort,
    StatusField::Spend,
    StatusField::Context,
    StatusField::Model,
    StatusField::PlanMode,
];

/// The field to drop next: the first in `DROP_ORDER`, taking a field that warns, such as one
/// past its threshold or an unavailable model, only after every field that does not, and
/// never the status.
fn drop_candidate(fields: &[(StatusField, Field)]) -> Option<usize> {
    fields
        .iter()
        .enumerate()
        .filter(|(_, (name, _))| *name != StatusField::Status)
        .min_by_key(|(_, (name, field))| {
            (
                field.alarm || field.warn,
                DROP_ORDER.iter().position(|d| d == name),
            )
        })
        .map(|(i, _)| i)
}

/// The columns `fields` take, joined with ` · `.
fn fields_width(fields: &[(StatusField, Field)]) -> usize {
    let joined: Vec<String> = fields.iter().map(|(_, f)| f.text()).collect();
    flat_width(&joined.join(" · "))
}

/// `fields` as spans cut to `width` columns: each command in the brand accent, its value dim
/// or, for a warning, in the warning color, the separators and the plain fields dim, and a
/// field past its threshold in the warning color, bold.
fn field_spans(fields: &[(StatusField, Field)], theme: &Theme, width: usize) -> Vec<Span<'static>> {
    let alarm = theme.warn.add_modifier(Modifier::BOLD);
    // Each span, and whether it is an icon's slot, which is drawn whole or not at all.
    let mut spans: Vec<(Span<'static>, bool)> = Vec::new();
    for (i, (_, field)) in fields.iter().enumerate() {
        if i > 0 {
            spans.push((Span::styled(" · ", theme.dim), false));
        }
        match field.command {
            Some(command) => {
                spans.push((
                    Span::styled(command, if field.alarm { alarm } else { theme.brand }),
                    false,
                ));
                let style = match (field.alarm, field.warn) {
                    (true, _) => alarm,
                    (false, true) => theme.warn,
                    (false, false) => theme.dim,
                };
                spans.push((Span::styled(format!(": {}", field.value), style), false));
            }
            None => {
                let style = if field.alarm { alarm } else { theme.dim };
                if !field.icon.is_empty() {
                    spans.push((Span::styled(field.icon, icons::style(theme, style)), true));
                }
                spans.push((Span::styled(field.value.clone(), style), false));
            }
        }
        if let Some(detail) = field.detail.as_ref() {
            // The detail reads in the value's own style. Only plain fields carry one.
            let style = if field.alarm { alarm } else { theme.dim };
            spans.push((Span::styled(format!(" {detail}"), style), false));
        }
    }
    let mut left = width;
    let mut out = Vec::new();
    for (span, icon) in spans {
        if icon && cells_width(&span.content) > left {
            break;
        }
        let (text, cut) = fit_cut(&span.content, left);
        left -= cells_width(&text);
        if !text.is_empty() {
            out.push(Span::styled(text, span.style));
        }
        if cut {
            break;
        }
    }
    out
}

/// Renders the footer at `now_unix`: `notice` in place of the fields when it is `Some`, else
/// the fields `statusline` lists, in its order, then any field it leaves out that reached its
/// threshold. The caller decides which notice (if any) is active, since `App::notices` only
/// grows.
pub fn status_line_at(
    app: &App,
    notice: Option<&Notice>,
    statusline: &StatuslineConfig,
    theme: &Theme,
    width: u16,
    now_unix: i64,
) -> Line<'static> {
    let width = width as usize;
    if let Some(notice) = notice {
        return match notice {
            Notice::Info(t) => Line::from(Span::styled(fit(t.clone(), width), theme.dim)),
            // An error leads with its icon when the line has room for the icon's slot.
            Notice::Error(t) => {
                let icon = usize::from(icons::slot(theme.icons, Icon::Error).width);
                match width.checked_sub(icon).filter(|_| icon > 0) {
                    Some(room) => Line::from(icons::line_with(
                        theme,
                        "",
                        Icon::Error,
                        &fit(t.clone(), room),
                        theme.error,
                    )),
                    None => Line::from(Span::styled(fit(t.clone(), width), theme.error)),
                }
            }
        };
    }
    let thresholds = &statusline.thresholds;
    let mut fields: Vec<(StatusField, Field)> = statusline
        .fields
        .iter()
        .filter_map(|&name| {
            field_for(app, name, thresholds, theme.icons, now_unix).map(|f| (name, f))
        })
        .collect();
    fields.extend(
        StatusField::ALL
            .into_iter()
            .filter(|name| !statusline.fields.contains(name))
            // Only a field with a threshold set can reach it.
            .filter(|name| thresholds.get(*name).is_some())
            .filter_map(|name| {
                field_for(app, name, thresholds, theme.icons, now_unix)
                    .filter(|f| f.alarm)
                    .map(|f| (name, f))
            }),
    );
    while fields_width(&fields) > width {
        // A field's detail, such as when the spend resets, goes before any whole field.
        if let Some((_, field)) = fields.iter_mut().find(|(_, f)| f.detail.is_some()) {
            field.detail = None;
            continue;
        }
        let Some(i) = drop_candidate(&fields) else {
            break;
        };
        fields.remove(i);
    }
    Line::from(field_spans(&fields, theme, width))
}

/// [`status_line_at`] at a fixed time, for the tests that do not show when the spend resets.
#[cfg(test)]
pub fn status_line(
    app: &App,
    notice: Option<&Notice>,
    statusline: &StatuslineConfig,
    theme: &Theme,
    width: u16,
) -> Line<'static> {
    status_line_at(app, notice, statusline, theme, width, 0)
}

/// [`status_line`] with the default settings, as the footer was before `/statusline`.
#[cfg(test)]
pub fn footer_line(app: &App, notice: Option<&Notice>, theme: &Theme, width: u16) -> Line<'static> {
    status_line(app, notice, &StatuslineConfig::default(), theme, width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use scuttle_core::app::{App, Msg};
    use scuttle_core::config::BusyBehavior;
    use scuttle_core::panels::Fetched;
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
        app.update(scuttle_core::app::Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: serde_json::from_value(json!([{"id": 1, "role": "assistant", "content": [], "usage": {"input_tokens": 12000, "context_limit": 200000}}])).unwrap(),
        });
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
            has_more: None,
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
    fn a_notice_reads_as_one_line_with_one_space_per_line_break() {
        let app = App::new(BusyBehavior::Queue, true);
        let notice = Notice::Error("first\nsecond\r\nthird\rfourth\tfifth\u{7}!".into());
        let t = text(&footer_line(
            &app,
            Some(&notice),
            &Theme::terminal(true),
            80,
        ));
        assert_eq!(t, "first second third fourth    fifth!");
    }

    #[test]
    fn a_cut_notice_never_exceeds_the_width_or_splits_a_cluster() {
        let app = App::new(BusyBehavior::Queue, true);
        let notice = Notice::Info("tab\there \u{1F469}\u{200D}\u{1F4BB} e\u{301}\r\nend".into());
        for width in 0..=30u16 {
            let t = text(&footer_line(
                &app,
                Some(&notice),
                &Theme::terminal(true),
                width,
            ));
            assert!(cells_width(&t) <= width as usize, "{width}: {t:?}");
            assert!(!t.contains(['\t', '\r', '\n']), "{width}: {t:?}");
            assert!(!t.ends_with('\u{200D}'), "{width}: {t:?}");
        }
    }

    #[test]
    fn a_status_with_a_line_break_is_measured_as_drawn() {
        let mut app = live_app("running");
        app.update(stream(
            json!({"type": "retry", "retry": {"attempt": 1, "error": "rate\r\nlimited"}}),
        ));
        let t = status_text(&app);
        assert!(t.ends_with("retrying (attempt 1): rate limited"), "{t}");
        let width = cells_width(&t) as u16;
        assert_eq!(
            text(&footer_line(&app, None, &Theme::terminal(true), width)),
            t,
            "the width that decides what drops counts the break as one column"
        );
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
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: serde_json::from_value(json!([{"id": 1, "role": "assistant", "content": [], "usage": {"input_tokens": 9_000_000_000_000_000_000_i64, "context_limit": 9_100_000_000_000_000_000_i64}}])).unwrap(),
        });
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
        app.update(scuttle_core::app::Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: serde_json::from_value(json!([{"id": 1, "role": "assistant", "content": [], "usage": {"input_tokens": 12000}}])).unwrap(),
        });
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
            can_create_chats: true,
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
        assert!(t.contains("/organization: Coder · new chat"), "{t}");
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
                can_create_chats: true,
            })
            .collect();
        app.update(Msg::OrganizationsLoaded(orgs.clone()));
        app.update(Msg::Started {
            org_id: orgs[0].id,
            open_chat: None,
        });
        app.update(Msg::ModelsLoaded(vec![serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "display_name": "Big", "is_default": true, "enabled": true, "reasoning_efforts": []})).unwrap()]));
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}, "plan_mode": "plan"})).unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: serde_json::from_value(json!([{"id": 1, "role": "assistant", "content": [], "usage": {"input_tokens": 12000, "context_limit": 200000}}])).unwrap(),
        });
        app.update(stream(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        app
    }

    /// `busy_footer_app` with the workspace "dev" attached.
    fn workspace_footer_app() -> App {
        let mut app = busy_footer_app();
        let ws = uuid::Uuid::new_v4();
        app.update(Msg::WorkspacesLoaded(vec![
            scuttle_core::app::WorkspaceRef {
                id: ws,
                name: "dev".into(),
                ..Default::default()
            },
        ]));
        app.update(Msg::WorkspaceChosen(Some(ws)));
        app
    }

    #[test]
    fn the_attached_workspace_shows_before_the_organization() {
        let t = status_text(&workspace_footer_app());
        assert_eq!(
            t,
            "/model: Big · 12.0k/200.0k (6%) · /workspace: dev · /organization: Engineering Platform · /plan-mode: on · running"
        );
    }

    #[test]
    fn the_workspace_gives_way_before_the_organization() {
        let app = workspace_footer_app();
        let with_org = "/model: Big · 12.0k/200.0k (6%) · /organization: Engineering Platform · /plan-mode: on · running";
        let width = UnicodeWidthStr::width(with_org) as u16;
        assert_eq!(
            text(&footer_line(&app, None, &Theme::terminal(true), width)),
            with_org
        );
        let neither = "/model: Big · 12.0k/200.0k (6%) · /plan-mode: on · running";
        let width = UnicodeWidthStr::width(neither) as u16;
        assert_eq!(
            text(&footer_line(&app, None, &Theme::terminal(true), width)),
            neither
        );
        for width in 0..=140u16 {
            let t = text(&footer_line(&app, None, &Theme::terminal(true), width));
            assert!(
                UnicodeWidthStr::width(t.as_str()) <= width as usize,
                "{width}: {t:?}"
            );
        }
    }

    #[test]
    fn commands_are_in_the_brand_accent_and_values_are_dim() {
        let app = workspace_footer_app();
        let theme = Theme::terminal(true);
        let line = footer_line(&app, None, &theme, 140);
        for command in ["/model", "/workspace", "/organization", "/plan-mode"] {
            let span = line
                .spans
                .iter()
                .find(|s| s.content == command)
                .unwrap_or_else(|| panic!("{command} is not its own span: {line:?}"));
            assert_eq!(span.style, theme.brand, "{command}");
        }
        let value = line.spans.iter().find(|s| s.content == ": dev").unwrap();
        assert_eq!(value.style, theme.dim);
        let status = line.spans.iter().find(|s| s.content == "running").unwrap();
        assert_eq!(status.style, theme.dim);
    }

    #[test]
    fn the_footer_with_an_organization_never_exceeds_its_width() {
        let app = busy_footer_app();
        let full = status_text(&app);
        assert_eq!(
            full,
            "/model: Big · 12.0k/200.0k (6%) · /organization: Engineering Platform · /plan-mode: on · running"
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
        let without_org = "/model: Big · 12.0k/200.0k (6%) · /plan-mode: on · running";
        let width = UnicodeWidthStr::width(without_org) as u16;
        assert_eq!(
            text(&footer_line(&app, None, &Theme::terminal(true), width)),
            without_org
        );
    }

    #[test]
    fn plan_mode_shows_in_the_footer() {
        let mut app = live_app("waiting");
        assert!(!status_text(&app).contains("/plan-mode"));
        app.update(Msg::Command(scuttle_core::commands::Command::PlanMode(
            Some(true),
        )));
        let t = status_text(&app);
        assert!(t.contains("/plan-mode: on · waiting"), "{t}");
    }

    #[test]
    fn the_effort_shows_next_to_the_model() {
        let mut app = App::new(BusyBehavior::Queue, true);
        app.update(Msg::ModelsLoaded(vec![serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "display_name": "Thinker", "is_default": true, "enabled": true, "reasoning_efforts": ["low", "high"]})).unwrap()]));
        assert_eq!(
            status_text(&app),
            "/model: Thinker · /effort: high · new chat"
        );
        app.update(Msg::EffortChosen("low".into()));
        assert_eq!(
            status_text(&app),
            "/model: Thinker · /effort: low · new chat"
        );
    }

    #[test]
    fn an_unavailable_model_reads_as_unavailable_in_the_warning_color() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let old = uuid::Uuid::new_v4();
        app.update(Msg::ModelsLoaded(vec![serde_json::from_value(json!({"id": old, "display_name": "Old", "enabled": false, "reasoning_efforts": []})).unwrap()]));
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "last_model_config_id": old, "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: vec![],
        });
        let theme = Theme::terminal(true);
        let line = footer_line(&app, None, &theme, 120);
        let t = text(&line);
        assert!(t.starts_with("/model: Old (unavailable) · "), "{t}");
        assert_eq!(
            line.spans[0].style, theme.brand,
            "the command keeps the accent"
        );
        let value = line
            .spans
            .iter()
            .find(|s| s.content.contains("Old"))
            .unwrap();
        assert_eq!(value.style, theme.warn, "the value warns");
        // A deleted model is not in the list at all, so the field has no name to show.
        app.update(Msg::ModelsLoaded(vec![]));
        let t = text(&footer_line(&app, None, &theme, 120));
        assert!(t.starts_with("/model: unavailable · "), "{t}");
    }

    use scuttle_core::app::Effect;

    fn listing(fields: Vec<StatusField>, thresholds: Thresholds) -> StatuslineConfig {
        StatuslineConfig { fields, thresholds }
    }

    /// `app` with its spend loaded as `spent` of `limit` micros, through a refresh of its own.
    fn with_spend(app: &mut App, spent: i64, limit: i64) {
        with_spend_ending(app, spent, limit, None);
    }

    /// [`with_spend`] for a spend period that ends at `period_end`, an RFC 3339 time.
    fn with_spend_ending(app: &mut App, spent: i64, limit: i64, period_end: Option<&str>) {
        let generation = app
            .update(Msg::RefreshLimits)
            .iter()
            .find_map(|e| match e {
                Effect::FetchSpend { generation } => Some(*generation),
                _ => None,
            })
            .expect("a spend fetch");
        app.update(Msg::SpendLoaded {
            spend: Box::new(
                serde_json::from_value(json!({"current_spend_micros": spent,
                    "effective_budget": {"spend_limit_micros": limit, "limit_source": "group"},
                    "period_end": period_end}))
                .unwrap(),
            ),
            generation,
        });
    }

    #[test]
    fn the_spend_field_says_when_the_budget_resets_and_drops_that_first() {
        let mut app = busy_footer_app();
        with_spend_ending(
            &mut app,
            1_200_000,
            50_000_000,
            Some("2026-11-01T00:00:00Z"),
        );
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-06T00:00:00Z")
            .unwrap()
            .timestamp();
        let theme = Theme::terminal(true);
        let cfg = listing(
            vec![StatusField::Spend, StatusField::Status],
            Thresholds::default(),
        );
        let at = |width: u16| text(&status_line_at(&app, None, &cfg, &theme, width, now));
        assert_eq!(at(120), "spend $1.20/$50.00 (resets in 26d) · running");
        let full = "spend $1.20/$50.00 (resets in 26d) · running";
        let short = "spend $1.20/$50.00 · running";
        assert_eq!(
            at(cells_width(full) as u16 - 1),
            short,
            "the reset text goes first"
        );
        assert_eq!(at(cells_width(short) as u16), short);
        assert_eq!(
            at(cells_width(short) as u16 - 1),
            "running",
            "then the field"
        );
        for width in 0..=60u16 {
            assert!(cells_width(&at(width)) <= width as usize, "{width}");
        }
        let line = status_line_at(&app, None, &cfg, &theme, 120, now);
        let reset = line
            .spans
            .iter()
            .find(|s| s.content.contains("resets"))
            .unwrap();
        assert_eq!(reset.style, theme.dim, "it reads with the spend");
        let alarmed = listing(
            vec![StatusField::Spend, StatusField::Status],
            Thresholds {
                spend: Some(1),
                ..Thresholds::default()
            },
        );
        let line = status_line_at(&app, None, &alarmed, &theme, 120, now);
        let style_of = |needle: &str| {
            line.spans
                .iter()
                .find(|s| s.content.contains(needle))
                .unwrap()
                .style
        };
        let alarm = theme.warn.add_modifier(Modifier::BOLD);
        assert_eq!(style_of("spend"), alarm, "past its threshold");
        assert_eq!(style_of("resets"), alarm, "the reset text alarms with it");
    }

    #[test]
    fn the_status_survives_narrow_widths() {
        let app = busy_footer_app();
        let theme = Theme::terminal(true);
        for (width, want) in [
            (38, "/model: Big · /plan-mode: on · running"),
            (24, "/plan-mode: on · running"),
            (7, "running"),
            (4, "runn"),
        ] {
            assert_eq!(
                text(&footer_line(&app, None, &theme, width)),
                want,
                "{width}"
            );
        }
    }

    #[test]
    fn an_unavailable_model_gives_way_after_the_fields_that_warn_of_nothing() {
        let mut app = App::new(BusyBehavior::Queue, true);
        let old = uuid::Uuid::new_v4();
        app.update(Msg::ModelsLoaded(vec![serde_json::from_value(json!({"id": old, "display_name": "Old", "enabled": false, "reasoning_efforts": []})).unwrap()]));
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "last_model_config_id": old, "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}, "plan_mode": "plan"})).unwrap();
        app.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: vec![],
        });
        let theme = Theme::terminal(true);
        let status = connection_status(&app, theme.icons).text();
        assert_eq!(
            text(&footer_line(&app, None, &theme, 200)),
            format!("/model: Old (unavailable) · /plan-mode: on · {status}")
        );
        let want = format!("/model: Old (unavailable) · {status}");
        let width = UnicodeWidthStr::width(want.as_str()) as u16;
        assert_eq!(
            text(&footer_line(&app, None, &theme, width)),
            want,
            "the warning outlasts plan mode"
        );
    }

    /// `busy_footer_app` with every field showing: a workspace, an effort, two queued
    /// messages, the chat's cost, the spend, and the quota.
    fn every_field_app() -> App {
        let mut app = workspace_footer_app();
        app.update(Msg::ModelsLoaded(vec![serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "display_name": "Big", "is_default": true, "enabled": true, "reasoning_efforts": ["low", "high"]})).unwrap()]));
        app.update(stream(json!({"type": "queue_update", "queued_messages": [{"id": 1, "content": []}, {"id": 2, "content": []}]})));
        app.chat_cost = Some(CostState::Loaded(
            serde_json::from_value(json!({"total_cost_micros": 420000})).unwrap(),
        ));
        with_spend_ending(
            &mut app,
            1_200_000,
            50_000_000,
            Some("2026-11-01T00:00:00Z"),
        );
        let generation = app.update(Msg::RefreshLimits).iter().find_map(|e| match e {
            Effect::FetchQuota { generation, .. } => Some(*generation),
            _ => None,
        });
        app.update(Msg::QuotaLoaded {
            org: app.org_id.unwrap(),
            quota: serde_json::from_value(json!({"credits_consumed": 3, "budget": 10})).unwrap(),
            generation: generation.expect("a quota fetch"),
        });
        app.org_mcp = Fetched::Loaded(vec![]);
        app
    }

    #[test]
    fn narrowing_the_footer_drops_every_field_in_the_drop_order() {
        let app = every_field_app();
        let theme = Theme::terminal(true);
        let cfg = listing(StatusField::ALL.to_vec(), Thresholds::default());
        let marks = [
            (StatusField::Workspace, "/workspace: dev"),
            (StatusField::Organization, "/organization: "),
            (StatusField::Queue, "queue 2"),
            (StatusField::Mcp, "/mcp: 0 on"),
            (StatusField::Cost, "cost $0.42"),
            (StatusField::Quota, "quota 3/10"),
            (StatusField::Effort, "/effort: high"),
            (StatusField::Spend, "spend $1.20/$50.00"),
            (StatusField::Context, "12.0k/200.0k"),
            (StatusField::Model, "/model: Big"),
            (StatusField::PlanMode, "/plan-mode: on"),
        ];
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-06T00:00:00Z")
            .unwrap()
            .timestamp();
        let at = |width: u16| text(&status_line_at(&app, None, &cfg, &theme, width, now));
        let full = at(400);
        for (name, mark) in marks {
            assert!(full.contains(mark), "{name:?} shows at full width: {full}");
        }
        assert!(full.contains("(resets in 26d)"), "{full}");
        let mut kept: Vec<StatusField> = marks.iter().map(|(name, _)| *name).collect();
        let mut dropped = Vec::new();
        for width in (0..=cells_width(&full) as u16).rev() {
            let t = at(width);
            if width >= 7 {
                assert!(t.contains("running"), "{width}: the status stays: {t}");
            }
            if marks.iter().any(|(_, mark)| !t.contains(mark)) {
                assert!(
                    !t.contains("(resets"),
                    "{width}: the reset text goes before any field: {t}"
                );
            }
            kept.retain(|name| {
                let mark = marks.iter().find(|(n, _)| n == name).unwrap().1;
                let shows = t.contains(mark);
                if !shows {
                    dropped.push(*name);
                }
                shows
            });
        }
        assert_eq!(dropped, DROP_ORDER.to_vec());
    }

    #[test]
    fn spend_shows_by_default_once_it_loads() {
        let mut app = busy_footer_app();
        assert!(
            !status_text(&app).contains("spend"),
            "nothing before it loads"
        );
        with_spend(&mut app, 1_200_000, 50_000_000);
        assert_eq!(
            status_text(&app),
            "/model: Big · 12.0k/200.0k (6%) · /organization: Engineering Platform · /plan-mode: on · spend $1.20/$50.00 · running"
        );
    }

    #[test]
    fn the_listed_fields_show_in_their_order_with_cost_quota_and_queue_on_request() {
        let mut app = busy_footer_app();
        app.update(stream(json!({"type": "queue_update", "queued_messages": [{"id": 1, "content": []}, {"id": 2, "content": []}]})));
        app.chat_cost = Some(CostState::Loaded(
            serde_json::from_value(json!({"total_cost_micros": 420000})).unwrap(),
        ));
        app.update(Msg::RefreshLimits);
        app.update(Msg::QuotaLoaded {
            org: app.org_id.unwrap(),
            quota: serde_json::from_value(json!({"credits_consumed": 3, "budget": 10})).unwrap(),
            generation: 1,
        });
        let cfg = listing(
            vec![
                StatusField::Status,
                StatusField::Queue,
                StatusField::Cost,
                StatusField::Quota,
                StatusField::Model,
            ],
            Thresholds::default(),
        );
        let theme = Theme::terminal(true);
        assert_eq!(
            text(&status_line(&app, None, &cfg, &theme, 120)),
            "running · queue 2 · cost $0.42 · quota 3/10 · /model: Big"
        );
        app.update(Msg::QuotaLoaded {
            org: app.org_id.unwrap(),
            quota: serde_json::from_value(json!({"credits_consumed": 3, "budget": -1})).unwrap(),
            generation: 1,
        });
        assert_eq!(
            text(&status_line(&app, None, &cfg, &theme, 120)),
            "running · queue 2 · cost $0.42 · /model: Big",
            "no quota applies, so the field hides"
        );
    }

    #[test]
    fn a_hidden_field_past_its_threshold_shows_highlighted_and_gives_way_last() {
        let mut app = busy_footer_app();
        let theme = Theme::terminal(true);
        let cfg = listing(
            vec![StatusField::Model, StatusField::Status],
            Thresholds {
                spend: Some(80),
                ..Thresholds::default()
            },
        );
        with_spend(&mut app, 1_000_000, 10_000_000);
        assert_eq!(
            text(&status_line(&app, None, &cfg, &theme, 80)),
            "/model: Big · running",
            "below its threshold, a hidden field stays hidden"
        );
        with_spend(&mut app, 9_000_000, 10_000_000);
        let line = status_line(&app, None, &cfg, &theme, 80);
        assert_eq!(text(&line), "/model: Big · running · spend $9.00/$10.00");
        let spend = line
            .spans
            .iter()
            .find(|s| s.content.starts_with("spend"))
            .unwrap();
        assert_eq!(spend.style, theme.warn.add_modifier(Modifier::BOLD));
        assert_eq!(
            text(&status_line(&app, None, &cfg, &theme, 28)),
            "running · spend $9.00/$10.00",
            "the model gives way before the warning"
        );
        for width in 0..=60u16 {
            let t = text(&status_line(&app, None, &cfg, &theme, width));
            assert!(cells_width(&t) <= width as usize, "{width}: {t:?}");
        }
    }

    #[test]
    fn context_past_its_threshold_is_highlighted_in_place() {
        let app = busy_footer_app();
        let theme = Theme::terminal(true);
        let mut cfg = StatuslineConfig::default();
        cfg.thresholds.context = Some(5);
        let line = status_line(&app, None, &cfg, &theme, 120);
        let context = line
            .spans
            .iter()
            .find(|s| s.content == "12.0k/200.0k (6%)")
            .unwrap();
        assert_eq!(context.style, theme.warn.add_modifier(Modifier::BOLD));
        cfg.thresholds.context = Some(7);
        let line = status_line(&app, None, &cfg, &theme, 120);
        let context = line
            .spans
            .iter()
            .find(|s| s.content == "12.0k/200.0k (6%)")
            .unwrap();
        assert_eq!(context.style, theme.dim, "6% is short of 7%");
    }

    #[test]
    fn nerd_icons_mark_the_connection_and_error_notices_and_the_labels_stay() {
        let nerd = Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal(true)
        };
        let text_theme = Theme::terminal(true);
        let mut app = App::new(BusyBehavior::Queue, true);
        app.connection = Connection::Connecting;
        assert!(text(&footer_line(&app, None, &nerd, 80)).contains("\u{eb2d} connecting"));
        let plain = text(&footer_line(&app, None, &text_theme, 80));
        assert!(plain.contains("connecting"), "{plain}");
        assert!(!plain.contains('\u{eb2d}'), "{plain}");
        app.connection = Connection::Reconnecting { attempt: 2 };
        assert!(
            text(&footer_line(&app, None, &nerd, 80)).contains("\u{ead0} reconnecting (attempt 2)")
        );
        app.plan_mode = true;
        assert!(
            text(&footer_line(&app, None, &nerd, 80)).contains("/plan-mode: on"),
            "the fields keep their /command: labels"
        );
        let error = Notice::Error("Could not send message: HTTP 409".into());
        let line = footer_line(&app, Some(&error), &nerd, 80);
        assert_eq!(text(&line), "\u{ea87} Could not send message: HTTP 409");
        assert_eq!(line.spans[0].style, nerd.error);
        assert_eq!(
            text(&footer_line(&app, Some(&error), &text_theme, 80)),
            "Could not send message: HTTP 409"
        );
        let narrow = footer_line(&app, Some(&error), &nerd, 10);
        assert_eq!(text(&narrow), "\u{ea87} Could no");
        assert!(cells_width(&text(&narrow)) <= 10);
        let info = Notice::Info("Settings applied.".into());
        assert_eq!(
            text(&footer_line(&app, Some(&info), &nerd, 80)),
            "Settings applied.",
            "an info notice takes no icon"
        );
    }

    #[test]
    fn the_connection_glyph_is_plain_under_no_color_and_never_cut_from_its_space() {
        use crate::theme::Colors;
        let nerd = Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal_with(true, Colors::None)
        };
        let mut app = App::new(BusyBehavior::Queue, true);
        app.connection = Connection::Connecting;
        let line = footer_line(&app, None, &nerd, 80);
        let glyph = line
            .spans
            .iter()
            .find(|s| s.content.contains('\u{eb2d}'))
            .unwrap_or_else(|| panic!("no glyph in {line:?}"));
        assert_eq!(glyph.content, "\u{eb2d} ", "the glyph has its own span");
        assert_eq!(
            glyph.style,
            ratatui::style::Style::new(),
            "plain under NO_COLOR"
        );
        let colored = Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal(true)
        };
        let line = footer_line(&app, None, &colored, 80);
        assert_eq!(text(&line), "\u{eb2d} connecting");
        assert_eq!(line.spans[0].style, colored.icon(colored.dim));
        for width in 0..4 {
            let shown = text(&footer_line(&app, None, &colored, width));
            assert!(
                !shown.contains('\u{eb2d}') || shown.starts_with("\u{eb2d} "),
                "the glyph keeps its slot's space at width {width}: {shown:?}"
            );
            assert!(cells_width(&shown) <= usize::from(width), "{shown:?}");
        }
    }

    #[test]
    fn the_mcp_field_counts_the_servers_the_next_message_uses() {
        let mut app = busy_footer_app();
        let theme = Theme::terminal(true);
        let cfg = listing(
            vec![StatusField::Mcp, StatusField::Status],
            Thresholds::default(),
        );
        assert_eq!(
            text(&status_line(&app, None, &cfg, &theme, 120)),
            "running",
            "hidden until the organization's list loads"
        );
        let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        app.org_mcp = Fetched::Loaded(
            serde_json::from_value(json!([
                {"id": a, "display_name": "A", "availability": "default_off", "enabled": true, "tool_allow_list": [], "tool_deny_list": []},
                {"id": b, "display_name": "B", "availability": "default_off", "enabled": true, "tool_allow_list": [], "tool_deny_list": []}
            ]))
            .unwrap(),
        );
        assert_eq!(
            text(&status_line(&app, None, &cfg, &theme, 120)),
            "/mcp: 0 on · running"
        );
        app.chat.as_mut().unwrap().mcp_server_ids = vec![a, b];
        assert_eq!(
            text(&status_line(&app, None, &cfg, &theme, 120)),
            "/mcp: 2 on · running"
        );
        app.mcp_next = Some(vec![a]);
        assert_eq!(
            text(&status_line(&app, None, &cfg, &theme, 120)),
            "/mcp: 1 on · running",
            "a pending /mcp change counts"
        );
        let line = status_line(&app, None, &cfg, &theme, 120);
        let command = line.spans.iter().find(|s| s.content == "/mcp").unwrap();
        assert_eq!(command.style, theme.brand, "it reads as a command field");
        assert!(
            !text(&footer_line(&app, None, &theme, 200)).contains("/mcp"),
            "the default footer leaves it out"
        );
    }
}
