mod activity;
mod app;
mod art;
mod clipboard;
mod composer;
mod footer;
mod help;
mod highlight;
mod icons;
mod links;
mod markdown;
mod notify;
mod overlay;
mod paths;
mod picker;
mod runtime;
mod save;
mod selection;
mod subagent;
mod table;
mod terminal;
mod theme;
mod toast;
mod transcript_view;
#[cfg(test)]
mod turn_tests;
mod wrap;

use std::ops::ControlFlow;
use std::path::PathBuf;
use std::process::ExitCode;

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

/// A seed for the random spinner picks, from the clock. It is never zero, because a zero seed
/// stays zero and would always pick braille.
fn spinner_seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    nanos | 1
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

/// The most channel messages applied before a draw, so a burst of watch or stream traffic
/// cannot keep the screen from updating.
const MAX_DRAIN: usize = 256;

/// Feeds `first`, then the messages already queued behind it, to the UI, up to `MAX_DRAIN` in
/// all, so a burst of stream deltas costs one draw instead of one per delta.
fn update_queued(
    tui: &mut app::Tui,
    first: Msg,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Msg>,
) -> Vec<Effect> {
    let mut effects = tui.update(first);
    for _ in 1..MAX_DRAIN {
        let Ok(msg) = rx.try_recv() else {
            break;
        };
        effects.extend(tui.update(msg));
    }
    effects
}

/// The effects one main-loop iteration runs: `pending`, from the key, message, or wakeup
/// that started it, then the usage refreshes due at `now`. They are checked on every
/// iteration, not only at the timer wakeup, since the biased select would let steady channel
/// traffic hold the timer off. Handoffs run to the end inside the loop, so a refresh never
/// starts during one; one that came due meanwhile runs at the next iteration.
fn iteration_effects(
    tui: &mut app::Tui,
    mut pending: Vec<Effect>,
    now: std::time::Instant,
) -> Vec<Effect> {
    pending.extend(tui.poll_usage(now));
    pending
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

/// One item from the terminal event stream.
type InputItem = std::io::Result<crossterm::event::Event>;

/// Whether `item` only moves the mouse pointer.
fn is_move(item: &InputItem) -> bool {
    use crossterm::event::{Event, MouseEventKind};
    matches!(item, Ok(Event::Mouse(m)) if m.kind == MouseEventKind::Moved)
}

/// Skips a pointer move to the last move already queued behind it, so a burst of motion costs
/// one draw. The first queued item that is not a move comes back as the second value, to be
/// handled after the next draw, since the loop handles one terminal event per draw.
fn latest_move(
    first: InputItem,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<InputItem>,
) -> (InputItem, Option<InputItem>) {
    if !is_move(&first) {
        return (first, None);
    }
    let mut latest = first;
    while let Ok(item) = events.try_recv() {
        if !is_move(&item) {
            return (latest, Some(item));
        }
        latest = item;
    }
    (latest, None)
}

/// The terminal events the main loop handles one per draw, with a burst of pointer moves taken
/// as its last move.
#[derive(Default)]
struct InputQueue {
    /// The item that ended the last burst of moves, handled after the draw that follows it.
    held: Option<InputItem>,
}

impl InputQueue {
    /// The item held back behind a burst of moves, which the loop handles before it waits for
    /// anything else.
    fn take_held(&mut self) -> Option<InputItem> {
        self.held.take()
    }

    /// `first`, or the last move of the burst it starts, holding back the item that ends it.
    fn next(
        &mut self,
        first: InputItem,
        events: &mut tokio::sync::mpsc::UnboundedReceiver<InputItem>,
    ) -> InputItem {
        let (latest, held) = latest_move(first, events);
        self.held = held;
        latest
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

/// How long exiting waits for work still running in the background, such as an attachment
/// read stuck on a stalled network mount, before leaving it behind.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

/// Runs `app` on `runtime`, then shuts the runtime down, waiting at most `grace` for its
/// tasks. Dropping a runtime instead waits for every `spawn_blocking` task, however long it
/// takes, so quitting could hang on a read that never returns.
fn run_bounded(
    runtime: tokio::runtime::Runtime,
    app: impl std::future::Future<Output = ExitCode>,
    grace: std::time::Duration,
) -> ExitCode {
    let code = runtime.block_on(app);
    runtime.shutdown_timeout(grace);
    code
}

/// Whether the welcome may suggest a Nerd Font, and where to remember that it did: the tip
/// shows until the state file at `path` records it. Without a state path nothing can
/// remember it, so it shows at each start.
fn nerd_tip(path: Option<std::path::PathBuf>) -> (bool, Option<std::path::PathBuf>) {
    match path {
        Some(path) if scuttle_core::state::load(&path).nerd_font_tip_shown => (false, None),
        path => (true, path),
    }
}

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("scuttle: could not start: {e}");
            return ExitCode::FAILURE;
        }
    };
    run_bounded(runtime, run(), SHUTDOWN_GRACE)
}

/// scuttle, from reading the arguments to restoring the terminal; `main` runs it on a runtime
/// whose shutdown is bounded.
async fn run() -> ExitCode {
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
    // The art file's lines, else the bundled Coder wordmark.
    let art = local
        .welcome
        .art_file
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| t.lines().map(str::to_owned).collect())
        .unwrap_or_else(art::wordmark_lines);
    let (tip, tip_state) = nerd_tip(scuttle_core::state::state_path(&|k| {
        std::env::var(k).ok().filter(|v| !v.is_empty())
    }));
    let welcome = transcript_view::Welcome {
        url: display_origin(&session.url),
        user: String::new(),
        art,
        art_accent: local.welcome.art_color == config::ArtColor::Accent,
        show: local.welcome.show,
        tip,
    };
    highlight::warm();
    let dark = detect_dark();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut runtime = runtime::Runtime::new(client, session.token.clone(), tx);
    let mut tui = app::Tui::new(
        local.clone(),
        config_path,
        theme::Theme::terminal_with(dark, theme::Colors::detect(|k| std::env::var(k).ok())),
        welcome,
        spinner_seed(),
    );
    // The icons follow `NERD_FONT` while config.toml sets no `icons`; it is read once, here.
    tui.set_icon_env(
        std::env::var("NERD_FONT")
            .ok()
            .as_deref()
            .and_then(config::IconSet::from_env),
    );
    // Read once, here: where saves go, and whether a saved file lands on another machine.
    tui.set_home(std::env::var_os("HOME").map(PathBuf::from));
    tui.core.over_ssh = runtime::over_ssh(|k| std::env::var_os(k).is_some());
    if let Some(path) = tip_state {
        tui.remember_nerd_font_tip_in(path);
    }

    let rejected = || {
        eprintln!(
            "scuttle: the session token was rejected. Run `coder login {}`.",
            display_origin(&session.url)
        );
        ExitCode::FAILURE
    };
    // Both answers are needed before the first frame, so wait for them together.
    let (version, organizations) = tokio::join!(runtime.server_version(), runtime.organizations());
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

    // Installed before the terminal changes, so a signal from here on leaves through
    // `terminal::finish`, which restores it.
    let mut shutdown = match terminal::Shutdown::install() {
        Ok(shutdown) => shutdown,
        Err(e) => {
            eprintln!("scuttle: could not watch for signals: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut term = match terminal::enter(local.mouse) {
        Ok(t) => t,
        Err(e) => {
            let _ = terminal::finish();
            eprintln!("scuttle: could not set up the terminal: {e}");
            return ExitCode::FAILURE;
        }
    };
    tui.set_keyboard_enhanced(terminal::keyboard_enhanced());
    let mut input = terminal::Input::start();
    let mut notifier = notify::Notifier::new();
    let mut pending = first;
    pending.extend(tui.update(Msg::SessionStarted));
    let mut failure = None;
    let mut queue = InputQueue::default();
    let code = 'main: loop {
        // A program that takes over the terminal must get every key typed while it runs, so
        // the input thread is paused around each handoff. Its Ctrl+C must not end scuttle,
        // and the `/git` socket closes so full diffs do not pile up while the loop waits.
        if tui.take_editor_request() {
            let _signals = terminal::handoff_signals();
            runtime.pause_git_watch();
            input.pause();
            pending.extend(tui.open_editor());
            input.resume();
            runtime.resume_git_watch();
        }
        let effects = iteration_effects(
            &mut tui,
            std::mem::take(&mut pending),
            std::time::Instant::now(),
        );
        for effect in effects {
            if !effect.runs_in_main() {
                if !tui.apply_ui_effect(&effect) {
                    runtime.run(effect);
                }
                continue;
            }
            match effect {
                Effect::Page(text) => {
                    let signals = terminal::handoff_signals();
                    runtime.pause_git_watch();
                    input.pause();
                    let mouse = tui.core.mouse;
                    let (handed, resumed) = app::handoff_round_trip(
                        terminal::leave,
                        || runtime::page(&text),
                        || terminal::resume(mouse),
                    );
                    input.resume();
                    runtime.resume_git_watch();
                    drop(signals);
                    tui.set_keyboard_enhanced(terminal::keyboard_enhanced());
                    tui.after_handoff(&text, handed, resumed);
                    if tui.must_quit() {
                        break 'main ExitCode::FAILURE;
                    }
                }
                Effect::EditSettings => {
                    let signals = terminal::handoff_signals();
                    runtime.pause_git_watch();
                    input.pause();
                    // Its only effect is the `Quit` of a failed restore, which `must_quit`
                    // answers here, since `pending` is replaced before it would run.
                    let _ = tui.edit_settings();
                    input.resume();
                    runtime.resume_git_watch();
                    drop(signals);
                    if tui.must_quit() {
                        break 'main ExitCode::FAILURE;
                    }
                }
                Effect::Quit => break 'main ExitCode::SUCCESS,
                other => unreachable!("{other:?} does not run in the main loop"),
            }
        }
        if tui.take_full_redraw() && term.clear().is_err() {
            break ExitCode::FAILURE;
        }
        // Written between draws, as the title is, so no sequence lands inside a frame.
        for alert in tui.take_desktop_alerts() {
            let _ = notifier.notify(tui.notifications(), &alert);
        }
        if let Some(title) = tui.take_title() {
            let _ = terminal::set_title(&title);
        }
        if term.draw(|f| tui.draw(f)).is_err() {
            break ExitCode::FAILURE;
        }
        let deadline = earliest(
            earliest(
                tui.notice_deadline(),
                tui.animation_deadline(std::time::Instant::now()),
            ),
            tui.usage_deadline(),
        );
        // Each iteration handles at most one terminal event before the next draw: the composer
        // learns its wrap width only when it renders, so a second key must not arrive before
        // that. Channel messages may be drained in a batch, and a timer wakeup handles no input.
        let item = match queue.take_held() {
            Some(item) => Some(item),
            // Biased with the signal first, so a hangup that also ends the input reports
            // 129 rather than the input's exit code.
            None => tokio::select! {
                biased;
                code = shutdown.recv() => break ExitCode::from(code),
                item = input.events.recv() => item,
                Some(msg) = rx.recv() => {
                    pending = update_queued(&mut tui, msg, &mut rx);
                    continue;
                }
                // The refresh this wakeup is for runs at the top of the loop.
                () = sleep_until(deadline) => {
                    tui.tick();
                    continue;
                }
                else => break ExitCode::SUCCESS,
            },
        };
        let item = item.map(|first| queue.next(first, &mut input.events));
        match next_input(item) {
            ControlFlow::Continue(ev) => pending = tui.handle(ev),
            ControlFlow::Break(None) => break ExitCode::SUCCESS,
            ControlFlow::Break(Some(e)) => {
                failure = Some(format!("could not read terminal input: {e}"));
                break ExitCode::FAILURE;
            }
        }
    };
    let _ = terminal::finish();
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
                art_accent: true,
                show: true,
                tip: true,
            },
            0,
        )
    }

    #[test]
    fn every_iteration_runs_the_refreshes_due_whatever_woke_it() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let now = std::time::Instant::now();
        // An iteration woken by a channel message, not the timer, still asks for the limits.
        let effects = iteration_effects(&mut t, vec![Effect::Quit], now);
        assert_eq!(effects.first(), Some(&Effect::Quit), "{effects:?}");
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::FetchSpend { .. })),
            "{effects:?}"
        );
        assert_eq!(
            iteration_effects(&mut t, vec![], now),
            vec![],
            "nothing again before the next deadline"
        );
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
    fn queued_messages_up_to_the_cap_are_applied_before_the_next_draw() {
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

    #[test]
    fn a_burst_of_pointer_moves_is_handled_as_its_last_move() {
        use crossterm::event::{
            Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind,
        };
        let moved = |column| {
            Ok(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column,
                row: 3,
                modifiers: KeyModifiers::NONE,
            }))
        };
        let key = || {
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('a'),
                KeyModifiers::NONE,
            )))
        };
        let column = |item: &InputItem| match item {
            Ok(Event::Mouse(m)) => m.column,
            other => panic!("not a move: {other:?}"),
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for item in [moved(2), moved(3), key(), moved(5)] {
            tx.send(item).unwrap();
        }
        let (latest, held) = latest_move(moved(1), &mut rx);
        assert_eq!(column(&latest), 3, "the moves before the key are skipped");
        assert!(
            matches!(held, Some(Ok(Event::Key(_)))),
            "the key waits for the next draw: {held:?}"
        );
        let (latest, held) = latest_move(rx.try_recv().unwrap(), &mut rx);
        assert_eq!(column(&latest), 5);
        assert!(held.is_none(), "the queue ran dry");
        let (first, held) = latest_move(key(), &mut rx);
        assert!(
            matches!(first, Ok(Event::Key(_))) && held.is_none(),
            "a key takes nothing from the queue"
        );
        tx.send(moved(9)).unwrap();
        let (_, _) = latest_move(key(), &mut rx);
        assert_eq!(
            column(&rx.try_recv().unwrap()),
            9,
            "a key leaves the moves queued"
        );
    }

    #[test]
    fn a_key_behind_a_burst_of_moves_comes_before_anything_queued_after_it() {
        use crossterm::event::{
            Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind,
        };
        let moved = |column| {
            Ok(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }))
        };
        let key = |c| {
            Ok(Event::Key(KeyEvent::new(
                KeyCode::Char(c),
                KeyModifiers::NONE,
            )))
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for item in [moved(2), key('a'), key('b'), moved(4), moved(6)] {
            tx.send(item).unwrap();
        }
        // Each step is what one loop iteration handles: the held item if there is one, else
        // the next one received.
        let mut queue = InputQueue::default();
        let mut handled = Vec::new();
        for _ in 0..4 {
            let item = match queue.take_held() {
                Some(item) => item,
                None => {
                    let first = rx.try_recv().unwrap_or_else(|_| moved(1));
                    queue.next(first, &mut rx)
                }
            };
            handled.push(match item {
                Ok(Event::Mouse(m)) => format!("move {}", m.column),
                Ok(Event::Key(k)) => format!("key {}", k.code),
                other => format!("{other:?}"),
            });
        }
        assert_eq!(handled, ["move 2", "key a", "key b", "move 6"]);
        assert!(rx.try_recv().is_err() && queue.take_held().is_none());
    }

    #[test]
    fn a_burst_is_drained_in_bounded_batches() {
        let mut t = tui();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for n in 0..MAX_DRAIN + 44 {
            tx.send(Msg::ModelsFailed {
                message: format!("failure {n}"),
            })
            .unwrap();
        }
        let first = rx.try_recv().unwrap();
        update_queued(&mut t, first, &mut rx);
        assert_eq!(t.core.notices.len(), MAX_DRAIN);
        let left = std::iter::from_fn(|| rx.try_recv().ok()).count();
        assert_eq!(left, 44);
    }

    #[test]
    fn exiting_leaves_a_stuck_blocking_read_behind() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        // Nothing is sent until the test ends, like a read on a stalled network mount.
        let (release, stuck) = std::sync::mpsc::channel::<()>();
        let (started_tx, started) = std::sync::mpsc::channel::<()>();
        let (done_tx, done) = std::sync::mpsc::channel::<ExitCode>();
        let began = std::time::Instant::now();
        // On its own thread, so an exit that waits for the read fails the test instead of
        // hanging it.
        std::thread::spawn(move || {
            let code = run_bounded(
                runtime,
                async move {
                    tokio::task::spawn_blocking(move || {
                        let _ = started_tx.send(());
                        let _ = stuck.recv();
                    });
                    started.recv().expect("the blocking read started");
                    ExitCode::SUCCESS
                },
                std::time::Duration::from_millis(100),
            );
            let _ = done_tx.send(code);
        });
        let code = done
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("exit waited for the stuck read");
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(
            began.elapsed() < std::time::Duration::from_secs(5),
            "exit waited {:?} for the stuck read",
            began.elapsed()
        );
        drop(release);
    }

    #[test]
    fn the_nerd_font_tip_is_offered_until_the_state_remembers_it() {
        let dir = std::env::temp_dir().join(format!("scuttle-tip-{}", uuid::Uuid::new_v4()));
        let path = dir.join("scuttle/state.toml");
        assert_eq!(nerd_tip(Some(path.clone())), (true, Some(path.clone())));
        scuttle_core::state::record_nerd_font_tip(&path).unwrap();
        assert_eq!(nerd_tip(Some(path.clone())), (false, None));
        assert_eq!(
            nerd_tip(None),
            (true, None),
            "without a state path the tip shows at each start"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
