//! Entering and leaving the full-screen terminal, restored on exit and on panic.

use std::io::{Write, stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Once};

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
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
    execute!(out, EnterAlternateScreen, EnableBracketedPaste)?;
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
    let _ = execute!(
        out,
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    );
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
    fn get(&self) -> InputState {
        *self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

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
                if input_ready(INPUT_POLL)? {
                    crossterm::event::read().map(Some)
                } else {
                    Ok(None)
                }
            }),
            parsed_events,
            drain_terminal,
            output_open,
        )
    }

    /// Starts the thread on `source`. `pending` takes what crossterm has already parsed, and
    /// the thread delivers it before it acknowledges a pause, so `drain` at `resume` discards
    /// only what was typed during the handoff.
    ///
    /// A second thread asks `open` every [`LIVENESS_CHECK`] whether the terminal is still
    /// there and, once it is not, ends the input with an error, even while `source` is stuck
    /// inside crossterm.
    fn start_with(
        mut source: Source,
        pending: fn() -> Vec<Event>,
        drain: fn(),
        open: fn() -> bool,
    ) -> Input {
        let (tx, events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            state: Mutex::new(InputState::Reading),
            changed: Condvar::new(),
        });
        let watch_tx = tx.clone();
        let watch_shared = shared.clone();
        // Stops once the input thread has, so the channel still closes after it.
        std::thread::spawn(move || {
            while !watch_tx.is_closed() && watch_shared.get() != InputState::Ended {
                if !open() {
                    let _ = watch_tx.send(Err(closed_terminal()));
                    return;
                }
                std::thread::sleep(LIVENESS_CHECK);
            }
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
    /// Stops the thread and waits for it, usually one poll interval. A thread still stuck in
    /// a read after [`INPUT_JOIN_GRACE`] is left to end with the process.
    fn drop(&mut self) {
        let ended = {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if *state != InputState::Ended {
                *state = InputState::Quit;
                self.shared.changed.notify_all();
            }
            let (state, _) = self
                .shared
                .changed
                .wait_timeout_while(state, INPUT_JOIN_GRACE, |s| *s != InputState::Ended)
                .unwrap_or_else(|e| e.into_inner());
            *state == InputState::Ended
        };
        if let Some(thread) = self.thread.take()
            && ended
        {
            let _ = thread.join();
        }
    }
}

/// Takes the events crossterm has already parsed, and any input already waiting.
fn parsed_events() -> Vec<Event> {
    let mut events = Vec::new();
    while let Ok(true) = input_waiting() {
        match crossterm::event::read() {
            Ok(event) => events.push(event),
            Err(_) => break,
        }
    }
    events
}

/// Reads and discards the input already waiting on the terminal.
fn drain_terminal() {
    while let Ok(true) = input_waiting() {
        if crossterm::event::read().is_err() {
            break;
        }
    }
}

/// How often the input's watcher checks that the terminal is still open.
const LIVENESS_CHECK: std::time::Duration = std::time::Duration::from_millis(250);

/// How long dropping [`Input`] waits for its thread, which may be stuck inside crossterm on a
/// closed terminal, before leaving it to end with the process.
const INPUT_JOIN_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

/// The error that ends the input once the terminal is gone.
fn closed_terminal() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "the terminal closed")
}

/// Whether `e`, from reading, writing, or polling the terminal, means the terminal is gone:
/// end of file, or EIO, EBADF, or ENXIO from a hung up, revoked, or closed terminal.
pub fn terminal_gone(e: &std::io::Error) -> bool {
    if e.kind() == std::io::ErrorKind::UnexpectedEof {
        return true;
    }
    #[cfg(unix)]
    {
        matches!(
            e.raw_os_error(),
            Some(libc::EIO | libc::EBADF | libc::ENXIO)
        )
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Whether input is waiting for crossterm to read now.
fn input_waiting() -> std::io::Result<bool> {
    input_ready(std::time::Duration::ZERO)
}

/// Waits up to `timeout` for an event crossterm can read without blocking. A closed terminal
/// is an error: crossterm would retry its read of one forever, so it is only asked while the
/// terminal is open, either for an event it already parsed or once new input arrived.
fn input_ready(timeout: std::time::Duration) -> std::io::Result<bool> {
    #[cfg(unix)]
    if let Ok(tty) = TtyFd::open() {
        wait_for_input(tty.fd(), std::time::Duration::ZERO)?;
        if crossterm::event::poll(std::time::Duration::ZERO)? {
            return Ok(true);
        }
        if timeout.is_zero() || !wait_for_input(tty.fd(), timeout)? {
            return Ok(false);
        }
        return crossterm::event::poll(std::time::Duration::ZERO);
    }
    crossterm::event::poll(timeout)
}

/// Whether the terminal scuttle draws on is still open.
fn output_open() -> bool {
    #[cfg(unix)]
    {
        fd_open(libc::STDOUT_FILENO)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The terminal crossterm reads: standard input when it is a terminal, else `/dev/tty`.
#[cfg(unix)]
#[derive(Clone, Copy)]
struct TtyFd(std::os::unix::io::RawFd);

#[cfg(unix)]
impl TtyFd {
    /// Opens the terminal once; later calls share that descriptor, which stays open for the
    /// life of the process, as crossterm's own does.
    fn open() -> std::io::Result<TtyFd> {
        static TTY: std::sync::OnceLock<Option<TtyFd>> = std::sync::OnceLock::new();
        TTY.get_or_init(|| {
            use std::os::unix::io::IntoRawFd;
            // SAFETY: `isatty` only inspects the descriptor.
            if unsafe { libc::isatty(libc::STDIN_FILENO) } == 1 {
                return Some(TtyFd(libc::STDIN_FILENO));
            }
            std::fs::File::open("/dev/tty")
                .ok()
                .map(|f| TtyFd(f.into_raw_fd()))
        })
        .ok_or_else(|| std::io::Error::other("no terminal"))
    }

    fn fd(self) -> std::os::unix::io::RawFd {
        self.0
    }
}

/// Whether `revents` from `poll(2)` says the descriptor's terminal is gone.
#[cfg(unix)]
fn hung_up(revents: libc::c_short) -> bool {
    revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

/// Waits up to `timeout` for input on `fd`: `true` once some is waiting, `false` when none
/// arrived, and an error once the terminal is gone.
#[cfg(unix)]
fn wait_for_input(
    fd: std::os::unix::io::RawFd,
    timeout: std::time::Duration,
) -> std::io::Result<bool> {
    let mut pollfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
    // SAFETY: `pollfd` is one valid, initialized entry, matching the count of 1.
    let ready = unsafe { libc::poll(&mut pollfd, 1, millis) };
    if ready < 0 {
        let e = std::io::Error::last_os_error();
        return match e.kind() {
            std::io::ErrorKind::Interrupted => Ok(false),
            _ => Err(e),
        };
    }
    if hung_up(pollfd.revents) {
        return Err(closed_terminal());
    }
    Ok(pollfd.revents & libc::POLLIN != 0)
}

/// Whether `fd` is still open for writing: an empty write fails with EIO once its terminal
/// has hung up or been revoked, and writes nothing otherwise.
#[cfg(unix)]
fn fd_open(fd: std::os::unix::io::RawFd) -> bool {
    // SAFETY: a zero-length write reads nothing from the buffer.
    let written = unsafe { libc::write(fd, std::ptr::null(), 0) };
    written >= 0 || !terminal_gone(&std::io::Error::last_os_error())
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
        let mut input = Input::start_with(
            source,
            Vec::new,
            || DRAINED.store(true, Ordering::SeqCst),
            || true,
        );
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
        let mut input = Input::start_with(source, Vec::new, || {}, || true);
        tx.send(Err(std::io::Error::other("tty closed"))).unwrap();
        assert!(next(&mut input).is_err());
        input.pause();
        input.resume();
    }

    #[test]
    fn dropping_the_receiver_ends_the_thread() {
        let (_tx, source) = source(Arc::new(AtomicBool::new(false)));
        let mut input = Input::start_with(source, Vec::new, || {}, || true);
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
        let mut input = Input::start_with(source, || vec![Event::FocusLost], || {}, || true);
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
        let mut input = Input::start_with(Box::new(|| panic!("boom")), Vec::new, || {}, || true);
        let thread = input.thread.take().unwrap();
        let _ = thread.join();
        assert!(pause_returns(input), "pause() waited forever");
    }

    #[test]
    fn a_closed_channel_ends_the_thread_while_the_input_lives() {
        let (_tx, idle) = source(Arc::new(AtomicBool::new(false)));
        let mut input = Input::start_with(idle, Vec::new, || {}, || true);
        input.events.close();
        let thread = input.thread.take().unwrap();
        assert!(ends(thread), "an idle poll notices the closed channel");
        assert_eq!(*input.shared.state.lock().unwrap(), InputState::Ended);

        let (tx, busy) = source(Arc::new(AtomicBool::new(false)));
        let mut input = Input::start_with(busy, Vec::new, || {}, || true);
        input.events.close();
        tx.send(Ok(Event::FocusGained)).unwrap();
        let thread = input.thread.take().unwrap();
        assert!(ends(thread), "a failed send ends the thread");
        assert!(pause_returns(input));
    }

    #[test]
    fn errors_from_a_closed_terminal_say_it_is_gone() {
        use std::io::{Error, ErrorKind};
        for errno in [libc::EIO, libc::EBADF, libc::ENXIO] {
            assert!(
                terminal_gone(&Error::from_raw_os_error(errno)),
                "errno {errno}"
            );
        }
        assert!(terminal_gone(&Error::from(ErrorKind::UnexpectedEof)));
        assert!(terminal_gone(&closed_terminal()));
        for kind in [
            ErrorKind::WouldBlock,
            ErrorKind::Interrupted,
            ErrorKind::Other,
        ] {
            assert!(!terminal_gone(&Error::from(kind)), "{kind:?}");
        }
        assert!(!terminal_gone(&Error::from_raw_os_error(libc::EAGAIN)));
    }

    #[cfg(unix)]
    #[test]
    fn poll_events_from_a_closed_terminal_say_it_is_gone() {
        for revents in [libc::POLLHUP, libc::POLLERR, libc::POLLNVAL] {
            assert!(hung_up(revents), "{revents:#x}");
            assert!(hung_up(revents | libc::POLLIN), "{revents:#x} with input");
        }
        assert!(!hung_up(libc::POLLIN));
        assert!(!hung_up(0));
    }

    /// A pseudo terminal's two sides, as raw descriptors.
    #[cfg(unix)]
    fn pty() -> (libc::c_int, libc::c_int) {
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: both out pointers are valid, and the name, termios, and size are optional.
        let r = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(r, 0, "openpty: {}", std::io::Error::last_os_error());
        (master, slave)
    }

    #[cfg(unix)]
    #[test]
    fn a_terminal_whose_other_side_closed_is_gone_not_readable() {
        let (master, slave) = pty();
        assert!(
            !wait_for_input(slave, Duration::ZERO).unwrap(),
            "no input yet"
        );
        assert!(fd_open(slave), "a live terminal is open");
        // SAFETY: `master` came from `openpty` and is closed once.
        unsafe { libc::close(master) };
        let err = wait_for_input(slave, Duration::from_millis(100))
            .expect_err("a closed terminal is an error, not input");
        assert!(terminal_gone(&err), "{err}");
        assert!(!fd_open(slave), "writes to a closed terminal fail");
        // SAFETY: `slave` came from `openpty` and is closed once.
        unsafe { libc::close(slave) };
    }

    #[test]
    fn a_closed_terminal_ends_the_input_even_while_a_read_is_stuck() {
        let mut input = Input::start_with(
            Box::new(|| {
                loop {
                    std::thread::park();
                }
            }),
            Vec::new,
            || {},
            || false,
        );
        let err = next(&mut input).expect_err("the input ends with an error");
        assert!(terminal_gone(&err), "{err}");
    }

    #[test]
    fn dropping_the_input_returns_while_its_read_is_stuck() {
        let input = Input::start_with(
            Box::new(|| {
                loop {
                    std::thread::park();
                }
            }),
            Vec::new,
            || {},
            || true,
        );
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            drop(input);
            let _ = tx.send(());
        });
        assert!(
            rx.recv_timeout(Duration::from_secs(3)).is_ok(),
            "dropping the input waited for a read that never returns"
        );
    }

    #[test]
    fn the_channel_closes_once_the_input_thread_ends() {
        let mut input = Input::start_with(Box::new(|| panic!("boom")), Vec::new, || {}, || true);
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(input.events.blocking_recv().is_none());
        });
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(3)),
            Ok(true),
            "the main loop learns the input ended"
        );
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
