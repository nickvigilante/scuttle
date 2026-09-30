mod activity;
mod app;
mod clipboard;
mod composer;
mod footer;
mod help;
mod highlight;
mod links;
mod markdown;
mod picker;
mod runtime;
mod selection;
mod terminal;
mod theme;
mod transcript_view;
#[cfg(test)]
mod turn_tests;
mod wrap;

use std::ops::ControlFlow;
use std::process::ExitCode;

use futures::StreamExt;
use scuttle_core::app::{Effect, Msg, Notice, OrgRef};
use scuttle_core::config;

fn detect_dark() -> bool {
    if std::env::var_os("SCUTTLE_NO_TERMINAL_QUERY").is_some() {
        return true;
    }
    let mut options = terminal_colorsaurus::QueryOptions::default();
    options.timeout = std::time::Duration::from_millis(150);
    !matches!(
        terminal_colorsaurus::theme_mode(options),
        Ok(terminal_colorsaurus::ThemeMode::Light)
    )
}

/// The deployment URL as `scheme://host[:port]`, without any userinfo, path, or query.
fn display_origin(url: &url::Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    }
}

/// Waits until `deadline`, or forever when there is none.
async fn sleep_until(deadline: Option<std::time::Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(tokio::time::Instant::from_std(d)).await,
        None => std::future::pending().await,
    }
}

/// The earlier of two optional deadlines.
fn earliest(
    a: Option<std::time::Instant>,
    b: Option<std::time::Instant>,
) -> Option<std::time::Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Feeds `first`, then every message already queued behind it, to the UI, so a burst of stream
/// deltas costs one draw instead of one per delta.
fn update_queued(
    tui: &mut app::Tui,
    first: Msg,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Msg>,
) -> Vec<Effect> {
    let mut effects = tui.update(first);
    while let Ok(msg) = rx.try_recv() {
        effects.extend(tui.update(msg));
    }
    effects
}

/// Unwraps one item from the terminal event stream, or says why the loop must stop: `None`
/// when the input ended, or the error text when reading it failed.
fn next_input(
    item: Option<std::io::Result<crossterm::event::Event>>,
) -> ControlFlow<Option<String>, crossterm::event::Event> {
    match item {
        Some(Ok(event)) => ControlFlow::Continue(event),
        Some(Err(e)) => ControlFlow::Break(Some(e.to_string())),
        None => ControlFlow::Break(None),
    }
}

/// Starts the app from the startup organization lookup, picking the organization new chats
/// use.
fn startup(
    tui: &mut app::Tui,
    organizations: Result<Vec<OrgRef>, String>,
    saved: Option<uuid::Uuid>,
    open_chat: Option<uuid::Uuid>,
) -> Vec<Effect> {
    let organizations = match organizations {
        Ok(organizations) => organizations,
        Err(message) => return tui.update(Msg::OrganizationsFailed { message, open_chat }),
    };
    match scuttle_core::app::pick_organization(saved, &organizations) {
        Some(org) => {
            let mut effects = tui.update(Msg::OrganizationsLoaded(organizations));
            effects.extend(tui.update(Msg::Started {
                org_id: org,
                open_chat,
            }));
            effects
        }
        None => tui.update(Msg::OrganizationsFailed {
            message: "you are not a member of any organization".into(),
            open_chat,
        }),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let open_chat = match std::env::args().nth(1).map(|a| a.parse::<uuid::Uuid>()) {
        None => None,
        Some(Ok(id)) => Some(id),
        Some(Err(_)) => {
            eprintln!("usage: scuttle [chat-id]");
            return ExitCode::from(2);
        }
    };
    let session = match coder_sdk::discover_session() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("scuttle: {e}");
            return ExitCode::FAILURE;
        }
    };
    let client = match coder_sdk::Client::new(&session) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("scuttle: {e}");
            return ExitCode::FAILURE;
        }
    };
    let config_path = config::config_path(&|k| std::env::var(k).ok().filter(|v| !v.is_empty()));
    let local = match config_path.as_deref().map(config::load).transpose() {
        Ok(c) => c.unwrap_or_default(),
        Err(e) => {
            eprintln!("scuttle: {e}");
            return ExitCode::FAILURE;
        }
    };
    let art = local
        .welcome
        .art_file
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| t.lines().map(str::to_owned).collect())
        .unwrap_or_default();
    let welcome = transcript_view::Welcome {
        url: display_origin(&session.url),
        user: String::new(),
        art,
        show: local.welcome.show,
    };
    highlight::warm();
    let dark = detect_dark();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut runtime = runtime::Runtime::new(client.clone(), tx);
    let mut tui = app::Tui::new(
        local.clone(),
        config_path,
        theme::Theme::terminal(dark),
        welcome,
    );

    let rejected = || {
        eprintln!(
            "scuttle: the session token was rejected. Run `coder login {}`.",
            display_origin(&session.url)
        );
        ExitCode::FAILURE
    };
    // Both answers are needed before the first frame, so wait for them together.
    let (version, organizations) = tokio::join!(client.server_version(), runtime.organizations());
    match version {
        Ok(version) => {
            if let Some(w) = scuttle_core::skew::skew_warning(&version, coder_sdk::GENERATED_FROM) {
                tui.core.notices.push(Notice::Info(w));
            }
        }
        Err(coder_sdk::Error::Unauthorized) => return rejected(),
        Err(_) => {}
    }
    let organizations = match organizations {
        Ok(organizations) => Ok(organizations),
        Err(coder_sdk::Error::Unauthorized) => return rejected(),
        Err(e) => Err(e.to_string()),
    };
    let first = startup(&mut tui, organizations, local.organization, open_chat);

    let mut term = match terminal::enter(local.mouse) {
        Ok(t) => t,
        Err(e) => {
            let _ = terminal::leave();
            eprintln!("scuttle: could not set up the terminal: {e}");
            return ExitCode::FAILURE;
        }
    };
    tui.set_keyboard_enhanced(terminal::keyboard_enhanced());
    let mut events = crossterm::event::EventStream::new();
    let mut pending = first;
    let mut failure = None;
    let code = 'main: loop {
        for effect in std::mem::take(&mut pending) {
            if effect == Effect::Quit {
                break 'main ExitCode::SUCCESS;
            }
            if !tui.apply_ui_effect(&effect) {
                runtime.run(effect);
            }
        }
        if tui.take_full_redraw() && term.clear().is_err() {
            break ExitCode::FAILURE;
        }
        if term.draw(|f| tui.draw(f)).is_err() {
            break ExitCode::FAILURE;
        }
        let deadline = earliest(
            tui.notice_deadline(),
            tui.animation_deadline(std::time::Instant::now()),
        );
        // Each iteration handles at most one terminal event before the next draw: the composer
        // learns its wrap width only when it renders, so a second key must not arrive before
        // that. Channel messages may be drained in a batch, and a timer wakeup handles no input.
        tokio::select! {
            item = events.next() => match next_input(item) {
                ControlFlow::Continue(ev) => pending = tui.handle(ev),
                ControlFlow::Break(None) => break ExitCode::SUCCESS,
                ControlFlow::Break(Some(e)) => {
                    failure = Some(format!("could not read terminal input: {e}"));
                    break ExitCode::FAILURE;
                }
            },
            Some(msg) = rx.recv() => pending = update_queued(&mut tui, msg, &mut rx),
            () = sleep_until(deadline) => tui.tick(),
            else => break ExitCode::SUCCESS,
        }
    };
    let _ = terminal::leave();
    match failure.or_else(|| tui.take_fatal()) {
        Some(e) => {
            eprintln!("scuttle: {e}");
            ExitCode::FAILURE
        }
        None => code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tui() -> app::Tui {
        app::Tui::new(
            config::LocalConfig::default(),
            None,
            theme::Theme::terminal(true),
            transcript_view::Welcome {
                url: "https://x".into(),
                user: String::new(),
                art: vec![],
                show: true,
            },
        )
    }

    #[test]
    fn the_displayed_url_drops_userinfo_path_and_query() {
        let origin = |s: &str| display_origin(&s.parse().unwrap());
        assert_eq!(
            origin("https://user:hunter2@coder.example.com/some/path?q=1#f"),
            "https://coder.example.com"
        );
        assert_eq!(origin("http://127.0.0.1:3000/"), "http://127.0.0.1:3000");
        assert_eq!(
            origin("https://coder.example.com:443"),
            "https://coder.example.com"
        );
        assert_eq!(origin("https://[::1]:8443/x"), "https://[::1]:8443");
    }

    #[test]
    fn ended_or_failed_input_stops_the_loop() {
        let event = crossterm::event::Event::FocusGained;
        assert_eq!(
            next_input(Some(Ok(event.clone()))),
            ControlFlow::Continue(event)
        );
        assert_eq!(next_input(None), ControlFlow::Break(None));
        assert_eq!(
            next_input(Some(Err(std::io::Error::other("tty closed")))),
            ControlFlow::Break(Some("tty closed".into()))
        );
    }

    #[test]
    fn every_queued_message_is_applied_before_the_next_draw() {
        let mut t = tui();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for n in 0..3 {
            tx.send(Msg::ModelsFailed {
                message: format!("failure {n}"),
            })
            .unwrap();
        }
        let first = rx.try_recv().unwrap();
        update_queued(&mut t, first, &mut rx);
        assert_eq!(t.core.notices.len(), 3);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn earliest_picks_the_sooner_deadline() {
        let now = std::time::Instant::now();
        let later = now + std::time::Duration::from_secs(1);
        assert_eq!(earliest(Some(later), Some(now)), Some(now));
        assert_eq!(earliest(None, Some(later)), Some(later));
        assert_eq!(earliest(Some(now), None), Some(now));
        assert_eq!(earliest(None, None), None);
    }

    fn org(name: &str) -> OrgRef {
        OrgRef {
            id: uuid::Uuid::new_v4(),
            name: name.into(),
            display_name: String::new(),
            is_default: true,
            can_create_chats: true,
        }
    }

    #[test]
    fn a_failed_organization_load_with_a_chat_id_still_loads_the_chat() {
        let mut t = tui();
        let id = uuid::Uuid::new_v4();
        let effects = startup(&mut t, Err("HTTP 500".into()), None, Some(id));
        assert!(effects.contains(&Effect::LoadChat(id)), "{effects:?}");
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Error(
                "Could not load your organization: HTTP 500".into()
            ))
        );
    }

    #[test]
    fn no_organization_with_a_chat_id_still_loads_the_chat() {
        let mut t = tui();
        let id = uuid::Uuid::new_v4();
        let effects = startup(&mut t, Ok(vec![]), None, Some(id));
        assert!(effects.contains(&Effect::LoadChat(id)), "{effects:?}");
        assert!(
            matches!(t.core.notices.last(), Some(Notice::Error(m)) if m.contains("not a member of any organization"))
        );
    }

    #[test]
    fn a_failed_organization_load_on_a_blank_chat_only_explains() {
        let mut t = tui();
        assert!(startup(&mut t, Err("HTTP 500".into()), None, None).is_empty());
        assert!(
            matches!(t.core.notices.last(), Some(Notice::Error(m)) if m.starts_with("Could not load your organization"))
        );
    }

    #[test]
    fn a_loaded_organization_starts_the_app_in_it() {
        let mut t = tui();
        let coder = org("coder");
        let id = uuid::Uuid::new_v4();
        let effects = startup(&mut t, Ok(vec![coder.clone()]), None, Some(id));
        assert!(
            effects.contains(&Effect::FetchModels(coder.id)),
            "{effects:?}"
        );
        assert!(effects.contains(&Effect::LoadChat(id)));
        assert_eq!(t.core.org_id, Some(coder.id));
    }
}
