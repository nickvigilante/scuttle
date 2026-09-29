//! Entering and leaving the full-screen terminal, restored on exit and on panic.

use std::io::{Write, stdout};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
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

/// Switches to the alternate screen in raw mode and returns the terminal to draw on.
pub fn enter(mouse: bool) -> std::io::Result<DefaultTerminal> {
    PANIC_HOOK.call_once(|| {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = leave();
            hook(info);
        }));
    });
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

pub fn set_mouse(enabled: bool) -> std::io::Result<()> {
    let mut out = stdout();
    if enabled {
        execute!(out, EnableMouseCapture)
    } else {
        execute!(out, DisableMouseCapture)
    }
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
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::enhancement_supported;

    #[test]
    fn skipping_terminal_queries_skips_the_enhancement_probe() {
        assert!(!enhancement_supported(true, || panic!("probed")));
        assert!(enhancement_supported(false, || true));
        assert!(!enhancement_supported(false, || false));
    }
}
