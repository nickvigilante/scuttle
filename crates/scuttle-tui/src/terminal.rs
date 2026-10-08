//! Entering and leaving the full-screen terminal, restored on exit and on panic.

use std::io::{Write, stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Once};

use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::DefaultTerminal;
use ratatui::backend::CrosstermBackend;

static PANIC_HOOK: Once = Once::new();
/// Whether `resume` pushed keyboard enhancement flags that `leave` must pop.
static ENHANCED: AtomicBool = AtomicBool::new(false);

/// Whether `enter` saved the window title on the title stack, so `finish` pops it only once.
static TITLE_PUSHED: AtomicBool = AtomicBool::new(false);

/// Saves the window title on the terminal's title stack (XTWINOPS `CSI 22 ; 0 t`), so
/// `finish` can put it back. A terminal without a title stack ignores it.
pub const PUSH_TITLE: &str = "\x1b[22;0t";

/// Restores the window title `PUSH_TITLE` saved (XTWINOPS `CSI 23 ; 0 t`).
pub const POP_TITLE: &str = "\x1b[23;0t";

/// Writes `text` to the terminal as it is.
fn write_raw(text: &str) -> std::io::Result<()> {
    let mut out = stdout();
    out.write_all(text.as_bytes())?;
    out.flush()
}

/// The OSC 2 sequence that sets the window title to `title`. Control characters are dropped,
/// so a title can neither end the sequence early nor start another one.
pub fn title_sequence(title: &str) -> String {
    let clean: String = title.chars().filter(|c| !c.is_control()).collect();
    format!("\x1b]2;{clean}\x07")
}

/// Sets the window title.
pub fn set_title(title: &str) -> std::io::Result<()> {
    write_raw(&title_sequence(title))
}

/// Leaves the terminal for good: everything `leave` restores, then the window title `enter`
/// saved. Safe to call more than once.
pub fn finish() -> std::io::Result<()> {
    let left = leave();
    if TITLE_PUSHED.swap(false, Ordering::SeqCst) {
        let _ = write_raw(POP_TITLE);
    }
    left
}

/// Switches to the alternate screen in raw mode and returns the terminal to draw on, after
/// saving the window title for `finish` to restore.
pub fn enter(mouse: bool) -> std::io::Result<DefaultTerminal> {
    PANIC_HOOK.call_once(|| {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = finish();
            hook(info);
        }));
    });
    if write_raw(PUSH_TITLE).is_ok() {
        TITLE_PUSHED.store(true, Ordering::SeqCst);
    }
    resume(mouse)?;
    ratatui::Terminal::new(CrosstermBackend::new(stdout()))
}

/// Re-applies every mode `enter` sets, for example after an external editor ran.
pub fn resume(mouse: bool) -> std::io::Result<()> {
    enable_raw_mode()?;
    let mut out = stdout();
    enter_modes(&mut out)?;
    // Only push flags the terminal understands, so `leave` never pops a stack it did not push.
    let skip_query = std::env::var_os("SCUTTLE_NO_TERMINAL_QUERY").is_some();
    if enhancement_supported(skip_query, || {
        crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false)
    }) && execute!(
        out,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )
    .is_ok()
    {
        ENHANCED.store(true, Ordering::SeqCst);
    }
    set_mouse(mouse)
}

/// Switches `out` to the alternate screen with bracketed paste and focus reporting, which
/// tells scuttle whether its window is focused, so a desktop notification goes out only
/// while it is not.
fn enter_modes(out: &mut impl Write) -> std::io::Result<()> {
    execute!(
        out,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableFocusChange
    )
}

/// Turns off on `out` what `enter_modes` and `set_mouse` turned on, and shows the cursor.
fn leave_modes(out: &mut impl Write) -> std::io::Result<()> {
    execute!(
        out,
        DisableFocusChange,
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    )
}

/// Whether to push keyboard enhancement flags. `skip_query` (from `SCUTTLE_NO_TERMINAL_QUERY`)
/// treats the terminal as unsupported without sending the probe.
fn enhancement_supported(skip_query: bool, probe: impl FnOnce() -> bool) -> bool {
    !skip_query && probe()
}

/// Whether the terminal reports Shift+Enter distinctly, because enhancement flags were pushed.
pub fn keyboard_enhanced() -> bool {
    ENHANCED.load(Ordering::SeqCst)
}

/// Alternate scroll mode (`CSI ? 1007`): while it is on and mouse capture is off, terminals
/// turn the wheel into arrow keys, which would recall composer history.
pub fn alternate_scroll(on: bool) -> &'static str {
    if on { "\x1b[?1007h" } else { "\x1b[?1007l" }
}

/// Turns mouse capture on or off, with alternate scroll mode following it, so the wheel
/// scrolls the transcript with capture on and does nothing with it off.
pub fn set_mouse(enabled: bool) -> std::io::Result<()> {
    let mut out = stdout();
    if enabled {
        execute!(out, EnableMouseCapture)?;
    } else {
        execute!(out, DisableMouseCapture)?;
    }
    out.write_all(alternate_scroll(enabled).as_bytes())?;
    out.flush()
}

/// Restores every mode `enter` changed. Safe to call more than once.
pub fn leave() -> std::io::Result<()> {
    let mut out = stdout();
    if ENHANCED.swap(false, Ordering::SeqCst) {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = leave_modes(&mut out);
    let _ = disable_raw_mode();
    // Most terminals start with alternate scroll mode on, so the shell gets it back.
    let _ = out.write_all(alternate_scroll(true).as_bytes());
    out.flush()
}

/// Keeps scuttle alive through Ctrl+\ while a program it handed the terminal to runs, as
/// `system(3)` does. Leaving raw mode turns the terminal's signal keys back on, and they
/// signal the whole foreground process group, scuttle included. Dropping it restores the
/// default action, so Ctrl+\ outside a handoff, which raw mode delivers as a key, is unchanged.
/// Ctrl+C sends SIGINT, which [`Shutdown`] catches for the whole run and ignores while a
/// handoff is under way.
///
/// The signal is caught, not ignored: a caught signal resets to its default in the child at
/// exec, while an ignored one would stay ignored in the pager or editor.
pub struct HandoffSignals(());

#[cfg(unix)]
static OUTSIDE_HANDOFF: std::sync::LazyLock<Arc<AtomicBool>> =
    std::sync::LazyLock::new(|| Arc::new(AtomicBool::new(OUTSIDE_HANDOFF_AT_START)));

/// `OUTSIDE_HANDOFF` before the first handoff: scuttle starts outside one.
#[cfg(unix)]
const OUTSIDE_HANDOFF_AT_START: bool = true;

/// Starts a handoff's [`HandoffSignals`]. The SIGQUIT handler is installed on the first handoff
/// and stays, running the default action whenever no handoff is under way. The handoff flag it
/// sets also tells [`Shutdown`]'s SIGINT handler to ignore Ctrl+C.
pub fn handoff_signals() -> HandoffSignals {
    #[cfg(unix)]
    {
        static INSTALL: Once = Once::new();
        INSTALL.call_once(|| {
            let _ = signal_hook::flag::register_conditional_default(
                signal_hook::consts::SIGQUIT,
                OUTSIDE_HANDOFF.clone(),
            );
        });
        OUTSIDE_HANDOFF.store(false, Ordering::SeqCst);
    }
    HandoffSignals(())
}

impl Drop for HandoffSignals {
    fn drop(&mut self) {
        #[cfg(unix)]
        OUTSIDE_HANDOFF.store(true, Ordering::SeqCst);
    }
}

/// The SIGINT action: outside a handoff the signal asks [`Shutdown::recv`] for an exit. During
/// a handoff it does not, since Ctrl+C there was for the pager or the editor, which got it too.
#[cfg(unix)]
fn on_interrupt(outside_handoff: &AtomicBool, exit_requested: &AtomicBool) {
    if outside_handoff.load(Ordering::SeqCst) {
        exit_requested.store(true, Ordering::SeqCst);
    }
}

/// Set by the SIGINT handler when a SIGINT asked scuttle to exit, and taken by
/// [`Shutdown::recv`].
#[cfg(unix)]
static EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// The signals that end scuttle: SIGINT, SIGTERM, and SIGHUP, caught for the whole run, so
/// the main loop leaves through `finish` and the terminal gets every mode and the window
/// title back. While a handoff runs, the loop is busy with it, so SIGTERM or SIGHUP waits
/// until the pager or editor exits, and a SIGINT is ignored.
#[cfg(unix)]
pub struct Shutdown {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Shutdown {
    /// Starts catching the signals. It must run inside the tokio runtime.
    pub fn install() -> std::io::Result<Shutdown> {
        use tokio::signal::unix::{SignalKind, signal};
        static INSTALL: std::sync::OnceLock<std::io::Result<()>> = std::sync::OnceLock::new();
        // The handler decides when the signal arrives, while the handoff flag still holds,
        // since tokio's stream reports it only later. Registered before tokio's own handler,
        // it runs first, so the decision is recorded before `recv` can wake.
        INSTALL
            .get_or_init(|| {
                // Forces the `LazyLock` here, so the handler never runs its initializer, which
                // allocates.
                let outside = Arc::clone(&OUTSIDE_HANDOFF);
                // SAFETY: the action only loads the `AtomicBool` inside `outside`, an `Arc`
                // that is already allocated and never dropped, and stores to the static
                // `EXIT_REQUESTED`. Atomic loads and stores are async-signal-safe.
                unsafe {
                    signal_hook::low_level::register(signal_hook::consts::SIGINT, move || {
                        on_interrupt(&outside, &EXIT_REQUESTED);
                    })
                }
                .map(drop)
            })
            .as_ref()
            .map_err(|e| std::io::Error::new(e.kind(), e.to_string()))?;
        Ok(Shutdown {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            hangup: signal(SignalKind::hangup())?,
        })
    }

    /// Waits for the next signal that ends scuttle and returns the exit code it asks for:
    /// 128 plus its number, as a shell reports a process the signal ended.
    pub async fn recv(&mut self) -> u8 {
        loop {
            tokio::select! {
                _ = self.interrupt.recv() => {
                    if EXIT_REQUESTED.swap(false, Ordering::SeqCst) {
                        return 130;
                    }
                }
                _ = self.terminate.recv() => return 143,
                _ = self.hangup.recv() => return 129,
            }
        }
    }
}

/// Elsewhere scuttle has no signals to catch, so `recv` never returns.
#[cfg(not(unix))]
pub struct Shutdown;

#[cfg(not(unix))]
impl Shutdown {
    /// Nothing to install.
    pub fn install() -> std::io::Result<Shutdown> {
        Ok(Shutdown)
    }

    /// Never returns.
    pub async fn recv(&mut self) -> u8 {
        std::future::pending().await
    }
}

/// Reads one input event, or `None` when none arrived within its poll interval.
type Source = Box<dyn FnMut() -> std::io::Result<Option<Event>> + Send>;

/// How long the input thread waits for an event before it checks for a pause.
const INPUT_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// What the main loop wants of the input thread, and what the thread last acknowledged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputState {
    Reading,
    PauseRequested,
    /// The thread is parked outside `poll` and `read`, so it holds no crossterm lock.
    Paused,
    /// The main loop dropped the input, so the thread must exit.
    Quit,
    /// The thread exited, after a read error or a closed channel.
    Ended,
}

#[derive(Debug)]
struct Shared {
    state: Mutex<InputState>,
    changed: Condvar,
}

impl Shared {
    fn set(&self, state: InputState) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = state;
        self.changed.notify_all();
    }
}

/// Marks the input thread `Ended` when dropped, however the thread exits.
struct EndOnDrop(Arc<Shared>);

impl Drop for EndOnDrop {
    fn drop(&mut self) {
        self.0.set(InputState::Ended);
    }
}

/// The terminal's input, read on a thread of its own and sent to the main loop.
///
/// A program that takes over the terminal, such as the pager or `$EDITOR`, must get every key
/// typed while it runs, so the main loop pauses the thread around each handoff. `pause`
/// returns only once the thread is parked outside crossterm's `poll` and `read`.
pub struct Input {
    pub events: tokio::sync::mpsc::UnboundedReceiver<std::io::Result<Event>>,
    shared: Arc<Shared>,
    /// Drains input typed during a handoff, before the thread reads again.
    drain: fn(),
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Input {
    /// Starts reading the terminal.
    pub fn start() -> Input {
        Input::start_with(
            Box::new(|| {
                if crossterm::event::poll(INPUT_POLL)? {
                    crossterm::event::read().map(Some)
                } else {
                    Ok(None)
                }
            }),
            parsed_events,
            drain_terminal,
        )
    }

    /// Starts the thread on `source`. `pending` takes what crossterm has already parsed, and
    /// the thread delivers it before it acknowledges a pause, so `drain` at `resume` discards
    /// only what was typed during the handoff.
    fn start_with(mut source: Source, pending: fn() -> Vec<Event>, drain: fn()) -> Input {
        let (tx, events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            state: Mutex::new(InputState::Reading),
            changed: Condvar::new(),
        });
        let thread_shared = shared.clone();
        let thread = std::thread::spawn(move || {
            // Set on every exit, a panic included, so `pause` never waits on a dead thread.
            let ended = EndOnDrop(thread_shared);
            let shared = &ended.0;
            loop {
                {
                    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                    if *state == InputState::PauseRequested {
                        // Typed before the handoff, so it belongs to scuttle, not the child.
                        for event in pending() {
                            let _ = tx.send(Ok(event));
                        }
                        *state = InputState::Paused;
                        shared.changed.notify_all();
                    }
                    while *state == InputState::Paused {
                        state = shared
                            .changed
                            .wait(state)
                            .unwrap_or_else(|e| e.into_inner());
                    }
                    if *state == InputState::Quit {
                        break;
                    }
                }
                let event = match source() {
                    Ok(None) if tx.is_closed() => break,
                    Ok(None) => continue,
                    Ok(Some(event)) => Ok(event),
                    Err(e) => Err(e),
                };
                let failed = event.is_err();
                if tx.send(event).is_err() || failed {
                    break;
                }
            }
        });
        Input {
            events,
            shared,
            drain,
            thread: Some(thread),
        }
    }

    /// Stops reading the terminal, and returns once the thread holds no crossterm lock.
    pub fn pause(&self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if *state != InputState::Reading {
            return;
        }
        *state = InputState::PauseRequested;
        while *state == InputState::PauseRequested {
            state = self
                .shared
                .changed
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Discards input typed during the handoff that the program left unread, then reads the
    /// terminal again. Input typed before the pause was already delivered.
    pub fn resume(&self) {
        (self.drain)();
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if *state == InputState::Paused {
            *state = InputState::Reading;
            self.shared.changed.notify_all();
        }
    }
}

impl Drop for Input {
    /// Stops the thread and waits for it, at most one poll interval.
    fn drop(&mut self) {
        {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if *state != InputState::Ended {
                *state = InputState::Quit;
                self.shared.changed.notify_all();
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Takes the events crossterm has already parsed, and any input already waiting.
fn parsed_events() -> Vec<Event> {
    let mut events = Vec::new();
    while let Ok(true) = crossterm::event::poll(std::time::Duration::ZERO) {
        match crossterm::event::read() {
            Ok(event) => events.push(event),
            Err(_) => break,
        }
    }
    events
}

/// Reads and discards the input already waiting on the terminal.
fn drain_terminal() {
    while let Ok(true) = crossterm::event::poll(std::time::Duration::ZERO) {
        if crossterm::event::read().is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{Sender, channel};
    use std::time::Duration;

    /// A source fed by the test. It fails the test if it is read while `paused` is set.
    fn source(paused: Arc<AtomicBool>) -> (Sender<std::io::Result<Event>>, Source) {
        let (tx, rx) = channel::<std::io::Result<Event>>();
        let source: Source = Box::new(move || {
            assert!(
                !paused.load(Ordering::SeqCst),
                "the input was read while paused"
            );
            let event = rx.recv_timeout(Duration::from_millis(10));
            assert!(
                !paused.load(Ordering::SeqCst),
                "a read was still running when the pause was acknowledged"
            );
            match event {
                Ok(event) => event.map(Some),
                Err(_) => Ok(None),
            }
        });
        (tx, source)
    }

    fn next(input: &mut Input) -> std::io::Result<Event> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match input.events.try_recv() {
                Ok(event) => return event,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    assert!(std::time::Instant::now() < deadline, "no event arrived");
                    std::thread::yield_now();
                }
                Err(e) => panic!("the input ended: {e}"),
            }
        }
    }

    static DRAINED: AtomicBool = AtomicBool::new(false);

    #[test]
    fn a_paused_input_reads_nothing_until_it_resumes_and_drains_first() {
        let paused = Arc::new(AtomicBool::new(false));
        let (tx, source) = source(paused.clone());
        let mut input =
            Input::start_with(source, Vec::new, || DRAINED.store(true, Ordering::SeqCst));
        tx.send(Ok(Event::FocusGained)).unwrap();
        assert_eq!(next(&mut input).unwrap(), Event::FocusGained);
        input.pause();
        paused.store(true, Ordering::SeqCst);
        tx.send(Ok(Event::FocusLost)).unwrap();
        assert!(
            input.events.try_recv().is_err(),
            "nothing is read while paused"
        );
        paused.store(false, Ordering::SeqCst);
        input.resume();
        assert!(
            DRAINED.load(Ordering::SeqCst),
            "resume drains the terminal first"
        );
        assert_eq!(next(&mut input).unwrap(), Event::FocusLost);
    }

    #[test]
    fn a_read_error_ends_the_input_and_pausing_it_then_returns() {
        let (tx, source) = source(Arc::new(AtomicBool::new(false)));
        let mut input = Input::start_with(source, Vec::new, || {});
        tx.send(Err(std::io::Error::other("tty closed"))).unwrap();
        assert!(next(&mut input).is_err());
        input.pause();
        input.resume();
    }

    #[test]
    fn dropping_the_receiver_ends_the_thread() {
        let (_tx, source) = source(Arc::new(AtomicBool::new(false)));
        let mut input = Input::start_with(source, Vec::new, || {});
        let thread = input.thread.take().unwrap();
        drop(input);
        thread.join().unwrap();
    }

    /// Waits for `thread` to finish, at most five seconds; `true` once it has.
    fn ends(thread: std::thread::JoinHandle<()>) -> bool {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(thread.join().is_ok());
        });
        rx.recv_timeout(Duration::from_secs(5)).is_ok()
    }

    /// Runs `pause` on a thread of its own; `true` once it returned within five seconds.
    fn pause_returns(input: Input) -> bool {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            input.pause();
            let _ = tx.send(());
            drop(input);
        });
        rx.recv_timeout(Duration::from_secs(5)).is_ok()
    }

    #[test]
    fn events_parsed_before_a_pause_are_delivered_not_drained() {
        let (_tx, source) = source(Arc::new(AtomicBool::new(false)));
        let mut input = Input::start_with(source, || vec![Event::FocusLost], || {});
        input.pause();
        assert_eq!(
            input.events.try_recv().ok().map(Result::unwrap),
            Some(Event::FocusLost),
            "what crossterm had parsed reaches the main loop"
        );
        input.resume();
    }

    #[test]
    fn a_panic_on_the_input_thread_ends_it_so_pausing_returns() {
        let mut input = Input::start_with(Box::new(|| panic!("boom")), Vec::new, || {});
        let thread = input.thread.take().unwrap();
        let _ = thread.join();
        assert!(pause_returns(input), "pause() waited forever");
    }

    #[test]
    fn a_closed_channel_ends_the_thread_while_the_input_lives() {
        let (_tx, idle) = source(Arc::new(AtomicBool::new(false)));
        let mut input = Input::start_with(idle, Vec::new, || {});
        input.events.close();
        let thread = input.thread.take().unwrap();
        assert!(ends(thread), "an idle poll notices the closed channel");
        assert_eq!(*input.shared.state.lock().unwrap(), InputState::Ended);

        let (tx, busy) = source(Arc::new(AtomicBool::new(false)));
        let mut input = Input::start_with(busy, Vec::new, || {});
        input.events.close();
        tx.send(Ok(Event::FocusGained)).unwrap();
        let thread = input.thread.take().unwrap();
        assert!(ends(thread), "a failed send ends the thread");
        assert!(pause_returns(input));
    }

    /// Serializes the tests that take `handoff_signals`, so no test can end another's handoff
    /// while it raises a signal.
    #[cfg(unix)]
    static HANDOFF_TESTS: Mutex<()> = Mutex::new(());

    /// Without the handler, the raise kills the test binary. SIGINT is caught by `Shutdown`
    /// for the whole run instead, which `ctrl_c_during_a_handoff_never_ends_scuttle` covers.
    #[cfg(unix)]
    #[test]
    fn ctrl_backslash_during_a_handoff_leaves_scuttle_running() {
        let _serial = HANDOFF_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let failed_start = || -> std::io::Result<()> {
            let _signals = handoff_signals();
            signal_hook::low_level::raise(signal_hook::consts::SIGQUIT)?;
            Err(std::io::Error::other("the child did not start"))
        };
        assert!(failed_start().is_err());
        assert!(
            OUTSIDE_HANDOFF.load(Ordering::SeqCst),
            "the default action is back once the handoff ends, even when the child failed"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_interrupt_asks_for_an_exit_only_outside_a_handoff() {
        let outside = AtomicBool::new(OUTSIDE_HANDOFF_AT_START);
        let exit = AtomicBool::new(false);
        on_interrupt(&outside, &exit);
        assert!(
            exit.load(Ordering::SeqCst),
            "a SIGINT before any handoff asks for an exit"
        );

        let exit = AtomicBool::new(false);
        outside.store(false, Ordering::SeqCst);
        on_interrupt(&outside, &exit);
        assert!(
            !exit.load(Ordering::SeqCst),
            "a SIGINT during a handoff asks for none"
        );

        outside.store(true, Ordering::SeqCst);
        on_interrupt(&outside, &exit);
        assert!(
            exit.load(Ordering::SeqCst),
            "a SIGINT after the handoff ends asks for an exit"
        );
    }

    /// The raises are safe: `Shutdown::install` catches SIGINT before either one. The test
    /// builds its own runtime so the serializing lock is never held across an await.
    #[cfg(unix)]
    #[test]
    fn ctrl_c_during_a_handoff_never_ends_scuttle() {
        let _serial = HANDOFF_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut shutdown = Shutdown::install().unwrap();
            {
                let _signals = handoff_signals();
                signal_hook::low_level::raise(signal_hook::consts::SIGINT).unwrap();
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(300), shutdown.recv())
                    .await
                    .is_err(),
                "a SIGINT during a handoff asked for no exit"
            );
            signal_hook::low_level::raise(signal_hook::consts::SIGINT).unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), shutdown.recv()).await,
                Ok(130),
                "a SIGINT outside a handoff exits with 130"
            );
        });
    }

    #[test]
    fn focus_reporting_is_on_while_scuttle_runs_and_off_once_it_leaves() {
        let mut on = Vec::new();
        enter_modes(&mut on).unwrap();
        let on = String::from_utf8(on).unwrap();
        assert!(on.contains("\x1b[?1004h"), "{on:?}");
        let mut off = Vec::new();
        leave_modes(&mut off).unwrap();
        let off = String::from_utf8(off).unwrap();
        assert!(off.contains("\x1b[?1004l"), "{off:?}");
        assert!(
            off.contains("\x1b[?2004l") && off.contains("\x1b[?1049l"),
            "{off:?}"
        );
    }

    #[test]
    fn alternate_scroll_is_turned_off_with_the_mouse_and_back_on_after() {
        assert_eq!(super::alternate_scroll(false), "\x1b[?1007l");
        assert_eq!(super::alternate_scroll(true), "\x1b[?1007h");
    }

    #[test]
    fn skipping_terminal_queries_skips_the_enhancement_probe() {
        assert!(!enhancement_supported(true, || panic!("probed")));
        assert!(enhancement_supported(false, || true));
        assert!(!enhancement_supported(false, || false));
    }

    #[test]
    fn the_title_sequence_is_osc_2_without_control_characters() {
        assert_eq!(
            super::title_sequence("scuttle · Fix"),
            "\x1b]2;scuttle · Fix\x07"
        );
        assert_eq!(super::title_sequence("a\x1b\x07\u{9c}b"), "\x1b]2;ab\x07");
        assert_eq!(super::PUSH_TITLE, "\x1b[22;0t");
        assert_eq!(super::POP_TITLE, "\x1b[23;0t");
    }
}
