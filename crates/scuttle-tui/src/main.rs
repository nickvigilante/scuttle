mod app;
mod clipboard;
mod composer;
mod footer;
mod highlight;
mod markdown;
mod picker;
mod runtime;
mod terminal;
mod theme;
mod transcript_view;
mod wrap;

use std::ops::ControlFlow;
use std::process::ExitCode;

use futures::StreamExt;
use scuttle_core::app::{Effect, Msg, Notice};
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

/// Waits until `deadline`, or forever when there is none.
async fn sleep_until(deadline: Option<std::time::Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(tokio::time::Instant::from_std(d)).await,
        None => std::future::pending().await,
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
        url: session.url.to_string(),
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

    if let Ok(version) = client.server_version().await
        && let Some(w) = scuttle_core::skew::skew_warning(&version, coder_sdk::GENERATED_FROM)
    {
        tui.core.notices.push(Notice::Info(w));
    }
    let first = match runtime.organization().await {
        Ok(org) => tui.update(Msg::Started {
            org_id: org,
            open_chat,
        }),
        Err(e) => {
            tui.core.notices.push(Notice::Error(format!(
                "Could not load your organization: {e}"
            )));
            vec![]
        }
    };

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
        let deadline = tui.notice_deadline();
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
            () = sleep_until(deadline) => {}
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
}
