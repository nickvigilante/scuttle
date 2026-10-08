//! The TUI: owns UI state, turns terminal events into core messages, and draws.

use std::collections::{HashSet, VecDeque};
use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use scuttle_core::app::{App, CopyTarget, Effect, Msg, Notice, Picker, QuestionKey, QueueAction};
use scuttle_core::attachments::ChipState;
use scuttle_core::config::{self, LocalConfig, StatusField, StatuslineConfig};
use scuttle_core::density::SendShortcut;
use scuttle_core::files::ConflictChoice;
use scuttle_core::line_edit::Edit;
use scuttle_core::skills::{self, MenuKind};

use crate::activity::{SPINNER_INTERVAL, SpinnerStyle, activity_line, next_seed, shows_activity};
use crate::clipboard::{Clipboard, CopyOutcome};
use crate::composer::{Composer, ComposerAction, SLASH_ROWS, is_send_key, send_now_label};
use crate::footer::status_line_at;
use crate::help::help_lines_for;
use crate::icons::{self, Icon, IconSet};
use crate::links;
use crate::overlay::{Overlay, OverlayOutcome, ViewCtx};
use crate::picker::{PickerChoice, PickerState};
use crate::selection::{Pos, Selection, selected_text};
use crate::table;
use crate::theme::Theme;
use crate::transcript_view::{self, BlockId, HitTarget, View, Welcome};

/// How long a notice replaces the status footer when no key is pressed.
pub const NOTICE_TTL: Duration = Duration::from_secs(5);

/// How long a notice may wait for its turn in the footer before it is dropped unseen, since it
/// no longer describes the moment.
pub const NOTICE_STALE: Duration = Duration::from_secs(10);

/// Whether the waiting notice at `i` keeps its turn through a key press and `NOTICE_STALE`:
/// a startup notice, one of the first `startup`, arrived before the user saw anything, and an
/// error is the only report that something failed, so neither is dropped unseen.
fn keeps_its_turn(notices: &[Notice], startup: usize, i: usize) -> bool {
    i < startup || matches!(notices.get(i), Some(Notice::Error(_)))
}

/// How long an active error shows before a key press may clear it, so one that arrives while
/// the user types is still seen.
pub const ERROR_MIN_SHOWN: Duration = Duration::from_millis(1500);

/// How long the copy confirmation in the composer's rule stays when no key is pressed.
pub const COPY_TTL: Duration = Duration::from_secs(2);

/// How often the AI spend and the workspace quota refresh while scuttle runs, as the web UI's
/// quota does.
pub const LIMITS_EVERY: Duration = Duration::from_secs(60);

/// The rows at the bottom of `transcript` that `menu`, drawn over it, covers.
fn covered_by(transcript: Rect, menu: Option<Rect>) -> u16 {
    menu.map_or(0, |m| transcript.bottom().saturating_sub(m.y))
}

/// Where `overlay` draws over `transcript`: along its bottom edge, over the whole height for
/// the full-height overlays, such as `/chats` and `/model`, as tall as its rows for the panels
/// whose every action must show, and ten rows tall for the rest.
fn overlay_area(overlay: &Overlay, view: Option<&table::TableView>, transcript: Rect) -> Rect {
    let h = match (overlay, view) {
        _ if overlay.full_height() => transcript.height,
        // Sized from its rows, so every action shows without scrolling.
        (Overlay::WorkspaceDetails(_) | Overlay::Statusline(_), Some(view)) => {
            table::height(view, transcript.width).min(transcript.height)
        }
        _ => 10.min(transcript.height),
    };
    Rect {
        y: transcript.y + transcript.height - h,
        height: h,
        ..transcript
    }
}

/// What Ctrl+O says when the composer holds nothing to copy.
const NOTHING_TO_COPY: &str = "Nothing to copy: the composer is empty.";

/// Why a draft that lost part of a snippet's token, by any edit, was not sent.
fn broken_paste(token: &str) -> String {
    format!(
        "Not sent: {token} was changed, so its paste would be left out. Restore it, or delete the whole token."
    )
}

/// The confirmation a copy leaves in the composer's rule, and the draw that first showed it.
#[derive(Debug, Clone)]
struct CopyNote {
    text: String,
    since: Option<Instant>,
}

impl CopyNote {
    fn new(chars: usize) -> CopyNote {
        let text = match chars {
            1 => "Copied 1 character".to_owned(),
            n => format!("Copied {n} characters"),
        };
        CopyNote { text, since: None }
    }
}

/// Screens narrower or shorter than this keep every cell for content instead of a margin.
const MIN_PADDED: (u16, u16) = (20, 8);

/// The most characters of a chat title the window title shows.
const TITLE_MAX: usize = 100;

/// The user's home directory, for `~/` in `@path` and `/attach`.
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// The screen inside a one-cell margin on every side, or all of it on a tiny terminal.
fn padded(area: Rect) -> Rect {
    if area.width >= MIN_PADDED.0 && area.height >= MIN_PADDED.1 {
        area.inner(Margin::new(1, 1))
    } else {
        area
    }
}

const PLACEHOLDER_SHIFT: &str =
    "Message the agent. Enter to send, Shift+Enter for a new line, /help for commands.";
const PLACEHOLDER_ALT: &str =
    "Message the agent. Enter to send, Alt+Enter for a new line, /help for commands.";

/// What the dim row above the composer says after the key that sends a queued message now.
const QUEUED_HINT: &str = "sends the next queued message now";

/// The composer hint for the send preference and the keyboard enhancement state. In modifier
/// mode it names `send_now`, the key `composer::send_now_label` picks, so a Mac shows ⌘↵.
fn placeholder_text(shortcut: SendShortcut, enhanced: bool, send_now: &str) -> String {
    match (shortcut, enhanced) {
        (SendShortcut::Enter, true) => PLACEHOLDER_SHIFT.to_owned(),
        (SendShortcut::Enter, false) => PLACEHOLDER_ALT.to_owned(),
        (SendShortcut::ModifierEnter, _) => format!(
            "Message the agent. {send_now} to send, Enter for a new line, /help for commands."
        ),
    }
}

/// What became of the program a terminal handoff was for.
#[derive(Debug)]
pub enum Handed {
    /// Leaving the terminal failed, so the program never ran.
    NotHandedOver(std::io::Error),
    /// The program ran, with this outcome.
    Ran(std::io::Result<()>),
}

/// Leaves the terminal, runs a program that takes it over, such as the editor or the pager,
/// and resumes. Resume is attempted even when leaving failed, but the program only runs on a
/// terminal that was fully left. Returns the program's fate and the resume's outcome.
pub fn handoff_round_trip(
    leave: impl FnOnce() -> std::io::Result<()>,
    run: impl FnOnce() -> std::io::Result<()>,
    resume: impl FnOnce() -> std::io::Result<()>,
) -> (Handed, std::io::Result<()>) {
    let handed = match leave() {
        Ok(()) => Handed::Ran(run()),
        Err(e) => Handed::NotHandedOver(e),
    };
    let resumed = resume();
    (handed, resumed)
}

/// Runs `$EDITOR`, else `$VISUAL`, else `vi`, on `path` in a terminal handoff, resuming with
/// mouse capture `mouse`. Returns the editor's outcome and the terminal's resume outcome.
fn run_editor(path: &std::path::Path, mouse: bool) -> (std::io::Result<()>, std::io::Result<()>) {
    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .ok()
        .filter(|e| !e.trim().is_empty())
        .unwrap_or_else(|| "vi".into());
    let (handed, resumed) = handoff_round_trip(
        crate::terminal::leave,
        || {
            // `$1` keeps the path out of the shell parse while still allowing
            // `EDITOR="code -w"`.
            let status = crate::runtime::child_command("sh")
                .arg("-c")
                .arg(format!("{editor} \"$1\""))
                .arg("sh")
                .arg(path)
                .status()?;
            if status.success() {
                Ok(())
            } else {
                Err(std::io::Error::other(format!(
                    "editor exited with {status}"
                )))
            }
        },
        || crate::terminal::resume(mouse),
    );
    let edited = match handed {
        Handed::Ran(edited) => edited,
        Handed::NotHandedOver(e) => Err(e),
    };
    (edited, resumed)
}

/// The notice for copying `url`, called `what`, such as a URL that opened no browser.
fn copy_url_notice(what: &str, url: &str, outcome: CopyOutcome) -> Notice {
    match outcome {
        CopyOutcome::Copied => Notice::Info(format!("Copied {what}: {url}")),
        CopyOutcome::CopiedWithWarning(w) => Notice::Info(format!("Copied {what}: {url}. {w}")),
        CopyOutcome::Failed(_) => Notice::Error(format!("Could not copy {what}: {url}")),
    }
}

/// The notice for copying the chat URL after `/web` opened no browser.
fn web_copy_notice(url: &str, outcome: CopyOutcome) -> Notice {
    copy_url_notice("the chat URL", url, outcome)
}

/// The local time, with its UTC offset, which names a pasted snippet's file.
fn local_now() -> crate::composer::PasteTime {
    chrono::Local::now().fixed_offset()
}

/// Seconds since the Unix epoch, for relative times in overlays.
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The local time zone's current UTC offset, which the core needs for local times.
fn local_offset() -> chrono::FixedOffset {
    use chrono::Offset;
    chrono::Local::now().offset().fix()
}

/// The editor key for `key`, if the one-line editor uses it. The editor holds one line, so the
/// keys that move to the ends of a line or of the whole text all go to its ends.
pub(crate) fn edit_key(key: KeyEvent) -> Option<Edit> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let sup = key.modifiers.contains(KeyModifiers::SUPER);
    match key.code {
        KeyCode::Home => return Some(Edit::Home),
        KeyCode::End => return Some(Edit::End),
        KeyCode::Left if sup => return Some(Edit::Home),
        KeyCode::Right if sup => return Some(Edit::End),
        KeyCode::Char('a') if ctrl => return Some(Edit::Home),
        KeyCode::Char('e') if ctrl => return Some(Edit::End),
        _ => {}
    }
    if ctrl {
        return None;
    }
    Some(match key.code {
        KeyCode::Char(c) => Edit::Char(c),
        KeyCode::Backspace => Edit::Backspace,
        KeyCode::Delete => Edit::Delete,
        KeyCode::Left => Edit::Left,
        KeyCode::Right => Edit::Right,
        KeyCode::Enter => Edit::Submit,
        KeyCode::Esc => Edit::Cancel,
        _ => return None,
    })
}

/// The notice for copying a link that opened no browser. A link the runtime refuses to open
/// says so, since nothing else would explain why the click did not open it.
fn link_copy_notice(url: &str, outcome: CopyOutcome) -> Notice {
    let notice = copy_url_notice("the link", url, outcome);
    if links::web_link(url).is_some() {
        return notice;
    }
    let why = "Only http and https links open in a browser.";
    match notice {
        Notice::Info(text) => Notice::Info(format!("{why} {text}")),
        Notice::Error(text) => Notice::Error(format!("{why} {text}")),
    }
}

/// Whether `msg` can change what the slash menu lists: the skills, the user, or the open
/// chat's record. A refresh arrives inside `Msg::ForChat` and `Msg::ForRefresh`, so this looks
/// inside them.
fn changes_menu(msg: &Msg) -> bool {
    match msg {
        Msg::SkillsLoaded(_)
        | Msg::SkillsFailed(_)
        | Msg::UserLoaded(_)
        | Msg::ChatLoaded { .. }
        | Msg::ChatRefreshed(_) => true,
        Msg::ForChat { msg, .. } | Msg::ForRefresh { msg, .. } => changes_menu(msg),
        _ => false,
    }
}

/// `text` as one line for the one-line editor: line breaks become spaces, and other control
/// characters, tabs included, are dropped.
fn one_line_paste(text: &str) -> String {
    text.replace("\r\n", " ")
        .chars()
        .filter_map(|c| match c {
            '\n' | '\r' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect()
}

/// A left-button press in the transcript that has not been released.
#[derive(Debug, Clone, Copy)]
struct Drag {
    anchor: Pos,
    /// Whether the pointer left the anchor cell; a release without moving is a click.
    moved: bool,
    /// The transcript line kept at the top of the screen while the button is held, so new
    /// output cannot scroll the text out from under the pointer.
    top: usize,
    /// The transcript area at the press. Both ends of the drag map through it, so the
    /// activity row appearing or leaving mid-drag cannot shift the selection.
    area: Rect,
}

pub struct Tui {
    pub core: App,
    pub composer: Composer,
    /// The effort slider, the one picker drawn above the composer.
    pub picker: Option<PickerState>,
    /// The table overlay, such as the model, workspace, or organization list.
    pub overlay: Option<Overlay>,
    pub last_copied: Option<String>,
    /// The text last handed to the pager.
    pub last_paged: Option<String>,
    config: LocalConfig,
    config_path: Option<PathBuf>,
    /// The footer's fields and warnings in effect. `/statusline` applies and saves each
    /// change at once, so this holds no unsaved edits: it is what `config.toml` holds, unless
    /// a save failed, when it keeps that change for this session.
    statusline: StatuslineConfig,
    theme: Theme,
    /// What `NERD_FONT` asked for at startup, which picks the icons while `config.toml` sets
    /// no `icons`.
    icon_env: Option<IconSet>,
    welcome: Welcome,
    /// Where to remember that the Nerd Font tip was shown, until the first draw that shows it.
    nerd_tip_state: Option<PathBuf>,
    toggles: HashSet<BlockId>,
    view: View,
    area: Rect,
    /// The terminal's width at the last draw, which decides the optional `/chats` columns.
    screen_width: u16,
    scroll_from_bottom: usize,
    last_ctrl_c: Option<Instant>,
    show_help: bool,
    /// How far the help overlay is scrolled, in lines.
    help_scroll: u16,
    /// Created on the first copy: opening the native clipboard can block on a stale display.
    clipboard: Option<Clipboard>,
    /// The footer's notice: an index into `core.notices` and when it became active.
    active_notice: Option<(usize, Instant)>,
    /// How many of `core.notices` the footer has already considered.
    notices_seen: usize,
    /// Indices into `core.notices` that arrived but were never active, oldest first, each with
    /// the draw that first saw it.
    unshown_notices: VecDeque<(usize, Instant)>,
    /// How many of `core.notices` were there at the first draw. These startup notices, such as
    /// the version-skew warning, take their turns however long they wait and whatever keys
    /// are pressed, since the user has not seen the screen yet.
    startup_notices: Option<usize>,
    /// The copy confirmation, shown in the composer's rule until `COPY_TTL` after its first
    /// draw or the next key.
    copied: Option<CopyNote>,
    /// Set when the screen may hold foreign output, for example after the external editor.
    needs_full_redraw: bool,
    /// Whether keyboard enhancement flags are active, which decides the send and newline keys.
    keyboard_enhanced: bool,
    /// Set when the app must quit with an error, for example when the terminal cannot be restored.
    fatal: Option<String>,
    /// The window title last handed to the terminal.
    shown_title: Option<String>,
    /// Set by Ctrl+G. The main loop runs the editor, since it must pause its input first.
    editor_requested: bool,
    /// When the Tui was made; the spinner frame is a function of the time since.
    epoch: Instant,
    /// `now_unix()` at `epoch`, so a draw's relative times follow the `now` it is given.
    epoch_unix: i64,
    /// When the AI spend and the workspace quota refresh next. A turn that ends or a change of
    /// organization sets it to `epoch`, which has passed, so the next wakeup refreshes them.
    limits_due: Instant,
    /// Set when the open chat's cost should be asked for at the next wakeup: after a turn, a
    /// chat switch, or the footer starting to show cost. The core decides whether to ask.
    cost_due: bool,
    /// The spinner style of the current turn, picked when a turn starts.
    spinner: SpinnerStyle,
    /// The state a random spinner pick steps with `next_seed` at each turn start. Zero stays
    /// zero, so a Tui made with seed 0 always draws braille under the `"random"` setting.
    seed: u64,
    /// Set by `tick` for a timer wakeup, where only the clock changed, so `draw_at` reuses the
    /// transcript lines instead of rebuilding them. Cleared by every draw.
    reuse_view: bool,
    /// The transcript width `view` was built for.
    view_width: u16,
    /// The transcript's `history_resets` count when `view` was built.
    view_resets: u64,
    /// How many times `draw_at` rebuilt the transcript lines; tests check timer frames reuse them.
    view_builds: usize,
    /// The overlay's rows as last drawn, with the minute of `now_unix` they were built in and
    /// the width they were built for. A timer frame reuses them within that minute at that width
    /// and repaints only the spinners.
    overlay_view: Option<(table::TableView, i64, u16)>,
    /// Where the last frame drew `/subagents`' list and preview, which take the wheel apart.
    subagent_panes: Option<(Rect, Rect)>,
    /// How many times `draw_at` rebuilt the overlay's rows; tests check timer frames reuse them.
    overlay_builds: usize,
    /// How many times `draw_at` rebuilt the preview's lines; tests check timer frames reuse them.
    preview_builds: usize,
    /// The preview's transcript lines as last built, keyed by the previewed chat, its stream
    /// generation, and the width. A timer frame reuses them; a preview event drops them.
    preview_view: Option<((uuid::Uuid, u64, u16), View)>,
    /// The highlighted text, kept after release until the next press or key.
    selection: Option<Selection>,
    drag: Option<Drag>,
    /// Where the mouse pointer last moved, for lighting up the link under it.
    pointer: Option<(u16, u16)>,
    /// The rows the last rebuild of `view` added above the lines drawn before it, from an
    /// older page of history; 0 when it added none.
    prepended_rows: usize,
    /// The message `Effect::ScrollToMessage` asked for, put at the top once a draw shows it.
    jump_to: Option<i64>,
    /// Counts the changes that can alter the transcript lines: every message to the core,
    /// and every block toggled. A draw rebuilds `view` only when this moved since the build,
    /// so a key that only edits the composer or scrolls reuses the lines.
    view_revision: u64,
    /// `view_revision` when `view` was built.
    view_built_revision: u64,
    /// Where the question menu covers the transcript, as last drawn, so the mouse cannot
    /// reach the rows under it.
    menu_area: Option<Rect>,
    /// The resolved paths this draft's `@path` mentions were already attached from, or left
    /// out for, so a second send sends them instead of attaching them again.
    mentioned: HashSet<String>,
    /// The dot-named path parts, such as `.env`, in text pasted into this draft. A mention of
    /// a dotfile attaches only when its dot was typed, not pasted.
    pasted_dots: HashSet<String>,
    /// `mentioned` and `pasted_dots` of the draft last handed to the core. A refusal that
    /// gives the text back and keeps its chips, now or once its uploads finish, puts them
    /// back, so a resend attaches none of those mentions again.
    sent_mentions: HashSet<String>,
    sent_pasted_dots: HashSet<String>,
    /// The command whose argument the slash menu was completing after the last key or paste,
    /// so entering an argument menu asks the core for its list once.
    arguments_for: Option<&'static str>,
}

/// The marker before a pinned chat in `/chats`: `chats.pin_icon` when `config` sets it, else
/// `set`'s pin.
fn pin_icon_for(config: &LocalConfig, set: IconSet) -> &str {
    match config.chats.pin_icon.as_deref() {
        Some(icon) => icon,
        None => icons::slot(set, Icon::Pin).text,
    }
}

impl Tui {
    pub fn new(
        config: LocalConfig,
        config_path: Option<PathBuf>,
        theme: Theme,
        welcome: Welcome,
        seed: u64,
    ) -> Tui {
        // The environment's answer arrives through `set_icon_env`, so until then the file decides.
        let theme = Theme {
            icons: IconSet::resolve(config.icons, None),
            ..theme
        };
        let spinner = SpinnerStyle::pick(config.spinner, seed);
        let mut composer = Composer::new(config.composer_max_lines);
        composer.set_gutter_style(theme.dim);
        let mut core = App::new(config.busy_behavior, config.mouse);
        core.saved_org = config.organization;
        core.saved_efforts = config.efforts.clone();
        core.cost_in_footer = config.statusline.fields.contains(&StatusField::Cost);
        let statusline = config.statusline.clone();
        let epoch = Instant::now();
        Tui {
            core,
            composer,
            picker: None,
            overlay: None,
            last_copied: None,
            last_paged: None,
            config,
            config_path,
            statusline,
            theme,
            icon_env: None,
            welcome,
            nerd_tip_state: None,
            toggles: HashSet::new(),
            view: View::default(),
            area: Rect::default(),
            screen_width: 0,
            scroll_from_bottom: 0,
            last_ctrl_c: None,
            show_help: false,
            help_scroll: 0,
            clipboard: None,
            active_notice: None,
            notices_seen: 0,
            unshown_notices: VecDeque::new(),
            startup_notices: None,
            copied: None,
            needs_full_redraw: false,
            keyboard_enhanced: true,
            fatal: None,
            shown_title: None,
            editor_requested: false,
            epoch,
            epoch_unix: now_unix(),
            limits_due: epoch,
            cost_due: false,
            spinner,
            seed,
            reuse_view: false,
            view_width: 0,
            view_resets: 0,
            view_builds: 0,
            overlay_view: None,
            subagent_panes: None,
            overlay_builds: 0,
            preview_builds: 0,
            preview_view: None,
            selection: None,
            drag: None,
            pointer: None,
            prepended_rows: 0,
            jump_to: None,
            view_revision: 0,
            view_built_revision: 0,
            menu_area: None,
            mentioned: HashSet::new(),
            pasted_dots: HashSet::new(),
            sent_mentions: HashSet::new(),
            sent_pasted_dots: HashSet::new(),
            arguments_for: None,
        }
    }

    /// Records whether the terminal reports modified Enter keys distinctly, which decides the
    /// composer's send and newline keys and its hint.
    pub fn set_keyboard_enhanced(&mut self, enhanced: bool) {
        self.keyboard_enhanced = enhanced;
        self.composer.set_enhanced(enhanced);
        self.refresh_placeholder();
    }

    /// Records what `NERD_FONT` asks for, which `main` reads once at startup, and draws with
    /// the icons it picks while `config.toml` sets none. Tests pass a value instead of reading
    /// the environment.
    pub fn set_icon_env(&mut self, env: Option<IconSet>) {
        self.icon_env = env;
        self.apply_icons();
    }

    /// Records the state file that remembers the Nerd Font tip once a draw shows it, which
    /// `main` passes only while the tip was never shown. Tests pass a temporary path.
    pub fn remember_nerd_font_tip_in(&mut self, path: PathBuf) {
        self.nerd_tip_state = Some(path);
    }

    /// Records the home directory, which `main` reads once, and the save directory it gives
    /// with `files.save_dir`. Tests pass a temporary directory.
    pub fn set_home(&mut self, home: Option<PathBuf>) {
        self.core.save_dir =
            config::save_dir(&self.config.files, home.as_deref(), std::path::Path::is_dir);
        self.core.home = home;
    }

    /// Draws with the icons `config.toml` and `NERD_FONT` pick, rebuilding the lines and rows
    /// that show them.
    fn apply_icons(&mut self) {
        let icons = IconSet::resolve(self.config.icons, self.icon_env);
        if self.theme.icons != icons {
            self.theme.icons = icons;
            self.view_revision += 1;
            self.overlay_view = None;
            self.preview_view = None;
        }
    }

    /// The marker before a pinned chat in `/chats`: `chats.pin_icon` when the file sets it,
    /// else the icon set's pin.
    fn pin_icon(&self) -> &str {
        pin_icon_for(&self.config, self.theme.icons)
    }

    /// Picks the composer hint from the send preference and the keyboard enhancement state.
    fn refresh_placeholder(&mut self) {
        let text = placeholder_text(
            self.core.prefs.send_shortcut,
            self.keyboard_enhanced,
            self.send_now_key(),
        );
        if self.composer.widget().placeholder_text() != text {
            self.composer.set_placeholder(&text);
        }
    }

    /// The reason the app must quit with an error, if any. Resets it.
    pub fn take_fatal(&mut self) -> Option<String> {
        self.fatal.take()
    }

    /// Whether the app must quit now, for example because the terminal could not be restored.
    pub fn must_quit(&self) -> bool {
        self.fatal.is_some()
    }

    /// Takes the terminal back after the main loop paged `text`. `handed` is what became of
    /// the pager and `resumed` the terminal's outcome. The pager drew over the screen and may
    /// have set its own window title, so the next draw repaints both; a terminal that could
    /// not be restored makes the app quit.
    pub fn after_handoff(&mut self, text: &str, handed: Handed, resumed: std::io::Result<()>) {
        self.last_paged = Some(text.to_owned());
        self.needs_full_redraw = true;
        self.shown_title = None;
        match handed {
            Handed::NotHandedOver(e) => self.notice(Notice::Error(format!(
                "Could not hand the terminal to the pager: {e}"
            ))),
            Handed::Ran(Err(e)) => self.notice(Notice::Error(format!("The pager failed: {e}"))),
            Handed::Ran(Ok(())) => {}
        }
        if let Err(e) = resumed {
            self.restore_failed("pager", e);
        }
    }

    /// Whether Ctrl+G asked for the editor since the last call. Resets the request.
    pub fn take_editor_request(&mut self) -> bool {
        std::mem::take(&mut self.editor_requested)
    }

    /// The window title for the open chat: `scuttle · <title>`, or `scuttle` on a blank,
    /// loading, or untitled chat. Whitespace runs become one space and other control
    /// characters go, so a title cannot break out of the terminal's title sequence.
    pub fn window_title(&self) -> String {
        let title = self
            .core
            .chat
            .as_ref()
            .and_then(|c| c.title.as_deref())
            .unwrap_or_default();
        let title: String = title
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .filter(|c| !c.is_control())
            .take(TITLE_MAX)
            .collect();
        if title.is_empty() {
            "scuttle".into()
        } else {
            format!("scuttle · {title}")
        }
    }

    /// The window title when it changed since the last call, for the main loop to set.
    pub fn take_title(&mut self) -> Option<String> {
        let title = self.window_title();
        if self.shown_title.as_deref() == Some(title.as_str()) {
            return None;
        }
        self.shown_title = Some(title.clone());
        Some(title)
    }

    /// Makes the app quit because the terminal could not be restored after `program` had it.
    fn restore_failed(&mut self, program: &str, e: std::io::Error) {
        self.fatal = Some(format!(
            "could not restore the terminal after the {program}: {e}"
        ));
    }

    /// Feeds a message to the core and keeps UI state consistent with the result.
    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        let open = self.core.chat_id;
        let turns = self.core.turns_ended();
        let org = self.core.current_org();
        self.view_revision += 1;
        // The menu depends only on the skills, the user, and the open chat's record, so it is
        // rebuilt after the messages that change them, or when the open chat changes, rather
        // than after every stream delta.
        let menu_changes = changes_menu(&msg);
        if matches!(msg, Msg::ForPreview { .. }) {
            self.preview_view = None;
        }
        let idle = self.core.activity().is_none();
        let mut effects = self.core.update(msg);
        // A turn starts when the agent goes from idle to working, which is when a random
        // spinner draws its style.
        if idle && self.core.activity().is_some() {
            self.seed = next_seed(self.seed);
            self.spinner = SpinnerStyle::pick(self.config.spinner, self.seed);
        }
        // The preview lives only as long as the `/subagents` popup. A chat switch makes the
        // popup's list stale, and another overlay replaces it once its effect is applied.
        if matches!(self.overlay, Some(Overlay::Subagents(_))) {
            let replaced = effects.iter().any(|e| match e {
                Effect::ShowChats(_) => true,
                // A table for a held draft waits while this popup is open.
                Effect::ShowPicker(Picker::Model) if self.core.holds_draft_for_model() => false,
                Effect::ShowPicker(kind) => Overlay::is_table(*kind),
                _ => false,
            });
            if self.core.chat_id != open {
                self.overlay = None;
            }
            if replaced || self.core.chat_id != open {
                effects.extend(self.core.update(Msg::PreviewChat(None)));
            }
        }
        if self.welcome.user.is_empty()
            && let Some(me) = self.core.me.as_ref()
        {
            self.welcome.user = me.username.clone();
        }
        if menu_changes || self.core.chat_id != open {
            self.composer.set_menu(self.core.slash_menu());
        }
        self.prune_live_toggles();
        self.refresh_placeholder();
        // A finished turn spent money and may have used credits, and the quota belongs to an
        // organization, so the limits refresh at the next wakeup instead of waiting out the
        // minute.
        if self.core.turns_ended() != turns {
            self.limits_due = self.epoch;
            self.cost_due = true;
        }
        if self.core.current_org() != org {
            self.limits_due = self.epoch;
        }
        if self.core.chat_id != open {
            self.cost_due = true;
        }
        // A list an open argument menu completes from may have loaded, changed, or moved to
        // another organization.
        self.sync_arguments();
        effects
    }

    /// Hands the composer the argument entries for the command its text is completing, so the
    /// open menu shows the list as the core holds it now.
    fn sync_arguments(&mut self) {
        let text = self.composer.text();
        if let Some(query) = skills::argument_query(&text) {
            self.composer
                .set_arguments(query.command, self.core.argument_menu(query.command));
        }
    }

    /// Follows the composer into and out of an argument menu after a key or a paste. Entering
    /// one asks the core for its list once, which fetches a list that failed to load.
    fn follow_arguments(&mut self) -> Vec<Effect> {
        let command = skills::argument_query(&self.composer.text()).map(|q| q.command);
        let entered = command.filter(|c| self.arguments_for != Some(*c));
        self.arguments_for = command;
        let effects = match entered {
            Some(command) => self.update(Msg::ArgumentsWanted { command }),
            None => vec![],
        };
        self.sync_arguments();
        effects
    }

    /// Live-turn block ids are reused by the next turn, so expansions must not carry over.
    fn prune_live_toggles(&mut self) {
        if self.core.transcript.live.is_empty() {
            self.toggles.retain(|(owner, _)| owner.is_some());
        }
    }

    fn notice(&mut self, n: Notice) {
        self.core.notices.push(n);
    }

    /// Makes the newest notice pushed since the last call active, and expires an old one. Once
    /// no notice is active, the oldest notice that was never shown takes its turn, unless it
    /// waited `NOTICE_STALE`; a startup notice or an error never goes stale, and an error a
    /// newer notice displaces takes the next turn. An error matching the active notice or one
    /// still waiting is dropped, so a failure repeated per keystroke shows once.
    pub fn sync_notice(&mut self, now: Instant) {
        let count = self.core.notices.len();
        let startup = *self.startup_notices.get_or_insert(count);
        // An error repeating the active one or one still waiting would only show it again.
        let notices = &self.core.notices;
        let mut held: Vec<usize> = self
            .active_notice
            .iter()
            .chain(&self.unshown_notices)
            .map(|&(i, _)| i)
            .collect();
        let mut fresh = Vec::new();
        for i in self.notices_seen..count {
            let repeat = matches!(notices.get(i), Some(Notice::Error(_)))
                && held.iter().any(|&j| notices.get(j) == notices.get(i));
            if !repeat {
                held.push(i);
                fresh.push(i);
            }
        }
        if !fresh.is_empty() {
            if let Some((i, since)) = self.active_notice
                && matches!(self.core.notices.get(i), Some(Notice::Error(_)))
            {
                self.unshown_notices.push_front((i, since));
            }
            self.unshown_notices
                .extend(fresh.into_iter().map(|i| (i, now)));
            self.active_notice = self.unshown_notices.pop_back().map(|(i, _)| (i, now));
        }
        self.notices_seen = count;
        if self
            .active_notice
            .is_some_and(|(_, since)| now.duration_since(since) >= NOTICE_TTL)
        {
            self.active_notice = None;
        }
        let notices = &self.core.notices;
        self.unshown_notices.retain(|&(i, arrived)| {
            keeps_its_turn(notices, startup, i) || now.duration_since(arrived) < NOTICE_STALE
        });
        if self.active_notice.is_none() {
            self.active_notice = self.unshown_notices.pop_front().map(|(i, _)| (i, now));
        }
    }

    /// The notice the footer shows instead of the status line, if any.
    pub fn active_notice(&self) -> Option<&Notice> {
        self.active_notice
            .and_then(|(i, _)| self.core.notices.get(i))
    }

    /// When the active notice or the copy notice expires, so the loop can redraw then.
    pub fn notice_deadline(&self) -> Option<Instant> {
        let notice = self.active_notice.map(|(_, since)| since + NOTICE_TTL);
        let copied = self
            .copied
            .as_ref()
            .and_then(|c| c.since)
            .map(|since| since + COPY_TTL);
        notice.into_iter().chain(copied).min()
    }

    /// Whether the next draw must repaint every cell. Resets the request.
    pub fn take_full_redraw(&mut self) -> bool {
        std::mem::take(&mut self.needs_full_redraw)
    }

    /// When the next spinner frame or relative-time change is due, or `None` while nothing
    /// animates.
    pub fn animation_deadline(&self, now: Instant) -> Option<Instant> {
        let animating = self.core.activity().is_some()
            || self
                .core
                .chips
                .iter()
                .any(|c| c.state == ChipState::Uploading)
            || self
                .overlay
                .as_ref()
                .is_some_and(|o| o.animates(&self.core));
        let spin = animating.then(|| now + SPINNER_INTERVAL);
        let minute = self
            .overlay
            .as_ref()
            .is_some_and(Overlay::shows_times)
            .then(|| self.next_minute(now));
        spin.into_iter().chain(minute).min()
    }

    /// When `draw_at`'s minute next turns after `now`, so relative times move on.
    fn next_minute(&self, now: Instant) -> Instant {
        let secs = now.saturating_duration_since(self.epoch).as_secs();
        let unix = self.unix_at(now);
        self.epoch + Duration::from_secs(secs + (60 - unix.rem_euclid(60)) as u64)
    }

    /// The Unix time at `now`, counted from `epoch` so a draw follows the `now` it is given.
    fn unix_at(&self, now: Instant) -> i64 {
        self.epoch_unix + now.saturating_duration_since(self.epoch).as_secs() as i64
    }

    /// Marks the next draw as a timer wakeup: nothing but the clock changed since the last one.
    pub fn tick(&mut self) {
        self.reuse_view = true;
    }

    /// When the next refresh of the limits or the chat's cost is due, for the main loop's
    /// deadline, or `None` when nothing can refresh: after a `401`, or while neither limit
    /// refreshes and the cost is not due.
    pub fn usage_deadline(&self) -> Option<Instant> {
        if self.core.limits_stopped() {
            None
        } else if self.cost_due {
            Some(self.epoch)
        } else {
            self.core.limits_refresh().then_some(self.limits_due)
        }
    }

    /// Runs the refreshes due at `now`, and nothing before then. The main loop calls this on
    /// every iteration, never while a handoff has the terminal, since the loop runs each
    /// handoff to the end before it continues.
    pub fn poll_usage(&mut self, now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();
        if now >= self.limits_due && self.core.limits_refresh() {
            self.limits_due = now + LIMITS_EVERY;
            effects.extend(self.update(Msg::RefreshLimits));
        }
        if std::mem::take(&mut self.cost_due) {
            effects.extend(self.update(Msg::RefreshCost));
        }
        effects
    }

    /// Copies `text` as the screen shows it: the terminal draws no control character, so
    /// none is copied, whether the text came from the transcript, a code block, `/copy`, or
    /// the composer.
    fn copy(&mut self, text: String) {
        let text = crate::markdown::drawable(&text).into_owned();
        let chars = text.chars().count();
        // Tests have no clipboard, so their copies confirm as a real one would.
        let outcome = self.write_clipboard(text).unwrap_or(CopyOutcome::Copied);
        self.report_copy(outcome, chars);
    }

    /// Copies everything typed in the composer, every line of it, and confirms it in the
    /// composer's rule as any copy does. The draft stays, so it can still be sent.
    fn copy_draft(&mut self) -> Vec<Effect> {
        let text = self.composer.expanded_text();
        if text.trim().is_empty() {
            self.notice(Notice::Info(NOTHING_TO_COPY.into()));
            return vec![];
        }
        self.copy(text);
        vec![]
    }

    /// Puts `text` on the clipboard, opening it on first use. Tests only record the text, so
    /// they return `None`.
    fn write_clipboard(&mut self, text: String) -> Option<CopyOutcome> {
        self.last_copied = Some(text.clone());
        if cfg!(test) {
            return None;
        }
        Some(
            self.clipboard
                .get_or_insert_with(Clipboard::new)
                .copy(&text),
        )
    }

    /// Copies a URL that opened no browser, reporting the copy with `notice`.
    fn copy_url(&mut self, url: &str, notice: impl FnOnce(CopyOutcome) -> Notice) {
        if let Some(outcome) = self.write_clipboard(url.to_owned()) {
            self.notice(notice(outcome));
        }
    }

    /// Confirms a copy of `chars` characters in the composer's rule. A warning goes to the footer
    /// too, and a failure only there.
    fn report_copy(&mut self, outcome: CopyOutcome, chars: usize) {
        match outcome {
            CopyOutcome::Copied => self.copied = Some(CopyNote::new(chars)),
            CopyOutcome::CopiedWithWarning(w) => {
                self.copied = Some(CopyNote::new(chars));
                self.notice(Notice::Info(w));
            }
            CopyOutcome::Failed(why) => self.notice(Notice::Error(format!("Copy failed: {why}"))),
        }
    }

    /// Starts the copy notice's clock on its first draw, and clears it once `COPY_TTL` passed.
    fn sync_copied(&mut self, now: Instant) {
        if let Some(note) = self.copied.as_mut() {
            let since = *note.since.get_or_insert(now);
            if now.saturating_duration_since(since) >= COPY_TTL {
                self.copied = None;
            }
        }
    }

    /// Whether the `/model` table must not open now: an overlay, the help, a picker, or the
    /// one-line editor is open, or the composer holds text the user is typing.
    fn model_pick_waits(&self) -> bool {
        self.overlay.is_some()
            || self.picker.is_some()
            || self.show_help
            || self.core.editor.is_some()
            || !self.composer.text().trim().is_empty()
    }

    /// Handles effects the UI owns. Returns false for effects the runtime should run.
    pub fn apply_ui_effect(&mut self, effect: &Effect) -> bool {
        match effect {
            // The table for a held draft is already open, or waits until nothing else is open
            // and the composer is empty, so it never replaces an overlay or takes typed keys.
            Effect::ShowPicker(Picker::Model)
                if self.core.holds_draft_for_model() && self.model_pick_waits() =>
            {
                if !matches!(self.overlay, Some(Overlay::Model(_))) {
                    let _ = self.core.update(Msg::ModelPickDeferred);
                }
            }
            Effect::ShowPicker(kind) => match Overlay::open(*kind, &self.core) {
                Some(overlay) => self.overlay = Some(overlay),
                None => self.picker = Some(PickerState::open(&self.core)),
            },
            Effect::ShowHelp => {
                self.show_help = true;
                self.help_scroll = 0;
            }
            Effect::ShowChats(query) => {
                self.overlay = Some(Overlay::chats(query.clone(), &self.core));
            }
            Effect::ShowSubagents => {
                // The selection starts where the core started the preview.
                self.overlay = Some(Overlay::Subagents(crate::overlay::SubagentsState {
                    table: crate::table::TableState {
                        selected: self.core.first_subagent().map(table::RowKey::Chat),
                        ..Default::default()
                    },
                    scroll: 0,
                }));
            }
            Effect::ShowQueue => {
                self.overlay = Some(Overlay::Queue(crate::table::TableState::default()));
            }
            // The newest file starts selected by its key, so a file that arrives while the
            // table is open goes above it instead of taking the highlight.
            Effect::ShowFiles => {
                let first = scuttle_core::files::file_rows(
                    self.core.chat.as_deref(),
                    &self.core.transcript,
                )
                .first()
                .map(|f| table::RowKey::File(f.id));
                self.overlay = Some(Overlay::Files(crate::table::TableState {
                    selected: first,
                    ..Default::default()
                }));
            }
            Effect::ScrollToMessage(id) => {
                self.jump_to = Some(*id);
                self.view_revision += 1;
            }
            // The live turn is at the end of the transcript.
            Effect::ScrollToLatest => {
                self.jump_to = None;
                self.drag = None;
                self.scroll_from_bottom = 0;
            }
            Effect::ShowInfo => {
                self.overlay = Some(Overlay::Info(crate::table::TableState::default()));
            }
            Effect::ShowWorkspace => {
                self.overlay = Some(Overlay::WorkspaceDetails(
                    crate::table::TableState::default(),
                ));
            }
            Effect::ShowGit => {
                self.overlay = Some(Overlay::Git(crate::table::TableState::default()));
            }
            Effect::ShowMcp => {
                self.overlay = Some(Overlay::Mcp(crate::table::TableState::default()));
            }
            Effect::ShowStatusline => {
                self.overlay = Some(Overlay::statusline(&self.statusline));
            }
            Effect::ShowUsage => {
                self.overlay = Some(Overlay::Usage(crate::table::TableState::default()));
            }
            Effect::CopyText { text, what } => {
                if let Some(outcome) = self.write_clipboard(text.clone()) {
                    let notice = copy_url_notice(what, text, outcome);
                    self.notice(notice);
                }
            }
            Effect::Copy(CopyTarget::LastMessage) => {
                let text = self
                    .core
                    .transcript
                    .messages()
                    .rev()
                    .find(|m| m.role.as_ref().map(|r| r.as_str()) == Some("assistant"))
                    .map(|m| {
                        m.content
                            .iter()
                            .filter(|p| p.type_.as_ref().map(|t| t.as_str()) == Some("text"))
                            .filter_map(|p| p.text.clone())
                            .collect::<Vec<_>>()
                            .join("\n\n")
                    });
                match text {
                    Some(t) => self.copy(t),
                    None => self.notice(Notice::Error("Nothing to copy yet.".into())),
                }
            }
            Effect::Copy(CopyTarget::CodeBlock(n)) => {
                match n
                    .checked_sub(1)
                    .and_then(|i| self.view.last_code_blocks.get(i))
                    .cloned()
                {
                    Some(code) => self.copy(code),
                    None => self.notice(Notice::Error(format!(
                        "The last message has no code block {n}."
                    ))),
                }
            }
            Effect::SetMouse(enabled) => {
                if !cfg!(test) {
                    let _ = crate::terminal::set_mouse(*enabled);
                }
                if !*enabled {
                    self.end_drag();
                    self.selection = None;
                    self.pointer = None;
                }
                let note = if *enabled {
                    "Mouse capture on. Drag to select and copy; hold Shift (Option in iTerm2 or Terminal.app) for the terminal's own selection."
                } else {
                    "Mouse capture off."
                };
                self.notice(Notice::Info(note.into()));
                // `self.config` follows the file, so `/settings` compares against what it holds.
                // A failed save is the newest notice, so it shows first and the next key cannot
                // drop it while it waits behind the note.
                if let Some(path) = self.config_path.as_ref() {
                    match config::set_mouse(path, *enabled) {
                        Ok(()) => self.config.mouse = *enabled,
                        Err(e) => self.notice(Notice::Error(e.to_string())),
                    }
                }
            }
            Effect::SaveOrganization(id) => {
                if let Some(path) = self.config_path.as_ref() {
                    match config::set_organization(path, *id) {
                        Ok(()) => self.config.organization = Some(*id),
                        Err(e) => self.notice(Notice::Error(e.to_string())),
                    }
                }
            }
            Effect::SaveEffort { model, effort } => {
                // A failed write leaves the in-memory choice alone; this is a quiet
                // convenience, unlike `SaveOrganization`, so no error is shown, except that
                // a read-only file says so, as every other save does.
                if let Some(path) = self.config_path.as_ref() {
                    match config::set_effort(path, *model, effort) {
                        Ok(()) => {
                            self.config.efforts.insert(*model, effort.clone());
                        }
                        Err(e @ config::ConfigError::ReadOnly) => {
                            self.notice(Notice::Error(e.to_string()));
                        }
                        Err(_) => {}
                    }
                }
            }
            Effect::ClearView => {
                self.toggles.clear();
                self.view_revision += 1;
                self.selection = None;
                self.drag = None;
                self.scroll_from_bottom = 0;
                self.composer.reset_history_position();
                self.mentioned.clear();
                self.pasted_dots.clear();
                self.sent_mentions.clear();
                self.sent_pasted_dots.clear();
                // The reset closed the attached workspace's panels in the core.
                if matches!(
                    self.overlay,
                    Some(Overlay::Git(_) | Overlay::WorkspaceDetails(_))
                ) {
                    self.overlay = None;
                }
            }
            Effect::CopyWebUrl(url) => self.copy_url(url, |o| web_copy_notice(url, o)),
            Effect::CopyLink(url) => self.copy_url(url, |o| link_copy_notice(url, o)),
            Effect::RestoreComposer(text) => {
                // A restored `/workspace ` opens the argument menu without a key, so the next
                // key asks for the list as entering it does.
                self.arguments_for = None;
                // Chips that survived the refusal are the ones its mentions made.
                if !self.core.chips.is_empty() {
                    self.mentioned.extend(self.sent_mentions.drain());
                    self.pasted_dots.extend(self.sent_pasted_dots.drain());
                }
                // Keep anything typed since the failed request, after the restored text.
                let current = self.composer.text();
                // A send that was only a paste gives back no text; its paste is a chip again.
                if text.is_empty() {
                } else if current.trim().is_empty() {
                    self.composer.set_text(text);
                } else {
                    self.composer.set_text(&format!("{text}\n\n{current}"));
                }
                self.sync_arguments();
            }
            _ => return false,
        }
        true
    }

    /// Replaces the composer text with what the editor saved, or leaves it untouched on failure.
    pub fn edit_with(
        &mut self,
        run: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let path = std::env::temp_dir().join(format!("scuttle-{}.md", uuid::Uuid::new_v4()));
        {
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&path)?;
            if let Err(e) = file.write_all(self.composer.text().as_bytes()) {
                let _ = std::fs::remove_file(&path);
                return Err(e);
            }
        }
        let result = run(&path).and_then(|_| std::fs::read_to_string(&path));
        self.needs_full_redraw = true;
        let _ = std::fs::remove_file(&path);
        let text = result?;
        self.composer.set_text(text.trim_end_matches('\n'));
        Ok(())
    }

    /// The first screen row showing a click target that matches `pred`.
    #[cfg(test)]
    pub fn row_of(&self, pred: impl Fn(&HitTarget) -> bool) -> Option<u16> {
        let top = self.top_line();
        let hit = self.view.hits.iter().find(|h| pred(&h.target))?;
        let line = hit.lines.start.max(top);
        (line < top + self.area.height as usize).then(|| self.area.y + (line - top) as u16)
    }

    fn max_scroll(&self) -> usize {
        self.view
            .lines
            .len()
            .saturating_sub(self.area.height as usize)
    }

    /// The transcript line at the top of the screen. A held drag pins it, even past the usual
    /// bottom when the transcript grew taller mid-drag.
    fn top_line(&self) -> usize {
        match self.drag {
            Some(drag) => drag.top,
            None => self.max_scroll().saturating_sub(self.scroll_from_bottom),
        }
    }

    fn scroll_up(&mut self, lines: usize) {
        if let Some(drag) = self.drag.as_mut() {
            drag.top = drag.top.saturating_sub(lines);
            return;
        }
        self.scroll_from_bottom = (self.scroll_from_bottom + lines).min(self.max_scroll());
    }

    fn scroll_down(&mut self, lines: usize) {
        let max = self.max_scroll();
        if let Some(drag) = self.drag.as_mut() {
            if drag.top < max {
                drag.top = (drag.top + lines).min(max);
            }
            return;
        }
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(lines);
    }

    /// Ends any jump to a file's message, waiting here for its draw or in the core for an
    /// older page, since the user is scrolling on their own.
    fn user_scrolled(&mut self) -> Vec<Effect> {
        self.jump_to = None;
        // Only asked while a jump waits, since every message to the core rebuilds the lines.
        if self.core.looking_for_file() {
            self.update(Msg::UserScrolled)
        } else {
            vec![]
        }
    }

    /// Asks for older history with `msg` once the view reaches the top: `Msg::LoadOlder` from
    /// PageUp, or `Msg::ScrolledToTop` from the wheel. The core ignores it when the chat is
    /// whole, a page is already on its way, or, for the wheel, a page just failed.
    fn load_older_at_top(&mut self, msg: Msg) -> Vec<Effect> {
        if self.top_line() == 0 {
            self.update(msg)
        } else {
            vec![]
        }
    }

    fn inside(&self, column: u16, row: u16) -> bool {
        row >= self.area.y
            && row < self.area.y + self.area.height
            && column >= self.area.x
            && column < self.area.x + self.area.width
    }

    /// The transcript cell under the pointer, clamped into the transcript. During a drag it
    /// maps through the transcript area as it was at the press.
    fn pos_at(&self, column: u16, row: u16) -> Pos {
        let area = self.drag.map_or(self.area, |d| d.area);
        let bottom = area.y + area.height.saturating_sub(1);
        let right = area.x + area.width.saturating_sub(1);
        let row = row.clamp(area.y, bottom);
        Pos {
            line: self.top_line() + (row - area.y) as usize,
            col: column.clamp(area.x, right) - area.x,
        }
    }

    /// Acts on a click on transcript row `line`: copies a code block, toggles a block, or
    /// saves an attached file.
    fn click(&mut self, line: usize) -> Vec<Effect> {
        let target = self
            .view
            .hits
            .iter()
            .rev()
            .find(|h| h.lines.contains(&line))
            .map(|h| h.target.clone());
        match target {
            Some(HitTarget::CopyCode(code)) => self.copy(code),
            Some(HitTarget::Toggle(id)) => {
                let was_toggled = self.toggles.remove(&id);
                if !was_toggled {
                    self.toggles.insert(id);
                }
                self.view_revision += 1;
            }
            Some(HitTarget::SaveFile(file)) => {
                return self.update(Msg::FileAction(scuttle_core::files::FileAction::Save(file)));
            }
            None => {}
        }
        vec![]
    }

    /// The rows of the link under the pointer, as (transcript line, columns), while mouse
    /// capture is on, nothing covers the transcript, and no button is held. Every row of the
    /// link lights up together, however it wraps, since they share its `LinkHit::link`.
    fn hovered_link(&self) -> Vec<(usize, Range<u16>)> {
        let Some((column, row)) = self.pointer else {
            return vec![];
        };
        if !self.core.mouse
            || self.drag.is_some()
            || self.mouse_covered(column, row)
            || !self.inside(column, row)
        {
            return vec![];
        }
        let pos = self.pos_at(column, row);
        let links = &self.view.links;
        let Some(hit) = links
            .iter()
            .find(|l| l.line == pos.line && l.cols.contains(&pos.col))
        else {
            return vec![];
        };
        links
            .iter()
            .filter(|l| l.link == hit.link)
            .map(|l| (l.line, l.cols.clone()))
            .collect()
    }

    /// The URL of the link under transcript cell `pos`, if any.
    fn link_at(&self, pos: Pos) -> Option<String> {
        self.view
            .links
            .iter()
            .find(|l| l.line == pos.line && l.cols.contains(&pos.col))
            .map(|l| l.url.clone())
    }

    pub fn handle(&mut self, event: Event) -> Vec<Effect> {
        self.handle_at(event, Instant::now(), local_now())
    }

    /// Handles `event` as arriving at `now`, at local time `local`. A paste needs both:
    /// pasting the same text again soon after expands its token, and a snippet's file is
    /// named by the local time it arrived.
    pub fn handle_at(
        &mut self,
        event: Event,
        now: Instant,
        local: crate::composer::PasteTime,
    ) -> Vec<Effect> {
        // Every mouse event says where the pointer is, a press or a drag included, so the
        // hover follows it even where no move is reported.
        if let Event::Mouse(m) = &event {
            self.pointer = Some((m.column, m.row));
        }
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.key(key, now),
            Event::Paste(text) => {
                // The save question holds the keyboard, so a paste reaches nothing it covers,
                // a save-as line open under it included.
                if self.core.save_conflict.is_some() {
                    return vec![];
                }
                if self.core.editor.is_some() {
                    let mut effects = Vec::new();
                    for c in one_line_paste(&text).chars() {
                        effects.extend(self.update(Msg::Edit(Edit::Char(c))));
                    }
                    return effects;
                }
                // An overlay holds the keyboard, so a paste goes to its filter, or nowhere.
                if self.show_help || self.picker.is_some() {
                    return vec![];
                }
                if self.overlay.is_some() {
                    let ctx = ViewCtx {
                        app: &self.core,
                        theme: &self.theme,
                        now_unix: now_unix(),
                        offset: local_offset(),
                        elapsed: self.epoch.elapsed(),
                        width: self.screen_width,
                        // Borrows only the config, so the overlay can be borrowed mutably.
                        pin_icon: pin_icon_for(&self.config, self.theme.icons),
                    };
                    let commit = self
                        .overlay
                        .as_mut()
                        .and_then(|o| o.paste_filter(&one_line_paste(&text), &ctx));
                    return match commit {
                        Some(msg) => self.update(msg),
                        None => vec![],
                    };
                }
                let max = scuttle_core::attachments::MAX_FILE_BYTES;
                if text.len() as u64 > max {
                    self.notice(Notice::Error(format!(
                        "The paste is {} bytes, over the {max}-byte limit for an attachment, so it was not added.",
                        text.len()
                    )));
                    return vec![];
                }
                self.pasted_dots
                    .extend(crate::paths::dot_parts(&text).map(str::to_owned));
                self.composer.paste_at(&text, now, local);
                self.follow_arguments()
            }
            // A move only moves the hover, so it is taken before anything that acts on a press.
            Event::Mouse(m) if m.kind == MouseEventKind::Moved => vec![],
            // The wheel moves an open table overlay as Up and Down do, wherever the pointer
            // is: the selection where rows can be selected, else the panel's scroll. In
            // `/subagents` only the list moves the selection, since each move switches the
            // preview's stream; over the preview the wheel scrolls it. The help box, the
            // slider, and the one-line editor keep ignoring it.
            // The save question covers the overlay under it, so the wheel moves nothing there.
            Event::Mouse(m)
                if matches!(
                    m.kind,
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                ) && self.core.save_conflict.is_some() =>
            {
                vec![]
            }
            Event::Mouse(m)
                if matches!(
                    m.kind,
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                ) && self.overlay.is_some()
                    && !self.show_help
                    && self.picker.is_none()
                    && self.core.editor.is_none() =>
            {
                let up = m.kind == MouseEventKind::ScrollUp;
                if let Some(Overlay::Subagents(s)) = self.overlay.as_mut() {
                    let at = |r: Rect| r.contains(Position::new(m.column, m.row));
                    match self.subagent_panes {
                        Some((list, _)) if at(list) => {}
                        // The next draw clamps it to what the preview holds.
                        Some((_, preview)) if at(preview) => {
                            s.scroll = if up {
                                s.scroll + 3
                            } else {
                                s.scroll.saturating_sub(3)
                            };
                            return vec![];
                        }
                        _ => return vec![],
                    }
                }
                let code = if up { KeyCode::Up } else { KeyCode::Down };
                self.overlay_key(KeyEvent::new(code, KeyModifiers::NONE))
            }
            // The question menu takes the wheel as it takes Up and Down.
            Event::Mouse(m)
                if matches!(
                    m.kind,
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                ) && !self.overlay_showing()
                    && self.core.question_menu().is_some()
                    && self
                        .menu_area
                        .is_some_and(|a| a.contains(Position::new(m.column, m.row))) =>
            {
                let key = if m.kind == MouseEventKind::ScrollUp {
                    QuestionKey::Up
                } else {
                    QuestionKey::Down
                };
                self.update(Msg::QuestionKey(key))
            }
            // A held drag still gets its moves and release, wherever the pointer goes.
            Event::Mouse(m) if self.drag.is_none() && self.mouse_covered(m.column, m.row) => {
                vec![]
            }
            Event::Mouse(m) => {
                let scrolled_up = m.kind == MouseEventKind::ScrollUp;
                let mut effects = if matches!(
                    m.kind,
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                ) {
                    self.user_scrolled()
                } else {
                    vec![]
                };
                effects.extend(self.mouse(m));
                if scrolled_up {
                    effects.extend(self.load_older_at_top(Msg::ScrolledToTop));
                }
                effects
            }
            _ => vec![],
        }
    }

    fn mouse(&mut self, m: MouseEvent) -> Vec<Effect> {
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.selection = None;
                self.end_drag();
                if self.core.mouse && !self.overlay_showing() && self.inside(m.column, m.row) {
                    self.drag = Some(Drag {
                        anchor: self.pos_at(m.column, m.row),
                        moved: false,
                        top: self.top_line(),
                        area: self.area,
                    });
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => self.drag_to(m.column, m.row),
            MouseEventKind::Up(MouseButton::Left) => {
                self.extend_to(m.column, m.row);
                let Some(drag) = self.drag else {
                    return vec![];
                };
                let mut effects = vec![];
                if !drag.moved {
                    // The view is still pinned at the press, so this is the link that was
                    // under the pointer then, even if the agent streamed more since.
                    match self.link_at(drag.anchor) {
                        Some(url) => effects.push(Effect::OpenLink(url)),
                        None => effects.extend(self.click(drag.anchor.line)),
                    }
                } else if let Some(selection) = self.selection {
                    let text = selected_text(&self.view, &selection);
                    if !text.is_empty() {
                        self.copy(text);
                    }
                }
                self.end_drag();
                return effects;
            }
            MouseEventKind::ScrollUp => self.scroll_up(3),
            MouseEventKind::ScrollDown => self.scroll_down(3),
            _ => {}
        }
        vec![]
    }

    /// Extends the selection to the pointer, scrolling a line when it leaves the transcript.
    fn drag_to(&mut self, column: u16, row: u16) {
        let Some(drag) = self.drag else {
            return;
        };
        if row < drag.area.y {
            self.scroll_up(1);
        } else if row >= drag.area.y + drag.area.height {
            self.scroll_down(1);
        }
        self.extend_to(column, row);
    }

    fn extend_to(&mut self, column: u16, row: u16) {
        let head = self.pos_at(column, row);
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        drag.moved |= head != drag.anchor;
        if drag.moved {
            self.selection = Some(Selection {
                anchor: drag.anchor,
                head,
            });
        }
    }

    /// Keeps the reader's place after a rebuild replaced `drawn`, the view last drawn with
    /// `top_drawn` at the top. While scrolled up or holding a drag, the text at the top stays
    /// on the same screen row whatever was added or removed above or below it: an older page,
    /// an answer rule, a tool result, a block toggled open or shut, the live turn being
    /// persisted, or a rewrap at a new width. A reader at the bottom keeps following the
    /// stream. `prepended` is the rows an older page added, when one came in.
    fn keep_top(&mut self, drawn: &View, top_drawn: usize, prepended: Option<usize>) {
        if self.drag.is_none() && self.scroll_from_bottom == 0 {
            return;
        }
        let top = transcript_view::relocate(drawn, &self.view, top_drawn)
            .unwrap_or(top_drawn + prepended.unwrap_or(0));
        match self.drag.as_mut() {
            Some(drag) => drag.top = top,
            None => self.scroll_from_bottom = self.max_scroll().saturating_sub(top),
        }
    }

    /// Moves a selection and a held drag's anchor with their text after a rebuild at the same
    /// width replaced `drawn`, while the rows they cover only moved; when the rebuild changed
    /// them, both end.
    fn keep_marks(&mut self, drawn: &View, prepended: bool) {
        let marked = self
            .selection
            .map(|s| s.bounds())
            .into_iter()
            .flat_map(|(start, end)| [start.line, end.line])
            .chain(self.drag.map(|d| d.anchor.line))
            .fold(None, |span: Option<(usize, usize)>, line| {
                Some(span.map_or((line, line), |(a, b)| (a.min(line), b.max(line))))
            });
        let Some((first, last)) = marked else {
            return;
        };
        let moved = transcript_view::relocate(drawn, &self.view, first)
            .map(|row| row as isize - first as isize);
        // Rows that stayed where they were keep a selection as it was, even while the text
        // under it streams in, unless an older page came in above them.
        if moved == Some(0) && !prepended {
            return;
        }
        // Without an older page, rows whose blocks cannot be followed are checked in place.
        let moved = moved.or((!prepended).then_some(0));
        let moved_only = moved.is_some_and(|by| {
            (first..=last).all(|line| {
                let shifted = line.checked_add_signed(by);
                drawn.lines.get(line).is_some_and(|l| {
                    shifted.and_then(|s| self.view.lines.get(s)) == Some(l)
                        && shifted.and_then(|s| self.view.meta.get(s)) == drawn.meta.get(line)
                })
            })
        });
        let Some(by) = moved.filter(|_| moved_only) else {
            self.selection = None;
            self.end_drag();
            return;
        };
        if let Some(drag) = self.drag.as_mut() {
            drag.anchor.line = drag.anchor.line.saturating_add_signed(by);
        }
        if let Some(selection) = self.selection.as_mut() {
            selection.anchor.line = selection.anchor.line.saturating_add_signed(by);
            selection.head.line = selection.head.line.saturating_add_signed(by);
        }
    }

    /// Lets go of a held drag, handing its pinned top back to the scroll position.
    fn end_drag(&mut self) {
        if let Some(drag) = self.drag.take() {
            let max = self.max_scroll();
            self.scroll_from_bottom = max - drag.top.min(max);
        }
    }

    /// Whether the help box, the slider, a table overlay, or the slash menu covers the
    /// transcript.
    fn overlay_showing(&self) -> bool {
        self.show_help
            || self.picker.is_some()
            || self.overlay.is_some()
            || self.core.save_conflict.is_some()
            || !self.composer.slash_matches().is_empty()
    }

    /// Whether the mouse at `column` and `row` is over something drawn on the transcript, so
    /// its wheel ticks and clicks must not reach the rows underneath: anywhere while an
    /// overlay holds the keyboard, or inside the question menu.
    fn mouse_covered(&self, column: u16, row: u16) -> bool {
        self.overlay_showing()
            || self.menu_area.is_some_and(|a| {
                (a.x..a.x + a.width).contains(&column) && (a.y..a.y + a.height).contains(&row)
            })
    }

    /// The hint under the transcript while no question menu shows: questions Esc hid, a
    /// message held for a model pick, or a plan to implement. Only the held message shows
    /// while the composer holds text. Without keyboard enhancement, Ctrl+Enter
    /// arrives as Enter, so only `/implement` is offered.
    fn hint(&self) -> Option<String> {
        if self.core.question_menu().is_some() {
            return None;
        }
        let empty = self.composer.text().is_empty();
        if empty && self.core.questions_hidden() {
            Some("Questions hidden: Tab shows them".to_owned())
        } else if self.core.holds_draft_for_model() {
            // Shown while typing too, since a refusal then leaves the picker closed. While
            // the send key sends a queued message instead, the row names both.
            Some(if self.sends_now() {
                format!(
                    "Held message: /model to send it. {} {QUEUED_HINT}",
                    self.send_now_key()
                )
            } else {
                "Held message: pick a model with /model to send it.".to_owned()
            })
        } else if !empty || !self.core.plan_ready() {
            None
        } else if self.keyboard_enhanced {
            Some("Implement the plan: Ctrl+Enter or /implement".to_owned())
        } else {
            Some("Implement the plan: /implement".to_owned())
        }
    }

    /// The key that sends the first queued message now, as the queued hint, `/help`, and the
    /// composer placeholder name it, with the macOS key symbols on macOS.
    fn send_now_key(&self) -> &'static str {
        send_now_label(
            self.core.prefs.send_shortcut,
            self.keyboard_enhanced,
            cfg!(target_os = "macos"),
        )
    }

    /// Whether the send key sends the first queued message now: messages wait, the composer
    /// holds only whitespace, nothing is attached, and no question menu, overlay, save
    /// question, picker, or help takes the key. Both the queued hint and `key` ask this, so the hint shows exactly
    /// when the key works.
    fn sends_now(&self) -> bool {
        self.overlay.is_none()
            && self.core.save_conflict.is_none()
            && self.picker.is_none()
            && !self.show_help
            && !self.core.transcript.queued.is_empty()
            && self.composer.text().trim().is_empty()
            && self.core.chips.is_empty()
            && self.core.question_menu().is_none()
    }

    /// The dim hint under the transcript while the send key would send the first queued
    /// message now.
    fn queued_hint(&self) -> Option<String> {
        self.sends_now()
            .then(|| format!("{} {QUEUED_HINT}", self.send_now_key()))
    }

    fn key(&mut self, key: KeyEvent, now: Instant) -> Vec<Effect> {
        // An error that just appeared stays until it has been up long enough to read.
        let notices = &self.core.notices;
        if self.active_notice.is_none_or(|(i, since)| {
            !matches!(notices.get(i), Some(Notice::Error(_)))
                || now.duration_since(since) >= ERROR_MIN_SHOWN
        }) {
            self.active_notice = None;
        }
        // The user is acting now, so notices still waiting their turn are about the past,
        // except the startup notices, which arrived before the user saw anything, and errors.
        let startup = self.startup_notices.unwrap_or(0);
        let notices = &self.core.notices;
        self.unshown_notices
            .retain(|&(i, _)| keeps_its_turn(notices, startup, i));
        self.copied = None;
        self.selection = None;
        self.end_drag();
        // Handled before the overlays, so a double Ctrl+C quits from anywhere.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            let now = Instant::now();
            if self
                .last_ctrl_c
                .is_some_and(|t| now.duration_since(t) < Duration::from_secs(2))
            {
                return vec![Effect::Quit];
            }
            self.last_ctrl_c = Some(now);
            self.show_help = false;
            self.picker = None;
            let closing = self.overlay.as_ref().and_then(Overlay::close_msg);
            self.overlay = None;
            // Cancelling is harmless while no editor is open: `Edit::Cancel` then finds
            // `core.editor` already `None` and returns no effects.
            self.update(Msg::Edit(Edit::Cancel));
            self.update(Msg::ConflictAnswer(ConflictChoice::Cancel));
            self.notice(Notice::Info("Press Ctrl+C again to quit.".into()));
            return match closing {
                Some(msg) => self.update(msg),
                None => vec![],
            };
        }
        if self.show_help {
            let page = self.area.height.saturating_sub(2).max(1);
            match key.code {
                KeyCode::Up => self.help_scroll = self.help_scroll.saturating_sub(1),
                KeyCode::Down => self.help_scroll = self.help_scroll.saturating_add(1),
                KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(page),
                KeyCode::PageDown => self.help_scroll = self.help_scroll.saturating_add(page),
                _ => self.show_help = false,
            }
            return vec![];
        }
        // The save question holds the keyboard until a choice, even over a save-as line that
        // was open when it arrived. Only a letter answers it: most terminals report a held key
        // as repeated presses, so a held Enter that asked the question would otherwise answer
        // it, and ask again.
        if self.core.save_conflict.is_some() {
            // A second guard, for terminals that do report a repeat.
            if key.kind == KeyEventKind::Repeat {
                return vec![];
            }
            let plain = !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
            let choice = match key.code {
                KeyCode::Char('k' | 'K') if plain => ConflictChoice::KeepBoth,
                KeyCode::Char('r' | 'R') if plain => ConflictChoice::Replace,
                KeyCode::Char('c' | 'C') if plain => ConflictChoice::Cancel,
                KeyCode::Esc => ConflictChoice::Cancel,
                _ => return vec![],
            };
            return self.update(Msg::ConflictAnswer(choice));
        }
        // The one-line editor takes every key it uses, even over an open overlay.
        if self.core.editor.is_some() {
            return match edit_key(key) {
                Some(edit) => self.update(Msg::Edit(edit)),
                None => vec![],
            };
        }
        if let Some(picker) = self.picker.as_mut() {
            let choice = picker.handle_key(key);
            return match choice {
                Some(PickerChoice::Cancel) => {
                    self.picker = None;
                    vec![]
                }
                Some(PickerChoice::Effort(level)) => {
                    self.picker = None;
                    self.update(Msg::EffortChosen(level))
                }
                None => vec![],
            };
        }
        if self.overlay.is_some() {
            return self.overlay_key(key);
        }
        // Taken before the composer, which would otherwise use Ctrl+R for redo.
        if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.update(Msg::Command(scuttle_core::commands::Command::Chats(None)));
        }
        // No other binding uses Ctrl+O, the composer's text area included. An open overlay
        // covers the draft, so the key reaches it there instead, and it does nothing.
        if key.code == KeyCode::Char('o') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.copy_draft();
        }
        // With nothing typed, the question menu takes its keys, then Ctrl+Enter implements a
        // proposed plan, both ahead of "Send now" below; typing anything leaves the keys to
        // the composer, so a free-form reply and history recall still work.
        if self.composer.text().is_empty() {
            if self.core.question_menu().is_some() {
                let question = match key.code {
                    KeyCode::Up => Some(QuestionKey::Up),
                    KeyCode::Down => Some(QuestionKey::Down),
                    KeyCode::Enter => Some(QuestionKey::Enter),
                    KeyCode::Left => Some(QuestionKey::Back),
                    KeyCode::Esc => Some(QuestionKey::Dismiss),
                    _ => None,
                };
                if let Some(question) = question {
                    return self.update(Msg::QuestionKey(question));
                }
            }
            if key.code == KeyCode::Tab && self.core.questions_hidden() {
                return self.update(Msg::QuestionKey(QuestionKey::Show));
            }
            if key.code == KeyCode::Enter
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && self.core.plan_ready()
            {
                return self.update(Msg::Command(scuttle_core::commands::Command::Implement));
            }
        }
        let page = self.area.height.max(1) as usize;
        match key.code {
            KeyCode::PageUp => {
                let mut effects = self.user_scrolled();
                self.scroll_up(page);
                effects.extend(self.load_older_at_top(Msg::LoadOlder));
                return effects;
            }
            KeyCode::PageDown => {
                let effects = self.user_scrolled();
                self.scroll_down(page);
                return effects;
            }
            // End moves to the end of the line being typed; once the cursor is there, as on
            // an empty composer, it jumps to the latest message. Ctrl+End and Shift+End stay
            // with the composer.
            KeyCode::End if key.modifiers.is_empty() && self.composer.at_line_end() => {
                let effects = self.user_scrolled();
                self.scroll_from_bottom = 0;
                return effects;
            }
            _ => {}
        }
        // The send key on an empty composer is "Send now" for the first queued message, but
        // never while attachments wait for a message to carry them.
        if is_send_key(key, self.core.prefs.send_shortcut, self.keyboard_enhanced)
            && self.composer.text().trim().is_empty()
        {
            if !self.core.chips.is_empty() {
                self.notice(Notice::Info(
                    "Type a message to send the attachments.".into(),
                ));
                return vec![];
            }
            if self.sends_now() {
                return self.update(Msg::QueueAction(QueueAction::PromoteFirst));
            }
        }
        if key.code == KeyCode::Backspace
            && self.composer.text().is_empty()
            && !self.core.chips.is_empty()
        {
            return self.update(Msg::RemoveLastChip);
        }
        let action = self.composer.handle_key(key, self.core.prefs.send_shortcut);
        // The key may have entered or left an argument menu, such as `/workspace `.
        let mut effects = self.follow_arguments();
        effects.extend(match action {
            ComposerAction::Submit(text) => {
                self.scroll_from_bottom = 0;
                self.submit(text)
            }
            // Esc leaves an idle subagent for its parent, but only with nothing typed or
            // attached, so it never discards a draft or skips interrupting a running turn.
            ComposerAction::Interrupt
                if self.composer.text().trim().is_empty()
                    && self.core.chips.is_empty()
                    && self.core.can_return_to_parent() =>
            {
                self.update(Msg::Command(scuttle_core::commands::Command::Parent))
            }
            ComposerAction::Interrupt => self.update(Msg::Interrupt),
            ComposerAction::OpenEditor => {
                self.editor_requested = true;
                vec![]
            }
            ComposerAction::CompletePath(partial) => {
                if let Some(done) = crate::paths::complete(&partial, home().as_deref()) {
                    self.composer.replace_last_word(&format!("@{done}"));
                }
                vec![]
            }
            ComposerAction::BrokenPaste(token) => {
                self.notice(Notice::Error(broken_paste(&token)));
                vec![]
            }
            ComposerAction::None => vec![],
        });
        effects
    }

    /// Draws the subagent preview, newest lines at the bottom, `scroll` lines up from there,
    /// and returns how far it can scroll. `open_selected` says the open chat's own row is
    /// selected, which the preview never streams. A timer frame (`ticked`) reuses the lines
    /// built for the same chat, stream, and width.
    fn draw_preview(
        &mut self,
        f: &mut Frame,
        area: Rect,
        scroll: usize,
        open_selected: bool,
        ticked: bool,
    ) -> usize {
        f.render_widget(Clear, area);
        let block = Block::default().borders(Borders::ALL).title(" Preview ");
        let inner = block.inner(area);
        f.render_widget(block, area);
        if open_selected {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "This is the open chat.",
                    self.theme.dim,
                ))),
                inner,
            );
            return 0;
        }
        let Some(preview) = self.core.preview.as_ref() else {
            self.preview_view = None;
            return 0;
        };
        let key = (preview.chat, preview.generation(), inner.width);
        let reuse = ticked && self.preview_view.as_ref().is_some_and(|(k, _)| *k == key);
        if !reuse {
            let source = transcript_view::TranscriptSource {
                transcript: &preview.transcript,
                prefs: &self.core.prefs,
                activity: None,
                history: None,
                chats: Some(&self.core.chats),
                saveable: None,
            };
            let view = transcript_view::build_transcript(
                &source,
                &self.config.density,
                &HashSet::new(),
                None,
                &self.theme,
                inner.width,
            );
            self.preview_view = Some((key, view));
            self.preview_builds += 1;
        }
        let Some((_, view)) = self.preview_view.as_ref() else {
            return 0;
        };
        let mut head = Vec::new();
        if let Some(error) = preview.error.as_ref() {
            head.push(Line::from(Span::styled(
                format!("The preview stream failed: {error}. Retrying."),
                self.theme.error,
            )));
        } else if view.lines.is_empty() {
            head.push(Line::from(Span::styled("Connecting…", self.theme.dim)));
        }
        let total = head.len() + view.lines.len();
        let height = inner.height as usize;
        let max = total.saturating_sub(height);
        let top = max - scroll.min(max);
        let shown: Vec<Line> = head
            .iter()
            .chain(view.lines.iter())
            .skip(top)
            .take(height)
            .cloned()
            .collect();
        f.render_widget(Paragraph::new(shown), inner);
        max
    }

    /// Hands a key to the open overlay and applies what it asks for.
    fn overlay_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let ctx = ViewCtx {
            app: &self.core,
            theme: &self.theme,
            now_unix: now_unix(),
            offset: local_offset(),
            elapsed: self.epoch.elapsed(),
            width: self.screen_width,
            // Borrows only the config, so the overlay can be taken mutably below.
            pin_icon: pin_icon_for(&self.config, self.theme.icons),
        };
        let Some(overlay) = self.overlay.as_mut() else {
            return vec![];
        };
        match overlay.handle_key(key, &ctx) {
            OverlayOutcome::Stay => vec![],
            OverlayOutcome::Close => {
                self.overlay = None;
                vec![]
            }
            OverlayOutcome::CloseWith(msg) => {
                self.overlay = None;
                self.update(msg)
            }
            OverlayOutcome::Send(msg) => self.update(msg),
            OverlayOutcome::Statusline(statusline) => self.set_statusline(statusline),
        }
    }

    /// Sends the composer's text. The filesystem is read here rather than in the core: the
    /// `@path` mentions in a message or a skill trigger attach their files first, and
    /// `/attach` gets `~/` expanded. A built-in command's argument is not a message, so it is
    /// not scanned.
    fn submit(&mut self, text: String) -> Vec<Effect> {
        use scuttle_core::commands::{self, Command};
        let trimmed = text.trim();
        if trimmed.starts_with('/') {
            match commands::parse(trimmed) {
                Ok(Command::Attach(path)) => {
                    let path = crate::paths::expand(&path, home().as_deref());
                    let path = path.to_string_lossy().into_owned();
                    return self.update(Msg::Command(Command::Attach(path)));
                }
                // The core sends a skill as a message, so its mentions attach too.
                Err(_)
                    if scuttle_core::skills::rewrite(trimmed, &self.core.slash_menu())
                        .is_some() => {}
                // A command's argument is not a message, so a snippet in it goes in as text.
                _ => {
                    let text = self.composer.expand_tokens(&text);
                    return self.update(Msg::Submit(text));
                }
            }
        }
        if let Some(held) = self.hold_for_mentions(&text) {
            return held;
        }
        // The tokens leave the text, as the web UI leaves a large paste out of the message,
        // and their snippets go as text files the message carries. A draft that is only a
        // paste sends the file alone, as the web UI does.
        let (message, snippets) = self.composer.split_snippets(&text);
        self.sent_mentions = std::mem::take(&mut self.mentioned);
        self.sent_pasted_dots = std::mem::take(&mut self.pasted_dots);
        let mut effects = Vec::new();
        for snippet in snippets {
            effects.extend(self.update(Msg::AttachPaste {
                name: snippet.file_name(),
                text: snippet.text,
            }));
        }
        effects.extend(self.update(Msg::Submit(message)));
        effects
    }

    /// Attaches the files `text` mentions with `@path` that no earlier send of this draft
    /// handled, then puts the text back and holds it for a second send key, so a mention
    /// never sends a file the user has not seen as a chip. The chips upload only at that
    /// second send, so one removed before it never reaches the server. A dotfile whose dot
    /// was pasted rather than typed is left out. `None` when no mention is new, so the text
    /// can go.
    fn hold_for_mentions(&mut self, text: &str) -> Option<Vec<Effect>> {
        let mut effects = Vec::new();
        let mut attached = 0;
        let mut left_out = Vec::new();
        let mut refused = Vec::new();
        for path in crate::paths::at_paths(text, home().as_deref()) {
            if !self.mentioned.insert(path.clone()) {
                continue;
            }
            if crate::paths::dot_parts(&path).any(|part| self.pasted_dots.contains(part)) {
                let name = std::path::Path::new(&path)
                    .file_name()
                    .map_or(path.clone(), |n| n.to_string_lossy().into_owned());
                left_out.push(name);
                continue;
            }
            effects.extend(self.update(Msg::AttachMention(path)));
            // A file of a type Coder refuses becomes a failed chip, not an attachment.
            match self.core.chips.last() {
                Some(chip) if matches!(chip.state, ChipState::Failed(_)) => {
                    refused.push(chip.name.clone());
                }
                _ => attached += 1,
            }
        }
        if attached == 0 && left_out.is_empty() && refused.is_empty() {
            return None;
        }
        self.composer.set_text(text);
        let mut notice = Vec::new();
        match attached {
            0 => {}
            1 => notice.push("Attached 1 file from @ mentions.".to_owned()),
            n => notice.push(format!("Attached {n} files from @ mentions.")),
        }
        if !refused.is_empty() {
            notice.push(format!(
                "Could not attach {}: its chip says why.",
                refused.join(", ")
            ));
        }
        if !left_out.is_empty() {
            notice.push(format!(
                "Left out {}: a dotfile attaches only when you type its leading dot.",
                left_out.join(", ")
            ));
        }
        notice.push(match attached {
            0 => "Send again to send the message without them.".to_owned(),
            1 => "Send again to send it.".to_owned(),
            _ => "Send again to send them.".to_owned(),
        });
        self.notice(Notice::Info(notice.join(" ")));
        Some(effects)
    }

    /// Hands the terminal to `$EDITOR` with the composer's text, and takes the edit back.
    /// The main loop pauses its input around this, so the editor gets every key.
    pub fn open_editor(&mut self) -> Vec<Effect> {
        let mouse = self.core.mouse;
        self.open_editor_with(|path| run_editor(path, mouse))
    }

    /// `open_editor` with `run` in place of the editor. `run` edits the file at its path and
    /// returns how the edit went, then how the terminal came back.
    fn open_editor_with(
        &mut self,
        run: impl FnOnce(&std::path::Path) -> (std::io::Result<()>, std::io::Result<()>),
    ) -> Vec<Effect> {
        let mut resumed = Ok(());
        let result = self.edit_with(|path| {
            let (edited, resume) = run(path);
            resumed = resume;
            edited
        });
        self.set_keyboard_enhanced(crate::terminal::keyboard_enhanced());
        if let Err(e) = result {
            self.notice(Notice::Error(format!("Editor failed: {e}")));
        }
        // The edit may have left the text in an argument menu.
        let mut effects = self.follow_arguments();
        effects.extend(self.finish_editor(resumed));
        effects
    }

    /// Hands the terminal to `$EDITOR` on the config file, then applies what changed. The main
    /// loop pauses its input around this, as it does for Ctrl+G.
    pub fn edit_settings(&mut self) -> Vec<Effect> {
        let mouse = self.core.mouse;
        let mut resumed = Ok(());
        let mut handed = false;
        self.edit_settings_with(|path| {
            handed = true;
            let (edited, resume) = run_editor(path, mouse);
            resumed = resume;
            edited
        });
        if !handed {
            return vec![];
        }
        self.needs_full_redraw = true;
        self.set_keyboard_enhanced(crate::terminal::keyboard_enhanced());
        // The resume restored the capture that was on before; a changed `mouse` takes over.
        if self.core.mouse != mouse {
            let _ = crate::terminal::set_mouse(self.core.mouse);
        }
        self.finish_editor(resumed)
    }

    /// `edit_settings` with `run` in place of the editor: creates the file from the template
    /// when it is missing, runs `run` on it, and applies the file when `run` succeeds.
    fn edit_settings_with(&mut self, run: impl FnOnce(&std::path::Path) -> std::io::Result<()>) {
        let Some(path) = self.config_path.clone() else {
            self.notice(Notice::Error(
                "scuttle has no config file location: set HOME or XDG_CONFIG_HOME.".into(),
            ));
            return;
        };
        if let Err(e) = config::create_if_missing(&path) {
            self.notice(Notice::Error(format!(
                "Could not create the config file: {e}"
            )));
            return;
        }
        if let Err(e) = run(&path) {
            self.notice(Notice::Error(format!(
                "Editor failed: {e}. The settings were not reloaded."
            )));
            return;
        }
        match config::load(&path) {
            Ok(new) => self.apply_settings(new),
            Err(e) => self.notice(Notice::Error(format!(
                "Settings not applied: {e}. The previous settings stay in effect."
            ))),
        }
    }

    /// Makes `statusline` the footer's settings, and asks for the chat's cost at the next
    /// wakeup when the footer starts showing it.
    fn apply_statusline(&mut self, statusline: StatuslineConfig) {
        let shows_cost = statusline.fields.contains(&StatusField::Cost);
        if shows_cost && !self.core.cost_in_footer {
            self.cost_due = true;
        }
        self.core.cost_in_footer = shows_cost;
        self.statusline = statusline;
    }

    /// Applies footer settings from `/statusline` and saves them. A save that fails keeps
    /// them for this session and says why; `self.config` keeps what the file holds.
    fn set_statusline(&mut self, statusline: StatuslineConfig) -> Vec<Effect> {
        self.apply_statusline(statusline.clone());
        if let Some(path) = self.config_path.as_ref() {
            match config::set_statusline(path, &statusline) {
                Ok(()) => self.config.statusline = statusline,
                Err(e) => self.notice(Notice::Error(e.to_string())),
            }
        }
        vec![]
    }

    /// Applies a reloaded config: the keys that can change live take effect now, and the
    /// notice names any other changed key, which waits for a restart.
    fn apply_settings(&mut self, new: LocalConfig) {
        let changed = new != self.config;
        let restart = config::restart_keys(&self.config, &new);
        if new.mouse != self.core.mouse {
            self.core.mouse = new.mouse;
            if !new.mouse {
                self.end_drag();
                self.selection = None;
                self.pointer = None;
            }
        }
        self.core.busy = new.busy_behavior;
        self.composer.set_max_lines(new.composer_max_lines);
        // The footer's fields, their order, and their thresholds apply at once, as
        // `/statusline` changes them.
        self.apply_statusline(new.statusline.clone());
        if new.spinner != self.config.spinner {
            self.seed = next_seed(self.seed);
            self.spinner = SpinnerStyle::pick(new.spinner, self.seed);
        }
        // The density, the pin icon, and the icons are read when the lines and the rows are
        // built.
        self.view_revision += 1;
        self.overlay_view = None;
        self.core.save_dir = config::save_dir(
            &new.files,
            self.core.home.as_deref(),
            std::path::Path::is_dir,
        );
        self.config = new;
        self.apply_icons();
        let note = match (changed, restart.as_slice()) {
            (false, _) => "Settings unchanged.".to_owned(),
            (true, []) => "Settings applied.".to_owned(),
            (true, [one]) => format!("Settings applied. {one} applies on restart."),
            (true, many) => format!("Settings applied. {} apply on restart.", many.join(", ")),
        };
        self.notice(Notice::Info(note));
    }

    /// Quits with an error when the terminal could not be restored after the editor, since
    /// drawing on a terminal in an unknown mode would garble the screen. The editor may have
    /// set its own window title, so the next draw sets scuttle's again.
    fn finish_editor(&mut self, resumed: std::io::Result<()>) -> Vec<Effect> {
        self.shown_title = None;
        match resumed {
            Ok(()) => vec![],
            Err(e) => {
                self.restore_failed("editor", e);
                vec![Effect::Quit]
            }
        }
    }

    pub fn draw(&mut self, f: &mut Frame) {
        self.draw_at(f, Instant::now());
    }

    /// Draws the screen as of `now`, which picks the spinner frame and expires notices.
    pub fn draw_at(&mut self, f: &mut Frame, now: Instant) {
        self.prune_live_toggles();
        self.sync_notice(now);
        self.sync_copied(now);
        self.screen_width = f.area().width;
        let outer = padded(f.area());
        let activity = self.core.activity();
        let activity_height = u16::from(activity.is_some());
        // One row, first match wins: the questions or plan hint, then the dim queued hint, so
        // each comes back when the one above it goes. The copy confirmation is drawn in the
        // composer's rule instead, so a copy never moves the transcript.
        let status_row: Option<(String, Style)> = self
            .hint()
            .map(|h| (h, self.theme.accent))
            .or_else(|| self.queued_hint().map(|h| (h, self.theme.dim)));
        let hint_height = u16::from(status_row.is_some());
        // Drawn on every frame, timer frames included, so an upload's spinner advances.
        let chip_frame = self
            .spinner
            .frame(now.saturating_duration_since(self.epoch));
        // The chips wrap, up to a third of the screen.
        let chip_lines = self.chip_lines(
            chip_frame,
            outer.width,
            usize::from((outer.height / 3).max(1)),
        );
        let chips_height = chip_lines.len() as u16;
        // The text rows and the rules above and below them, never under three rows.
        let composer_height = self.composer.height(outer.width).min(
            outer
                .height
                .saturating_sub(2 + activity_height + chips_height + hint_height)
                .max(3),
        );
        let [
            transcript,
            hint_row,
            activity_row,
            chips_row,
            composer,
            footer,
        ] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(hint_height),
            Constraint::Length(activity_height),
            Constraint::Length(chips_height),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ])
        .areas(outer);
        let top_drawn = self.top_line();
        self.area = transcript;
        // The tip belongs to the blank chat scuttle starts on, so it goes for good once a
        // chat opens.
        if self.welcome.tip && self.core.chat_id.is_some() {
            self.welcome.tip = false;
            self.view_revision += 1;
        }
        // Lines drawn at another width or before a history reset may now hold other text, so
        // a selection or held drag over them no longer means what it did.
        let resets = self.core.transcript.history_resets();
        let stale = self.view_width != transcript.width || self.view_resets != resets;
        let ticked = std::mem::take(&mut self.reuse_view);
        let reuse = !stale && (ticked || self.view_built_revision == self.view_revision);
        if !reuse {
            self.view_built_revision = self.view_revision;
            let drawn = std::mem::replace(
                &mut self.view,
                transcript_view::build(
                    &self.core,
                    &self.config.density,
                    &self.toggles,
                    &self.welcome,
                    &self.theme,
                    transcript.width,
                ),
            );
            self.view_width = transcript.width;
            let drawn_resets = std::mem::replace(&mut self.view_resets, resets);
            self.view_builds += 1;
            self.prepended_rows = 0;
            if !stale {
                let prepended = transcript_view::prepended_rows(&drawn, &self.view);
                self.prepended_rows = prepended.unwrap_or(0);
                self.keep_top(&drawn, top_drawn, prepended);
                self.keep_marks(&drawn, prepended.is_some());
            } else if self.view_resets == drawn_resets {
                // Only the width changed: blocks keep their ids through a rewrap.
                self.keep_top(&drawn, top_drawn, None);
            }
        } else if self.drag.is_none() && self.scroll_from_bottom > 0 {
            // The lines are unchanged, but the transcript may be shorter or taller, as when
            // the composer grows, so the reader keeps the same top line.
            self.scroll_from_bottom = self.max_scroll().saturating_sub(top_drawn);
        }
        if stale {
            self.selection = None;
            self.end_drag();
        }
        self.scroll_from_bottom = self.scroll_from_bottom.min(self.max_scroll());
        // A jump waits for the draw whose lines hold its message, as after an older page.
        if let Some(id) = self.jump_to
            && let Some(&row) = self.view.message_rows.get(&id)
        {
            self.jump_to = None;
            self.drag = None;
            self.scroll_from_bottom = self.max_scroll().saturating_sub(row);
        }
        let top = self.top_line();
        let shown = top..top + transcript.height as usize;
        if let Some(tip) = self.view.nerd_tip.as_ref()
            && tip.start < shown.end
            && shown.start < tip.end
            && let Some(path) = self.nerd_tip_state.take()
        {
            // Best effort: a failed write only shows the tip once more at the next start.
            let _ = scuttle_core::state::record_nerd_font_tip(&path);
        }
        let visible: Vec<Line> = self
            .view
            .lines
            .iter()
            .skip(top)
            .take(transcript.height as usize)
            .cloned()
            .collect();
        f.render_widget(Paragraph::new(visible), transcript);
        // A line's own style only covers its text, so the tint is painted across the row.
        for (row, meta) in self
            .view
            .meta
            .iter()
            .skip(top)
            .take(transcript.height as usize)
            .enumerate()
        {
            if meta.user {
                let line = Rect {
                    y: transcript.y + row as u16,
                    height: 1,
                    ..transcript
                };
                f.buffer_mut().set_style(line, self.theme.user_tint);
            }
        }
        // The markers of blocks in progress show the spinner's frame. Painting the cells keeps
        // a timer frame from rebuilding the lines, as with the tint.
        if !self.view.spinners.is_empty() {
            let frame = self
                .spinner
                .frame(now.saturating_duration_since(self.epoch));
            let buf = f.buffer_mut();
            for &line in &self.view.spinners {
                if let Some(row) = line
                    .checked_sub(top)
                    .filter(|row| *row < transcript.height as usize)
                    && let Some(cell) = buf.cell_mut((transcript.x, transcript.y + row as u16))
                {
                    cell.set_symbol(frame);
                }
            }
        }
        // The link under the pointer is painted over the lines, like the tint, so a move
        // reuses them.
        for (line, cols) in self.hovered_link() {
            if let Some(row) = line
                .checked_sub(top)
                .filter(|row| *row < transcript.height as usize)
            {
                let cells = Rect {
                    x: transcript.x + cols.start,
                    y: transcript.y + row as u16,
                    width: cols.end - cols.start,
                    height: 1,
                };
                f.buffer_mut()
                    .set_style(cells.intersection(transcript), self.theme.link_hover);
            }
        }
        if let Some(selection) = self.selection {
            let shown = self
                .view
                .lines
                .len()
                .saturating_sub(top)
                .min(transcript.height as usize);
            for row in 0..shown {
                let line = top + row;
                if self.view.meta.get(line).is_some_and(|m| m.rule) {
                    continue;
                }
                if let Some((from, to)) = selection.columns(line, transcript.width) {
                    let cells = Rect {
                        x: transcript.x + from,
                        y: transcript.y + row as u16,
                        width: to - from,
                        height: 1,
                    };
                    f.buffer_mut().set_style(cells, self.theme.selection);
                }
            }
        }
        // The rows at the bottom of the transcript that a menu, the picker, or an overlay
        // covers, where a marker says nothing to the reader.
        let mut covered = 0u16;
        if chips_height > 0 {
            f.render_widget(Paragraph::new(chip_lines), chips_row);
        }
        if let Some((text, style)) = status_row {
            f.render_widget(Paragraph::new(Span::styled(text, style)), hint_row);
        }
        // The copy notice below is drawn over the top rule, so it never moves the transcript.
        let frame = Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(self.theme.dim);
        let inner = frame.inner(composer);
        f.render_widget(frame, composer);
        if let Some(note) = self.copied.as_ref()
            && composer.height > 0
        {
            // `── Copied N characters ──` over the rule's own dashes, else `── Copied ──`, else
            // the plain rule, so the notice is never cut mid-word.
            let x = composer.x.saturating_add(2);
            let room = usize::from(composer.right().saturating_sub(x.saturating_add(2)));
            let text = [format!(" {} ", note.text), " Copied ".to_owned()]
                .into_iter()
                .find(|t| crate::wrap::cells_width(t) <= room);
            if let Some(text) = text {
                f.buffer_mut()
                    .set_stringn(x, composer.y, &text, room, self.theme.dim);
            }
        }
        f.render_widget(self.composer.widget(), inner);
        f.render_widget(
            Paragraph::new(status_line_at(
                &self.core,
                self.active_notice(),
                &self.statusline,
                &self.theme,
                footer.width,
                self.unix_at(now),
            )),
            footer,
        );
        let matches = self.composer.slash_matches();
        if !matches.is_empty() {
            let selected = self.composer.slash_selected().min(matches.len() - 1);
            // Scrolled just far enough that the highlight is the last row shown.
            let first = selected.saturating_sub(SLASH_ROWS - 1);
            let shown = matches.len().min(SLASH_ROWS);
            let hidden = matches.len() - (first + shown);
            let h = (shown as u16 + 2).min(transcript.height);
            let area = Rect {
                y: transcript.y + transcript.height - h,
                height: h,
                ..transcript
            };
            let lines: Vec<Line> = matches
                .iter()
                .enumerate()
                .skip(first)
                .take(shown)
                .map(|(i, e)| {
                    let group = match e.kind {
                        MenuKind::Personal => "  personal skill",
                        MenuKind::Workspace => "  workspace skill",
                        _ => "",
                    };
                    let label = match e.kind {
                        MenuKind::Note => Span::styled(e.label.clone(), self.theme.dim),
                        _ => Span::styled(e.label.clone(), self.theme.accent),
                    };
                    let mark = if i == selected { "› " } else { "  " };
                    Line::from(vec![
                        Span::styled(mark, self.theme.accent),
                        label,
                        Span::raw("  "),
                        Span::styled(e.description.clone(), self.theme.dim),
                        Span::styled(group, self.theme.dim),
                    ])
                })
                .collect();
            let mut block = Block::default().borders(Borders::ALL);
            if hidden > 0 {
                block = block.title_bottom(Line::from(Span::styled(
                    format!(" ↓ {hidden} more "),
                    self.theme.dim,
                )));
            }
            f.render_widget(Clear, area);
            f.render_widget(Paragraph::new(lines).block(block), area);
            covered = covered.max(h);
        }
        self.menu_area = self.draw_question_menu(f, transcript);
        covered = covered.max(covered_by(transcript, self.menu_area));
        if let Some(picker) = self.picker.as_ref() {
            let h = picker.height().min(transcript.height);
            covered = covered.max(h);
            picker.render(
                f,
                Rect {
                    y: transcript.y + transcript.height - h,
                    height: h,
                    ..transcript
                },
                &self.theme,
            );
        }
        if let Some(overlay) = self.overlay.as_ref() {
            let elapsed = now.saturating_duration_since(self.epoch);
            let now_unix = self.unix_at(now);
            let minute = now_unix.div_euclid(60);
            // Only the clock changed on a timer frame, so the rows stay and the spinners are
            // painted over them, unless the relative times may have moved on.
            let fresh = ticked
                && self.overlay_view.as_ref().is_some_and(|(_, built, width)| {
                    *built == minute && *width == transcript.width
                });
            if !fresh {
                let ctx = ViewCtx {
                    app: &self.core,
                    theme: &self.theme,
                    now_unix,
                    offset: local_offset(),
                    elapsed,
                    width: self.screen_width,
                    pin_icon: self.pin_icon(),
                };
                self.overlay_view = Some((overlay.view(&ctx), minute, transcript.width));
                self.overlay_builds += 1;
            }
            let area = overlay_area(
                overlay,
                self.overlay_view.as_ref().map(|(view, _, _)| view),
                transcript,
            );
            covered = covered.max(area.height);
            let spinner = Some(self.spinner.frame(elapsed));
            let scroll = overlay.preview_scroll();
            self.subagent_panes = None;
            if let Some(scroll) = scroll
                && let Some((view, _, _)) = self.overlay_view.as_ref()
            {
                let list = (view.rows.len() as u16 + 4).clamp(5, (area.height / 2).max(5));
                let [top, bottom] =
                    Layout::vertical([Constraint::Length(list), Constraint::Min(1)]).areas(area);
                let open_selected = matches!(
                    overlay.state().selected_row(view).map(|r| &r.key),
                    Some(table::RowKey::Chat(id)) if Some(*id) == self.core.chat_id
                );
                table::render(f, top, view, overlay.state(), &self.theme, spinner);
                self.subagent_panes = Some((top, bottom));
                let max = self.draw_preview(f, bottom, scroll, open_selected, ticked);
                if let Some(Overlay::Subagents(s)) = self.overlay.as_mut() {
                    s.scroll = s.scroll.min(max);
                }
            } else if let Some((view, _, _)) = self.overlay_view.as_ref() {
                let max = table::render(f, area, view, overlay.state(), &self.theme, spinner);
                if let Some(overlay) = self.overlay.as_mut() {
                    overlay.clamp_scroll(max);
                }
                if let Some(Overlay::Chats(chats)) = self.overlay.as_ref()
                    && let Some(prompt) = chats.archive.as_ref()
                {
                    crate::overlay::render_archive_prompt(f, area, prompt, &self.theme);
                }
            }
        } else {
            self.overlay_view = None;
            self.preview_view = None;
            self.subagent_panes = None;
        }
        if let Some(conflict) = self.core.save_conflict.as_ref() {
            crate::overlay::render_conflict_prompt(
                f,
                transcript,
                conflict,
                self.core.home.as_deref(),
                &self.theme,
            );
        }
        // The row stays reserved while it gives way to the transcript's own marker, so a
        // turn moving between thinking, tools, and writing never moves the transcript.
        let visible = top..top + usize::from(transcript.height.saturating_sub(covered));
        if let Some(activity) = activity.as_ref()
            && shows_activity(activity, &self.view.spinners, &self.view.thinking, visible)
        {
            let elapsed = now.saturating_duration_since(self.epoch);
            f.render_widget(
                Paragraph::new(activity_line(activity, self.spinner, elapsed, &self.theme)),
                activity_row,
            );
        }
        if self.show_help {
            let lines = help_lines_for(
                &self.theme,
                transcript.width.saturating_sub(2),
                self.keyboard_enhanced,
                self.send_now_key(),
            );
            let h = (lines.len() as u16 + 2).min(transcript.height);
            let max_scroll = (lines.len() as u16).saturating_sub(h.saturating_sub(2));
            self.help_scroll = self.help_scroll.min(max_scroll);
            let title = if max_scroll > 0 {
                " Help (Up and Down scroll, any other key closes) "
            } else {
                " Help "
            };
            let area = Rect {
                y: transcript.y,
                height: h,
                ..transcript
            };
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(lines)
                    .scroll((self.help_scroll, 0))
                    .block(Block::default().borders(Borders::ALL).title(title)),
                area,
            );
        }
        self.draw_editor(f, composer);
    }

    /// The chips above the composer, laid out in at most `max_rows` rows of `width` columns: a
    /// chip that does not fit on a row starts the next one, and one wider than a row wraps by
    /// itself. Chips past the last row are summed up as "+N more" at its end.
    fn chip_lines(&self, frame: &str, width: u16, max_rows: usize) -> Vec<Line<'static>> {
        let width = usize::from(width);
        // Each row, with how many chips started before it and by its end.
        let mut rows: Vec<(Vec<Span<'static>>, usize, usize)> = Vec::new();
        let mut row: Vec<Span<'static>> = Vec::new();
        let mut row_start = 0;
        let mut used = 0usize;
        for (i, chip) in self.core.chips.iter().enumerate() {
            let (text, style) = match chip.state {
                ChipState::Uploading => (
                    format!("{} {frame} uploading", chip.name),
                    self.theme.accent,
                ),
                ChipState::Failed(_) => (chip.label(), self.theme.error),
                ChipState::Ready(_) | ChipState::Held(_) | ChipState::Pasted => {
                    (chip.label(), self.theme.accent)
                }
            };
            // The icon leads the name inside the brackets; a failed chip's says it failed.
            let icon = match chip.state {
                ChipState::Failed(_) => Icon::Error,
                _ => Icon::Chip,
            };
            let spans = icons::line_with(&self.theme, "[", icon, &format!("{text}]"), style);
            let w: usize = spans
                .iter()
                .map(|s| crate::wrap::cells_width(&s.content))
                .sum();
            if used > 0 && used + w > width {
                rows.push((std::mem::take(&mut row), row_start, i));
                row_start = i;
                used = 0;
            }
            if w > width {
                let wrapped = crate::wrap::wrap_line(&Line::from(spans), width as u16);
                for (n, line) in wrapped.into_iter().enumerate() {
                    let start = if n == 0 { i } else { i + 1 };
                    rows.push((line.spans, start, i + 1));
                }
                row_start = i + 1;
                continue;
            }
            row.extend(spans);
            row.push(Span::raw(" "));
            used += w + 1;
        }
        let total = self.core.chips.len();
        if !row.is_empty() {
            rows.push((row, row_start, total));
        }
        if rows.len() > max_rows {
            rows.truncate(max_rows.max(1));
            if let Some((spans, start, end)) = rows.last_mut() {
                let more = |n: usize| Span::styled(format!("+{n} more"), self.theme.dim);
                let row_width: usize = spans
                    .iter()
                    .map(|s| crate::wrap::cells_width(&s.content))
                    .sum();
                let tail = more(total - *end);
                if row_width + crate::wrap::cells_width(&tail.content) <= width {
                    spans.push(tail);
                } else {
                    *spans = vec![more(total - *start)];
                }
            }
        }
        rows.into_iter()
            .map(|(spans, _, _)| Line::from(spans))
            .collect()
    }

    /// Draws the pending question menu at the bottom of the transcript while the composer is
    /// empty, with the question and each option wrapped to the box, and returns the area it
    /// covers.
    fn draw_question_menu(&self, f: &mut Frame, transcript: Rect) -> Option<Rect> {
        if !self.composer.text().is_empty() {
            return None;
        }
        let menu = self.core.question_menu()?;
        let width = transcript.width.saturating_sub(2);
        let header = if menu.header.trim().is_empty() {
            "the agent asks".to_owned()
        } else {
            menu.header.clone()
        };
        let mut lines = crate::wrap::wrap_line(
            &Line::from(Span::styled(
                format!("Question {} of {}: {header}", menu.number, menu.count),
                self.theme.accent,
            )),
            width,
        );
        lines.extend(crate::wrap::wrap_line(
            &Line::from(menu.question.clone()),
            width,
        ));
        // The rows of the selected option, so a box cut to the transcript keeps it in view.
        let mut selected_rows = 0..0;
        let mut row =
            |lines: &mut Vec<Line<'static>>, selected: bool, label: String, description: String| {
                let mark = if selected { "› " } else { "  " };
                let style = if selected {
                    self.theme.accent
                } else {
                    Style::default()
                };
                let body = Line::from(vec![
                    Span::styled(label, style),
                    Span::styled(format!("  {description}"), self.theme.dim),
                ]);
                let start = lines.len();
                lines.extend(crate::wrap::hang(
                    vec![Span::styled(mark, style)],
                    &body,
                    width,
                ));
                if selected {
                    selected_rows = start..lines.len();
                }
            };
        for (i, choice) in menu.options.iter().enumerate() {
            row(
                &mut lines,
                i == menu.selected,
                choice.label.clone(),
                choice.description.clone(),
            );
        }
        row(
            &mut lines,
            menu.selected == menu.options.len(),
            "Other…".into(),
            "type your own answer".into(),
        );
        let title = if menu.number > 1 {
            " Up and Down choose, Enter answers, Left goes back, Esc hides "
        } else {
            " Up and Down choose, Enter answers, Esc hides "
        };
        let h = (lines.len() as u16 + 2).min(transcript.height);
        let shown = usize::from(h.saturating_sub(2));
        let scroll = selected_rows.end.saturating_sub(shown) as u16;
        let area = Rect {
            y: transcript.y + transcript.height - h,
            height: h,
            ..transcript
        };
        f.render_widget(Clear, area);
        f.render_widget(
            Paragraph::new(lines)
                .scroll((scroll, 0))
                .block(Block::default().borders(Borders::ALL).title(title)),
            area,
        );
        Some(area)
    }

    /// Draws the one-line editor as a box over the composer.
    fn draw_editor(&self, f: &mut Frame, composer: Rect) {
        let Some(editor) = self.core.editor.as_ref() else {
            return;
        };
        let (title, loading) = match editor.target {
            scuttle_core::app::EditTarget::Rename(_) => {
                (" Rename chat (Enter saves, Esc cancels) ", "Loading…")
            }
            scuttle_core::app::EditTarget::Title(_) => {
                (" Title (Enter saves, Esc cancels) ", "Proposing a title…")
            }
            scuttle_core::app::EditTarget::Other => {
                (" Other answer (Enter sends, Esc cancels) ", "Loading…")
            }
            scuttle_core::app::EditTarget::SaveAs(_) => {
                (" Save as (Enter saves, Esc cancels) ", "Loading…")
            }
        };
        // The box is three rows, as tall as the smallest composer, and ends at its bottom rule.
        let bottom = composer.y + composer.height;
        let height = 3.min(bottom);
        let area = Rect {
            y: bottom - height,
            height,
            ..composer
        };
        f.render_widget(Clear, area);
        let inner_width = area.width.saturating_sub(2) as usize;
        let line = if editor.loading {
            Line::from(Span::styled(loading, self.theme.dim))
        } else {
            let (visible, _) =
                visible_editor_text(editor.line.text(), editor.line.cursor(), inner_width);
            Line::from(visible)
        };
        f.render_widget(
            Paragraph::new(line).block(Block::default().borders(Borders::ALL).title(title)),
            area,
        );
        if !editor.loading {
            let (_, column) =
                visible_editor_text(editor.line.text(), editor.line.cursor(), inner_width);
            f.set_cursor_position((area.x + 1 + column, area.y + 1));
        }
    }
}

/// The portion of `text` that fits in `width` columns, scrolled so `cursor` (a grapheme
/// index) stays inside it, and the column to draw its caret at. Never splits a grapheme
/// cluster, and never scrolls past the start.
fn visible_editor_text(text: &str, cursor: usize, width: usize) -> (String, u16) {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    let width = width.max(1);
    let graphemes: Vec<&str> = text.graphemes(true).collect();
    let mut cols = Vec::with_capacity(graphemes.len() + 1);
    let mut col = 0usize;
    cols.push(0);
    for g in &graphemes {
        col += g.width();
        cols.push(col);
    }
    let cursor = cursor.min(graphemes.len());
    let cursor_col = cols[cursor];
    // Scroll left just enough that the cursor's column is the box's last one; `start` lands
    // on a grapheme boundary, so nothing wider than one cell is cut in half.
    let start = if cursor_col >= width {
        let min_col = cursor_col + 1 - width;
        cols.partition_point(|&c| c < min_col)
    } else {
        0
    };
    let visible = graphemes[start..].concat();
    let column = (cursor_col - cols[start]) as u16;
    (visible, column)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::spinner_frame;
    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use scuttle_core::live::LiveBlock;
    use serde_json::json;

    fn tui() -> Tui {
        Tui::new(
            scuttle_core::config::LocalConfig::default(),
            None,
            Theme::terminal(true),
            Welcome {
                url: "https://x".into(),
                user: "nick".into(),
                art: vec![],
                art_accent: true,
                show: true,
                tip: true,
            },
            0,
        )
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, mods))
    }

    fn screen(t: &mut Tui, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| t.draw(f)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn typing_and_enter_submits_through_core() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        for c in "hi".chars() {
            t.handle(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(effects.as_slice(), [Effect::CreateChat { text, .. }] if text == "hi"));
    }

    #[test]
    fn enter_on_an_empty_composer_sends_the_first_queued_message_now() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let id = uuid::Uuid::new_v4();
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(
                serde_json::from_value(json!({"id": id, "children": [], "files": [],
                "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}))
                .unwrap(),
            ),
            messages: vec![],
        });
        assert!(t.handle(key(KeyCode::Enter, KeyModifiers::NONE)).is_empty());
        t.update(stream(json!({"type": "queue_update", "queued_messages": [
            {"id": 7, "content": [{"type": "text", "text": "then run the tests"}]}
        ]})));
        assert_eq!(
            t.handle(key(KeyCode::Enter, KeyModifiers::NONE)),
            vec![Effect::PromoteQueued { chat: id, id: 7 }]
        );
        t.composer.set_text("a new message");
        assert!(
            matches!(
                t.handle(key(KeyCode::Enter, KeyModifiers::NONE)).as_slice(),
                [Effect::SendMessage { .. }]
            ),
            "with text in the composer, Enter sends it"
        );
    }

    #[test]
    fn ctrl_c_twice_quits() {
        let mut t = tui();
        assert!(
            t.handle(key(KeyCode::Char('c'), KeyModifiers::CONTROL))
                .is_empty()
        );
        assert_eq!(
            t.handle(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            vec![Effect::Quit]
        );
    }

    #[test]
    fn click_on_a_code_block_copies_it() {
        let (mut t, row) = tui_with_a_code_block();
        click(&mut t, row);
        assert_eq!(t.last_copied.as_deref(), Some("echo hi\n"));
    }

    #[test]
    fn click_hit_testing_accounts_for_the_margin_offset() {
        let mut t = tui();
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap();
        t.core.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: serde_json::from_value(json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": "```\necho hi\n```"}]}])).unwrap(),
        });
        let shown = screen(&mut t, 60, 20);
        let rows: Vec<&str> = shown.lines().collect();
        assert!(
            rows[0].trim().is_empty(),
            "top margin row holds no content: {:?}",
            rows[0]
        );
        let code_row = rows
            .iter()
            .position(|r| r.contains("echo hi"))
            .expect("code block on screen") as u16;
        assert!(code_row > 0, "the code row sits below the top margin");
        // The margin row is not backed by any transcript line, so a click there must miss.
        click(&mut t, 0);
        assert_eq!(t.last_copied, None, "the margin row is not a hit target");
        // The code block's actual screen row, shifted down by the margin, still resolves.
        click(&mut t, code_row);
        assert_eq!(t.last_copied.as_deref(), Some("echo hi\n"));
    }

    #[test]
    fn the_slash_menu_shows_aliases() {
        let mut t = tui();
        t.composer.set_text("/q");
        let shown = screen(&mut t, 60, 16);
        assert!(shown.contains("/quit (/exit)"), "{shown}");
    }

    #[test]
    fn slash_model_opens_the_picker_and_escape_closes_it() {
        let mut t = tui();
        // `/model` only opens the picker once the model list has loaded.
        t.core.update(Msg::ModelsLoaded(
            serde_json::from_value(json!([{"id": uuid::Uuid::new_v4(), "display_name": "A", "enabled": true, "reasoning_efforts": []}]))
                .unwrap(),
        ));
        let effects = t
            .core
            .update(Msg::Command(scuttle_core::commands::Command::Model(None)));
        for e in &effects {
            t.apply_ui_effect(e);
        }
        assert!(t.overlay.is_some());
        t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(t.overlay.is_none());
    }

    #[test]
    fn the_model_table_draws_inside_the_margin_and_filters_as_you_type() {
        let mut t = tui();
        let provider = uuid::Uuid::new_v4();
        t.core.providers = serde_json::from_value(json!([
            {"id": provider, "display_name": "Provider", "available": true}
        ]))
        .unwrap();
        t.core.update(Msg::ModelsLoaded(
            serde_json::from_value(json!([
                {"id": uuid::Uuid::new_v4(), "display_name": "GPT-5", "ai_provider_id": provider, "enabled": true, "reasoning_efforts": []},
                {"id": uuid::Uuid::new_v4(), "display_name": "Claude Sonnet", "ai_provider_id": provider, "enabled": true, "reasoning_efforts": []}
            ]))
            .unwrap(),
        ));
        let effects = t.update(Msg::Submit("/model".into()));
        for e in &effects {
            t.apply_ui_effect(e);
        }
        assert!(t.overlay.is_some(), "{effects:?}");
        assert!(t.picker.is_none(), "the slider stays closed");
        assert_eq!(
            t.animation_deadline(Instant::now()),
            None,
            "an open table does not keep the screen redrawing"
        );
        for c in "son".chars() {
            t.handle(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert!(t.composer.text().is_empty(), "typing goes to the filter");
        let (w, h) = (60u16, 20u16);
        let shown = screen(&mut t, w, h);
        assert!(shown.contains(" Model "), "{shown}");
        assert!(shown.contains("> son"), "{shown}");
        assert!(shown.contains("Claude Sonnet"), "{shown}");
        assert!(!shown.contains("GPT-5"), "{shown}");
        for row in shown.lines() {
            assert!(row.starts_with(' ') && row.ends_with(' '), "{row:?}");
        }
    }

    #[test]
    fn slash_effort_opens_the_slider_and_enter_saves_the_level() {
        let mut t = tui();
        t.core.update(Msg::ModelsLoaded(
            serde_json::from_value(json!([
                {"id": uuid::Uuid::new_v4(), "display_name": "Thinker", "enabled": true, "is_default": true, "reasoning_efforts": ["low", "high"]}
            ]))
            .unwrap(),
        ));
        let open = |t: &mut Tui| {
            let effects = t.update(Msg::Submit("/effort".into()));
            for e in &effects {
                t.apply_ui_effect(e);
            }
            assert!(t.picker.is_some());
        };
        open(&mut t);
        let shown = screen(&mut t, 60, 20);
        assert!(shown.contains("low   high"), "{shown}");
        t.handle(key(KeyCode::Left, KeyModifiers::NONE));
        t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(t.picker.is_none());
        assert_eq!(t.core.selected_effort.as_deref(), Some("low"));
        open(&mut t);
        t.handle(key(KeyCode::Right, KeyModifiers::NONE));
        t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(t.picker.is_none());
        assert_eq!(
            t.core.selected_effort.as_deref(),
            Some("low"),
            "Esc keeps the effort"
        );
    }

    #[test]
    fn editor_failure_keeps_composer_text() {
        let mut t = tui();
        t.composer.set_text("keep me");
        let result = t.edit_with(|_path| Err(std::io::Error::other("editor crashed")));
        assert!(result.is_err());
        assert_eq!(t.composer.text(), "keep me");
    }

    #[test]
    fn the_screen_keeps_a_one_cell_margin() {
        let mut t = tui();
        t.core.notices.push(Notice::Info("margin check".into()));
        t.sync_notice(Instant::now());
        let (w, h) = (60u16, 16u16);
        let shown = screen(&mut t, w, h);
        let rows: Vec<&str> = shown.lines().collect();
        assert!(rows[0].trim().is_empty(), "top row: {:?}", rows[0]);
        assert!(rows[h as usize - 1].trim().is_empty(), "bottom row");
        for row in &rows {
            assert!(row.starts_with(' ') && row.ends_with(' '), "{row:?}");
        }
        assert!(
            rows[h as usize - 2].starts_with(" margin check"),
            "the footer sits inside the margin: {:?}",
            rows[h as usize - 2]
        );
    }

    #[test]
    fn a_tiny_terminal_gives_up_the_margin() {
        let mut t = tui();
        let shown = screen(&mut t, 20, 6);
        let rows: Vec<&str> = shown.lines().collect();
        assert!(
            rows[2].starts_with('─'),
            "the composer's top rule starts at column 0: {rows:?}"
        );
        let padded = screen(&mut t, 40, 10);
        assert!(padded.lines().all(|r| r.starts_with(' ')), "{padded}");
    }

    #[test]
    fn the_composer_has_a_full_width_rule_above_and_below_its_text() {
        let mut t = tui();
        let (w, h) = (60u16, 16u16);
        let shown = screen(&mut t, w, h);
        let rows: Vec<&str> = shown.lines().collect();
        let footer = h as usize - 2;
        assert!(rows[footer - 2].contains("Message the agent"), "{shown}");
        for (row, which) in [(footer - 3, "above"), (footer - 1, "below")] {
            let rule = rows[row].trim();
            assert!(
                rule.chars().count() == usize::from(w) - 2 && rule.chars().all(|c| c == '─'),
                "a full-width rule sits {which} the text row: {shown}"
            );
        }
    }

    #[test]
    fn the_copy_notice_stays_in_the_top_rule_over_a_plain_bottom_rule() {
        let mut t = tui();
        t.copy("abc".into());
        let shown = screen(&mut t, 60, 16);
        let rows: Vec<&str> = shown.lines().collect();
        let footer = 16 - 2;
        assert!(
            rows[footer - 3]
                .trim_start()
                .starts_with("── Copied 3 characters ──"),
            "{shown}"
        );
        let bottom = rows[footer - 1].trim();
        assert!(
            !bottom.is_empty() && bottom.chars().all(|c| c == '─'),
            "the bottom rule never carries the notice: {shown}"
        );
    }

    #[test]
    fn the_slash_menu_shows_five_rows_and_says_how_many_more() {
        let mut t = tui();
        t.composer.set_text("/");
        let shown = screen(&mut t, 80, 24);
        for name in ["/new", "/chats", "/subagents", "/parent", "/model"] {
            assert!(shown.contains(name), "{name} is missing:\n{shown}");
        }
        assert!(
            !shown.contains("/effort"),
            "the sixth entry is hidden:\n{shown}"
        );
        let more = scuttle_core::commands::COMMANDS.len() - SLASH_ROWS;
        assert!(shown.contains(&format!("↓ {more} more")), "{shown}");
        assert!(
            shown.contains("› /new"),
            "the first entry is highlighted:\n{shown}"
        );
    }

    #[test]
    fn down_scrolls_the_slash_menu_to_keep_the_highlight_in_view() {
        let mut t = tui();
        t.handle(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for _ in 0..6 {
            t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        }
        let shown = screen(&mut t, 80, 24);
        assert!(shown.contains("› /workspace"), "{shown}");
        assert!(!shown.contains("/new "), "the top scrolled out:\n{shown}");
        let more = scuttle_core::commands::COMMANDS.len() - 7;
        assert!(shown.contains(&format!("↓ {more} more")), "{shown}");
    }

    #[test]
    fn draw_fits_small_terminals() {
        let mut t = tui();
        for (w, h) in [(40u16, 10u16), (20, 6), (120, 40)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| t.draw(f)).unwrap();
        }
    }

    #[test]
    fn restore_composer_puts_the_text_back() {
        let mut t = tui();
        assert!(t.apply_ui_effect(&Effect::RestoreComposer("lost words".into())));
        assert_eq!(t.composer.text(), "lost words");
    }

    #[test]
    fn restore_composer_keeps_what_was_typed_since() {
        let mut t = tui();
        t.composer.set_text("typed after");
        t.apply_ui_effect(&Effect::RestoreComposer("failed send".into()));
        assert_eq!(t.composer.text(), "failed send\n\ntyped after");
    }

    #[test]
    fn runtime_effects_are_not_ui_effects() {
        let mut t = tui();
        assert!(!t.apply_ui_effect(&Effect::FetchPrefs));
        assert!(!t.apply_ui_effect(&Effect::Quit));
    }

    #[test]
    fn live_turn_toggles_drop_when_the_live_turn_empties() {
        let mut t = tui();
        t.toggles.insert((None, 0));
        t.toggles.insert((Some(4), 1));
        t.core
            .transcript
            .live
            .blocks
            .push(LiveBlock::Text("streaming".into()));
        t.update(Msg::Refresh);
        assert!(t.toggles.contains(&(None, 0)), "live turn still has blocks");
        t.core.transcript.live.clear();
        t.update(Msg::Refresh);
        assert!(!t.toggles.contains(&(None, 0)));
        assert!(t.toggles.contains(&(Some(4), 1)));
    }

    #[test]
    fn a_new_notice_shows_until_a_key_press() {
        let mut t = tui();
        let now = Instant::now();
        t.core.notices.push(Notice::Info("hello there".into()));
        t.sync_notice(now);
        assert_eq!(t.active_notice(), Some(&Notice::Info("hello there".into())));
        assert!(screen(&mut t, 60, 10).contains("hello there"));
        t.handle(key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(t.active_notice(), None);
        assert!(!screen(&mut t, 60, 10).contains("hello there"));
    }

    #[test]
    fn an_error_stays_through_a_key_until_it_has_shown_its_minimum() {
        let mut t = tui();
        let now = Instant::now();
        // The first draw, so the notice below arrives while scuttle runs.
        t.sync_notice(now);
        t.core.notices.push(Notice::Error("upload failed".into()));
        t.sync_notice(now);
        let x = || key(KeyCode::Char('x'), KeyModifiers::NONE);
        t.handle_at(x(), now + Duration::from_millis(100), paste_time());
        assert_eq!(
            t.active_notice(),
            Some(&Notice::Error("upload failed".into())),
            "a key right after the error leaves it up"
        );
        t.handle_at(
            x(),
            now + ERROR_MIN_SHOWN - Duration::from_millis(1),
            paste_time(),
        );
        assert!(t.active_notice().is_some());
        t.handle_at(x(), now + ERROR_MIN_SHOWN, paste_time());
        assert_eq!(t.active_notice(), None);
        t.core.notices.push(Notice::Info("Archived.".into()));
        let later = now + Duration::from_secs(3);
        t.sync_notice(later);
        t.handle_at(x(), later, paste_time());
        assert_eq!(t.active_notice(), None, "an info clears at once");
    }

    #[test]
    fn a_notice_expires_after_five_seconds() {
        let mut t = tui();
        let now = Instant::now();
        t.core.notices.push(Notice::Error("boom".into()));
        t.sync_notice(now);
        assert_eq!(t.notice_deadline(), Some(now + NOTICE_TTL));
        t.sync_notice(now + Duration::from_secs(4));
        assert!(t.active_notice().is_some());
        t.sync_notice(now + NOTICE_TTL);
        assert_eq!(t.active_notice(), None);
        assert_eq!(t.notice_deadline(), None);
    }

    #[test]
    fn the_newest_notice_wins() {
        let mut t = tui();
        let now = Instant::now();
        t.core.notices.push(Notice::Info("first".into()));
        t.sync_notice(now);
        t.core.notices.push(Notice::Info("second".into()));
        t.core.notices.push(Notice::Info("third".into()));
        t.sync_notice(now);
        assert_eq!(t.active_notice(), Some(&Notice::Info("third".into())));
    }

    #[test]
    fn notices_that_were_never_shown_take_turns() {
        let mut t = tui();
        let now = Instant::now();
        t.core.notices.push(Notice::Info("version skew".into()));
        t.core.notices.push(Notice::Error("no organization".into()));
        t.sync_notice(now);
        assert_eq!(
            t.active_notice(),
            Some(&Notice::Error("no organization".into()))
        );
        t.sync_notice(now + NOTICE_TTL);
        assert_eq!(
            t.active_notice(),
            Some(&Notice::Info("version skew".into()))
        );
        assert_eq!(t.notice_deadline(), Some(now + NOTICE_TTL * 2));
        t.sync_notice(now + NOTICE_TTL * 2);
        assert_eq!(t.active_notice(), None);
    }

    #[test]
    fn a_key_drops_the_notices_waiting_their_turn() {
        let mut t = tui();
        let now = Instant::now();
        // The first draw, so the notices below arrive while scuttle runs.
        t.sync_notice(now);
        t.core.notices.push(Notice::Info("old one".into()));
        t.core.notices.push(Notice::Info("old two".into()));
        t.sync_notice(now);
        assert_eq!(t.active_notice(), Some(&Notice::Info("old two".into())));
        t.handle(key(KeyCode::Char('x'), KeyModifiers::NONE));
        t.sync_notice(now);
        assert_eq!(
            t.active_notice(),
            None,
            "the waiting notice does not replay after the key"
        );
        assert_eq!(t.notice_deadline(), None);
    }

    #[test]
    fn a_notice_that_waited_two_lifetimes_is_dropped_unseen() {
        let mut t = tui();
        let now = Instant::now();
        // The first draw, so the notices below arrive while scuttle runs.
        t.sync_notice(now);
        for text in ["one", "two", "three"] {
            t.core.notices.push(Notice::Info(text.into()));
        }
        t.sync_notice(now);
        assert_eq!(t.active_notice(), Some(&Notice::Info("three".into())));
        t.sync_notice(now + NOTICE_TTL);
        assert_eq!(t.active_notice(), Some(&Notice::Info("one".into())));
        t.sync_notice(now + NOTICE_TTL * 2);
        assert_eq!(
            t.active_notice(),
            None,
            "two waited ten seconds and no longer describes the moment"
        );
    }

    #[test]
    fn a_key_never_drops_an_error_waiting_its_turn() {
        let mut t = tui();
        let now = Instant::now();
        // The first draw, so the notices below arrive while scuttle runs.
        t.sync_notice(now);
        t.core.notices.push(Notice::Error("send failed".into()));
        t.core.notices.push(Notice::Info("Archived.".into()));
        t.sync_notice(now);
        assert_eq!(t.active_notice(), Some(&Notice::Info("Archived.".into())));
        t.handle(key(KeyCode::Char('x'), KeyModifiers::NONE));
        t.sync_notice(now + NOTICE_TTL);
        assert_eq!(
            t.active_notice(),
            Some(&Notice::Error("send failed".into())),
            "an error is the only report that something failed"
        );
    }

    #[test]
    fn an_error_waiting_its_turn_never_goes_stale() {
        let mut t = tui();
        let now = Instant::now();
        // The first draw, so the notices below arrive while scuttle runs.
        t.sync_notice(now);
        t.core.notices.push(Notice::Info("one".into()));
        t.core.notices.push(Notice::Error("two".into()));
        t.core.notices.push(Notice::Info("three".into()));
        t.sync_notice(now);
        assert_eq!(t.active_notice(), Some(&Notice::Info("three".into())));
        t.sync_notice(now + NOTICE_TTL);
        assert_eq!(t.active_notice(), Some(&Notice::Info("one".into())));
        t.sync_notice(now + NOTICE_STALE);
        assert_eq!(
            t.active_notice(),
            Some(&Notice::Error("two".into())),
            "the error waited two lifetimes and still shows"
        );
    }

    #[test]
    fn an_error_a_newer_notice_displaces_shows_next() {
        let mut t = tui();
        let now = Instant::now();
        // The first draw, so the notices below arrive while scuttle runs.
        t.sync_notice(now);
        t.core.notices.push(Notice::Error("boom".into()));
        t.sync_notice(now);
        assert_eq!(t.active_notice(), Some(&Notice::Error("boom".into())));
        let later = now + Duration::from_secs(1);
        t.core.notices.push(Notice::Info("later".into()));
        t.sync_notice(later);
        assert_eq!(t.active_notice(), Some(&Notice::Info("later".into())));
        t.sync_notice(later + NOTICE_TTL);
        assert_eq!(
            t.active_notice(),
            Some(&Notice::Error("boom".into())),
            "the displaced error takes the next turn"
        );
    }

    #[test]
    fn startup_notices_still_take_turns_after_a_key() {
        let mut t = tui();
        let now = Instant::now();
        t.core.notices.push(Notice::Info("version skew".into()));
        t.core.notices.push(Notice::Error("no organization".into()));
        t.sync_notice(now);
        assert_eq!(
            t.active_notice(),
            Some(&Notice::Error("no organization".into()))
        );
        t.handle(key(KeyCode::Char('x'), KeyModifiers::NONE));
        t.sync_notice(now + NOTICE_TTL);
        assert_eq!(
            t.active_notice(),
            Some(&Notice::Info("version skew".into())),
            "a notice there at the first draw keeps its turn"
        );
    }

    #[test]
    fn startup_notices_never_go_stale() {
        let mut t = tui();
        let now = Instant::now();
        for text in ["version skew", "no organization", "model unavailable"] {
            t.core.notices.push(Notice::Info(text.into()));
        }
        t.sync_notice(now);
        assert_eq!(
            t.active_notice(),
            Some(&Notice::Info("model unavailable".into()))
        );
        t.sync_notice(now + NOTICE_TTL);
        assert_eq!(
            t.active_notice(),
            Some(&Notice::Info("version skew".into()))
        );
        t.sync_notice(now + NOTICE_STALE * 3);
        assert_eq!(
            t.active_notice(),
            Some(&Notice::Info("no organization".into())),
            "a notice there at the first draw waits as long as it must"
        );
        t.sync_notice(now + NOTICE_STALE * 3 + NOTICE_TTL);
        assert_eq!(t.active_notice(), None);
    }

    #[test]
    fn the_key_that_holds_a_draft_never_drops_the_held_notice() {
        let (mut t, _) = tui_on_a_disabled_model();
        let now = Instant::now();
        t.sync_notice(now);
        t.composer.set_text("look");
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        apply_ui_effects(&mut t, effects);
        t.sync_notice(now);
        assert!(
            matches!(t.active_notice(), Some(Notice::Error(m)) if m.contains("Pick a model to send your message")),
            "{:?}",
            t.active_notice()
        );
    }

    #[test]
    fn a_key_never_drops_the_unavailable_model_warning_before_it_shows() {
        let mut t = tui();
        let now = Instant::now();
        // The first draw, before the chat loads.
        t.sync_notice(now);
        started(&mut t);
        let provider = uuid::Uuid::new_v4();
        let old = uuid::Uuid::new_v4();
        t.core.providers = serde_json::from_value(json!([
            {"id": provider, "display_name": "Provider", "available": true}
        ]))
        .unwrap();
        t.update(Msg::ModelsLoaded(
            serde_json::from_value(json!([
                {"id": old, "display_name": "Old", "ai_provider_id": provider, "enabled": false, "reasoning_efforts": []},
                {"id": uuid::Uuid::new_v4(), "display_name": "Fresh", "ai_provider_id": provider, "enabled": true, "is_default": true, "reasoning_efforts": []}
            ]))
            .unwrap(),
        ));
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "last_model_config_id": old, "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap();
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: vec![],
        });
        t.handle(key(KeyCode::Char('x'), KeyModifiers::NONE));
        t.sync_notice(now);
        assert!(
            matches!(t.active_notice(), Some(Notice::Error(m)) if m.contains("Old, is not available")),
            "{:?}",
            t.active_notice()
        );
        t.sync_notice(now + NOTICE_TTL);
        assert!(
            screen_at(&mut t, 100, 20, now + NOTICE_TTL).contains("/model: Old (unavailable)"),
            "the footer keeps saying so"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_key_that_saves_to_a_read_only_config_never_drops_its_notice() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("scuttle-ro-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "mouse = true\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let mut t = Tui::new(
            scuttle_core::config::LocalConfig::default(),
            Some(path.clone()),
            Theme::terminal(true),
            Welcome {
                url: "https://x".into(),
                user: String::new(),
                art: vec![],
                art_accent: true,
                show: true,
                tip: true,
            },
            0,
        );
        let now = Instant::now();
        t.sync_notice(now);
        t.composer.set_text("/mouse");
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        apply_ui_effects(&mut t, effects);
        t.sync_notice(now);
        let active = t.active_notice().cloned();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        assert!(
            matches!(&active, Some(Notice::Error(m)) if m.contains("config.toml is read-only")),
            "{active:?}"
        );
    }

    #[test]
    fn copy_warnings_go_to_the_footer_and_failures_say_why() {
        let mut t = tui();
        t.report_copy(CopyOutcome::CopiedWithWarning("check tmux".into()), 5);
        assert_eq!(
            t.copied.as_ref().map(|c| c.text.as_str()),
            Some("Copied 5 characters")
        );
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Info("check tmux".into()))
        );
        t.copied = None;
        t.report_copy(CopyOutcome::Failed("no display".into()), 5);
        assert!(t.copied.is_none(), "a failed copy confirms nothing");
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Error("Copy failed: no display".into()))
        );
    }

    #[test]
    fn a_drag_copy_says_how_much_it_copied_in_the_composer_rule_for_two_seconds() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": "alpha beta gamma"}]}]),
        );
        let shown = screen(&mut t, 60, 20);
        let (x, y) = find(&shown, "beta");
        let notices = t.core.notices.len();
        drag(&mut t, (x, y), (x + 9, y));
        assert_eq!(t.last_copied.as_deref(), Some("beta gamma"));
        let now = Instant::now();
        let shown = screen_at(&mut t, 60, 20, now);
        let rows: Vec<&str> = shown.lines().collect();
        let at = rows
            .iter()
            .position(|r| r.contains("Copied 10 characters"))
            .unwrap_or_else(|| panic!("no copy notice:\n{shown}"));
        assert!(
            rows[at]
                .trim_start()
                .starts_with("── Copied 10 characters ──"),
            "the notice sits in the composer's top rule:\n{shown}"
        );
        let rule_style = |t: &mut Tui, at: Instant| {
            let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
            term.draw(|f| t.draw_at(f, at)).unwrap();
            let buf = term.backend().buffer().clone();
            let x = (0..60)
                .find(|&x| buf[(x, at_row(&buf, "Copied"))].symbol() == "C")
                .unwrap();
            buf[(x, at_row(&buf, "Copied"))].style()
        };
        assert_eq!(
            rule_style(&mut t, now).fg,
            t.theme.dim.fg,
            "the notice is dim, like the rule"
        );
        assert_eq!(t.core.notices.len(), notices, "the footer gets no notice");
        assert_eq!(t.notice_deadline(), Some(now + COPY_TTL));
        assert!(
            screen_at(&mut t, 60, 20, now + Duration::from_secs(1))
                .contains("Copied 10 characters")
        );
        let later = screen_at(&mut t, 60, 20, now + COPY_TTL);
        assert!(!later.contains("Copied"), "it leaves after two seconds");
        let rule = later.lines().nth(at).unwrap().trim();
        assert!(
            !rule.is_empty() && rule.chars().all(|c| c == '─'),
            "the plain rule comes back:\n{later}"
        );
        assert_eq!(t.notice_deadline(), None);
    }

    /// The row of `buf` whose text contains `needle`.
    fn at_row(buf: &ratatui::buffer::Buffer, needle: &str) -> u16 {
        (0..buf.area.height)
            .find(|&y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .contains(needle)
            })
            .unwrap_or_else(|| panic!("no {needle}"))
    }

    #[test]
    fn a_copy_moves_no_row_for_a_reader_at_the_bottom() {
        let mut t = tui();
        numbered_rows(&mut t);
        let now = Instant::now();
        let before = screen_at(&mut t, 40, 24, now);
        let (x, y) = find(&before, "row 26");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 5, y);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x + 5, y);
        assert_eq!(t.last_copied.as_deref(), Some("row 26"));
        let during = screen_at(&mut t, 40, 24, now);
        assert!(during.contains("Copied 6 characters"), "{during}");
        let after = screen_at(&mut t, 40, 24, now + COPY_TTL);
        let transcript = |s: &str| s.lines().take(18).map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(
            transcript(&during),
            transcript(&before),
            "the copy moved the transcript:\n{during}"
        );
        assert_eq!(transcript(&after), transcript(&before), "{after}");
    }

    #[test]
    fn the_copy_notice_counts_characters_not_bytes() {
        let mut t = tui();
        t.copy("héllo 🦀".into());
        assert_eq!(
            t.copied.as_ref().map(|c| c.text.as_str()),
            Some("Copied 7 characters")
        );
        assert!(screen(&mut t, 60, 20).contains("── Copied 7 characters ──"));
    }

    #[test]
    fn a_copy_notice_too_wide_for_the_rule_shortens_or_leaves_the_plain_rule() {
        let rule_at = |t: &mut Tui, w: u16| {
            let shown = screen(t, w, 20);
            // The composer's top rule, the second rule from the bottom now that one sits
            // below the text too.
            shown
                .lines()
                .map(str::trim)
                .filter(|r| r.starts_with('─'))
                .rev()
                .nth(1)
                .unwrap_or_else(|| panic!("no top rule:\n{shown}"))
                .to_owned()
        };
        let mut t = tui();
        t.copy("abc".into());
        let rule = rule_at(&mut t, 20);
        assert!(
            rule.starts_with("── Copied ──") && !rule.contains("Copied 3"),
            "a short rule says only Copied: {rule:?}"
        );
        let rule = rule_at(&mut t, 10);
        assert!(
            !rule.is_empty() && rule.chars().all(|c| c == '─'),
            "a rule too short for Copied stays plain: {rule:?}"
        );
        assert!(rule_at(&mut t, 40).contains("── Copied 3 characters ──"));
    }

    #[test]
    fn a_key_clears_the_copy_notice() {
        let (mut t, row) = tui_with_a_code_block();
        click(&mut t, row);
        assert!(screen(&mut t, 60, 20).contains("Copied 8 characters"));
        t.handle(key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(!screen(&mut t, 60, 20).contains("Copied"));
    }

    #[test]
    fn the_copy_notice_leaves_the_hint_row_to_the_plan_hint() {
        let mut t = tui();
        started(&mut t);
        plan_ready(&mut t, vec![]);
        let now = Instant::now();
        assert!(screen_at(&mut t, 70, 20, now).contains("Implement the plan"));
        t.copy("abc".into());
        let shown = screen_at(&mut t, 70, 20, now);
        assert!(shown.contains("Copied 3 characters"), "{shown}");
        assert!(
            shown.contains("Implement the plan"),
            "the copy notice is in the rule, not the hint row:\n{shown}"
        );
        let later = screen_at(&mut t, 70, 20, now + COPY_TTL);
        assert!(later.contains("Implement the plan"), "{later}");
    }

    #[test]
    fn the_clipboard_is_not_created_at_startup() {
        let t = tui();
        assert!(t.clipboard.is_none());
    }

    #[test]
    fn without_keyboard_enhancement_the_placeholder_names_alt_enter() {
        let mut t = tui();
        assert!(
            t.composer
                .widget()
                .placeholder_text()
                .contains("Shift+Enter")
        );
        t.set_keyboard_enhanced(false);
        t.composer.set_text("");
        let placeholder = t.composer.widget().placeholder_text();
        assert!(
            placeholder.contains("Alt+Enter for a new line"),
            "{placeholder}"
        );
        assert!(!placeholder.contains("Shift+Enter"), "{placeholder}");
    }

    #[test]
    fn modifier_enter_without_enhancement_names_alt_enter_to_send() {
        let mut t = tui();
        t.core.prefs.send_shortcut = scuttle_core::density::SendShortcut::ModifierEnter;
        t.set_keyboard_enhanced(false);
        let alt = if cfg!(target_os = "macos") {
            "⌥↵"
        } else {
            "Alt+↵"
        };
        assert_eq!(
            t.composer.widget().placeholder_text(),
            format!("Message the agent. {alt} to send, Enter for a new line, /help for commands.")
        );
        t.set_keyboard_enhanced(true);
        let send = if cfg!(target_os = "macos") {
            "⌘↵"
        } else {
            "Ctrl+↵"
        };
        assert!(
            t.composer
                .widget()
                .placeholder_text()
                .contains(&format!("{send} to send"))
        );
    }

    #[test]
    fn loaded_prefs_update_the_placeholder() {
        let mut t = tui();
        t.set_keyboard_enhanced(false);
        let prefs = scuttle_core::density::DisplayPrefs {
            send_shortcut: scuttle_core::density::SendShortcut::ModifierEnter,
            ..Default::default()
        };
        t.update(Msg::PrefsLoaded(prefs));
        let alt = if cfg!(target_os = "macos") {
            "⌥↵"
        } else {
            "Alt+↵"
        };
        assert!(
            t.composer
                .widget()
                .placeholder_text()
                .contains(&format!("{alt} to send"))
        );
        // The Tui passes the enhancement state on, so Alt+Enter now sends.
        for c in "hi".chars() {
            t.handle(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::ALT));
        assert!(matches!(effects.as_slice(), [Effect::CreateChat { text, .. }] if text == "hi"));
    }

    #[test]
    fn ctrl_c_closes_the_help_box_and_a_second_press_quits() {
        let mut t = tui();
        t.apply_ui_effect(&Effect::ShowHelp);
        assert!(
            t.handle(key(KeyCode::Char('c'), KeyModifiers::CONTROL))
                .is_empty()
        );
        assert!(!t.show_help);
        assert_eq!(
            t.handle(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            vec![Effect::Quit]
        );
    }

    #[test]
    fn help_scrolls_on_a_short_screen() {
        let mut t = tui();
        t.apply_ui_effect(&Effect::ShowHelp);
        let top = screen(&mut t, 60, 18);
        assert!(top.contains("/model"), "{top}");
        assert!(!top.contains("Ctrl+C twice"), "{top}");
        // Scroll to the bottom without depending on today's exact help length: keep pressing
        // PageDown until the screen stops changing, capped so a scrolling regression fails fast
        // instead of looping.
        let mut bottom = screen(&mut t, 60, 18);
        let mut passed_the_keys = false;
        for _ in 0..100 {
            t.handle(key(KeyCode::PageDown, KeyModifiers::NONE));
            let next = screen(&mut t, 60, 18);
            if next == bottom {
                break;
            }
            bottom = next;
            passed_the_keys |= bottom.contains("Ctrl+C twice");
        }
        assert!(passed_the_keys, "the last key scrolled into view");
        // The /chats search notes follow the keys and end the help.
        assert!(bottom.contains("title:\"fix ci\""), "{bottom}");
        assert!(t.show_help);
        t.handle(key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(!t.show_help);
        t.apply_ui_effect(&Effect::ShowHelp);
        assert!(
            screen(&mut t, 60, 18).contains("/model"),
            "reopening starts at the top"
        );
    }

    #[test]
    fn ctrl_c_closes_a_picker_and_a_second_press_quits() {
        let mut t = tui();
        t.core.update(Msg::ModelsLoaded(vec![]));
        t.apply_ui_effect(&Effect::ShowPicker(scuttle_core::app::Picker::Model));
        assert!(t.overlay.is_some());
        assert!(
            t.handle(key(KeyCode::Char('c'), KeyModifiers::CONTROL))
                .is_empty()
        );
        assert!(t.overlay.is_none());
        assert_eq!(
            t.handle(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            vec![Effect::Quit]
        );
    }

    fn tui_with_a_code_block() -> (Tui, u16) {
        let mut t = tui();
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap();
        t.core.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: serde_json::from_value(json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": "```\necho hi\n```"}]}])).unwrap(),
        });
        let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
        term.draw(|f| t.draw(f)).unwrap();
        let row = t
            .row_of(|target| matches!(target, HitTarget::CopyCode(_)))
            .expect("code block on screen");
        (t, row)
    }

    fn click(t: &mut Tui, row: u16) {
        mouse(t, MouseEventKind::Down(MouseButton::Left), 2, row);
        mouse(t, MouseEventKind::Up(MouseButton::Left), 2, row);
    }

    /// Whether `text` holds a control character other than a line break or a tab.
    fn has_control(text: &str) -> bool {
        text.chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    }

    /// A Tui showing a model message with an escape sequence and a bell in its text and in a
    /// code block, after a user message with the same.
    fn control_tui() -> Tui {
        let mut t = tui();
        loaded(
            &mut t,
            json!([
                {"id": 1, "role": "user", "content": [{"type": "text", "text": "say \u{1b}[1mhi\u{7} now"}]},
                {"id": 2, "role": "assistant", "content": [{"type": "text",
                    "text": "red \u{1b}[31malert\u{7} here\n\n```\ncode\u{1b}[0m\u{7}\n```"}]}
            ]),
        );
        t
    }

    #[test]
    fn a_model_message_with_control_characters_copies_without_them() {
        let mut t = control_tui();
        let shown = screen(&mut t, 60, 20);
        let effects = t.update(Msg::Command(scuttle_core::commands::Command::Copy(None)));
        show(&mut t, effects);
        let copied = t.last_copied.clone().expect("/copy copied");
        assert!(!has_control(&copied), "/copy: {copied:?}");
        assert!(copied.contains("red [31malert here"), "/copy: {copied:?}");
        let effects = t.update(Msg::Command(scuttle_core::commands::Command::Copy(Some(1))));
        show(&mut t, effects);
        let code = t.last_copied.clone().expect("/copy 1 copied");
        assert!(!has_control(&code), "/copy 1: {code:?}");
        let (x, y) = find(&shown, "red");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 20, y);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x + 20, y);
        let dragged = t.last_copied.clone().expect("the drag copied");
        assert!(!has_control(&dragged), "drag: {dragged:?}");
        assert_eq!(dragged, "red [31malert here");
    }

    #[test]
    fn a_drag_copies_exactly_the_cells_shown() {
        let mut t = control_tui();
        let shown = screen(&mut t, 60, 20);
        let (x, y) = find(&shown, "say");
        let row: Vec<char> = shown.lines().nth(usize::from(y)).unwrap().chars().collect();
        let end = x + 16;
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), end, y);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), end, y);
        let cells: String = row[usize::from(x)..=usize::from(end)].iter().collect();
        assert_eq!(
            t.last_copied.as_deref(),
            Some(cells.trim_end()),
            "the copy is the cells on screen"
        );
        assert!(!has_control(t.last_copied.as_deref().unwrap()));
    }

    #[test]
    fn a_code_block_copy_leaves_out_format_characters_as_the_screen_does() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text",
                "text": "```\nsafe\u{202e}evil\u{200b}x\u{2066}y\u{feff}z\n```"}]}]),
        );
        let shown = screen(&mut t, 60, 20);
        assert!(shown.contains("safeevilxyz"), "{shown}");
        let effects = t.update(Msg::Command(scuttle_core::commands::Command::Copy(Some(1))));
        show(&mut t, effects);
        assert_eq!(t.last_copied.as_deref(), Some("safeevilxyz\n"));
    }

    /// Ctrl+O copies what the composer shows: the terminal draws no control character, so
    /// the copy leaves out one that was pasted in.
    #[test]
    fn ctrl_o_leaves_out_a_pasted_control_character_as_the_composer_does() {
        let mut t = tui();
        t.handle(Event::Paste("a\u{7}b\u{1b}c".into()));
        assert!(
            screen(&mut t, 60, 20).contains("abc"),
            "the composer shows abc"
        );
        t.handle(key(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert_eq!(t.last_copied.as_deref(), Some("abc"));
    }

    fn mouse(t: &mut Tui, kind: MouseEventKind, column: u16, row: u16) {
        t.handle(Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }));
    }

    /// The style of the cell at `x`, `y` when the screen is drawn.
    fn style_at(t: &mut Tui, w: u16, h: u16, x: u16, y: u16) -> Style {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| t.draw(f)).unwrap();
        term.backend().buffer()[(x, y)].style()
    }

    fn lit(style: Style) -> bool {
        style.add_modifier.contains(ratatui::style::Modifier::BOLD)
            && style.fg == Some(ratatui::style::Color::Indexed(38))
    }

    #[test]
    fn the_link_under_the_pointer_lights_up_and_the_others_stay_plain() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text",
                "text": "See [the docs](https://coder.com/docs) or [more](https://x.example)."}]}]),
        );
        let shown = screen(&mut t, 60, 20);
        let (x, y) = find(&shown, "the docs");
        let (mx, my) = find(&shown, "more");
        assert!(
            !lit(style_at(&mut t, 60, 20, x, y)),
            "nothing lights up before a move"
        );
        let builds = t.view_builds;
        mouse(&mut t, MouseEventKind::Moved, x + 2, y);
        assert!(lit(style_at(&mut t, 60, 20, x, y)));
        assert!(
            lit(style_at(&mut t, 60, 20, x + 7, y)),
            "the whole link lights up"
        );
        assert!(
            !lit(style_at(&mut t, 60, 20, mx, my)),
            "another link stays plain"
        );
        assert_eq!(t.view_builds, builds, "a move never rebuilds the lines");
        mouse(&mut t, MouseEventKind::Moved, 1, y);
        assert!(
            !lit(style_at(&mut t, 60, 20, x, y)),
            "moving off puts it back"
        );
    }

    #[test]
    fn a_wrapped_link_lights_up_on_every_row() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text",
                "text": "[alpha beta gamma delta epsilon](https://coder.com)"}]}]),
        );
        let shown = screen(&mut t, 20, 16);
        let (ax, ay) = find(&shown, "alpha");
        let (ex, ey) = find(&shown, "epsilon");
        assert!(ey > ay, "the link wraps:\n{shown}");
        mouse(&mut t, MouseEventKind::Moved, ex + 1, ey);
        assert!(
            lit(style_at(&mut t, 20, 16, ax, ay)),
            "the first row lights up too"
        );
    }

    #[test]
    fn no_link_lights_up_under_a_menu_during_a_drag_or_with_the_mouse_off() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text",
                "text": "See [the docs](https://coder.com/docs)."}]}]),
        );
        let shown = screen(&mut t, 60, 20);
        let (x, y) = find(&shown, "the docs");
        mouse(&mut t, MouseEventKind::Moved, x + 2, y);
        assert!(lit(style_at(&mut t, 60, 20, x, y)));
        t.composer.set_text("/");
        assert!(
            !lit(style_at(&mut t, 60, 20, x, y)),
            "the slash menu covers the transcript"
        );
        t.composer.set_text("");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x + 2, y);
        assert!(
            !lit(style_at(&mut t, 60, 20, x, y)),
            "a held button selects instead"
        );
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x + 2, y);
        t.core.mouse = false;
        assert!(
            !lit(style_at(&mut t, 60, 20, x, y)),
            "only while capture is on"
        );
    }

    #[test]
    fn a_link_in_a_table_cell_lights_up_on_both_of_its_rows_and_not_under_help() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text",
                "text": "| head | note |\n| --- | --- |\n| [cargo test](https://x.example) | ok |\n"}]}]),
        );
        let shown = screen(&mut t, 20, 16);
        let (cx, cy) = find(&shown, "cargo");
        let (tx, ty) = find(&shown, "test ");
        assert_eq!(ty, cy + 1, "the cell wraps:\n{shown}");
        mouse(&mut t, MouseEventKind::Moved, tx + 1, ty);
        assert!(
            lit(style_at(&mut t, 20, 16, cx, cy)),
            "the first row lights up"
        );
        assert!(lit(style_at(&mut t, 20, 16, tx + 3, ty)));
        assert!(
            !lit(style_at(&mut t, 20, 16, cx - 2, cy)),
            "the border stays plain"
        );
        t.show_help = true;
        assert!(t.hovered_link().is_empty(), "/help covers the transcript");
        t.show_help = false;
        t.menu_area = Some(Rect::new(tx, ty, 6, 1));
        assert!(
            t.hovered_link().is_empty(),
            "the question menu covers the link"
        );
        t.menu_area = None;
        assert_eq!(t.hovered_link().len(), 2);
    }

    #[test]
    fn a_wrapped_table_link_lights_up_on_both_rows_beside_another_link() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text",
                "text": "| head | note |\n| --- | --- |\n| [cargo test](https://x.example) | [ok](https://y.example) |\n"}]}]),
        );
        let shown = screen(&mut t, 20, 16);
        let (cx, cy) = find(&shown, "cargo");
        let (tx, ty) = find(&shown, "test ");
        let (ox, oy) = find(&shown, "ok");
        assert_eq!(
            (ty, oy),
            (cy + 1, cy),
            "the cell wraps beside the other link:\n{shown}"
        );
        mouse(&mut t, MouseEventKind::Moved, tx + 1, ty);
        assert!(
            lit(style_at(&mut t, 20, 16, cx, cy)),
            "the first row lights up"
        );
        assert!(lit(style_at(&mut t, 20, 16, tx, ty)));
        assert!(
            !lit(style_at(&mut t, 20, 16, ox, oy)),
            "the other link stays plain"
        );
        mouse(&mut t, MouseEventKind::Moved, ox, oy);
        assert!(lit(style_at(&mut t, 20, 16, ox, oy)));
        assert!(!lit(style_at(&mut t, 20, 16, cx, cy)));
        assert!(!lit(style_at(&mut t, 20, 16, tx, ty)));
    }

    #[test]
    fn a_drag_that_ends_off_a_link_leaves_no_link_lit() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text",
                "text": "See [the docs](https://coder.com/docs) and some plain words."}]}]),
        );
        let shown = screen(&mut t, 60, 20);
        let (x, y) = find(&shown, "the docs");
        let (px, py) = find(&shown, "plain");
        mouse(&mut t, MouseEventKind::Moved, x + 2, y);
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x + 2, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), px, py);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), px, py);
        assert!(
            !lit(style_at(&mut t, 60, 20, x, y)),
            "the pointer is on plain text now"
        );
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x + 2, y);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x + 2, y);
        assert!(
            lit(style_at(&mut t, 60, 20, x, y)),
            "a click without a move still counts"
        );
    }

    /// The screen column and row where `needle` first appears.
    fn find(shown: &str, needle: &str) -> (u16, u16) {
        use unicode_width::UnicodeWidthStr;
        shown
            .lines()
            .enumerate()
            .find_map(|(y, row)| {
                row.find(needle)
                    .map(|i| (row[..i].width() as u16, y as u16))
            })
            .unwrap_or_else(|| panic!("{needle:?} not on screen:\n{shown}"))
    }

    fn numbered_rows(t: &mut Tui) {
        let messages: Vec<_> = (1..=30)
            .map(|i| json!({"id": i, "role": "assistant", "content": [{"type": "text", "text": format!("row {i:02}")}]}))
            .collect();
        loaded(t, serde_json::Value::Array(messages));
    }

    fn reversed(t: &mut Tui, w: u16, h: u16, x: u16, y: u16) -> bool {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| t.draw(f)).unwrap();
        term.backend().buffer()[(x, y)]
            .modifier
            .contains(ratatui::style::Modifier::REVERSED)
    }

    /// Whether the first cell of `needle` on screen is highlighted, wherever it now is.
    fn highlighted(t: &mut Tui, w: u16, h: u16, needle: &str) -> bool {
        let (x, y) = find(&screen(t, w, h), needle);
        reversed(t, w, h, x, y)
    }

    /// Presses at `from`, drags to `to`, and releases there.
    fn drag(t: &mut Tui, from: (u16, u16), to: (u16, u16)) {
        mouse(t, MouseEventKind::Down(MouseButton::Left), from.0, from.1);
        mouse(t, MouseEventKind::Drag(MouseButton::Left), to.0, to.1);
        mouse(t, MouseEventKind::Up(MouseButton::Left), to.0, to.1);
    }

    #[test]
    fn dragging_selects_highlights_and_copies_on_release() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": "alpha beta gamma"}]}]),
        );
        let shown = screen(&mut t, 60, 20);
        let (x, y) = find(&shown, "beta");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 9, y);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x + 9, y);
        assert_eq!(t.last_copied.as_deref(), Some("beta gamma"));
        assert!(reversed(&mut t, 60, 20, x, y));
        assert!(reversed(&mut t, 60, 20, x + 9, y));
        assert!(!reversed(&mut t, 60, 20, x + 10, y));
    }

    #[test]
    fn a_selection_across_wrapped_lines_copies_the_original_text() {
        let text = "one two three four five six seven eight nine ten eleven twelve";
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": text}]}]),
        );
        let shown = screen(&mut t, 24, 20);
        let (x0, y0) = find(&shown, "one two");
        let (x1, y1) = find(&shown, "twelve");
        assert!(
            y1 > y0 + 1,
            "the text wraps over at least three rows:\n{shown}"
        );
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x0, y0);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x1 + 5, y1);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x1 + 5, y1);
        assert_eq!(t.last_copied.as_deref(), Some(text));
    }

    #[test]
    fn a_selection_stays_on_its_text_when_scrolled() {
        let mut t = tui();
        numbered_rows(&mut t);
        let shown = screen(&mut t, 40, 24);
        let (x, y) = find(&shown, "row 26");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 5, y);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x + 5, y);
        assert_eq!(t.last_copied.as_deref(), Some("row 26"));
        mouse(&mut t, MouseEventKind::ScrollUp, x, y);
        let scrolled = screen(&mut t, 40, 24);
        assert_eq!(find(&scrolled, "row 26"), (x, y + 3), "{scrolled}");
        assert!(
            reversed(&mut t, 40, 24, x, y + 3),
            "the highlight moved with the text"
        );
        assert!(
            !reversed(&mut t, 40, 24, x, y),
            "and left the old screen row"
        );
    }

    #[test]
    fn streaming_during_a_drag_keeps_the_text_under_the_pointer() {
        let mut t = tui();
        numbered_rows(&mut t);
        let shown = screen(&mut t, 40, 24);
        let (x, y) = find(&shown, "row 26");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        for seq in 1..=10 {
            t.update(live_part(
                seq,
                json!({"type": "text", "text": "streamed line\n\n"}),
            ));
            let now = screen(&mut t, 40, 24);
            assert_eq!(find(&now, "row 26"), (x, y), "the text moved:\n{now}");
        }
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 5, y);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x + 5, y);
        assert_eq!(t.last_copied.as_deref(), Some("row 26"));
    }

    #[test]
    fn a_key_press_clears_the_selection() {
        let mut t = tui();
        numbered_rows(&mut t);
        let shown = screen(&mut t, 40, 24);
        let (x, y) = find(&shown, "row 26");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 5, y);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x + 5, y);
        // The copy notice is drawn in the composer's rule, so the selection stays put.
        assert!(reversed(&mut t, 40, 24, x, y));
        t.handle(key(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(!reversed(&mut t, 40, 24, x, y));
    }

    fn status(s: &str) -> Msg {
        stream(json!({"type": "status", "status": {"status": s}}))
    }

    #[test]
    fn a_drag_keeps_its_text_when_the_activity_row_appears() {
        let mut t = tui();
        numbered_rows(&mut t);
        let shown = screen(&mut t, 40, 24);
        let (x, y) = find(&shown, "row 26");
        let (bx, by) = find(&shown, "row 30");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        t.update(status("running"));
        let now = screen(&mut t, 40, 24);
        assert!(
            now.contains("Working…"),
            "the activity row appeared:\n{now}"
        );
        assert_eq!(find(&now, "row 26"), (x, y), "the text moved:\n{now}");
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 5, y);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x + 5, y);
        assert_eq!(t.last_copied.as_deref(), Some("row 26"));
        assert!(
            highlighted(&mut t, 40, 24, "row 26"),
            "the highlight is on the text"
        );

        // The press lands on the last transcript row, which the activity row then covers.
        let mut t = tui();
        numbered_rows(&mut t);
        assert_eq!(find(&screen(&mut t, 40, 24), "row 30"), (bx, by));
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), bx, by);
        t.update(status("running"));
        screen(&mut t, 40, 24);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), bx + 5, by);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), bx + 5, by);
        assert_eq!(t.last_copied.as_deref(), Some("row 30"));
    }

    #[test]
    fn a_drag_keeps_its_text_when_the_activity_row_disappears() {
        let mut t = tui();
        numbered_rows(&mut t);
        t.update(status("running"));
        let shown = screen(&mut t, 40, 24);
        assert!(shown.contains("Working…"), "{shown}");
        let (x, y) = find(&shown, "row 26");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        t.update(status("waiting"));
        let now = screen(&mut t, 40, 24);
        assert!(!now.contains("Working…"), "the activity row left:\n{now}");
        assert_eq!(find(&now, "row 26"), (x, y), "the text moved:\n{now}");
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 5, y);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x + 5, y);
        assert_eq!(t.last_copied.as_deref(), Some("row 26"));
        assert!(
            highlighted(&mut t, 40, 24, "row 26"),
            "the highlight is on the text"
        );
    }

    #[test]
    fn a_selection_across_a_rule_leaves_the_rule_out() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([
                {"id": 1, "role": "assistant", "content": [
                    {"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args": {"command": "ls"}}
                ]},
                {"id": 2, "role": "tool", "content": [
                    {"type": "tool-result", "tool_call_id": "a", "tool_name": "execute", "result": {"output": "ok"}}
                ]},
                {"id": 3, "role": "assistant", "content": [{"type": "text", "text": "All fixed."}]}
            ]),
        );
        let shown = screen(&mut t, 40, 20);
        let (x0, y0) = find(&shown, "execute");
        let (x1, y1) = find(&shown, "All fixed.");
        let (_, rule) = find(&shown, "────");
        assert!(y0 < rule && rule < y1, "{shown}");
        drag(&mut t, (x0, y0), (x1 + 9, y1));
        let copied = t.last_copied.clone().expect("copied");
        assert!(copied.starts_with("execute"), "{copied:?}");
        assert!(copied.ends_with("All fixed."), "{copied:?}");
        assert!(!copied.contains('─'), "the rule is left out: {copied:?}");
        assert!(
            !copied.contains("\n\n\n"),
            "the rule leaves one paragraph break: {copied:?}"
        );
        assert!(
            !reversed(&mut t, 40, 20, 20, rule),
            "the rule is not highlighted"
        );
    }

    #[test]
    fn a_click_acts_on_release_and_survives_a_jiggle() {
        let (mut t, row) = tui_with_a_code_block();
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), 2, row);
        assert_eq!(t.last_copied, None, "a press alone does nothing");
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), 2, row);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), 2, row);
        assert_eq!(
            t.last_copied.as_deref(),
            Some("echo hi\n"),
            "a movement within the cell is still a click"
        );
    }

    #[test]
    fn a_click_toggles_a_tool_block() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args": {"command": "ls"}}
            ]}]),
        );
        screen(&mut t, 60, 20);
        let row = t
            .row_of(|target| matches!(target, HitTarget::Toggle(_)))
            .expect("tool block on screen");
        click(&mut t, row);
        assert_eq!(t.toggles.len(), 1, "the click toggled the block");
        click(&mut t, row);
        assert!(t.toggles.is_empty(), "a second click toggles it back");
    }

    #[test]
    fn an_empty_selection_copies_nothing() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": "short"}]}]),
        );
        let shown = screen(&mut t, 60, 20);
        let (_, y) = find(&shown, "short");
        let notices = t.core.notices.len();
        drag(&mut t, (40, y), (50, y));
        assert_eq!(t.last_copied, None);
        assert_eq!(t.core.notices.len(), notices, "no notice");
    }

    #[test]
    fn with_the_mouse_off_nothing_is_selected() {
        let mut t = tui();
        numbered_rows(&mut t);
        let shown = screen(&mut t, 40, 24);
        let (x, y) = find(&shown, "row 26");
        t.update(Msg::Command(scuttle_core::commands::Command::Mouse));
        assert!(!t.core.mouse);
        drag(&mut t, (x, y), (x + 5, y));
        assert_eq!(t.last_copied, None);
        assert!(!reversed(&mut t, 40, 24, x, y));
    }

    #[test]
    fn turning_the_mouse_off_mid_drag_releases_the_view() {
        let mut t = tui();
        numbered_rows(&mut t);
        let shown = screen(&mut t, 40, 24);
        let (x, y) = find(&shown, "row 26");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 5, y);
        for e in t.update(Msg::Command(scuttle_core::commands::Command::Mouse)) {
            t.apply_ui_effect(&e);
        }
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x + 5, y);
        assert_eq!(t.last_copied, None, "the drag ended with the capture");
        assert!(!reversed(&mut t, 40, 24, x, y));
    }

    #[test]
    fn clicks_are_ignored_while_an_overlay_is_showing() {
        let (mut t, row) = tui_with_a_code_block();
        t.apply_ui_effect(&Effect::ShowHelp);
        click(&mut t, row);
        assert_eq!(t.last_copied, None, "help box");
        t.show_help = false;
        t.core.update(Msg::ModelsLoaded(vec![]));
        t.apply_ui_effect(&Effect::ShowPicker(scuttle_core::app::Picker::Model));
        click(&mut t, row);
        assert_eq!(t.last_copied, None, "picker");
        t.overlay = None;
        t.composer.set_text("/co");
        click(&mut t, row);
        assert_eq!(t.last_copied, None, "slash menu");
        t.composer.set_text("");
        click(&mut t, row);
        assert!(t.last_copied.is_some());
    }

    #[test]
    fn the_editor_file_is_private_new_and_removed_afterwards() {
        let mut t = tui();
        t.composer.set_text("draft");
        let mut seen = None;
        t.edit_with(|path| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(path)?.permissions().mode();
                assert_eq!(mode & 0o777, 0o600);
            }
            assert_eq!(std::fs::read_to_string(path)?, "draft");
            seen = Some(path.to_owned());
            Ok(())
        })
        .unwrap();
        assert!(!seen.unwrap().exists());
    }

    #[test]
    fn the_handoff_round_trip_resumes_even_when_leaving_fails() {
        let mut ran = false;
        let mut resumed = false;
        let (handed, resume) = handoff_round_trip(
            || Err(std::io::Error::other("leave failed")),
            || {
                ran = true;
                Ok(())
            },
            || {
                resumed = true;
                Ok(())
            },
        );
        assert!(matches!(handed, Handed::NotHandedOver(_)), "{handed:?}");
        assert!(resume.is_ok());
        assert!(!ran, "the program must not run on a half-restored terminal");
        assert!(resumed);
    }

    #[test]
    fn a_failed_resume_quits_with_an_error() {
        let mut t = tui();
        let effects = t.finish_editor(Err(std::io::Error::other("tty gone")));
        assert_eq!(effects, vec![Effect::Quit]);
        assert!(t.take_fatal().unwrap().contains("tty gone"));
        assert!(t.finish_editor(Ok(())).is_empty());
        assert_eq!(t.take_fatal(), None);
    }

    #[test]
    fn returning_from_the_editor_requests_a_full_redraw() {
        let mut t = tui();
        assert!(!t.take_full_redraw());
        t.edit_with(|path| std::fs::write(path, "edited\n"))
            .unwrap();
        assert_eq!(t.composer.text(), "edited");
        assert!(t.take_full_redraw());
        assert!(!t.take_full_redraw());
    }

    fn screen_at(t: &mut Tui, w: u16, h: u16, now: Instant) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| t.draw_at(f, now)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Loads a chat with `messages` into the Tui.
    fn loaded(t: &mut Tui, messages: serde_json::Value) {
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap();
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: serde_json::from_value(messages).unwrap(),
        });
    }

    fn stream(v: serde_json::Value) -> Msg {
        Msg::Stream(coder_sdk::StreamEvent {
            kind: coder_sdk::StreamEventType::parse(v["type"].as_str().unwrap_or_default()),
            event: serde_json::from_value(v.clone()).ok(),
            raw: v,
        })
    }

    fn live_part(seq: i64, part: serde_json::Value) -> Msg {
        stream(
            json!({"type": "message_part", "message_part": {"history_version": 1, "generation_attempt": 1, "seq": seq, "part": part}}),
        )
    }

    #[test]
    fn each_turn_picks_a_spinner_and_a_pinned_style_always_wins() {
        use crate::activity::{SpinnerStyle, next_seed};
        use scuttle_core::config::SpinnerSetting;
        let seed = 1;
        let mut t = Tui::new(
            scuttle_core::config::LocalConfig::default(),
            None,
            Theme::terminal(true),
            Welcome {
                url: "https://x".into(),
                user: "nick".into(),
                art: vec![],
                art_accent: true,
                show: true,
                tip: true,
            },
            seed,
        );
        loaded(&mut t, json!([]));
        t.update(stream(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        assert_eq!(
            t.spinner,
            SpinnerStyle::pick(SpinnerSetting::Random, next_seed(seed))
        );
        // A seed of 1 steps to a number that picks `SpinnerStyle::ALL[1]`, which is `Line`.
        assert_eq!(t.spinner, SpinnerStyle::Line);
        let epoch = t.epoch;
        let shown = screen_at(&mut t, 60, 16, epoch);
        assert!(shown.contains("- Working…"), "{shown}");
        t.update(stream(
            json!({"type": "status", "status": {"status": "waiting"}}),
        ));
        t.config.spinner = SpinnerSetting::Bar;
        t.update(stream(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        assert_eq!(
            t.spinner,
            SpinnerStyle::Bar,
            "the setting wins over the seed"
        );
    }

    #[test]
    fn the_spinner_shows_from_submit_and_animates() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        loaded(&mut t, json!([]));
        let now = Instant::now();
        assert_eq!(t.animation_deadline(now), None, "no timer while idle");
        for c in "hi".chars() {
            t.handle(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            t.animation_deadline(now),
            Some(now + crate::activity::SPINNER_INTERVAL)
        );
        let first = screen_at(&mut t, 60, 16, now);
        assert!(first.contains("Waiting for the agent…"), "{first}");
        let later = screen_at(&mut t, 60, 16, now + crate::activity::SPINNER_INTERVAL);
        assert_ne!(first, later, "the spinner advanced");
    }

    /// A Tui whose transcript fills more than a 60 by 16 screen, so its latest rows sit at
    /// the bottom, where the menus and overlays draw.
    fn long_tui() -> Tui {
        let mut t = tui();
        let messages: Vec<serde_json::Value> = (1..=30)
            .map(|i| json!({"id": i, "role": "user", "content": [{"type": "text", "text": format!("message {i}")}]}))
            .collect();
        loaded(&mut t, json!(messages));
        t.update(stream(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        t
    }

    /// The question menu is drawn only while the agent waits, when no thinking or tool
    /// activity repeats a marker, so this pins only that the rows it covers are counted.
    #[test]
    fn the_question_menu_counts_as_covering_the_transcript() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        loaded(
            &mut t,
            json!([{"id": 2, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "q1",
                "tool_name": "ask_user_question", "args": {"questions": [{"header": "Scope", "question": "Which crate?",
                "options": [{"label": "core", "description": "state"}, {"label": "tui", "description": "drawing"}]}]}}]}]),
        );
        screen(&mut t, 70, 24);
        let menu = t.menu_area.expect("the question menu is drawn");
        assert_eq!(covered_by(t.area, Some(menu)), menu.height);
        assert_eq!(covered_by(t.area, None), 0);
    }

    #[test]
    fn a_marker_under_an_overlay_leaves_the_activity_row_showing() {
        let mut t = long_tui();
        t.update(live_part(1, json!({"type": "reasoning", "text": "hmm"})));
        let shown = screen(&mut t, 60, 16);
        assert!(!shown.contains("Thinking…"), "the marker says it: {shown}");
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        let covered = screen(&mut t, 60, 16);
        assert!(covered.contains("Thinking…"), "{covered}");
    }

    #[test]
    fn a_marker_under_the_slash_menu_or_the_picker_leaves_the_activity_row_showing() {
        let mut t = long_tui();
        t.update(live_part(1, json!({"type": "reasoning", "text": "hmm"})));
        assert!(!screen(&mut t, 60, 16).contains("Thinking…"));
        t.composer.set_text("/");
        let covered = screen(&mut t, 60, 16);
        assert!(
            covered.contains("Thinking…"),
            "under the slash menu: {covered}"
        );
        t.composer.set_text("");
        assert!(!screen(&mut t, 60, 16).contains("Thinking…"));
        t.core.update(Msg::ModelsLoaded(
            serde_json::from_value(json!([
                {"id": uuid::Uuid::new_v4(), "display_name": "Thinker", "enabled": true, "is_default": true, "reasoning_efforts": ["low", "high"]}
            ]))
            .unwrap(),
        ));
        let effects = t.update(Msg::Submit("/effort".into()));
        show(&mut t, effects);
        assert!(t.picker.is_some());
        let covered = screen(&mut t, 60, 16);
        assert!(covered.contains("Thinking…"), "under the picker: {covered}");
    }

    #[test]
    fn the_spinner_keeps_going_while_the_agent_streams() {
        let mut t = tui();
        loaded(&mut t, json!([]));
        t.update(stream(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        assert!(screen(&mut t, 60, 16).contains("Working…"));
        t.update(live_part(1, json!({"type": "reasoning", "text": "hmm"})));
        // The transcript animates the reasoning and the tool call, so the row gives way.
        let thinking = screen(&mut t, 60, 16);
        assert!(
            !t.view.spinners.is_empty() && !thinking.contains("Thinking…"),
            "{thinking}"
        );
        assert!(
            thinking.contains("Thinking"),
            "the transcript's marker says it: {thinking}"
        );
        t.update(live_part(2, json!({"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args_delta": "{"})));
        let tool = screen(&mut t, 60, 16);
        assert!(
            !t.view.spinners.is_empty() && !tool.contains("Running execute…"),
            "{tool}"
        );
        assert!(
            tool.contains("execute"),
            "the transcript's marker says it: {tool}"
        );
        t.update(live_part(3, json!({"type": "text", "text": "Done"})));
        assert!(screen(&mut t, 60, 16).contains("Writing…"));
        t.update(stream(
            json!({"type": "status", "status": {"status": "waiting"}}),
        ));
        let idle = screen(&mut t, 60, 16);
        assert!(
            !idle.contains("Writing…") && !idle.contains("Working…"),
            "{idle}"
        );
        assert_eq!(t.animation_deadline(Instant::now()), None);
    }

    #[test]
    fn a_spinner_tick_reuses_the_transcript_lines() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": "hello"}]}]),
        );
        t.update(stream(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        let now = Instant::now();
        screen_at(&mut t, 60, 16, now);
        assert_eq!(t.view_builds, 1);
        t.tick();
        let ticked = screen_at(&mut t, 60, 16, now + crate::activity::SPINNER_INTERVAL);
        assert_eq!(
            t.view_builds, 1,
            "a timer frame must not rebuild the transcript"
        );
        assert!(
            ticked.contains("hello") && ticked.contains("Working…"),
            "{ticked}"
        );
        t.update(Msg::Refresh);
        screen_at(&mut t, 60, 16, now);
        assert_eq!(t.view_builds, 2, "a message rebuilds");
        t.tick();
        screen_at(&mut t, 70, 16, now);
        assert_eq!(t.view_builds, 3, "a resize rebuilds even on a timer frame");
    }
    #[test]
    fn the_tint_fills_the_row_on_light_and_dark_terminals() {
        use ratatui::style::Color;
        for dark in [true, false] {
            let theme = Theme::terminal(dark);
            let mut t = Tui::new(
                scuttle_core::config::LocalConfig::default(),
                None,
                theme,
                Welcome {
                    url: "https://x".into(),
                    user: "nick".into(),
                    art: vec![],
                    art_accent: true,
                    show: true,
                    tip: true,
                },
                0,
            );
            loaded(
                &mut t,
                json!([{"id": 1, "role": "user", "content": [{"type": "text", "text": "my question"}]}]),
            );
            let (w, h) = (50u16, 12u16);
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| t.draw(f)).unwrap();
            let buf = term.backend().buffer().clone();
            let y = (0..h)
                .find(|&y| {
                    (0..w)
                        .map(|x| buf[(x, y)].symbol())
                        .collect::<String>()
                        .contains("my question")
                })
                .unwrap();
            let tint = theme.user_tint.bg.unwrap();
            for x in 1..w - 1 {
                assert_eq!(buf[(x, y)].bg, tint, "dark={dark} x={x}");
            }
            assert_ne!(buf[(0, y)].bg, tint, "the margin stays untinted");
            let marker = (0..w).find(|&x| buf[(x, y)].symbol() == "›").unwrap();
            assert_eq!(
                buf[(marker, y)].fg,
                Color::Blue,
                "the marker keeps its color"
            );
        }
        assert_eq!(
            Theme::terminal(false).user_tint.bg,
            Some(Color::Indexed(254))
        );
        assert_eq!(
            Theme::terminal(true).user_tint.bg,
            Some(Color::Indexed(236))
        );
    }

    #[test]
    fn the_tint_covers_wrapped_rows_and_nothing_else() {
        let mut t = tui();
        let long = "wrapped words ".repeat(6);
        loaded(
            &mut t,
            json!([
                {"id": 1, "role": "user", "content": [{"type": "text", "text": long.trim()}]},
                {"id": 2, "role": "assistant", "content": [{"type": "text", "text": "Answer."}]}
            ]),
        );
        let (w, h) = (30u16, 16u16);
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| t.draw(f)).unwrap();
        let buf = term.backend().buffer().clone();
        let tint = t.theme.user_tint.bg.unwrap();
        let tinted: Vec<u16> = (0..h).filter(|&y| buf[(w / 2, y)].bg == tint).collect();
        assert!(
            tinted.len() >= 2,
            "the continuation row is tinted too: {tinted:?}"
        );
        for &y in &tinted {
            assert_ne!(buf[(0, y)].bg, tint, "left margin");
            assert_ne!(buf[(w - 1, y)].bg, tint, "right margin");
        }
        let answer = (0..h)
            .find(|&y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .contains("Answer.")
            })
            .unwrap();
        assert!(!tinted.contains(&answer));
    }

    #[test]
    fn choosing_an_organization_saves_it_to_the_config() {
        let dir = std::env::temp_dir().join(format!("scuttle-org-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        let mut t = Tui::new(
            scuttle_core::config::LocalConfig::default(),
            Some(path.clone()),
            Theme::terminal(true),
            Welcome {
                url: "https://x".into(),
                user: String::new(),
                art: vec![],
                art_accent: true,
                show: true,
                tip: true,
            },
            0,
        );
        let id = uuid::Uuid::new_v4();
        assert!(t.apply_ui_effect(&Effect::SaveOrganization(id)));
        assert_eq!(
            scuttle_core::config::load(&path).unwrap().organization,
            Some(id)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn choosing_an_effort_on_a_blank_chat_saves_it_to_the_config() {
        let dir = std::env::temp_dir().join(format!("scuttle-effort-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        let mut t = Tui::new(
            scuttle_core::config::LocalConfig::default(),
            Some(path.clone()),
            Theme::terminal(true),
            Welcome {
                url: "https://x".into(),
                user: String::new(),
                art: vec![],
                art_accent: true,
                show: true,
                tip: true,
            },
            0,
        );
        let model = uuid::Uuid::new_v4();
        t.core.update(Msg::ModelsLoaded(
            serde_json::from_value(json!([
                {"id": model, "display_name": "Thinker", "enabled": true, "is_default": true, "reasoning_efforts": ["low", "high"]}
            ]))
            .unwrap(),
        ));
        let effects = t.core.update(Msg::EffortChosen("high".into()));
        assert_eq!(
            effects,
            vec![Effect::SaveEffort {
                model,
                effort: "high".into()
            }]
        );
        for e in &effects {
            assert!(t.apply_ui_effect(e));
        }
        assert_eq!(
            scuttle_core::config::load(&path)
                .unwrap()
                .efforts
                .get(&model),
            Some(&"high".to_string())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_mouse_save_to_a_read_only_config_says_so() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("scuttle-ro-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "mouse = true\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let mut t = Tui::new(
            scuttle_core::config::LocalConfig::default(),
            Some(path.clone()),
            Theme::terminal(true),
            Welcome {
                url: "https://x".into(),
                user: String::new(),
                art: vec![],
                art_accent: true,
                show: true,
                tip: true,
            },
            0,
        );
        t.apply_ui_effect(&Effect::SetMouse(false));
        assert!(
            t.core
                .notices
                .iter()
                .any(|n| matches!(n, Notice::Error(m) if m.contains("config.toml is read-only"))),
            "{:?}",
            t.core.notices
        );
        assert!(t.config.mouse, "the file still says true");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "mouse = true\n");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn an_effort_save_to_a_read_only_config_says_so() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("scuttle-ro-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "mouse = true\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let mut t = Tui::new(
            scuttle_core::config::LocalConfig::default(),
            Some(path.clone()),
            Theme::terminal(true),
            Welcome {
                url: "https://x".into(),
                user: String::new(),
                art: vec![],
                art_accent: true,
                show: true,
                tip: true,
            },
            0,
        );
        let model = uuid::Uuid::new_v4();
        t.apply_ui_effect(&Effect::SaveEffort {
            model,
            effort: "high".into(),
        });
        assert!(
            t.core
                .notices
                .iter()
                .any(|n| matches!(n, Notice::Error(m) if m.contains("config.toml is read-only"))),
            "{:?}",
            t.core.notices
        );
        assert_eq!(t.config.efforts.get(&model), None, "nothing was saved");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "mouse = true\n");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_effort_save_keeps_the_choice_and_shows_no_error() {
        let dir = std::env::temp_dir().join(format!("scuttle-effort-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        std::fs::create_dir_all(&dir).unwrap();
        // Unparsable TOML makes the write fail inside `config::set_effort`.
        std::fs::write(&path, "not valid toml [[[").unwrap();
        let mut t = Tui::new(
            scuttle_core::config::LocalConfig::default(),
            Some(path.clone()),
            Theme::terminal(true),
            Welcome {
                url: "https://x".into(),
                user: String::new(),
                art: vec![],
                art_accent: true,
                show: true,
                tip: true,
            },
            0,
        );
        let notices_before = t.core.notices.len();
        assert!(t.apply_ui_effect(&Effect::SaveEffort {
            model: uuid::Uuid::new_v4(),
            effort: "high".into(),
        }));
        assert_eq!(
            t.core.notices.len(),
            notices_before,
            "a failed effort save shows no error, unlike a failed organization save"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_organization_picker_choice_reaches_the_core() {
        use scuttle_core::app::OrgRef;
        let mut t = tui();
        let org = |name: &str, is_default| OrgRef {
            id: uuid::Uuid::new_v4(),
            name: name.to_lowercase(),
            display_name: name.into(),
            is_default,
            can_create_chats: true,
        };
        let (product, coder) = (org("Product", false), org("Coder", true));
        t.core.update(Msg::OrganizationsLoaded(vec![
            product.clone(),
            coder.clone(),
        ]));
        t.core.update(Msg::Started {
            org_id: coder.id,
            open_chat: None,
        });
        let effects = t
            .core
            .update(Msg::Command(scuttle_core::commands::Command::Organization(
                None,
            )));
        for e in &effects {
            assert!(t.apply_ui_effect(e), "{e:?}");
        }
        assert!(t.overlay.is_some());
        t.handle(key(KeyCode::Up, KeyModifiers::NONE));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(t.overlay.is_none());
        assert!(
            effects.contains(&Effect::SaveOrganization(product.id)),
            "{effects:?}"
        );
        assert_eq!(t.core.org_id, Some(product.id));
    }

    #[test]
    fn with_every_organization_denied_the_picker_dims_them_all_and_refuses_each() {
        use scuttle_core::app::{Notice, OrgRef};
        let mut t = tui();
        let org = |name: &str, is_default| OrgRef {
            id: uuid::Uuid::new_v4(),
            name: name.to_lowercase(),
            display_name: name.into(),
            is_default,
            can_create_chats: false,
        };
        let (product, coder) = (org("Product", false), org("Coder", true));
        t.core.update(Msg::OrganizationsLoaded(vec![
            product.clone(),
            coder.clone(),
        ]));
        t.core.update(Msg::Started {
            org_id: coder.id,
            open_chat: None,
        });
        for (moves, label) in [(0, "Coder"), (1, "Product")] {
            let effects =
                t.core
                    .update(Msg::Command(scuttle_core::commands::Command::Organization(
                        None,
                    )));
            for e in &effects {
                assert!(t.apply_ui_effect(e), "{e:?}");
            }
            let overlay = t.overlay.as_ref().expect("the picker opens");
            let ctx = ViewCtx {
                app: &t.core,
                theme: &t.theme,
                now_unix: 0,
                offset: chrono::FixedOffset::east_opt(0).unwrap(),
                elapsed: Duration::ZERO,
                width: 80,
                pin_icon: "📌",
            };
            let kinds: Vec<table::RowKind> =
                overlay.view(&ctx).rows.iter().map(|r| r.kind).collect();
            assert_eq!(
                kinds,
                [table::RowKind::Dimmed, table::RowKind::Dimmed],
                "every row is dimmed"
            );
            for _ in 0..moves {
                t.handle(key(KeyCode::Up, KeyModifiers::NONE));
            }
            let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
            assert!(t.overlay.is_none());
            assert!(
                !effects
                    .iter()
                    .any(|e| matches!(e, Effect::SaveOrganization(_))),
                "choosing {label} saves nothing: {effects:?}"
            );
            assert_eq!(t.core.org_id, Some(coder.id));
            assert_eq!(
                t.core.notices.last(),
                Some(&Notice::Error(format!(
                    "You do not have permission to create chats in {label}."
                )))
            );
        }
    }

    #[test]
    fn slash_new_clears_the_view_and_keeps_the_draft() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": "old answer"}]}]),
        );
        t.toggles.insert((Some(1), 0));
        assert!(screen(&mut t, 60, 16).contains("old answer"));
        t.composer.set_text("draft to keep");
        let effects = t.update(Msg::Command(scuttle_core::commands::Command::New));
        for e in &effects {
            t.apply_ui_effect(e);
        }
        assert!(effects.contains(&Effect::CloseStream));
        assert!(t.toggles.is_empty());
        assert_eq!(t.composer.text(), "draft to keep");
        let shown = screen(&mut t, 60, 16);
        assert!(!shown.contains("old answer"), "{shown}");
        assert!(
            shown.contains("scuttle"),
            "the welcome screen is back: {shown}"
        );
    }

    #[test]
    fn slash_new_ends_a_held_drag_and_drops_the_selection_and_scroll() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        numbered_rows(&mut t);
        let shown = screen(&mut t, 40, 24);
        let (x, y) = find(&shown, "row 26");
        drag(&mut t, (x, y), (x + 5, y));
        assert!(t.selection.is_some());
        mouse(&mut t, MouseEventKind::ScrollUp, x, y);
        assert!(t.scroll_from_bottom > 0);
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        assert!(t.drag.is_some(), "the button is held");
        let effects = t.update(Msg::Command(scuttle_core::commands::Command::New));
        for e in &effects {
            t.apply_ui_effect(e);
        }
        assert!(t.drag.is_none());
        assert!(t.selection.is_none());
        assert_eq!(t.scroll_from_bottom, 0);
        let shown = screen(&mut t, 40, 24);
        assert!(!shown.contains("row 26"), "{shown}");
    }

    #[test]
    fn a_resize_drops_the_selection_and_ends_a_held_drag() {
        let mut t = tui();
        numbered_rows(&mut t);
        let shown = screen(&mut t, 40, 24);
        let (x, y) = find(&shown, "row 26");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 5, y);
        screen(&mut t, 40, 24);
        assert!(t.selection.is_some(), "a redraw at the same width keeps it");
        assert!(t.drag.is_some());
        screen(&mut t, 50, 24);
        assert!(t.selection.is_none());
        assert!(t.drag.is_none());
    }

    #[test]
    fn a_history_reset_drops_the_selection_and_ends_a_held_drag() {
        let mut t = tui();
        numbered_rows(&mut t);
        let shown = screen(&mut t, 40, 24);
        let (x, y) = find(&shown, "row 26");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 5, y);
        t.update(stream(json!({"type": "history_reset"})));
        t.update(stream(
            json!({"type": "message", "message": {"id": 1, "role": "assistant", "content": [{"type": "text", "text": "replaced"}]}}),
        ));
        screen(&mut t, 40, 24);
        assert!(
            t.selection.is_some(),
            "the replacement is buffered, so nothing changed yet"
        );
        t.update(stream(
            json!({"type": "status", "status": {"status": "waiting"}}),
        ));
        let shown = screen(&mut t, 40, 24);
        assert!(shown.contains("replaced"), "{shown}");
        assert!(t.selection.is_none());
        assert!(t.drag.is_none());
    }

    #[test]
    fn a_web_url_that_opened_no_browser_is_copied() {
        let mut t = tui();
        let url = "https://coder.example.com/agents/x".to_owned();
        assert!(t.apply_ui_effect(&Effect::CopyWebUrl(url.clone())));
        assert_eq!(t.last_copied.as_deref(), Some(url.as_str()));
    }

    #[test]
    fn workspace_opens_its_panel_and_copies_the_ssh_command() {
        let mut t = tui();
        assert!(t.apply_ui_effect(&Effect::ShowWorkspace));
        assert!(matches!(t.overlay, Some(Overlay::WorkspaceDetails(_))));
        assert!(t.apply_ui_effect(&Effect::CopyText {
            text: "ssh main.dev.nick.coder".into(),
            what: "the SSH command",
        }));
        assert_eq!(t.last_copied.as_deref(), Some("ssh main.dev.nick.coder"));
        assert_eq!(
            copy_url_notice(
                "the SSH command",
                "ssh main.dev.nick.coder",
                CopyOutcome::Copied
            ),
            Notice::Info("Copied the SSH command: ssh main.dev.nick.coder".into())
        );
    }

    #[test]
    fn the_web_url_copy_notice_names_the_url() {
        let url = "https://coder.example.com/agents/x";
        assert_eq!(
            web_copy_notice(url, CopyOutcome::Copied),
            Notice::Info("Copied the chat URL: https://coder.example.com/agents/x".into())
        );
        assert_eq!(
            web_copy_notice(
                url,
                CopyOutcome::CopiedWithWarning("tmux may drop it".into())
            ),
            Notice::Info(
                "Copied the chat URL: https://coder.example.com/agents/x. tmux may drop it".into()
            )
        );
        assert_eq!(
            web_copy_notice(url, CopyOutcome::Failed("no terminal".into())),
            Notice::Error("Could not copy the chat URL: https://coder.example.com/agents/x".into())
        );
    }

    fn release(t: &mut Tui, column: u16, row: u16) -> Vec<Effect> {
        t.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }))
    }

    fn open_docs() -> Vec<Effect> {
        vec![Effect::OpenLink("https://coder.com/docs".into())]
    }

    #[test]
    fn a_click_on_a_link_opens_it_and_a_drag_over_it_selects_it() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": "See [the docs](https://coder.com/docs) here."}]}]),
        );
        let (x, y) = find(&screen(&mut t, 60, 20), "the docs");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x + 2, y);
        assert_eq!(release(&mut t, x + 2, y), open_docs());
        assert_eq!(t.last_copied, None);
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 7, y);
        assert!(
            release(&mut t, x + 7, y).is_empty(),
            "a drag never opens the link"
        );
        assert_eq!(t.last_copied.as_deref(), Some("the docs"));
        let (x, y) = find(&screen(&mut t, 60, 20), "here.");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        assert!(
            release(&mut t, x, y).is_empty(),
            "the text beside a link is not the link"
        );
    }

    #[test]
    fn a_click_on_the_second_row_of_a_wrapped_link_opens_it() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": "Read [the very long documentation title here](https://coder.com/docs) please"}]}]),
        );
        let shown = screen(&mut t, 22, 20);
        let (x, y) = find(&shown, "documentation");
        let (_, first) = find(&shown, "Read");
        assert!(y > first, "the link wraps onto a second row:\n{shown}");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x + 3, y);
        assert_eq!(release(&mut t, x + 3, y), open_docs());
        let (x, y) = find(&shown, "please");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        assert!(release(&mut t, x, y).is_empty());
    }

    #[test]
    fn a_link_click_while_streaming_opens_the_link_under_the_press() {
        let mut t = tui();
        numbered_rows(&mut t);
        t.update(stream(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        t.update(live_part(
            1,
            json!({"type": "text", "text": "See [the docs](https://coder.com/docs)."}),
        ));
        let (x, y) = find(&screen(&mut t, 60, 16), "the docs");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        t.update(live_part(
            2,
            json!({"type": "text", "text": "\n\nMore.\n\nAnd more.\n\nStill more."}),
        ));
        let during = screen(&mut t, 60, 16);
        assert_eq!(
            find(&during, "the docs"),
            (x, y),
            "the held press pins the view:\n{during}"
        );
        assert_eq!(release(&mut t, x, y), open_docs());
    }

    #[test]
    fn link_text_in_a_code_block_copies_the_code_instead() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": "```\n[the docs](https://coder.com/docs)\n```"}]}]),
        );
        let (x, y) = find(&screen(&mut t, 60, 20), "the docs");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        assert!(release(&mut t, x, y).is_empty(), "code is never a link");
        assert_eq!(
            t.last_copied.as_deref(),
            Some("[the docs](https://coder.com/docs)\n")
        );
    }

    #[test]
    fn a_link_that_opened_no_browser_is_copied() {
        let mut t = tui();
        let url = "https://coder.com/docs".to_owned();
        assert!(!t.apply_ui_effect(&Effect::OpenLink(url.clone())));
        assert!(t.apply_ui_effect(&Effect::CopyLink(url.clone())));
        assert_eq!(t.last_copied.as_deref(), Some(url.as_str()));
        assert_eq!(
            copy_url_notice("the link", &url, CopyOutcome::Copied),
            Notice::Info("Copied the link: https://coder.com/docs".into())
        );
    }

    #[test]
    fn a_link_that_is_not_a_web_link_says_why_it_was_copied() {
        assert_eq!(
            link_copy_notice("https://coder.com/docs", CopyOutcome::Copied),
            Notice::Info("Copied the link: https://coder.com/docs".into()),
            "a web link that opened no browser needs no reason"
        );
        for url in ["file:///etc/passwd", "javascript:alert(1)", "-a Calculator"] {
            assert_eq!(
                link_copy_notice(url, CopyOutcome::Copied),
                Notice::Info(format!(
                    "Only http and https links open in a browser. Copied the link: {url}"
                )),
                "{url}"
            );
        }
        assert_eq!(
            link_copy_notice("file:///x", CopyOutcome::Failed("no terminal".into())),
            Notice::Error(
                "Only http and https links open in a browser. Could not copy the link: file:///x"
                    .into()
            )
        );
    }

    /// The first transcript row containing `needle` that is not the activity row, and its
    /// first visible character.
    fn marker_of(shown: &str, needle: &str) -> char {
        shown
            .lines()
            .find(|l| l.contains(needle) && !l.contains('…'))
            .and_then(|l| l.trim_start().chars().next())
            .unwrap_or_else(|| panic!("{needle:?} not on screen:\n{shown}"))
    }

    #[test]
    fn the_thinking_line_animates_with_the_spinner() {
        let mut t = tui();
        loaded(&mut t, json!([]));
        t.update(stream(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        t.update(live_part(1, json!({"type": "reasoning", "text": "hmm"})));
        let start = t.epoch;
        let first = screen_at(&mut t, 60, 16, start);
        assert_eq!(
            marker_of(&first, " Thinking").to_string(),
            crate::activity::spinner_frame(Duration::ZERO)
        );
        t.tick();
        let later = screen_at(&mut t, 60, 16, start + crate::activity::SPINNER_INTERVAL);
        assert_eq!(
            marker_of(&later, " Thinking").to_string(),
            crate::activity::spinner_frame(crate::activity::SPINNER_INTERVAL)
        );
        assert_eq!(t.view_builds, 1, "the frame came from a reused view");
        t.update(live_part(2, json!({"type": "text", "text": "Done."})));
        let answered = screen_at(&mut t, 60, 16, start);
        assert_eq!(
            marker_of(&answered, " Thinking"),
            '∴',
            "a finished thought stops"
        );
    }

    #[test]
    fn animating_a_long_transcript_reuses_its_lines() {
        let mut t = tui();
        let messages: Vec<_> = (1..=500)
            .map(|i| json!({"id": i, "role": "assistant", "content": [{"type": "text", "text": format!("message {i} with **markdown** and `code`")}]}))
            .collect();
        loaded(&mut t, serde_json::Value::Array(messages));
        t.update(stream(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        t.update(live_part(
            1,
            json!({"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args_delta": "{\"command\": \"make\"}"}),
        ));
        let start = t.epoch;
        let first = screen_at(&mut t, 60, 16, start);
        assert_eq!(t.view_builds, 1);
        assert_eq!(
            t.view.spinners.len(),
            1,
            "one marker animates, however long the chat"
        );
        assert_eq!(
            marker_of(&first, "execute(").to_string(),
            crate::activity::spinner_frame(Duration::ZERO)
        );
        for step in 1..=3u32 {
            t.tick();
            let at = crate::activity::SPINNER_INTERVAL * step;
            let shown = screen_at(&mut t, 60, 16, start + at);
            assert_eq!(
                marker_of(&shown, "execute(").to_string(),
                crate::activity::spinner_frame(at)
            );
        }
        assert_eq!(
            t.view_builds, 1,
            "timer frames never rebuild the transcript"
        );
    }

    #[test]
    fn copying_an_animated_row_copies_its_static_marker() {
        let mut t = tui();
        loaded(&mut t, json!([]));
        t.update(stream(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        t.update(live_part(
            1,
            json!({"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args_delta": "{\"command\": \"make\"}"}),
        ));
        let start = t.epoch;
        let shown = screen_at(&mut t, 60, 16, start);
        let (x, y) = find(&shown, "execute(make)");
        assert_eq!(
            marker_of(&shown, "execute(").to_string(),
            crate::activity::spinner_frame(Duration::ZERO),
            "the marker is animating"
        );
        drag(&mut t, (x - 2, y), (x + 12, y));
        assert_eq!(t.last_copied.as_deref(), Some("◌ execute(make)"));
    }

    #[test]
    fn a_sent_message_stills_an_unfinished_call_of_the_turn_before() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        loaded(
            &mut t,
            json!([
                {"id": 1, "role": "user", "content": [{"type": "text", "text": "build it"}]},
                {"id": 2, "role": "assistant", "content": [
                    {"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args": {"command": "make"}}
                ]}
            ]),
        );
        for c in "again".chars() {
            t.handle(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        let now = Instant::now();
        assert!(t.animation_deadline(now).is_some(), "the agent is awaited");
        let shown = screen_at(&mut t, 60, 16, now);
        assert!(shown.contains("Waiting for the agent…"), "{shown}");
        assert_eq!(marker_of(&shown, "execute(make)"), '◌');
        assert!(t.view.spinners.is_empty());
    }

    #[test]
    fn the_welcome_screen_names_the_signed_in_user() {
        let mut t = Tui::new(
            scuttle_core::config::LocalConfig::default(),
            None,
            Theme::terminal(true),
            Welcome {
                url: "https://x".into(),
                user: String::new(),
                art: vec![],
                art_accent: true,
                show: true,
                tip: true,
            },
            0,
        );
        assert!(!screen(&mut t, 60, 20).contains("Signed in as"));
        t.update(Msg::UserLoaded(scuttle_core::app::UserRef {
            id: uuid::Uuid::new_v4(),
            username: "nick".into(),
        }));
        assert!(screen(&mut t, 60, 20).contains("Signed in as nick"));
    }

    fn chats_tui() -> (Tui, uuid::Uuid, uuid::Uuid) {
        use scuttle_core::chat_list::ListQuery;
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let (root, child) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let chats = serde_json::from_value(json!([
            {"id": root, "title": "Fix the flaky watch reconnect test", "status": "waiting",
             "updated_at": "2026-09-30T10:00:00Z", "files": [], "mcp_server_ids": [],
             "inline_mcp_servers": [], "labels": {},
             "children": [{"id": child, "title": "explore", "status": "running", "parent_chat_id": root,
                "updated_at": "2026-09-30T10:00:00Z", "children": [], "files": [],
                "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}]},
            {"id": uuid::Uuid::new_v4(), "title": "Draft the M2 design", "status": "waiting",
             "updated_at": "2026-09-30T09:00:00Z", "children": [], "files": [],
             "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}
        ]))
        .unwrap();
        t.core.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats,
        });
        (t, root, child)
    }

    fn show(t: &mut Tui, effects: Vec<Effect>) {
        for e in &effects {
            t.apply_ui_effect(e);
        }
    }

    #[test]
    fn the_chats_columns_follow_the_terminal_width_that_help_names() {
        use scuttle_core::chat_list::ListQuery;
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let chats = serde_json::from_value(json!([
            {"id": uuid::Uuid::new_v4(), "title": "Fix the watch", "status": "waiting",
             "updated_at": "2026-09-30T10:00:00Z", "children": [], "files": [],
             "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {},
             "last_turn_summary": "Fixing the CI",
             "diff_status": {"pr_number": 12, "pull_request_state": "merged",
                "pull_request_draft": false}}
        ]))
        .unwrap();
        t.core.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats,
        });
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        for (width, pr, summary) in [
            (99, false, false),
            (100, false, true),
            (119, false, true),
            (120, true, true),
        ] {
            let text = screen(&mut t, width, 20);
            assert_eq!(text.contains("PR #12"), pr, "pull request at {width}");
            assert_eq!(
                text.contains("Fixing the CI"),
                summary,
                "summary at {width}"
            );
        }
    }

    /// A Tui with an open chat titled "Watch fix".
    fn info_tui() -> Tui {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let id = uuid::Uuid::new_v4();
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(
                serde_json::from_value(json!({"id": id, "title": "Watch fix", "children": [],
                "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}))
                .unwrap(),
            ),
            messages: vec![],
        });
        t
    }

    #[test]
    fn slash_usage_shows_the_limits_and_esc_closes_it() {
        let mut t = info_tui();
        let effects = t.update(Msg::Submit("/usage".into()));
        show(&mut t, effects);
        assert!(matches!(t.overlay, Some(Overlay::Usage(_))));
        t.update(Msg::SpendLoaded {
            spend: Box::new(
                serde_json::from_value(json!({"current_spend_micros": 1_200_000,
                    "effective_budget": {"spend_limit_micros": 50_000_000, "limit_source": "group"}}))
                .unwrap(),
            ),
            generation: 1,
        });
        let shown = flowed(&screen(&mut t, 80, 24));
        assert!(shown.contains("$1.20 of $50.00 (2%)"), "{shown}");
        assert!(shown.contains("Your group's budget"), "{shown}");
        t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(t.overlay.is_none());
        assert!(
            !t.core.usage_open,
            "the core stops refreshing the cost for it"
        );
    }

    #[test]
    fn a_chat_switch_under_usage_never_shows_the_last_chats_cost() {
        let mut t = info_tui();
        let left = t.core.chat_id.unwrap();
        let effects = t.update(Msg::Submit("/usage".into()));
        show(&mut t, effects);
        let cost = |micros: i64| {
            serde_json::from_value::<coder_sdk::types::CodersdkChatCost>(
                json!({"total_cost_micros": micros, "request_count": 3}),
            )
            .unwrap()
        };
        t.update(Msg::ForChat {
            chat: left,
            msg: Box::new(Msg::CostLoaded {
                cost: cost(9_990_000),
                generation: 1,
            }),
        });
        assert!(flowed(&screen(&mut t, 80, 24)).contains("$9.99 over 3 requests"));
        let other = uuid::Uuid::new_v4();
        t.update(Msg::OpenChat(other));
        let shown = flowed(&screen(&mut t, 80, 24));
        assert!(matches!(t.overlay, Some(Overlay::Usage(_))), "{shown}");
        assert!(!shown.contains("$9.99"), "{shown}");
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(
                serde_json::from_value(json!({"id": other, "children": [], "files": [],
                "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}))
                .unwrap(),
            ),
            messages: vec![],
        });
        assert!(
            t.poll_usage(Instant::now())
                .iter()
                .any(|e| matches!(e, Effect::FetchCost { chat, .. } if *chat == other)),
            "the opened chat's cost is asked for"
        );
        t.update(Msg::ForChat {
            chat: left,
            msg: Box::new(Msg::CostLoaded {
                cost: cost(9_990_000),
                generation: 2,
            }),
        });
        let shown = flowed(&screen(&mut t, 80, 24));
        assert!(
            !shown.contains("$9.99"),
            "a late reply for the chat left is dropped:\n{shown}"
        );
        assert!(shown.contains("Loading…"), "{shown}");
    }

    #[test]
    fn slash_info_shows_the_chat_and_esc_closes_it() {
        let mut t = info_tui();
        let effects = t.update(Msg::Submit("/info".into()));
        show(&mut t, effects);
        let shown = screen(&mut t, 80, 24);
        assert!(shown.contains("Watch fix"), "{shown}");
        assert!(shown.contains("Cost"), "{shown}");
        let effects = t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(effects.is_empty());
        assert!(t.overlay.is_none());
        assert!(t.core.info_panel.is_none());
    }

    /// The screen's text inside the overlay border, its rows joined by single spaces.
    fn flowed(shown: &str) -> String {
        shown
            .lines()
            .map(|l| l.trim_matches(|c: char| c == '│' || c.is_whitespace()))
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// A Tui with `/info` open on a chat with a long summary, a priced cost, and a warning.
    fn long_info_tui() -> (Tui, &'static str) {
        let summary = "Traced every caller of the watch socket through the runtime and the core, \
            then fixed the two that dropped events during a reconnect. Ends here.";
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let id = uuid::Uuid::new_v4();
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(serde_json::from_value(json!({"id": id, "title": "Watch fix",
                "parent_chat_id": uuid::Uuid::new_v4(), "summary": summary, "warnings": ["The last row."], "children": [],
                "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap()),
            messages: vec![],
        });
        let effects = t.update(Msg::Submit("/info".into()));
        show(&mut t, effects);
        t.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::CostLoaded {
                cost: serde_json::from_value(json!({"total_cost_micros": 1230000,
                    "request_count": 4, "unpriced_request_count": 1}))
                .unwrap(),
                generation: 1,
            }),
        });
        (t, summary)
    }

    #[test]
    fn long_info_values_wrap_instead_of_being_cut_off() {
        let (mut t, summary) = long_info_tui();
        let shown = flowed(&screen(&mut t, 80, 40));
        assert!(
            shown.contains("$1.23 over 4 requests, for the whole chat tree. Excludes unpriced usage from 1 request."),
            "{shown}"
        );
        assert!(shown.contains(summary), "{shown}");
    }

    #[test]
    fn a_short_terminal_scrolls_info_to_its_last_row() {
        let (mut t, _) = long_info_tui();
        let top = screen(&mut t, 80, 12);
        assert!(top.contains("Watch fix"), "{top}");
        assert!(!top.contains("The last row."), "{top}");
        for _ in 0..30 {
            t.handle(key(KeyCode::Down, KeyModifiers::NONE));
            screen(&mut t, 80, 12);
        }
        let bottom = screen(&mut t, 80, 12);
        assert!(bottom.contains("The last row."), "{bottom}");
        t.handle(key(KeyCode::Up, KeyModifiers::NONE));
        assert_ne!(
            screen(&mut t, 80, 12),
            bottom,
            "scrolling past the end does not need undoing"
        );
        t.handle(key(KeyCode::PageUp, KeyModifiers::NONE));
        t.handle(key(KeyCode::PageUp, KeyModifiers::NONE));
        assert!(screen(&mut t, 80, 12).contains("Watch fix"));
    }

    #[test]
    fn ctrl_c_closes_info_and_tells_the_core() {
        let mut t = info_tui();
        let effects = t.update(Msg::Submit("/info".into()));
        show(&mut t, effects);
        assert!(matches!(t.overlay, Some(Overlay::Info(_))));
        t.handle(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(t.overlay.is_none());
        assert!(t.core.info_panel.is_none(), "the cost stops refetching");
    }

    #[test]
    fn slash_chats_lists_chats_with_its_query_and_enter_opens_one() {
        let (mut t, root, _) = chats_tui();
        let effects = t.update(Msg::Submit("/chats watch".into()));
        show(&mut t, effects);
        let shown = screen(&mut t, 80, 20);
        assert!(shown.contains("Search: watch"), "{shown}");
        assert!(
            shown.contains("Fix the flaky watch reconnect test"),
            "{shown}"
        );
        assert!(!shown.contains("Draft the M2 design"), "{shown}");
        assert!(shown.contains("+1"), "{shown}");
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(t.overlay.is_none());
        assert!(effects.contains(&Effect::LoadChat(root)), "{effects:?}");
    }

    #[test]
    fn the_last_row_searches_the_server_and_shows_its_results() {
        use scuttle_core::chat_list::ListQuery;
        let (mut t, _, _) = chats_tui();
        show(&mut t, vec![Effect::ShowChats("fix watch".into())]);
        assert!(screen(&mut t, 80, 20).contains("Search all chats for \u{201c}fix watch\u{201d}"));
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            effects,
            vec![Effect::FetchChats {
                query: ListQuery::Search("fix watch".into()),
                offset: 0
            }]
        );
        assert!(screen(&mut t, 80, 20).contains("Searching all chats…"));
        t.update(Msg::ChatsLoaded {
            query: ListQuery::Search("fix watch".into()),
            offset: 0,
            chats: serde_json::from_value(json!([{"id": uuid::Uuid::new_v4(), "title": "Body mentions it",
                "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}]))
            .unwrap(),
        });
        let shown = screen(&mut t, 80, 20);
        assert!(shown.contains("Body mentions it"), "{shown}");
        assert!(
            !shown.contains("Fix the flaky"),
            "search results replace the local matches"
        );
        t.handle(key(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(screen(&mut t, 80, 20).contains("Fix the flaky"));
    }

    #[test]
    fn ctrl_r_opens_chats_and_right_shows_the_subagents() {
        let (mut t, _, _) = chats_tui();
        let effects = t.handle(key(KeyCode::Char('r'), KeyModifiers::CONTROL));
        show(&mut t, effects);
        assert!(t.overlay.is_some());
        assert!(!screen(&mut t, 80, 20).contains("explore"));
        t.handle(key(KeyCode::Right, KeyModifiers::NONE));
        assert!(screen(&mut t, 80, 20).contains("explore"));
        t.handle(key(KeyCode::Left, KeyModifiers::NONE));
        assert!(!screen(&mut t, 80, 20).contains("explore"));
    }

    #[test]
    fn tab_cycles_the_filters_and_the_archived_tab_loads_its_page() {
        let (mut t, _, _) = chats_tui();
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        t.handle(key(KeyCode::Tab, KeyModifiers::NONE));
        let shown = screen(&mut t, 80, 20);
        assert!(shown.contains("[active]"), "{shown}");
        assert!(
            shown.contains("Fix the flaky"),
            "a running subagent makes its root active"
        );
        assert!(!shown.contains("Draft the M2 design"), "{shown}");
        t.handle(key(KeyCode::Tab, KeyModifiers::NONE));
        let effects = t.handle(key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(
            effects,
            vec![Effect::FetchChats {
                query: scuttle_core::chat_list::ListQuery::Archived,
                offset: 0
            }]
        );
        assert!(screen(&mut t, 80, 20).contains("Loading chats…"));
    }

    #[test]
    fn running_chats_spin_and_the_header_says_when_updates_pause() {
        let (mut t, _, _) = chats_tui();
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        let now = Instant::now();
        assert!(
            t.animation_deadline(now).is_some(),
            "a running subagent spins"
        );
        assert!(screen(&mut t, 80, 20).contains("live updates paused"));
        t.update(Msg::WatchConnected);
        assert!(!screen(&mut t, 80, 20).contains("live updates paused"));
    }

    #[test]
    fn chat_spinners_move_on_timer_frames_without_rebuilding_the_rows() {
        use crate::activity::{SPINNER_INTERVAL, spinner_frame};
        let (mut t, _, _) = chats_tui();
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        let start = t.epoch;
        let first = screen_at(&mut t, 80, 20, start);
        assert!(
            first.contains(&format!("+1 {}", spinner_frame(Duration::ZERO))),
            "the collapsed root shows its running subagent: {first}"
        );
        t.handle(key(KeyCode::Right, KeyModifiers::NONE));
        let expanded = screen_at(&mut t, 80, 20, start);
        let row = |shown: &str| {
            shown
                .lines()
                .find(|l| l.contains("explore"))
                .unwrap()
                .to_owned()
        };
        assert!(
            row(&expanded).contains(spinner_frame(Duration::ZERO)),
            "{expanded}"
        );
        let builds = t.overlay_builds;
        for step in 1..=3u32 {
            t.tick();
            let at = SPINNER_INTERVAL * step;
            let shown = screen_at(&mut t, 80, 20, start + at);
            assert!(row(&shown).contains(spinner_frame(at)), "{shown}");
            assert!(
                !row(&shown).contains(spinner_frame(Duration::ZERO)),
                "{shown}"
            );
        }
        assert_eq!(
            t.overlay_builds, builds,
            "timer frames paint the spinners over the rows built before"
        );
        t.handle(key(KeyCode::Left, KeyModifiers::NONE));
        screen_at(&mut t, 80, 20, start);
        assert_eq!(t.overlay_builds, builds + 1, "a key rebuilds the rows");
        t.tick();
        screen_at(&mut t, 80, 20, start + Duration::from_secs(61));
        assert_eq!(
            t.overlay_builds,
            builds + 2,
            "a timer frame in a new minute rebuilds the relative times"
        );
        t.tick();
        screen_at(&mut t, 120, 20, start + Duration::from_secs(61));
        assert_eq!(
            t.overlay_builds,
            builds + 3,
            "a timer frame at a new width rebuilds the optional columns"
        );
    }

    #[test]
    fn opening_the_chat_already_loading_from_chats_does_nothing() {
        let (mut t, root, _) = chats_tui();
        assert!(
            t.update(Msg::OpenChat(root))
                .contains(&Effect::LoadChat(root))
        );
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        assert!(t.overlay.is_some());
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(t.overlay.is_none());
        assert!(effects.is_empty(), "{effects:?}");
    }

    #[test]
    fn help_lists_ctrl_r_for_chats() {
        assert!(
            crate::help::KEYS
                .iter()
                .any(|k| k.keys == "Ctrl+R" && k.action.contains("/chats"))
        );
    }

    #[test]
    fn an_idle_chat_list_redraws_when_the_minute_turns() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        t.core.update(Msg::ChatsLoaded {
            query: scuttle_core::chat_list::ListQuery::Default,
            offset: 0,
            chats: serde_json::from_value(json!([{"id": uuid::Uuid::new_v4(), "title": "idle",
                "status": "waiting", "updated_at": "2026-09-30T10:00:00Z", "children": [],
                "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}]))
            .unwrap(),
        });
        let now = t.epoch + Duration::from_millis(250);
        assert_eq!(
            t.animation_deadline(now),
            None,
            "nothing to redraw while closed"
        );
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        let secs = t.epoch_unix.rem_euclid(60) as u64;
        assert_eq!(
            t.animation_deadline(now),
            Some(t.epoch + Duration::from_secs(60 - secs)),
            "the next minute moves the relative times on"
        );
        t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(t.animation_deadline(now), None);
    }

    /// `/chats` open on two idle roots: "Ship the popup" on workspace "dev", listed first, and
    /// "Plain chat" with no workspace. Returns the two chats and the workspace.
    fn archive_tui() -> (Tui, uuid::Uuid, uuid::Uuid, uuid::Uuid) {
        use scuttle_core::chat_list::ListQuery;
        let mut t = tui();
        started(&mut t);
        let (with_ws, bare, ws) = (
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
        );
        let chats = serde_json::from_value(json!([
            {"id": with_ws, "title": "Ship the popup", "status": "waiting", "workspace_id": ws,
             "updated_at": "2026-09-30T10:00:00Z", "children": [], "files": [],
             "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}},
            {"id": bare, "title": "Plain chat", "status": "waiting",
             "updated_at": "2026-09-30T09:00:00Z", "children": [], "files": [],
             "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}
        ]))
        .unwrap();
        t.core.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats,
        });
        t.core.update(Msg::WorkspacesLoaded(vec![
            scuttle_core::app::WorkspaceRef {
                id: ws,
                name: "dev".into(),
                ..Default::default()
            },
        ]));
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        (t, with_ws, bare, ws)
    }

    /// `/chats` open on idle roots titled by `titles`, newest first, in `query`'s list.
    fn titled_chats_tui(
        titles: &[&str],
        query: scuttle_core::chat_list::ListQuery,
    ) -> (Tui, Vec<uuid::Uuid>) {
        let mut t = tui();
        started(&mut t);
        let ids: Vec<uuid::Uuid> = titles.iter().map(|_| uuid::Uuid::new_v4()).collect();
        let archived = query == scuttle_core::chat_list::ListQuery::Archived;
        let chats = serde_json::from_value(json!(
            titles
                .iter()
                .zip(&ids)
                .enumerate()
                .map(
                    |(i, (title, id))| json!({"id": id, "title": title, "status": "waiting",
                "archived": archived, "updated_at": format!("2026-09-30T{:02}:00:00Z", 20 - i),
                "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [],
                "labels": {}})
                )
                .collect::<Vec<_>>()
        ))
        .unwrap();
        t.core.update(Msg::ChatsLoaded {
            query,
            offset: 0,
            chats,
        });
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        (t, ids)
    }

    /// The chat `/chats` selects once drawn.
    fn selected_chat(t: &mut Tui) -> Option<uuid::Uuid> {
        screen(t, 100, 24);
        let (view, _, _) = t.overlay_view.as_ref()?;
        match t
            .overlay
            .as_ref()?
            .state()
            .selected_row(view)
            .map(|r| r.key.clone())
        {
            Some(table::RowKey::Chat(id)) => Some(id),
            _ => None,
        }
    }

    /// Archives the selected chat through the confirmation, and lands the server's answer.
    fn archive_selected(t: &mut Tui, chat: uuid::Uuid) {
        t.handle(ctrl_a());
        screen(t, 100, 24);
        assert!(!enter(t).is_empty(), "the archive was asked for");
        t.update(Msg::ChatUpdated {
            chat,
            change: scuttle_core::app::ChatChange::Archived(true),
        });
    }

    #[test]
    fn archiving_a_row_selects_the_row_above_it_or_the_first() {
        use scuttle_core::chat_list::ListQuery;
        let (mut t, ids) = titled_chats_tui(&["A", "B", "C", "D", "E"], ListQuery::Default);
        for _ in 0..3 {
            t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        }
        assert_eq!(selected_chat(&mut t), Some(ids[3]));
        archive_selected(&mut t, ids[3]);
        assert_eq!(selected_chat(&mut t), Some(ids[2]), "the row above D");
        // C is the row the fallback chose, and B is above it, while A is the first row.
        archive_selected(&mut t, ids[2]);
        assert_eq!(
            selected_chat(&mut t),
            Some(ids[1]),
            "the row above C, not the first"
        );
        archive_selected(&mut t, ids[1]);
        assert_eq!(selected_chat(&mut t), Some(ids[0]), "the row above B");
        archive_selected(&mut t, ids[0]);
        assert_eq!(
            selected_chat(&mut t),
            Some(ids[4]),
            "no row above: the new first"
        );
        archive_selected(&mut t, ids[4]);
        assert_eq!(selected_chat(&mut t), None, "nothing left to select");
    }

    #[test]
    fn a_refused_archive_keeps_the_selection() {
        use scuttle_core::chat_list::ListQuery;
        let (mut t, ids) = titled_chats_tui(&["A", "B", "C"], ListQuery::Default);
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        t.handle(ctrl_a());
        screen(&mut t, 100, 24);
        enter(&mut t);
        t.update(Msg::ChatUpdateFailed {
            chat: ids[1],
            change: scuttle_core::app::ChatChange::Archived(true),
            message: "the chat is running".into(),
        });
        assert_eq!(selected_chat(&mut t), Some(ids[1]));
    }

    #[test]
    fn the_neighbor_is_found_by_id_when_a_refresh_reorders_the_rows() {
        use scuttle_core::chat_list::ListQuery;
        let (mut t, ids) = titled_chats_tui(&["A", "B", "C", "D"], ListQuery::Default);
        for _ in 0..3 {
            t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        }
        assert_eq!(selected_chat(&mut t), Some(ids[3]));
        // One refresh, with no draw between: B is newest, C second, A third, and D is gone.
        let chat = |i: usize, title: &str, hour: u8| {
            json!({"id": ids[i], "title": title, "status": "waiting",
                "updated_at": format!("2026-09-30T{hour:02}:00:00Z"),
                "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [],
                "labels": {}})
        };
        let chats = serde_json::from_value(json!([
            chat(1, "B", 22),
            chat(2, "C", 21),
            chat(0, "A", 20)
        ]))
        .unwrap();
        t.core.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats,
        });
        assert_eq!(
            selected_chat(&mut t),
            Some(ids[2]),
            "C was above D, wherever it is now, and it is not the first row"
        );
    }

    #[test]
    fn unarchiving_in_the_archived_tab_selects_the_row_above_it() {
        use scuttle_core::chat_list::ListQuery;
        let (mut t, ids) = titled_chats_tui(&["W", "X", "Y", "Z"], ListQuery::Archived);
        for _ in 0..3 {
            t.handle(key(KeyCode::Tab, KeyModifiers::NONE));
        }
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(selected_chat(&mut t), Some(ids[2]));
        assert!(!t.handle(ctrl_a()).is_empty(), "an unarchive needs no box");
        t.update(Msg::ChatUpdated {
            chat: ids[2],
            change: scuttle_core::app::ChatChange::Archived(false),
        });
        assert_eq!(
            selected_chat(&mut t),
            Some(ids[1]),
            "the row above, not the first"
        );
    }

    fn ctrl_a() -> Event {
        key(KeyCode::Char('a'), KeyModifiers::CONTROL)
    }

    fn prompt_open(t: &Tui) -> bool {
        matches!(&t.overlay, Some(Overlay::Chats(c)) if c.archive.is_some())
    }

    #[test]
    fn archiving_asks_how_and_a_stray_enter_only_archives() {
        let (mut t, with_ws, _, _) = archive_tui();
        assert!(t.handle(ctrl_a()).is_empty());
        let shown = screen(&mut t, 100, 24);
        assert!(
            shown.contains("Archive \u{201c}Ship the popup\u{201d}?"),
            "{shown}"
        );
        assert!(
            shown.contains("Archive and delete workspace \u{201c}dev\u{201d}"),
            "{shown}"
        );
        assert!(shown.contains("Cancel"), "{shown}");
        assert!(
            !shown.contains("for good"),
            "no armed warning while Archive is selected: {shown}"
        );
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            effects,
            vec![Effect::UpdateChat {
                chat: with_ws,
                change: scuttle_core::app::ChatChange::Archived(true)
            }]
        );
        assert!(!prompt_open(&t), "the box closes");
        assert!(
            matches!(t.overlay, Some(Overlay::Chats(_))),
            "and /chats stays open"
        );
    }

    fn enter(t: &mut Tui) -> Vec<Effect> {
        t.handle(key(KeyCode::Enter, KeyModifiers::NONE))
    }

    fn press_d(t: &mut Tui) -> Vec<Effect> {
        t.handle(key(KeyCode::Char('d'), KeyModifiers::NONE))
    }

    /// The popup with the delete row selected and armed.
    fn armed_tui() -> (Tui, uuid::Uuid, uuid::Uuid) {
        let (mut t, with_ws, _, ws) = archive_tui();
        t.handle(ctrl_a());
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        assert!(enter(&mut t).is_empty(), "the first Enter only arms it");
        (t, with_ws, ws)
    }

    #[test]
    fn deleting_the_workspace_takes_enter_to_arm_and_d_to_confirm() {
        let (mut t, with_ws, _, ws) = archive_tui();
        t.handle(ctrl_a());
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        assert!(
            screen(&mut t, 100, 24)
                .contains("Up and Down choose, Enter arms the delete, Esc cancels"),
        );
        assert!(enter(&mut t).is_empty());
        let shown = screen(&mut t, 100, 24);
        assert!(
            shown.contains("Press D to delete workspace \u{201c}dev\u{201d} for good"),
            "{shown}"
        );
        assert_eq!(
            press_d(&mut t),
            vec![Effect::ArchiveAndDeleteWorkspace {
                chat: with_ws,
                workspace: ws
            }]
        );
        assert!(!prompt_open(&t));
    }

    #[test]
    fn a_held_enter_never_deletes_the_workspace() {
        let (mut t, _, _) = armed_tui();
        for _ in 0..200 {
            assert!(enter(&mut t).is_empty());
            assert!(prompt_open(&t));
        }
        let repeat = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('d'),
            KeyModifiers::NONE,
            crossterm::event::KeyEventKind::Repeat,
        ));
        assert!(t.handle(repeat).is_empty(), "a repeated d is ignored too");
        assert!(prompt_open(&t));
    }

    #[test]
    fn d_deletes_only_after_the_row_is_armed_and_nothing_disarmed_it() {
        let (mut t, _, _, _) = archive_tui();
        t.handle(ctrl_a());
        assert!(press_d(&mut t).is_empty(), "d on Archive does nothing");
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        assert!(
            press_d(&mut t).is_empty(),
            "d on an unarmed row does nothing"
        );
        assert!(prompt_open(&t));
        let (mut t, _, _) = armed_tui();
        t.handle(key(KeyCode::Up, KeyModifiers::NONE));
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        assert!(press_d(&mut t).is_empty(), "a move disarmed it");
        let (mut t, _, _) = armed_tui();
        t.handle(key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(press_d(&mut t).is_empty(), "any other key disarms it");
        let (mut t, _, _) = armed_tui();
        mouse(&mut t, MouseEventKind::ScrollUp, 5, 5);
        mouse(&mut t, MouseEventKind::ScrollDown, 5, 5);
        assert!(press_d(&mut t).is_empty(), "a wheel tick disarms it");
        assert!(prompt_open(&t));
    }

    #[test]
    fn a_workspace_that_changes_under_the_box_deletes_nothing() {
        use scuttle_core::chat_list::ListQuery;
        let (mut t, with_ws, _, _) = archive_tui();
        t.handle(ctrl_a());
        let other = uuid::Uuid::new_v4();
        t.core.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats: serde_json::from_value(json!([
                {"id": with_ws, "title": "Ship the popup", "status": "waiting",
                 "workspace_id": other, "updated_at": "2026-09-30T10:05:00Z", "children": [],
                 "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}
            ]))
            .unwrap(),
        });
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        assert!(enter(&mut t).is_empty());
        let effects = press_d(&mut t);
        assert!(effects.is_empty(), "{effects:?}");
        assert!(
            screen(&mut t, 100, 24).contains("workspace changed"),
            "the user is told why"
        );
    }

    /// The style of the first cell of `text` on the `w` by `h` screen, if it shows.
    fn style_of(t: &mut Tui, w: u16, h: u16, text: &str) -> Option<ratatui::style::Style> {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| t.draw(f)).unwrap();
        let buf = term.backend().buffer();
        (0..h).find_map(|y| {
            (0..w).find_map(|x| {
                let rest: String = (x..w).map(|x| buf[(x, y)].symbol().to_owned()).collect();
                rest.starts_with(text).then(|| buf[(x, y)].style())
            })
        })
    }

    #[test]
    fn the_archive_hint_says_what_enter_does_on_each_row() {
        let (mut t, _, _, _) = archive_tui();
        t.handle(ctrl_a());
        let confirms = "Up and Down choose, Enter confirms, Esc cancels";
        let arms = "Up and Down choose, Enter arms the delete, Esc cancels";
        let armed = "Press D to delete workspace \u{201c}dev\u{201d} for good, Esc cancels";
        assert!(screen(&mut t, 100, 24).contains(confirms), "on Archive");
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        let shown = screen(&mut t, 100, 24);
        assert!(shown.contains(arms), "on the delete row: {shown}");
        assert!(!shown.contains("Enter confirms"), "{shown}");
        assert_eq!(
            style_of(&mut t, 100, 24, arms).map(|s| s.fg),
            Some(t.theme.dim.fg)
        );
        t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(screen(&mut t, 100, 24).contains(armed), "once armed");
        assert_eq!(
            style_of(&mut t, 100, 24, armed).map(|s| s.fg),
            Some(t.theme.error.fg),
            "the armed hint uses the error style"
        );
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        assert!(screen(&mut t, 100, 24).contains(confirms), "on Cancel");
    }

    /// The archive box's top-left corner and size on `shown`: the `┌` before its title, and
    /// the `┐` and `└` that close it.
    fn archive_box(shown: &str) -> (usize, usize, usize, usize) {
        box_around(shown, " Archive \u{201c}")
    }

    #[test]
    fn the_archive_box_keeps_its_place_and_size_on_every_row_and_once_armed() {
        for (w, h) in [(100, 24), (60, 20), (44, 24), (34, 14)] {
            let (mut t, _, _, _) = archive_tui();
            t.handle(ctrl_a());
            let on_archive = archive_box(&screen(&mut t, w, h));
            t.handle(key(KeyCode::Down, KeyModifiers::NONE));
            let on_delete = archive_box(&screen(&mut t, w, h));
            assert!(enter(&mut t).is_empty());
            let armed = archive_box(&screen(&mut t, w, h));
            t.handle(key(KeyCode::Down, KeyModifiers::NONE));
            let on_cancel = archive_box(&screen(&mut t, w, h));
            assert_eq!(on_archive, on_delete, "{w}x{h}");
            assert_eq!(on_archive, armed, "{w}x{h}");
            assert_eq!(on_archive, on_cancel, "{w}x{h}");
        }
    }

    #[test]
    fn the_delete_choice_flags_itself_in_the_error_style_even_when_narrow() {
        for w in [100, 44, 34] {
            let (mut t, _, _, _) = archive_tui();
            t.handle(ctrl_a());
            let shown = screen(&mut t, w, 24);
            assert!(shown.contains("(can't be undone)"), "{w}: {shown}");
            assert!(!shown.contains("for good"), "no warning row: {shown}");
            assert_eq!(
                style_of(&mut t, w, 24, "(can't be undone)").map(|s| s.fg),
                Some(t.theme.error.fg),
                "{w}"
            );
        }
        let (mut t, _, _, _) = archive_tui();
        t.handle(ctrl_a());
        let narrow = screen(&mut t, 34, 24);
        assert!(narrow.contains("(can't be undone) Delete "), "{narrow}");
        assert!(
            !narrow.contains("Archive an"),
            "the row is never cut mid-word: {narrow}"
        );
        let wide = screen(&mut t, 100, 24);
        assert!(
            wide.contains("Archive and delete workspace \u{201c}dev\u{201d} (can't be undone)"),
            "{wide}"
        );
    }

    #[test]
    fn a_long_workspace_name_gives_way_before_the_flag() {
        let (mut t, _, _, _) = archive_tui();
        t.handle(ctrl_a());
        if let Some(Overlay::Chats(c)) = t.overlay.as_mut() {
            c.archive.as_mut().unwrap().workspace_name = Some("w".repeat(60));
        }
        let shown = screen(&mut t, 60, 20);
        let row = shown
            .lines()
            .find(|l| l.contains("(can't be undone)"))
            .unwrap_or_else(|| panic!("the flag shows whole: {shown}"));
        assert!(row.contains("w\u{2026}\u{201d} (can't be undone)"), "{row}");
    }

    #[test]
    fn a_long_title_ends_in_an_ellipsis_inside_the_border() {
        let (mut t, _, _, _) = archive_tui();
        t.handle(ctrl_a());
        if let Some(Overlay::Chats(c)) = t.overlay.as_mut() {
            c.archive.as_mut().unwrap().title = "Ship the popup ".repeat(10);
        }
        for w in [100, 44] {
            let shown = screen(&mut t, w, 24);
            let (left, top, width, _) = archive_box(&shown);
            let row: Vec<char> = shown.lines().nth(top).unwrap().chars().collect();
            let inside: String = row[left + 1..left + width - 1].iter().collect();
            assert!(inside.contains("\u{2026}\u{201d}?"), "{w}: {inside:?}");
            assert_eq!(row[left + width - 1], '┐', "{w}: the corner stays");
            assert!(
                matches!(row[left + width - 2], ' ' | '─'),
                "{w}: the title stops before the border: {inside:?}"
            );
        }
    }

    #[test]
    fn a_workspace_missing_from_the_list_is_named_by_its_short_id() {
        let (mut t, _, _, ws) = archive_tui();
        t.core.update(Msg::WorkspacesLoaded(vec![]));
        t.handle(ctrl_a());
        let shown = screen(&mut t, 100, 24);
        let short = &ws.to_string()[..8];
        assert!(
            shown.contains(&format!("Archive and delete workspace {short}")),
            "{shown}"
        );
    }

    #[test]
    fn a_second_ctrl_a_archives_without_deleting() {
        let (mut t, with_ws, _, _) = archive_tui();
        t.handle(ctrl_a());
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        let effects = t.handle(ctrl_a());
        assert_eq!(
            effects,
            vec![Effect::UpdateChat {
                chat: with_ws,
                change: scuttle_core::app::ChatChange::Archived(true)
            }],
            "even with the delete armed"
        );
    }

    #[test]
    fn esc_or_cancel_backs_out_and_keeps_chats_open() {
        let (mut t, _, _, _) = archive_tui();
        t.handle(ctrl_a());
        assert!(t.handle(key(KeyCode::Esc, KeyModifiers::NONE)).is_empty());
        assert!(!prompt_open(&t));
        assert!(
            matches!(t.overlay, Some(Overlay::Chats(_))),
            "Esc closed only the box"
        );
        t.handle(ctrl_a());
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        assert!(t.handle(key(KeyCode::Enter, KeyModifiers::NONE)).is_empty());
        assert!(!prompt_open(&t), "Cancel closes the box");
        assert!(matches!(t.overlay, Some(Overlay::Chats(_))));
        t.handle(ctrl_a());
        t.handle(Event::Paste("typed".into()));
        assert!(prompt_open(&t), "a paste goes nowhere while the box shows");
        assert!(
            matches!(&t.overlay, Some(Overlay::Chats(c)) if c.table.filter.is_empty()),
            "and never reaches the filter"
        );
    }

    #[test]
    fn a_chat_without_a_workspace_offers_no_delete() {
        let (mut t, _, bare, _) = archive_tui();
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        t.handle(ctrl_a());
        let shown = screen(&mut t, 100, 24);
        assert!(
            shown.contains("Archive \u{201c}Plain chat\u{201d}?"),
            "{shown}"
        );
        assert!(!shown.contains("Archive and delete"), "{shown}");
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        assert!(
            t.handle(key(KeyCode::Enter, KeyModifiers::NONE)).is_empty(),
            "Down from Archive lands on Cancel"
        );
        assert!(!prompt_open(&t));
        t.handle(ctrl_a());
        assert_eq!(
            t.handle(key(KeyCode::Enter, KeyModifiers::NONE)),
            vec![Effect::UpdateChat {
                chat: bare,
                change: scuttle_core::app::ChatChange::Archived(true)
            }]
        );
    }

    #[test]
    fn the_wheel_moves_the_archive_choice() {
        let (mut t, _, _, _) = archive_tui();
        t.handle(ctrl_a());
        mouse(&mut t, MouseEventKind::ScrollDown, 5, 5);
        assert!(
            screen(&mut t, 100, 24)
                .contains("Up and Down choose, Enter arms the delete, Esc cancels")
        );
        assert!(prompt_open(&t));
    }

    #[test]
    fn wheel_ticks_never_confirm_the_workspace_delete() {
        let (mut t, _, _, _) = archive_tui();
        t.handle(ctrl_a());
        let mut sent = Vec::new();
        for kind in [
            MouseEventKind::ScrollDown,
            MouseEventKind::ScrollDown,
            MouseEventKind::ScrollUp,
            MouseEventKind::ScrollDown,
            MouseEventKind::ScrollDown,
            MouseEventKind::ScrollDown,
        ] {
            sent.extend(t.handle(Event::Mouse(MouseEvent {
                kind,
                column: 5,
                row: 5,
                modifiers: KeyModifiers::NONE,
            })));
        }
        assert!(sent.is_empty(), "{sent:?}");
        assert!(
            matches!(&t.overlay, Some(Overlay::Chats(c)) if c.archive.as_ref().is_some_and(|p| !p.armed)),
            "the wheel never arms the delete"
        );
        assert!(prompt_open(&t));
    }

    #[test]
    fn ctrl_a_unarchives_an_archived_chat_at_once() {
        use scuttle_core::chat_list::{Filter, ListQuery};
        let mut t = tui();
        started(&mut t);
        let id = uuid::Uuid::new_v4();
        t.core.update(Msg::ChatsLoaded {
            query: ListQuery::Archived,
            offset: 0,
            chats: serde_json::from_value(json!([{"id": id, "title": "Old work",
                "status": "waiting", "archived": true, "workspace_id": uuid::Uuid::new_v4(),
                "updated_at": "2026-09-30T10:00:00Z", "children": [], "files": [],
                "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}]))
            .unwrap(),
        });
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        if let Some(Overlay::Chats(c)) = t.overlay.as_mut() {
            c.filter = Filter::Archived;
        }
        assert_eq!(
            t.handle(ctrl_a()),
            vec![Effect::UpdateChat {
                chat: id,
                change: scuttle_core::app::ChatChange::Archived(false)
            }]
        );
        assert!(!prompt_open(&t), "unarchiving asks nothing");
    }

    #[test]
    fn ctrl_a_on_a_running_family_refuses_immediately_without_confirming() {
        let (mut t, _, _) = chats_tui();
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        // Nothing selected yet defaults to the first row: the root with a running child.
        let effects = t.handle(key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert!(effects.is_empty());
        let shown = screen(&mut t, 80, 20);
        assert!(
            !prompt_open(&t),
            "no confirmation step for a refusal the core already knows: {shown}"
        );
        assert!(
            shown.contains("Wait for this chat and its subagents to stop"),
            "{shown}"
        );
    }

    #[test]
    fn ctrl_a_on_a_child_refuses_immediately_without_confirming() {
        let (mut t, _, _) = chats_tui();
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        // Nothing selected yet defaults to the root, so Right expands it in place.
        t.handle(key(KeyCode::Right, KeyModifiers::NONE));
        t.handle(key(KeyCode::Down, KeyModifiers::NONE)); // the expanded child row
        let effects = t.handle(key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert!(effects.is_empty());
        let shown = screen(&mut t, 80, 20);
        assert!(!prompt_open(&t), "{shown}");
        assert!(
            shown.contains("Only a root chat can be archived"),
            "{shown}"
        );
    }

    #[test]
    fn ctrl_e_renames_in_the_editor_and_enter_saves() {
        let (mut t, root, _) = chats_tui();
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        t.handle(key(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert!(screen(&mut t, 80, 20).contains("Rename chat"));
        for _ in 0..4 {
            t.handle(key(KeyCode::Backspace, KeyModifiers::NONE));
        }
        t.handle(key(KeyCode::Char('!'), KeyModifiers::NONE));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            effects,
            vec![Effect::UpdateChat {
                chat: root,
                change: scuttle_core::app::ChatChange::Title(
                    "Fix the flaky watch reconnect !".into()
                )
            }]
        );
        assert!(t.overlay.is_some(), "the list stays open after a rename");
    }

    #[test]
    fn the_title_editor_says_it_is_proposing_until_the_title_arrives() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let id = uuid::Uuid::new_v4();
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(
                serde_json::from_value(json!({"id": id, "children": [], "files": [],
                "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}))
                .unwrap(),
            ),
            messages: vec![],
        });
        t.update(Msg::Submit("/title".into()));
        let shown = screen(&mut t, 60, 16);
        assert!(shown.contains("Title"), "{shown}");
        assert!(shown.contains("Proposing a title…"), "{shown}");
        t.update(Msg::ForChat {
            chat: id,
            msg: Box::new(Msg::TitleProposed {
                title: "Watch fix".into(),
                generation: 1,
            }),
        });
        assert!(screen(&mut t, 60, 16).contains("Watch fix"));
    }

    #[test]
    fn ctrl_keys_are_swallowed_while_the_editor_is_open() {
        let (mut t, _, _) = chats_tui();
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        t.handle(key(KeyCode::Char('e'), KeyModifiers::CONTROL));
        let before = t.core.editor.as_ref().map(|e| e.line.text().to_owned());
        let effects = t.handle(key(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert!(effects.is_empty());
        assert_eq!(
            t.core.editor.as_ref().map(|e| e.line.text().to_owned()),
            before,
            "the ctrl combo reached neither the editor's text nor the overlay's pin action"
        );
        assert!(t.core.editor.is_some(), "the editor is still open");
    }

    #[test]
    fn the_first_ctrl_c_also_closes_the_editor() {
        let (mut t, _, _) = chats_tui();
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        t.handle(key(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert!(t.core.editor.is_some());
        t.handle(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(t.core.editor.is_none());
    }

    #[test]
    fn pasting_while_the_editor_is_open_types_into_it_not_the_composer() {
        let (mut t, _, _) = chats_tui();
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        t.handle(key(KeyCode::Char('e'), KeyModifiers::CONTROL));
        for _ in 0.."Fix the flaky watch reconnect test".len() {
            t.handle(key(KeyCode::Backspace, KeyModifiers::NONE));
        }
        t.handle(Event::Paste("pasted title".into()));
        assert_eq!(
            t.core.editor.as_ref().map(|e| e.line.text().to_owned()),
            Some("pasted title".to_owned())
        );
        assert_eq!(t.composer.text(), "", "the composer stayed empty");
    }

    #[test]
    fn the_editor_scrolls_so_a_long_title_keeps_the_cursor_visible() {
        let text = "Fix the flaky watch reconnect test";
        let cursor = text.chars().count();
        let (visible, column) = visible_editor_text(text, cursor, 10);
        assert!(visible.len() <= 10, "{visible}");
        assert!(visible.ends_with("test"), "{visible}");
        assert!(
            !visible.contains("Fix"),
            "scrolled past the start: {visible}"
        );
        assert_eq!(column, 9, "the cursor sits at the box's last column");

        let (visible, column) = visible_editor_text(text, 0, 10);
        assert_eq!(visible, text, "Home scrolls back to the start");
        assert_eq!(column, 0);
    }

    fn subagent_tui() -> (Tui, uuid::Uuid, uuid::Uuid) {
        let (mut t, root, child) = chats_tui();
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(serde_json::from_value(json!({"id": root, "title": "Fix the flaky watch reconnect test",
                "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap()),
            messages: vec![],
        });
        (t, root, child)
    }

    #[test]
    fn slash_subagents_previews_the_selection_live_and_enter_opens_it() {
        let (mut t, _, child) = subagent_tui();
        let effects = t.update(Msg::Submit("/subagents".into()));
        let generation = match effects.as_slice() {
            [
                Effect::ShowSubagents,
                Effect::OpenPreview {
                    chat, generation, ..
                },
            ] if *chat == child => *generation,
            other => panic!("{other:?}"),
        };
        show(&mut t, effects);
        let shown = screen(&mut t, 80, 24);
        assert!(shown.contains("explore"), "{shown}");
        assert!(shown.contains("Connecting…"), "{shown}");
        t.update(Msg::ForPreview {
            chat: child,
            generation,
            msg: Box::new(stream(
                json!({"type": "message", "message": {"id": 3, "role": "assistant",
                "content": [{"type": "text", "text": "Found 3 callers"}]}}),
            )),
        });
        assert!(screen(&mut t, 80, 24).contains("Found 3 callers"));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(t.overlay.is_none());
        assert!(effects.contains(&Effect::ClosePreview), "{effects:?}");
        assert!(effects.contains(&Effect::LoadChat(child)), "{effects:?}");
    }

    #[test]
    fn esc_on_an_idle_subagent_with_an_empty_composer_opens_the_parent() {
        let (mut t, root, child) = chats_tui();
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(serde_json::from_value(json!({"id": child, "parent_chat_id": root,
                "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap()),
            messages: vec![],
        });
        t.composer.set_text("draft");
        let effects = t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(
            !effects.contains(&Effect::LoadChat(root)),
            "a draft keeps Esc as interrupt"
        );
        t.composer.set_text("");
        let effects = t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(effects.contains(&Effect::LoadChat(root)), "{effects:?}");
    }

    #[test]
    fn esc_on_a_running_subagent_interrupts_it_instead_of_leaving() {
        let (mut t, root, child) = chats_tui();
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(serde_json::from_value(json!({"id": child, "parent_chat_id": root,
                "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap()),
            messages: vec![],
        });
        t.update(stream(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        let effects = t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!effects.contains(&Effect::LoadChat(root)), "{effects:?}");
        assert!(effects.contains(&Effect::Interrupt(child)), "{effects:?}");
    }

    #[test]
    fn slash_parent_on_a_root_chat_only_says_there_is_no_parent() {
        let (mut t, _, _) = subagent_tui();
        let notices = t.core.notices.len();
        assert!(t.update(Msg::Submit("/parent".into())).is_empty());
        assert!(t.overlay.is_none());
        assert_eq!(t.core.notices.len(), notices + 1);
        assert!(
            matches!(t.core.notices.last(), Some(Notice::Info(m)) if m == "This chat is not a subagent.")
        );
    }

    /// Opens `/subagents` on the root chat with the child previewed.
    fn previewing() -> (Tui, uuid::Uuid, uuid::Uuid) {
        let (mut t, root, child) = subagent_tui();
        let effects = t.update(Msg::Submit("/subagents".into()));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::OpenPreview { chat, .. } if *chat == child)),
            "{effects:?}"
        );
        show(&mut t, effects);
        assert!(matches!(t.overlay, Some(Overlay::Subagents(_))));
        (t, root, child)
    }

    #[test]
    fn esc_closes_the_subagents_popup_and_its_preview() {
        let (mut t, _, _) = previewing();
        let effects = t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(t.overlay.is_none());
        assert_eq!(effects, vec![Effect::ClosePreview]);
        assert!(t.core.preview.is_none());
    }

    #[test]
    fn ctrl_c_closes_the_subagents_popup_and_its_preview() {
        let (mut t, _, _) = previewing();
        let effects = t.handle(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(t.overlay.is_none());
        assert_eq!(effects, vec![Effect::ClosePreview]);
        assert!(t.core.preview.is_none());
    }

    #[test]
    fn opening_another_overlay_closes_the_preview() {
        let (mut t, _, _) = previewing();
        let effects = t.update(Msg::Command(scuttle_core::commands::Command::Chats(None)));
        assert!(effects.contains(&Effect::ClosePreview), "{effects:?}");
        assert!(t.core.preview.is_none());
        show(&mut t, effects);
        assert!(matches!(t.overlay, Some(Overlay::Chats(_))));
    }

    #[test]
    fn a_chat_switch_closes_the_subagents_popup_and_its_preview() {
        let (mut t, _, _) = previewing();
        let other = uuid::Uuid::new_v4();
        let effects = t.update(Msg::OpenChat(other));
        assert!(effects.contains(&Effect::ClosePreview), "{effects:?}");
        assert!(effects.contains(&Effect::LoadChat(other)), "{effects:?}");
        assert!(
            t.overlay.is_none(),
            "the popup listed the old chat's subagents"
        );
        assert!(t.core.preview.is_none());
    }

    /// A root with two subagents, "explore" open and "review" its sibling.
    fn sibling_tui() -> (Tui, uuid::Uuid, uuid::Uuid) {
        use scuttle_core::chat_list::ListQuery;
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let (root, open, sibling) = (
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
        );
        let child = |id: uuid::Uuid, title: &str| {
            json!({"id": id, "title": title, "status": "waiting", "parent_chat_id": root,
                "updated_at": "2026-09-30T10:00:00Z", "children": [], "files": [],
                "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})
        };
        let chats = serde_json::from_value(json!([
            {"id": root, "title": "Fix the flaky watch reconnect test", "status": "waiting",
             "updated_at": "2026-09-30T10:00:00Z", "files": [], "mcp_server_ids": [],
             "inline_mcp_servers": [], "labels": {},
             "children": [child(open, "explore"), child(sibling, "review")]}
        ]))
        .unwrap();
        t.core.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats,
        });
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(serde_json::from_value(child(open, "explore")).unwrap()),
            messages: vec![],
        });
        (t, open, sibling)
    }

    /// The row the table marks as selected.
    fn selected_line(shown: &str) -> String {
        shown
            .lines()
            .find(|l| l.contains('›'))
            .unwrap_or_default()
            .to_owned()
    }

    #[test]
    fn subagents_from_a_subagent_starts_on_its_sibling() {
        let (mut t, open, sibling) = sibling_tui();
        let effects = t.update(Msg::Submit("/subagents".into()));
        assert!(
            matches!(effects.as_slice(), [Effect::ShowSubagents, Effect::OpenPreview { chat, .. }] if *chat == sibling),
            "{effects:?}"
        );
        show(&mut t, effects);
        let shown = screen(&mut t, 80, 24);
        assert!(selected_line(&shown).contains("review"), "{shown}");
        assert!(!shown.contains("This is the open chat."), "{shown}");
        // Arrowing back onto the open chat closes the preview and says why it is blank.
        let effects = t.handle(key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(effects, vec![Effect::ClosePreview]);
        let shown = screen(&mut t, 80, 24);
        assert!(selected_line(&shown).contains("explore"), "{shown}");
        assert!(shown.contains("This is the open chat."), "{shown}");
        assert!(t.core.preview.is_none());
        assert_eq!(t.core.chat_id, Some(open));
    }

    #[test]
    fn an_only_child_says_the_open_chat_is_selected() {
        let (mut t, root, child) = chats_tui();
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(serde_json::from_value(json!({"id": child, "parent_chat_id": root, "title": "explore",
                "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap()),
            messages: vec![],
        });
        let effects = t.update(Msg::Submit("/subagents".into()));
        assert_eq!(effects, vec![Effect::ShowSubagents]);
        show(&mut t, effects);
        let shown = screen(&mut t, 80, 24);
        assert!(shown.contains("This is the open chat."), "{shown}");
    }

    #[test]
    fn a_subagent_whose_parent_is_not_loaded_lists_itself_and_says_so() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let (root, child) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(serde_json::from_value(json!({"id": child, "parent_chat_id": root, "title": "explore",
                "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap()),
            messages: vec![],
        });
        let effects = t.update(Msg::Submit("/subagents".into()));
        show(&mut t, effects);
        let shown = screen(&mut t, 80, 24);
        assert!(selected_line(&shown).contains("explore"), "{shown}");
        assert!(!shown.contains("This chat has no subagents."), "{shown}");
        assert!(shown.contains("The parent chat is not loaded"), "{shown}");
    }

    #[test]
    fn page_up_stops_at_the_top_of_the_preview() {
        let (mut t, _, child) = previewing();
        let generation = t.core.preview.as_ref().map(|p| p.generation()).unwrap();
        t.update(Msg::ForPreview {
            chat: child,
            generation,
            msg: Box::new(stream(
                json!({"type": "message", "message": {"id": 3, "role": "assistant",
                "content": [{"type": "text", "text": "Found 3 callers"}]}}),
            )),
        });
        for _ in 0..5 {
            t.handle(key(KeyCode::PageUp, KeyModifiers::NONE));
        }
        screen(&mut t, 80, 24);
        assert_eq!(
            t.overlay.as_ref().and_then(Overlay::preview_scroll),
            Some(0),
            "a preview that fits has nothing to scroll"
        );
        t.handle(key(KeyCode::PageDown, KeyModifiers::NONE));
        assert!(screen(&mut t, 80, 24).contains("Found 3 callers"));
    }

    #[test]
    fn the_preview_is_reused_on_timer_frames_and_rebuilt_on_its_events() {
        let (mut t, _, child) = previewing();
        let generation = t.core.preview.as_ref().map(|p| p.generation()).unwrap();
        let start = t.epoch;
        screen_at(&mut t, 80, 24, start);
        let builds = t.preview_builds;
        for step in 1..=3u32 {
            t.tick();
            screen_at(&mut t, 80, 24, start + SPINNER_INTERVAL * step);
        }
        assert_eq!(
            t.preview_builds, builds,
            "timer frames reuse the preview lines"
        );
        t.update(Msg::ForPreview {
            chat: child,
            generation,
            msg: Box::new(stream(
                json!({"type": "message", "message": {"id": 3, "role": "assistant",
                "content": [{"type": "text", "text": "Found 3 callers"}]}}),
            )),
        });
        t.tick();
        let shown = screen_at(&mut t, 80, 24, start + SPINNER_INTERVAL * 4);
        assert!(shown.contains("Found 3 callers"), "{shown}");
        assert_eq!(
            t.preview_builds,
            builds + 1,
            "a preview event rebuilds the lines"
        );
        screen_at(&mut t, 80, 24, start + SPINNER_INTERVAL * 4);
        assert_eq!(
            t.preview_builds,
            builds + 2,
            "a non-timer draw rebuilds the lines"
        );
    }

    #[test]
    fn esc_with_the_slash_menu_open_clears_it_instead_of_leaving_the_subagent() {
        let (mut t, root, child) = chats_tui();
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(serde_json::from_value(json!({"id": child, "parent_chat_id": root,
                "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap()),
            messages: vec![],
        });
        for c in "/pa".chars() {
            t.handle(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let effects = t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!effects.contains(&Effect::LoadChat(root)), "{effects:?}");
        assert_eq!(t.composer.text(), "");
        assert_eq!(t.core.chat_id, Some(child));
    }

    #[test]
    fn the_question_menu_takes_arrows_and_enter_while_the_composer_is_empty() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        loaded(
            &mut t,
            json!([{"id": 2, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "q1",
                "tool_name": "ask_user_question", "args": {"questions": [{"header": "Scope", "question": "Which crate?",
                "options": [{"label": "core", "description": "state"}, {"label": "tui", "description": "drawing"}]}]}}]}]),
        );
        let shown = screen(&mut t, 70, 24);
        assert!(shown.contains("Question 1 of 1: Scope"), "{shown}");
        assert!(shown.contains("drawing"), "{shown}");
        assert!(shown.contains("Other…"), "{shown}");
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { text, .. }] if text == "tui"),
            "{effects:?}"
        );
    }

    #[test]
    fn ctrl_enter_on_an_empty_composer_implements_a_proposed_plan() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        loaded(
            &mut t,
            json!([
                {"id": 1, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "p1",
                    "tool_name": "propose_plan", "args": {}}]},
                {"id": 2, "role": "tool", "content": [{"type": "tool-result", "tool_call_id": "p1",
                    "tool_name": "propose_plan", "result": {"ok": true, "file_id": "6f1c1b6e-8d4b-4c55-9a7e-1d2b3c4d5e6f"}}]}
            ]),
        );
        assert!(screen(&mut t, 70, 20).contains("Implement the plan: Ctrl+Enter or /implement"));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::CONTROL));
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { text, .. }] if text == "Implement the plan."),
            "{effects:?}"
        );
    }

    /// The order `/help` states for an empty composer: a ready plan comes before "Send now",
    /// even when Ctrl+Enter is also the send key.
    #[test]
    fn ctrl_enter_implements_a_ready_plan_before_sending_a_queued_message_now() {
        let mut t = tui();
        t.core.prefs.send_shortcut = scuttle_core::density::SendShortcut::ModifierEnter;
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        loaded(
            &mut t,
            json!([
                {"id": 1, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "p1",
                    "tool_name": "propose_plan", "args": {}}]},
                {"id": 2, "role": "tool", "content": [{"type": "tool-result", "tool_call_id": "p1",
                    "tool_name": "propose_plan", "result": {"ok": true, "file_id": "6f1c1b6e-8d4b-4c55-9a7e-1d2b3c4d5e6f"}}]}
            ]),
        );
        t.update(stream(json!({"type": "queue_update", "queued_messages": [
            {"id": 7, "content": [{"type": "text", "text": "then run the tests"}]}
        ]})));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::CONTROL));
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { text, .. }] if text == "Implement the plan."),
            "{effects:?}"
        );
        let shown = crate::help::help_lines(&t.theme, 200)
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            shown.contains("then Ctrl+Enter implements a ready plan, then the send key sends"),
            "{shown}"
        );
    }

    #[test]
    fn typing_hides_the_question_menu_and_left_and_esc_reach_it_only_when_empty() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        loaded(
            &mut t,
            json!([{"id": 2, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "q1",
            "tool_name": "ask_user_question", "args": {"questions": [
                {"header": "Scope", "question": "Which crate?", "options": [{"label": "core", "description": "state"}]},
                {"header": "Tests", "question": "Which kind?", "options": [{"label": "unit", "description": "fast"}]}
            ]}}]}]),
        );
        t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(screen(&mut t, 70, 24).contains("Question 2 of 2: Tests"));
        t.handle(key(KeyCode::Left, KeyModifiers::NONE));
        assert!(screen(&mut t, 70, 24).contains("Question 1 of 2: Scope"));
        t.handle(key(KeyCode::Char('x'), KeyModifiers::NONE));
        let shown = screen(&mut t, 70, 24);
        assert!(!shown.contains("Question 1 of 2"), "{shown}");
        t.handle(key(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(screen(&mut t, 70, 24).contains("Question 1 of 2: Scope"));
        t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        let shown = screen(&mut t, 70, 24);
        assert!(
            !shown.contains("Question 1 of 2"),
            "Esc hides the menu: {shown}"
        );
    }

    #[test]
    fn other_opens_the_one_line_editor_with_its_own_title() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        loaded(
            &mut t,
            json!([{"id": 2, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "q1",
                "tool_name": "ask_user_question", "args": {"questions": [{"header": "Scope", "question": "Which crate?",
                "options": [{"label": "core", "description": "state"}]}]}}]}]),
        );
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        let shown = screen(&mut t, 70, 24);
        assert!(
            shown.contains("Other answer (Enter sends, Esc cancels)"),
            "{shown}"
        );
        for c in "both".chars() {
            t.handle(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { text, .. }] if text == "Other: both"),
            "{effects:?}"
        );
    }

    #[test]
    fn an_at_path_to_a_file_is_attached_when_the_message_is_sent() {
        let dir = std::env::temp_dir().join(format!("scuttle-at-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.md");
        std::fs::write(&file, "x").unwrap();
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        t.composer.set_text(&format!("read @{}", file.display()));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            effects.is_empty(),
            "nothing uploads before the second send: {effects:?}"
        );
        assert_eq!(t.core.chips.len(), 1);
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(effects.as_slice(), [Effect::UploadFile { path, .. }] if *path == file.to_string_lossy()),
            "{effects:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_at_path_in_a_command_argument_attaches_nothing() {
        let dir = std::env::temp_dir().join(format!("scuttle-cmd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("x");
        std::fs::write(&file, "x").unwrap();
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        t.composer
            .set_text(&format!("/title see @{}", file.display()));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        std::fs::remove_dir_all(dir).unwrap();
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::UploadFile { .. })),
            "{effects:?}"
        );
        assert!(t.core.chips.is_empty());
    }

    #[test]
    fn attach_expands_a_leading_tilde_to_the_home_directory() {
        let Some(home) = home() else {
            return;
        };
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        t.composer.set_text("/attach ~/x.png");
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        let expected = home.join("x.png").to_string_lossy().into_owned();
        assert!(
            matches!(effects.as_slice(), [Effect::UploadFile { path, .. }] if *path == expected),
            "{effects:?}"
        );
    }

    #[test]
    fn the_slash_menu_lists_skills_with_their_group() {
        let mut t = tui();
        t.update(Msg::SkillsLoaded(vec![scuttle_core::skills::Skill {
            name: "deploy".into(),
            description: "Ship it".into(),
        }]));
        t.composer.set_text("/dep");
        let shown = screen(&mut t, 70, 16);
        assert!(shown.contains("/deploy"), "{shown}");
        assert!(shown.contains("Ship it"), "{shown}");
        assert!(shown.contains("personal skill"), "{shown}");
        t.handle(key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(t.composer.text(), "/deploy ");
    }

    fn workspace_chat(skill: &str) -> Box<coder_sdk::types::CodersdkChat> {
        Box::new(
            serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [], "files": [],
                "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {},
                "context": {"resources": [{"kind": "skill", "status": "ok", "skill_name": skill}]}}))
            .unwrap(),
        )
    }

    #[test]
    fn leaving_a_chat_drops_its_workspace_skills_from_the_menu() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let lint = |t: &mut Tui| {
            t.composer.set_text("/li");
            t.composer
                .slash_matches()
                .iter()
                .any(|e| e.label == "/lint")
        };
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: workspace_chat("lint"),
            messages: vec![],
        });
        assert!(lint(&mut t), "the open chat's workspace skill is listed");
        t.update(Msg::Command(scuttle_core::commands::Command::New));
        assert!(!lint(&mut t), "/new drops it");
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: workspace_chat("lint"),
            messages: vec![],
        });
        assert!(lint(&mut t));
        t.update(Msg::OpenChat(uuid::Uuid::new_v4()));
        assert!(
            !lint(&mut t),
            "switching chats drops it before the load lands"
        );
    }

    #[test]
    fn the_user_loading_rebuilds_the_menu_and_a_stream_delta_does_not() {
        let mut t = tui();
        loaded(&mut t, json!([]));
        t.composer.set_menu(vec![]);
        t.update(stream(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        t.composer.set_text("/");
        assert!(
            t.composer.slash_matches().is_empty(),
            "a stream event leaves the menu alone"
        );
        t.update(Msg::UserLoaded(scuttle_core::app::UserRef {
            id: uuid::Uuid::new_v4(),
            username: "nick".into(),
        }));
        assert!(
            t.composer.slash_matches().iter().any(|e| e.label == "/new"),
            "UserLoaded rebuilds the menu"
        );
    }

    #[test]
    fn tab_completes_a_slash_entry_or_an_at_path_and_shows_hidden_questions_when_empty() {
        let dir = std::env::temp_dir().join(format!("scuttle-tab-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.md"), "x").unwrap();
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        loaded(
            &mut t,
            json!([{"id": 2, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "q1",
                "tool_name": "ask_user_question", "args": {"questions": [{"header": "Scope", "question": "Which crate?",
                "options": [{"label": "core", "description": "state"}]}]}}]}]),
        );
        t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(t.core.questions_hidden());
        t.update(Msg::SkillsLoaded(vec![scuttle_core::skills::Skill {
            name: "deploy".into(),
            description: "Ship it".into(),
        }]));
        t.composer.set_text("/dep");
        t.handle(key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(
            t.composer.text(),
            "/deploy ",
            "Tab on a slash word completes the skill"
        );
        assert!(
            t.core.questions_hidden(),
            "Tab on a slash word only completes"
        );
        t.composer.set_text(&format!("read @{}/no", dir.display()));
        t.handle(key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(
            t.composer.text(),
            format!("read @{}/notes.md", dir.display())
        );
        assert!(t.core.questions_hidden(), "Tab on an @ word only completes");
        t.composer.set_text("");
        t.handle(key(KeyCode::Tab, KeyModifiers::NONE));
        assert!(
            !t.core.questions_hidden(),
            "Tab on an empty composer shows the questions"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn esc_hides_the_questions_behind_a_hint_and_tab_shows_them_again() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        loaded(
            &mut t,
            json!([{"id": 2, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "q1",
                "tool_name": "ask_user_question", "args": {"questions": [{"header": "Scope", "question": "Which crate?",
                "options": [{"label": "core", "description": "state"}]}]}}]}]),
        );
        t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        let shown = screen(&mut t, 70, 24);
        assert!(!shown.contains("Question 1 of 1"), "{shown}");
        assert!(
            shown.contains("Questions hidden: Tab shows them"),
            "{shown}"
        );
        t.handle(key(KeyCode::Tab, KeyModifiers::NONE));
        let shown = screen(&mut t, 70, 24);
        assert!(shown.contains("Question 1 of 1: Scope"), "{shown}");
        assert!(!shown.contains("Questions hidden"), "{shown}");
    }

    #[test]
    fn chips_show_above_the_composer_and_backspace_on_an_empty_composer_removes_one() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        t.update(Msg::Submit("/attach /tmp/shot.png".into()));
        t.update(Msg::FileUploaded {
            local: 1,
            file_id: uuid::Uuid::new_v4(),
            size: 1_258_291,
        });
        assert!(screen(&mut t, 60, 16).contains("[shot.png 1.2 MiB]"));
        t.composer.set_text("x");
        t.handle(key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(t.core.chips.len(), 1, "Backspace edits text first");
        t.handle(key(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(t.core.chips.is_empty());
    }

    #[test]
    fn an_uploading_chip_shows_a_spinner_that_animates() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let now = t.epoch;
        assert_eq!(t.animation_deadline(now), None, "no timer while idle");
        t.update(Msg::Submit("/attach /tmp/shot.png".into()));
        assert_eq!(
            t.animation_deadline(now),
            Some(now + crate::activity::SPINNER_INTERVAL),
            "an upload keeps the timer going"
        );
        let first = screen_at(&mut t, 60, 16, now);
        let frame = spinner_frame(Duration::ZERO);
        assert!(
            first.contains(&format!("[shot.png {frame} uploading]")),
            "{first}"
        );
        t.tick();
        let later = screen_at(&mut t, 60, 16, now + crate::activity::SPINNER_INTERVAL);
        let next = spinner_frame(crate::activity::SPINNER_INTERVAL);
        assert!(
            later.contains(&format!("[shot.png {next} uploading]")),
            "a timer frame advances the spinner: {later}"
        );
        t.update(Msg::FileUploaded {
            local: 1,
            file_id: uuid::Uuid::new_v4(),
            size: 10,
        });
        assert_eq!(t.animation_deadline(now), None, "the upload is done");
    }

    #[test]
    fn a_page_handoff_records_the_text_and_redraws() {
        let mut t = tui();
        assert!(
            !t.apply_ui_effect(&Effect::Page("+x\n".into())),
            "the main loop runs the pager, since it owns the terminal"
        );
        let notices = t.core.notices.len();
        t.after_handoff("+x\n", Handed::Ran(Ok(())), Ok(()));
        assert_eq!(t.last_paged.as_deref(), Some("+x\n"));
        assert!(t.take_full_redraw());
        assert!(!t.must_quit());
        assert_eq!(t.core.notices.len(), notices);
    }

    #[test]
    fn a_failed_pager_says_why_and_keeps_running() {
        let mut t = tui();
        t.after_handoff(
            "+x\n",
            Handed::Ran(Err(std::io::Error::other(
                "the pager exited with exit status: 127",
            ))),
            Ok(()),
        );
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Error(
                "The pager failed: the pager exited with exit status: 127".into()
            ))
        );
        assert!(t.take_full_redraw());
        assert!(!t.must_quit());
    }

    #[test]
    fn a_terminal_not_restored_after_the_pager_quits_with_an_error() {
        let mut t = tui();
        t.after_handoff(
            "+x\n",
            Handed::Ran(Ok(())),
            Err(std::io::Error::other("tty gone")),
        );
        assert!(t.must_quit());
        let fatal = t.take_fatal().unwrap();
        assert!(fatal.contains("after the pager"), "{fatal}");
        assert!(fatal.contains("tty gone"), "{fatal}");
    }

    #[test]
    fn a_terminal_that_cannot_be_handed_over_says_so() {
        let mut t = tui();
        t.after_handoff(
            "+x\n",
            Handed::NotHandedOver(std::io::Error::other("flush failed")),
            Ok(()),
        );
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Error(
                "Could not hand the terminal to the pager: flush failed".into()
            ))
        );
        assert!(!t.must_quit());
    }

    #[test]
    fn ctrl_g_asks_the_main_loop_for_the_editor() {
        let mut t = tui();
        assert!(!t.take_editor_request());
        assert!(
            t.handle(key(KeyCode::Char('g'), KeyModifiers::CONTROL))
                .is_empty()
        );
        assert!(t.take_editor_request());
        assert!(!t.take_editor_request(), "the request is taken once");
    }

    #[test]
    fn the_editor_and_the_pager_share_the_restore_failure() {
        let mut t = tui();
        t.finish_editor(Err(std::io::Error::other("tty gone")));
        assert_eq!(
            t.take_fatal().as_deref(),
            Some("could not restore the terminal after the editor: tty gone")
        );
        t.after_handoff(
            "+x\n",
            Handed::Ran(Ok(())),
            Err(std::io::Error::other("tty gone")),
        );
        assert_eq!(
            t.take_fatal().as_deref(),
            Some("could not restore the terminal after the pager: tty gone")
        );
    }

    #[test]
    fn page_up_at_the_top_of_a_long_chat_asks_for_older_messages() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let messages: Vec<serde_json::Value> = (1..=200)
            .map(|i| json!({"id": i, "role": "user", "content": [{"type": "text", "text": format!("m{i}")}]}))
            .collect();
        loaded(&mut t, serde_json::Value::Array(messages));
        screen(&mut t, 60, 20);
        let mut asked = Vec::new();
        for _ in 0..100 {
            asked = t.handle(key(KeyCode::PageUp, KeyModifiers::NONE));
            if !asked.is_empty() {
                break;
            }
            screen(&mut t, 60, 20);
        }
        assert!(
            matches!(asked.as_slice(), [Effect::LoadOlder { before_id: 1, .. }]),
            "{asked:?}"
        );
    }

    fn texts(ids: std::ops::RangeInclusive<i64>) -> Vec<serde_json::Value> {
        ids.map(|i| json!({"id": i, "role": "assistant", "content": [{"type": "text", "text": format!("m{i}")}]}))
            .collect()
    }

    /// Opens `messages`, a full newest page, and pages up to its top, which asks for the page
    /// before it; returns that request's generation.
    fn at_the_top(t: &mut Tui, messages: Vec<serde_json::Value>) -> u64 {
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        loaded(t, serde_json::Value::Array(messages));
        screen(t, 60, 20);
        for _ in 0..200 {
            let asked = t.handle(key(KeyCode::PageUp, KeyModifiers::NONE));
            screen(t, 60, 20);
            if let [Effect::LoadOlder { generation, .. }] = asked.as_slice() {
                return *generation;
            }
        }
        panic!("never asked for older messages");
    }

    fn older(t: &mut Tui, generation: u64, messages: Vec<serde_json::Value>) {
        let chat = t.core.chat_id.unwrap();
        t.update(Msg::ForChat {
            chat,
            msg: Box::new(Msg::OlderLoaded {
                messages: serde_json::from_value(serde_json::Value::Array(messages)).unwrap(),
                has_more: true,
                generation,
            }),
        });
    }

    /// `shown` with its "Loading older messages…" row blanked, the one row an older page
    /// replaces with its own last lines while the rest of the screen stays.
    fn without_loading_line(shown: &str) -> String {
        shown
            .lines()
            .map(|l| {
                if matches!(l.trim(), "Loading older messages\u{2026}" | "m300") {
                    ""
                } else {
                    l
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn an_older_page_keeps_the_reader_on_the_lines_they_were_reading() {
        let mut t = tui();
        let generation = at_the_top(&mut t, texts(301..=500));
        let before = screen(&mut t, 60, 20);
        assert_eq!(t.top_line(), 0);
        assert!(before.contains("m301"), "{before}");
        assert!(before.contains("Loading older messages"), "{before}");
        older(&mut t, generation, texts(101..=300));
        let after = screen(&mut t, 60, 20);
        assert_eq!(
            without_loading_line(&after),
            without_loading_line(&before),
            "the view must not jump"
        );
        assert_eq!(
            find(&after, "m300").1,
            find(&before, "Loading older messages").1,
            "the page's last message takes the loading line's row"
        );
        assert!(t.prepended_rows > 0);
        assert_eq!(
            t.top_line(),
            t.prepended_rows,
            "the line at the top moved down by exactly the rows the page added"
        );
        // The loading line and the blank row under it are above m301.
        assert_eq!(
            t.view.message_rows.get(&301).copied(),
            Some(t.prepended_rows + 2)
        );
        t.handle(key(KeyCode::PageUp, KeyModifiers::NONE));
        let shown = screen(&mut t, 60, 20);
        // m300 already shows on the loading line's row, so the page above starts at m299.
        assert!(shown.contains("m299"), "the older page is above: {shown}");
    }

    #[test]
    fn a_selection_follows_its_text_through_an_older_page() {
        let mut t = tui();
        let generation = at_the_top(&mut t, texts(301..=500));
        let shown = screen(&mut t, 60, 20);
        let (x, y) = find(&shown, "m302");
        let (x2, y2) = find(&shown, "m303");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x2 + 3, y2);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x2 + 3, y2);
        let text = selected_text(&t.view, &t.selection.unwrap());
        assert!(text.contains("m302") && text.contains("m303"), "{text}");
        assert!(reversed(&mut t, 60, 20, x, y));
        older(&mut t, generation, texts(101..=300));
        screen(&mut t, 60, 20);
        let selection = t
            .selection
            .expect("the selection stays on rows that only moved");
        assert_eq!(selected_text(&t.view, &selection), text);
        assert!(
            reversed(&mut t, 60, 20, x, y),
            "and stays highlighted in place"
        );
    }

    #[test]
    fn a_held_drag_stays_pinned_through_an_older_page() {
        let mut t = tui();
        let generation = at_the_top(&mut t, texts(301..=500));
        let before = screen(&mut t, 60, 20);
        let (x, y) = find(&before, "m302");
        let (x2, y2) = find(&before, "m303");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x2 + 3, y2);
        let text = selected_text(&t.view, &t.selection.unwrap());
        older(&mut t, generation, texts(101..=300));
        let after = screen(&mut t, 60, 20);
        assert_eq!(
            without_loading_line(&after),
            without_loading_line(&before),
            "the pinned view must not jump"
        );
        assert_eq!(
            find(&after, "m300").1,
            find(&before, "Loading older messages").1,
            "the page's last message takes the loading line's row"
        );
        let drag = t.drag.expect("the drag is still held");
        assert_eq!(drag.top, t.prepended_rows);
        assert_eq!(selected_text(&t.view, &t.selection.unwrap()), text);
        mouse(
            &mut t,
            MouseEventKind::Drag(MouseButton::Left),
            x2 + 3,
            y2 + 1,
        );
        let longer = selected_text(&t.view, &t.selection.unwrap());
        assert!(
            longer.starts_with(&text),
            "the drag goes on from its anchor: {longer}"
        );
    }

    #[test]
    fn a_selection_over_rows_an_older_page_changed_is_dropped() {
        let mut t = tui();
        let mut newest = texts(301..=301);
        newest.push(
            json!({"id": 302, "role": "tool", "content": [{"type": "tool-result",
            "tool_call_id": "c1", "tool_name": "execute", "result": {"output": "done"}}]}),
        );
        newest.extend(texts(303..=500));
        let generation = at_the_top(&mut t, newest);
        let before = screen(&mut t, 60, 20);
        assert!(
            before.contains("execute"),
            "the orphan result shows: {before}"
        );
        // From the orphan result's line, which the page folds into its call, to m303.
        let (x, y) = find(&before, "execute");
        let (x2, y2) = find(&before, "m303");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x2 + 3, y2);
        assert!(t.selection.is_some());
        let mut page = texts(101..=298);
        page.push(
            json!({"id": 299, "role": "assistant", "content": [{"type": "tool-call",
            "tool_call_id": "c1", "tool_name": "execute", "args": {"command": "make"}}]}),
        );
        // A user message between the call and m301 keeps m301 from becoming an answer with a
        // rule above it, which would move it within its own rows.
        page.push(
            json!({"id": 300, "role": "user", "content": [{"type": "text", "text": "m300"}]}),
        );
        older(&mut t, generation, page);
        let after = screen(&mut t, 60, 20);
        assert!(
            t.selection.is_none(),
            "the call took its result's line, so the rows below it moved"
        );
        assert!(t.drag.is_none());
        assert_eq!(
            find(&after, "m301").1,
            find(&before, "m301").1,
            "the first line read stays put"
        );
    }

    #[test]
    fn new_output_while_scrolled_up_keeps_the_view_still() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let messages: Vec<serde_json::Value> = (1..=30)
            .map(|i| {
                let role = if i % 2 == 0 { "assistant" } else { "user" };
                json!({"id": i, "role": role, "content": [{"type": "text", "text": format!("message {i}")}]})
            })
            .collect();
        loaded(&mut t, serde_json::Value::Array(messages));
        let now = Instant::now();
        screen_at(&mut t, 60, 20, now);
        t.handle(key(KeyCode::PageUp, KeyModifiers::NONE));
        let rows = |shown: String| {
            shown
                .lines()
                .take(10)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let before = rows(screen_at(&mut t, 60, 20, now));
        t.update(stream(
            json!({"type": "message", "message": {"id": 31, "role": "assistant",
            "content": [{"type": "text", "text": "a new answer\n\nwith two paragraphs"}]}}),
        ));
        let after = rows(screen_at(&mut t, 60, 20, now));
        assert_eq!(before, after, "the text under the reader did not move");
        t.handle(key(KeyCode::End, KeyModifiers::NONE));
        assert!(screen_at(&mut t, 60, 20, now).contains("with two paragraphs"));
    }

    #[test]
    fn a_reader_following_the_stream_keeps_following_through_an_older_page() {
        let mut t = tui();
        let generation = at_the_top(&mut t, texts(301..=500));
        t.handle(key(KeyCode::End, KeyModifiers::NONE));
        assert!(screen(&mut t, 60, 20).contains("m500"));
        older(&mut t, generation, texts(101..=300));
        t.update(stream(
            json!({"type": "message", "message": {"id": 501, "role": "assistant",
            "content": [{"type": "text", "text": "the newest answer"}]}}),
        ));
        let shown = screen(&mut t, 60, 20);
        assert!(t.prepended_rows > 0, "the same rebuild took the older page");
        assert_eq!(
            t.scroll_from_bottom, 0,
            "the reader still follows the stream"
        );
        assert!(shown.contains("the newest answer"), "{shown}");
    }

    /// A chat with a running tool call and a thinking block near its start, scrolled up so
    /// both are above the top of the screen; returns the screen.
    fn scrolled_past_work(t: &mut Tui) -> String {
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let mut messages = vec![
            json!({"id": 1, "role": "user", "content": [{"type": "text", "text": "go"}]}),
            json!({"id": 2, "role": "assistant", "content": [
                {"type": "reasoning", "text": "first thought\nsecond thought\nthird thought"},
                {"type": "tool-call", "tool_call_id": "c1", "tool_name": "execute", "args": {"command": "make"}}
            ]}),
        ];
        messages.extend(texts(3..=40));
        loaded(t, serde_json::Value::Array(messages));
        screen(t, 60, 20);
        for _ in 0..200 {
            t.handle(key(KeyCode::PageUp, KeyModifiers::NONE));
            if t.top_line() == 0 {
                break;
            }
        }
        t.handle(key(KeyCode::PageDown, KeyModifiers::NONE));
        let shown = screen(t, 60, 20);
        assert!(
            !shown.contains("execute") && !shown.contains("Thinking"),
            "the work is above the top: {shown}"
        );
        shown
    }

    #[test]
    fn a_message_above_growing_while_output_arrives_keeps_the_view_still() {
        let mut t = tui();
        // The transcript's rows, without the footer, which says when the stream connects.
        let transcript = |shown: String| {
            shown
                .lines()
                .take(15)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let before = transcript(scrolled_past_work(&mut t));
        t.update(stream(
            json!({"type": "message", "message": {"id": 41, "role": "tool",
            "content": [{"type": "tool-result", "tool_call_id": "c1", "tool_name": "execute",
                "result": {"output": "built"}}]}}),
        ));
        t.update(stream(
            json!({"type": "message", "message": {"id": 42, "role": "assistant",
            "content": [{"type": "text", "text": "after the build"}]}}),
        ));
        assert_eq!(
            transcript(screen(&mut t, 60, 20)),
            before,
            "the text under the reader did not move"
        );
        t.toggles.insert((Some(2), 0));
        t.update(stream(
            json!({"type": "message", "message": {"id": 43, "role": "assistant",
            "content": [{"type": "text", "text": "more output"}]}}),
        ));
        assert_eq!(
            transcript(screen(&mut t, 60, 20)),
            before,
            "nor when the thinking above expands"
        );
        t.toggles.clear();
        assert_eq!(
            transcript(screen(&mut t, 60, 20)),
            before,
            "nor when it collapses"
        );
    }

    #[test]
    fn an_answer_rule_an_older_page_adds_keeps_the_text_below_it_still() {
        let mut t = tui();
        let generation = at_the_top(&mut t, texts(301..=500));
        let before = screen(&mut t, 60, 20);
        let mut page = texts(101..=299);
        page.push(json!({"id": 300, "role": "assistant", "content": [
            {"type": "reasoning", "text": "older work"}]}));
        older(&mut t, generation, page);
        let after = screen(&mut t, 60, 20);
        assert!(
            t.view.meta.iter().any(|m| m.rule),
            "m301 became an answer with a rule above it"
        );
        let row = find(&before, "m301").1 as usize;
        assert_eq!(
            after.lines().skip(row).collect::<Vec<_>>(),
            before.lines().skip(row).collect::<Vec<_>>(),
            "the text from m301 down stays put"
        );
    }

    /// The newest page starts with a result whose call is on the page before it; pages up to
    /// the top and returns the request's generation.
    fn at_the_top_after_an_orphan(t: &mut Tui) -> u64 {
        let mut newest = vec![
            json!({"id": 301, "role": "tool", "content": [{"type": "tool-result",
            "tool_call_id": "c1", "tool_name": "execute", "result": {"output": "done"}}]}),
        ];
        newest.extend(texts(302..=500));
        at_the_top(t, newest)
    }

    /// The page before `at_the_top_after_an_orphan`'s, holding the orphan's call.
    fn page_with_the_call() -> Vec<serde_json::Value> {
        let mut page = texts(101..=298);
        page.push(
            json!({"id": 299, "role": "assistant", "content": [{"type": "tool-call",
            "tool_call_id": "c1", "tool_name": "execute", "args": {"command": "make"}}]}),
        );
        page.push(
            json!({"id": 300, "role": "user", "content": [{"type": "text", "text": "m300"}]}),
        );
        page
    }

    #[test]
    fn an_older_page_that_folds_the_top_result_keeps_the_reader_on_m302() {
        let mut t = tui();
        let generation = at_the_top_after_an_orphan(&mut t);
        let before = screen(&mut t, 60, 20);
        assert!(before.contains("execute"), "{before}");
        older(&mut t, generation, page_with_the_call());
        let after = screen(&mut t, 60, 20);
        assert_eq!(
            find(&after, "m302").1,
            find(&before, "m302").1,
            "the first surviving text stays on its row: {after}"
        );
    }

    #[test]
    fn a_selection_over_text_that_only_moved_survives_an_older_page() {
        let mut t = tui();
        let mut newest = texts(301..=301);
        newest.push(
            json!({"id": 302, "role": "tool", "content": [{"type": "tool-result",
            "tool_call_id": "c1", "tool_name": "execute", "result": {"output": "done"}}]}),
        );
        newest.extend(texts(303..=500));
        let generation = at_the_top(&mut t, newest);
        let before = screen(&mut t, 60, 20);
        let (x, y) = find(&before, "m303");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 3, y);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x + 3, y);
        let text = selected_text(&t.view, &t.selection.unwrap());
        assert_eq!(text, "m303");
        let mut page = page_with_the_call();
        page.pop();
        page.push(
            json!({"id": 300, "role": "user", "content": [{"type": "text", "text": "m300"}]}),
        );
        older(&mut t, generation, page);
        screen(&mut t, 60, 20);
        let selection = t
            .selection
            .expect("m303 only moved, if by less than m301 did");
        assert_eq!(selected_text(&t.view, &selection), text);
    }

    #[test]
    fn a_selection_whose_later_row_an_older_page_changed_is_cleared() {
        let mut t = tui();
        let mut newest = texts(301..=301);
        newest.push(
            json!({"id": 302, "role": "tool", "content": [{"type": "tool-result",
            "tool_call_id": "c1", "tool_name": "execute", "result": {"output": "done"}}]}),
        );
        newest.extend(texts(303..=500));
        let generation = at_the_top(&mut t, newest);
        let before = screen(&mut t, 60, 20);
        let (x, y) = find(&before, "m301");
        let (x2, y2) = find(&before, "m303");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x2 + 3, y2);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x2 + 3, y2);
        assert!(selected_text(&t.view, &t.selection.unwrap()).contains("execute"));
        older(&mut t, generation, page_with_the_call());
        screen(&mut t, 60, 20);
        assert!(
            t.selection.is_none(),
            "m301 only moved, but the result's row under the selection folded away"
        );
    }

    #[test]
    fn a_drag_held_across_the_turn_persisting_keeps_its_selection() {
        let mut t = tui();
        loaded(&mut t, serde_json::Value::Array(texts(1..=2)));
        t.update(live_part(1, json!({"type": "text", "text": "live alpha"})));
        let shown = screen(&mut t, 60, 20);
        let (x, y) = find(&shown, "live alpha");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 9, y);
        let text = selected_text(&t.view, &t.selection.unwrap());
        assert_eq!(text, "live alpha");
        t.update(stream(
            json!({"type": "message", "message": {"id": 3, "role": "assistant",
            "content": [{"type": "text", "text": "live alpha"}]}}),
        ));
        let after = screen(&mut t, 60, 20);
        assert_eq!(find(&after, "live alpha"), (x, y), "{after}");
        assert!(t.drag.is_some(), "the drag is still held");
        assert_eq!(selected_text(&t.view, &t.selection.unwrap()), text);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x + 9, y);
        assert_eq!(t.last_copied.as_deref(), Some("live alpha"));
    }

    #[test]
    fn a_turn_persisting_as_the_next_one_starts_keeps_the_reader_still() {
        let mut t = tui();
        loaded(&mut t, serde_json::Value::Array(texts(1..=2)));
        let answer = (1..=40)
            .map(|i| format!("live line {i}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        t.update(live_part(1, json!({"type": "text", "text": answer})));
        screen(&mut t, 60, 20);
        t.handle(key(KeyCode::PageUp, KeyModifiers::NONE));
        let transcript = |shown: String| {
            shown
                .lines()
                .take(15)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let before = transcript(screen(&mut t, 60, 20));
        assert!(before.iter().any(|l| l.contains("live line")), "{before:?}");
        t.update(stream(
            json!({"type": "message", "message": {"id": 3, "role": "assistant",
            "content": [{"type": "text", "text": answer}]}}),
        ));
        t.update(live_part(2, json!({"type": "text", "text": "next turn"})));
        let after = transcript(screen(&mut t, 60, 20));
        assert_eq!(after, before, "the reader stays on the persisted answer");
        t.handle(key(KeyCode::End, KeyModifiers::NONE));
        assert!(screen(&mut t, 60, 20).contains("next turn"));
    }

    /// The first row with text on `shown`, and its first word.
    fn first_text_row(shown: &str) -> (usize, String) {
        shown
            .lines()
            .enumerate()
            .find_map(|(row, l)| l.split_whitespace().next().map(|w| (row, w.to_owned())))
            .unwrap()
    }

    #[test]
    fn a_resize_while_scrolled_up_keeps_the_top_text() {
        let mut t = tui();
        let messages: Vec<serde_json::Value> = (1..=40)
            .map(|i| {
                json!({"id": i, "role": "assistant", "content": [{"type": "text",
                "text": format!("m{i} {}", "word ".repeat(9))}]})
            })
            .collect();
        loaded(&mut t, serde_json::Value::Array(messages));
        screen(&mut t, 60, 20);
        t.handle(key(KeyCode::PageUp, KeyModifiers::NONE));
        t.handle(key(KeyCode::PageUp, KeyModifiers::NONE));
        let before = first_text_row(&screen(&mut t, 60, 20));
        let narrow = screen(&mut t, 40, 20);
        assert!(
            narrow
                .lines()
                .any(|l| l.split_whitespace().next() == Some("word")),
            "the narrower screen wraps each message: {narrow}"
        );
        assert_eq!(first_text_row(&narrow), before);
    }

    #[test]
    fn the_top_row_stays_put_when_the_composer_grows_while_scrolled_up() {
        let mut t = tui();
        loaded(&mut t, serde_json::Value::Array(texts(1..=40)));
        screen(&mut t, 60, 20);
        t.handle(key(KeyCode::PageUp, KeyModifiers::NONE));
        let rows = |shown: String| shown.lines().take(8).map(str::to_owned).collect::<Vec<_>>();
        let before = rows(screen(&mut t, 60, 20));
        t.handle(Event::Paste("one\ntwo\nthree\nfour".into()));
        let after = screen(&mut t, 60, 20);
        assert!(after.contains("four"), "the composer grew: {after}");
        assert_eq!(rows(after), before);
    }

    /// Loads a chat titled `title` into the Tui and returns its ID.
    fn loaded_titled(t: &mut Tui, title: &str) -> uuid::Uuid {
        let id = uuid::Uuid::new_v4();
        let chat = serde_json::from_value(
            json!({"id": id, "title": title, "children": [], "files": [],
            "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}),
        )
        .unwrap();
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: vec![],
        });
        id
    }

    #[test]
    fn the_window_title_follows_the_open_chat() {
        let mut t = tui();
        started(&mut t);
        assert_eq!(t.take_title().as_deref(), Some("scuttle"));
        assert_eq!(t.take_title(), None, "an unchanged title is not set again");
        let id = loaded_titled(&mut t, "Fix the CI");
        assert_eq!(t.take_title().as_deref(), Some("scuttle · Fix the CI"));
        t.update(Msg::ChatUpdated {
            chat: id,
            change: scuttle_core::app::ChatChange::Title("Fix the flaky CI".into()),
        });
        assert_eq!(
            t.take_title().as_deref(),
            Some("scuttle · Fix the flaky CI"),
            "a rename shows"
        );
        t.update(Msg::Command(scuttle_core::commands::Command::New));
        assert_eq!(
            t.take_title().as_deref(),
            Some("scuttle"),
            "a new chat has none"
        );
        let other = uuid::Uuid::new_v4();
        t.update(Msg::OpenChat(other));
        assert_eq!(
            t.take_title(),
            None,
            "a chat still loading has no title yet"
        );
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(
                serde_json::from_value(json!({"id": other, "title": "Second", "children": [],
                    "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}))
                .unwrap(),
            ),
            messages: vec![],
        });
        assert_eq!(
            t.take_title().as_deref(),
            Some("scuttle · Second"),
            "a switch shows"
        );
    }

    #[test]
    fn control_characters_in_a_chat_title_never_reach_the_terminal() {
        let mut t = tui();
        loaded_titled(&mut t, "evil\x1b]2;pwned\x07\nsecond\tline\u{9b}x");
        let title = t.window_title();
        assert_eq!(title, "scuttle · evil]2;pwned second linex");
        let sequence = crate::terminal::title_sequence(&title);
        assert_eq!(sequence.matches('\x1b').count(), 1, "{sequence:?}");
        assert_eq!(sequence.matches('\x07').count(), 1, "{sequence:?}");
        assert!(sequence.starts_with("\x1b]2;") && sequence.ends_with('\x07'));
    }

    #[test]
    fn a_long_or_blank_title_is_cut_or_left_out() {
        let mut t = tui();
        loaded_titled(&mut t, &"a".repeat(150));
        assert_eq!(t.window_title(), format!("scuttle · {}", "a".repeat(100)));
        // A load applies only to a blank screen, so the blank title gets a Tui of its own.
        let mut t = tui();
        loaded_titled(&mut t, "  \n ");
        assert_eq!(t.window_title(), "scuttle");
    }

    #[test]
    fn a_long_multibyte_title_is_cut_at_a_character_boundary() {
        let mut t = tui();
        loaded_titled(&mut t, &"é🦀".repeat(80));
        assert_eq!(
            t.window_title(),
            format!("scuttle · {}", "é🦀".repeat(50)),
            "100 characters, not 100 bytes"
        );
    }

    #[test]
    fn a_handoff_sets_the_window_title_again() {
        let mut t = tui();
        loaded_titled(&mut t, "Fix the CI");
        assert!(t.take_title().is_some());
        t.after_handoff("text", Handed::Ran(Ok(())), Ok(()));
        assert_eq!(
            t.take_title().as_deref(),
            Some("scuttle · Fix the CI"),
            "the pager may have set its own title"
        );
        assert!(t.finish_editor(Ok(())).is_empty());
        assert_eq!(
            t.take_title().as_deref(),
            Some("scuttle · Fix the CI"),
            "the editor may have set its own title"
        );
    }

    fn started(t: &mut Tui) {
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
    }

    /// A chat whose last call proposed a plan, ready to implement.
    fn plan_ready(t: &mut Tui, before: Vec<serde_json::Value>) {
        let mut messages = before;
        messages.push(json!({"id": 901, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "p1",
            "tool_name": "propose_plan", "args": {}}]}));
        messages.push(json!({"id": 902, "role": "tool", "content": [{"type": "tool-result", "tool_call_id": "p1",
            "tool_name": "propose_plan", "result": {"ok": true, "file_id": "6f1c1b6e-8d4b-4c55-9a7e-1d2b3c4d5e6f"}}]}));
        loaded(t, serde_json::Value::Array(messages));
    }

    #[test]
    fn a_long_question_wraps_to_the_pane_at_80_columns() {
        let mut t = tui();
        started(&mut t);
        let question = "Should the reconnect backoff be shared between the main stream and the preview stream, or should each keep its own counter?";
        let description = "Keep the preview on its own counter, so a flaky preview never slows the main stream down";
        loaded(
            &mut t,
            json!([{"id": 2, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "q1",
                "tool_name": "ask_user_question", "args": {"questions": [{"header": "Backoff", "question": question,
                "options": [{"label": "shared", "description": "One counter"}, {"label": "separate", "description": description}]}]}}]}]),
        );
        let shown = screen(&mut t, 80, 24);
        let text = flowed(&shown);
        assert!(text.contains(question), "{shown}");
        assert!(text.contains(description), "{shown}");
        assert!(shown.contains("Other…"), "the box grew to fit: {shown}");
    }

    fn loaded_workspace(t: &mut Tui) {
        let ws = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "name": "dev",
            "owner_name": "nick", "template_name": "docker", "latest_build": {"status": "running"}}))
        .unwrap();
        t.core.workspace_panel = Some(scuttle_core::panels::Fetched::Loaded(Box::new(ws)));
    }

    #[test]
    fn the_workspace_panel_shows_every_action_in_full_without_scrolling() {
        let mut t = tui();
        loaded_workspace(&mut t);
        t.apply_ui_effect(&Effect::ShowWorkspace);
        let shown = screen(&mut t, 80, 24);
        for needle in [
            "dev, owned by nick",
            "Copy SSH command",
            "Open in web",
            "Detach",
            "Switch workspace",
        ] {
            assert!(shown.contains(needle), "{needle:?} is missing:\n{shown}");
        }
    }

    #[test]
    fn the_git_panel_shows_its_action_labels_in_full() {
        let mut t = tui();
        t.apply_ui_effect(&Effect::ShowGit);
        let shown = screen(&mut t, 80, 24);
        assert!(shown.contains("Open pull request"), "{shown}");
        assert!(shown.contains("View diff"), "{shown}");
    }

    #[test]
    fn chats_shows_a_search_row_with_its_placeholder() {
        let mut t = tui();
        t.apply_ui_effect(&Effect::ShowChats(String::new()));
        let shown = screen(&mut t, 80, 24);
        assert!(
            shown.contains("Search: Type to filter, or search all chats"),
            "{shown}"
        );
        t.handle(key(KeyCode::Char('x'), KeyModifiers::NONE));
        let shown = screen(&mut t, 80, 24);
        assert!(shown.contains("Search: x"), "{shown}");
        t.handle(Event::Paste(" status:error\nflaky".into()));
        let shown = screen(&mut t, 80, 24);
        assert!(shown.contains("Search: x status:error flaky"), "{shown}");
    }

    #[test]
    fn a_refused_search_shows_the_server_notice_in_chats() {
        let (mut t, _, _) = chats_tui();
        let effects = t.update(Msg::Submit("/chats title:ci flaky".into()));
        show(&mut t, effects);
        t.update(Msg::SearchChats("title:ci flaky".into()));
        t.update(Msg::ChatsFailed {
            query: scuttle_core::chat_list::ListQuery::Search("title:ci flaky".into()),
            message: "Invalid chat search query. \"search\" cannot be combined with \"title\"."
                .into(),
        });
        let shown = screen(&mut t, 120, 20);
        assert!(
            shown.contains(
                "Invalid chat search query. \"search\" cannot be combined with \"title\"."
            ),
            "{shown}"
        );
        assert!(!shown.contains('{'), "{shown}");
    }

    #[test]
    fn a_paste_with_a_filtered_overlay_open_goes_to_its_filter() {
        let (mut t, _, _) = chats_tui();
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        t.handle(Event::Paste("watch\nfix".into()));
        assert_eq!(
            t.overlay.as_ref().map(|o| o.state().filter.clone()),
            Some("watch fix".into())
        );
        assert_eq!(t.composer.text(), "", "the hidden composer stayed empty");
        t.overlay = None;
        t.apply_ui_effect(&Effect::ShowInfo);
        t.handle(Event::Paste("dropped".into()));
        assert_eq!(
            t.composer.text(),
            "",
            "an overlay without a filter drops it"
        );
    }

    #[test]
    fn the_wheel_moves_the_chats_selection_like_up_and_down() {
        let (mut t, root, _) = chats_tui();
        screen(&mut t, 80, 20);
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        let selected = |t: &Tui| match &t.overlay {
            Some(Overlay::Chats(c)) => c.table.selected.clone(),
            _ => panic!("/chats closed"),
        };
        mouse(&mut t, MouseEventKind::ScrollDown, 5, 5);
        let below = selected(&t);
        assert!(
            matches!(below, Some(table::RowKey::Chat(id)) if id != root),
            "{below:?}"
        );
        mouse(&mut t, MouseEventKind::ScrollUp, 5, 5);
        assert_eq!(selected(&t), Some(table::RowKey::Chat(root)));
        assert_eq!(t.scroll_from_bottom, 0, "the transcript under it stays put");
    }

    /// `/subagents` on a chat with two subagents, previewing the first, with a preview long
    /// enough to scroll, drawn at 80 by 30.
    fn previewing_two() -> (Tui, uuid::Uuid, uuid::Uuid) {
        use scuttle_core::chat_list::ListQuery;
        let mut t = tui();
        started(&mut t);
        let (root, first, second) = (
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
        );
        let child = |id, title| {
            json!({"id": id, "title": title, "status": "running", "parent_chat_id": root,
                "updated_at": "2026-09-30T10:00:00Z", "children": [], "files": [],
                "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})
        };
        let chats = serde_json::from_value(json!([
            {"id": root, "title": "Root", "status": "waiting", "updated_at": "2026-09-30T10:00:00Z",
             "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {},
             "children": [child(first, "explore"), child(second, "review")]}
        ]))
        .unwrap();
        t.core.update(Msg::ChatsLoaded {
            query: ListQuery::Default,
            offset: 0,
            chats,
        });
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(serde_json::from_value(json!({"id": root, "title": "Root",
                "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap()),
            messages: vec![],
        });
        let effects = t.update(Msg::Submit("/subagents".into()));
        show(&mut t, effects);
        let previewed = match &t.overlay {
            Some(Overlay::Subagents(s)) => s.table.selected.clone(),
            _ => panic!("/subagents did not open"),
        };
        let generation = t.core.preview.as_ref().map(|p| p.generation()).unwrap();
        let long: String = (1..=80).map(|i| format!("line {i}\n\n")).collect();
        let preview_of = match previewed {
            Some(table::RowKey::Chat(id)) => id,
            _ => first,
        };
        t.update(Msg::ForPreview {
            chat: preview_of,
            generation,
            msg: Box::new(stream(
                json!({"type": "message", "message": {"id": 3, "role": "assistant",
                "content": [{"type": "text", "text": long}]}}),
            )),
        });
        screen(&mut t, 80, 30);
        (t, first, second)
    }

    #[test]
    fn the_wheel_in_subagents_moves_the_list_only_over_the_list() {
        let (mut t, _, _) = previewing_two();
        let (list, preview) = t.subagent_panes.expect("the split is drawn");
        let selected = |t: &Tui| match &t.overlay {
            Some(Overlay::Subagents(s)) => s.table.selected.clone(),
            _ => panic!("/subagents closed"),
        };
        let start = selected(&t);
        let scroll = |t: &Tui| t.overlay.as_ref().and_then(Overlay::preview_scroll);
        // Over the preview, the wheel scrolls it and leaves the stream alone.
        let effects = t.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: preview.x + 2,
            row: preview.y + 1,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(selected(&t), start, "the preview's stream stays");
        assert_eq!(scroll(&t), Some(3), "the preview scrolls back");
        screen(&mut t, 80, 30);
        assert_eq!(scroll(&t), Some(3), "the long preview has room for it");
        mouse(
            &mut t,
            MouseEventKind::ScrollDown,
            preview.x + 2,
            preview.y + 1,
        );
        assert_eq!(scroll(&t), Some(0));
        mouse(
            &mut t,
            MouseEventKind::ScrollDown,
            preview.x + 2,
            preview.y + 1,
        );
        assert_eq!(selected(&t), start);
        // Off both panes, a tick does nothing.
        mouse(&mut t, MouseEventKind::ScrollDown, 0, 0);
        assert_eq!(selected(&t), start, "nothing moves off the panes");
        // Over the list, it moves the selection and previews the next subagent.
        let effects = t.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: list.x + 2,
            row: list.y + 2,
            modifiers: KeyModifiers::NONE,
        }));
        assert_ne!(selected(&t), start, "the list moves");
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::OpenPreview { .. })),
            "{effects:?}"
        );
    }

    #[test]
    fn the_wheel_moves_the_statusline_selection_without_reordering() {
        let mut t = tui();
        started(&mut t);
        t.apply_ui_effect(&Effect::ShowStatusline);
        let state = |t: &Tui| match &t.overlay {
            Some(Overlay::Statusline(s)) => (s.list.clone(), s.table.selected.clone()),
            _ => panic!("/statusline closed"),
        };
        let (rows, before) = state(&t);
        mouse(&mut t, MouseEventKind::ScrollDown, 5, 5);
        mouse(&mut t, MouseEventKind::ScrollDown, 5, 5);
        let (after_rows, after) = state(&t);
        assert_eq!(after_rows, rows, "a tick never reorders or toggles a field");
        assert_ne!(after, before, "it moves the selection");
    }

    #[test]
    fn the_wheel_leaves_the_effort_slider_alone() {
        let mut t = tui();
        t.core.update(Msg::ModelsLoaded(
            serde_json::from_value(json!([
                {"id": uuid::Uuid::new_v4(), "display_name": "Thinker", "enabled": true, "is_default": true, "reasoning_efforts": ["low", "high"]}
            ]))
            .unwrap(),
        ));
        let effects = t.update(Msg::Submit("/effort".into()));
        show(&mut t, effects);
        assert!(t.picker.is_some(), "the slider is open");
        let before = screen(&mut t, 60, 20);
        for kind in [MouseEventKind::ScrollUp, MouseEventKind::ScrollDown] {
            let effects = t.handle(Event::Mouse(MouseEvent {
                kind,
                column: 5,
                row: 5,
                modifiers: KeyModifiers::NONE,
            }));
            assert!(effects.is_empty(), "{effects:?}");
        }
        assert!(t.picker.is_some(), "the slider stays open");
        assert_eq!(screen(&mut t, 60, 20), before, "the slider keeps its level");
        assert_eq!(t.scroll_from_bottom, 0);
    }

    #[test]
    fn the_wheel_scrolls_a_read_only_panel() {
        let mut t = tui();
        started(&mut t);
        t.apply_ui_effect(&Effect::ShowInfo);
        mouse(&mut t, MouseEventKind::ScrollDown, 5, 5);
        mouse(&mut t, MouseEventKind::ScrollDown, 5, 5);
        assert_eq!(t.overlay.as_ref().map(|o| o.state().scroll), Some(2));
        mouse(&mut t, MouseEventKind::ScrollUp, 5, 5);
        assert_eq!(t.overlay.as_ref().map(|o| o.state().scroll), Some(1));
    }

    #[test]
    fn the_wheel_leaves_chats_alone_while_a_rename_is_typed() {
        let (mut t, _, _) = chats_tui();
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        t.handle(key(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert!(t.core.editor.is_some(), "the rename line is open");
        mouse(&mut t, MouseEventKind::ScrollDown, 5, 5);
        assert!(
            matches!(&t.overlay, Some(Overlay::Chats(c)) if c.table.selected.is_none()),
            "the selection under the rename line stays"
        );
        t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        t.apply_ui_effect(&Effect::ShowHelp);
        mouse(&mut t, MouseEventKind::ScrollDown, 5, 5);
        assert!(
            matches!(&t.overlay, Some(Overlay::Chats(c)) if c.table.selected.is_none()),
            "nor does the help box pass the wheel on"
        );
    }

    #[test]
    fn the_wheel_under_an_overlay_or_help_never_scrolls_the_transcript() {
        let mut t = tui();
        started(&mut t);
        let long: Vec<_> = (1..=60)
            .map(|i| json!({"id": i, "role": "assistant", "content": [{"type": "text", "text": format!("row {i:02}")}]}))
            .collect();
        loaded(&mut t, serde_json::Value::Array(long));
        screen(&mut t, 40, 24);
        t.apply_ui_effect(&Effect::ShowHelp);
        for _ in 0..30 {
            mouse(&mut t, MouseEventKind::ScrollUp, 5, 5);
        }
        assert_eq!(t.scroll_from_bottom, 0, "help");
        t.show_help = false;
        let (mut c, _, _) = chats_tui();
        loaded(
            &mut c,
            json!([{"id": 1, "role": "user", "content": [{"type": "text", "text": "x"}]}]),
        );
        screen(&mut c, 40, 24);
        show(&mut c, vec![Effect::ShowChats(String::new())]);
        let effects = c.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(
            effects.is_empty(),
            "no older page under /chats: {effects:?}"
        );
    }

    #[test]
    fn clicks_on_the_question_menu_never_reach_the_transcript_under_it() {
        let mut t = tui();
        started(&mut t);
        loaded(
            &mut t,
            json!([
                {"id": 1, "role": "assistant", "content": [{"type": "text", "text": "```\necho hi\n```"}]},
                {"id": 2, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "q1",
                    "tool_name": "ask_user_question", "args": {"questions": [{"header": "Scope", "question": "Which crate?",
                    "options": [{"label": "core", "description": "state"}]}]}}]}
            ]),
        );
        let shown = screen(&mut t, 60, 11);
        assert!(shown.contains("Question 1 of 1"), "{shown}");
        let row = t
            .row_of(|target| matches!(target, HitTarget::CopyCode(_)))
            .expect("the code block is under the menu");
        assert!(
            t.menu_area
                .is_some_and(|a| (a.y..a.y + a.height).contains(&row)),
            "{shown}"
        );
        click(&mut t, row);
        assert_eq!(t.last_copied, None, "{shown}");
        mouse(&mut t, MouseEventKind::ScrollUp, 5, row);
        assert_eq!(t.scroll_from_bottom, 0);
    }

    #[test]
    fn the_wheel_over_the_question_menu_moves_it_like_up_and_down() {
        let mut t = tui();
        started(&mut t);
        loaded(
            &mut t,
            json!([{"id": 2, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "q1",
                "tool_name": "ask_user_question", "args": {"questions": [{"header": "Scope", "question": "Which crate?",
                "options": [{"label": "core", "description": "state"}, {"label": "tui", "description": "drawing"}]}]}}]}]),
        );
        screen(&mut t, 70, 24);
        let menu = t.menu_area.expect("the question menu is drawn");
        let selected = |t: &Tui| t.core.question_menu().map(|m| m.selected);
        assert_eq!(selected(&t), Some(0));
        mouse(&mut t, MouseEventKind::ScrollDown, menu.x + 2, menu.y + 1);
        assert_eq!(selected(&t), Some(1));
        mouse(&mut t, MouseEventKind::ScrollUp, menu.x + 2, menu.y + 1);
        assert_eq!(selected(&t), Some(0));
        assert_eq!(t.scroll_from_bottom, 0, "the transcript stays put");
    }

    #[test]
    fn without_keyboard_enhancement_the_plan_hint_names_implement() {
        let mut t = tui();
        started(&mut t);
        plan_ready(&mut t, vec![]);
        t.set_keyboard_enhanced(false);
        let shown = screen(&mut t, 70, 20);
        assert!(shown.contains("Implement the plan: /implement"), "{shown}");
        assert!(!shown.contains("Ctrl+Enter"), "{shown}");
    }

    #[test]
    fn the_plan_hint_takes_its_own_row() {
        let mut t = tui();
        started(&mut t);
        let rows: Vec<_> = (1..=30)
            .map(|i| json!({"id": i, "role": "assistant", "content": [{"type": "text", "text": format!("row {i:02}")}]}))
            .collect();
        plan_ready(&mut t, rows);
        let shown = screen(&mut t, 70, 20);
        assert!(shown.contains("Implement the plan"), "{shown}");
        assert!(
            shown.contains("6f1c1b6e"),
            "the last transcript row stays visible: {shown}"
        );
    }

    #[test]
    fn esc_in_an_idle_subagent_keeps_pending_chips() {
        let (mut t, root, child) = chats_tui();
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(serde_json::from_value(json!({"id": child, "parent_chat_id": root,
                "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap()),
            messages: vec![],
        });
        t.update(Msg::Submit("/attach /tmp/shot.png".into()));
        let effects = t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!effects.contains(&Effect::LoadChat(root)), "{effects:?}");
        assert_eq!(t.core.chips.len(), 1);
    }

    #[test]
    fn the_send_key_with_chips_and_an_empty_composer_asks_for_a_message() {
        let mut t = tui();
        started(&mut t);
        loaded(&mut t, json!([]));
        t.update(stream(json!({"type": "queue_update", "queued_messages": [
            {"id": 7, "content": [{"type": "text", "text": "then run the tests"}]}
        ]})));
        t.update(Msg::Submit("/attach /tmp/shot.png".into()));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            effects.is_empty(),
            "nothing is sent or promoted: {effects:?}"
        );
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Info(
                "Type a message to send the attachments.".into()
            ))
        );
        assert_eq!(t.core.chips.len(), 1);
    }

    #[test]
    fn long_chips_wrap_so_every_one_shows() {
        let mut t = tui();
        started(&mut t);
        for name in [
            "a-rather-long-screenshot-name.png",
            "another-long-file-name.txt",
            "third.md",
        ] {
            t.update(Msg::Submit(format!("/attach /tmp/{name}")));
        }
        let shown = screen(&mut t, 50, 20);
        for name in [
            "a-rather-long-screenshot-name.png",
            "another-long-file-name.txt",
            "third.md",
        ] {
            assert!(shown.contains(name), "{name} is missing:\n{shown}");
        }
    }

    #[test]
    fn a_multi_line_paste_into_the_one_line_editor_becomes_one_line() {
        let (mut t, _, _) = chats_tui();
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        t.handle(key(KeyCode::Char('e'), KeyModifiers::CONTROL));
        for _ in 0.."Fix the flaky watch reconnect test".len() {
            t.handle(key(KeyCode::Backspace, KeyModifiers::NONE));
        }
        t.handle(Event::Paste("one\ntwo\r\nthree\tfour\u{7}".into()));
        assert_eq!(
            t.core.editor.as_ref().map(|e| e.line.text().to_owned()),
            Some("one two threefour".to_owned())
        );
    }

    #[test]
    fn the_editor_scrolls_wide_characters_without_splitting_them() {
        let text = "中文字符测试";
        let (visible, column) = visible_editor_text(text, 6, 7);
        assert_eq!(visible, "符测试", "whole ideographs only: {visible}");
        assert_eq!(column, 6, "the caret sits after the last ideograph");
        let (visible, column) = visible_editor_text(text, 0, 7);
        assert_eq!(visible, text);
        assert_eq!(column, 0);
    }

    #[test]
    fn show_git_opens_the_git_overlay_and_a_reset_closes_the_panels() {
        let mut t = tui();
        assert!(t.apply_ui_effect(&Effect::ShowGit));
        assert!(matches!(t.overlay, Some(Overlay::Git(_))));
        assert!(t.apply_ui_effect(&Effect::ClearView));
        assert!(t.overlay.is_none(), "a reset closes /git");
        t.apply_ui_effect(&Effect::ShowWorkspace);
        t.apply_ui_effect(&Effect::ClearView);
        assert!(t.overlay.is_none(), "a reset closes /workspace");
        t.apply_ui_effect(&Effect::ShowInfo);
        t.apply_ui_effect(&Effect::ClearView);
        assert!(
            matches!(t.overlay, Some(Overlay::Info(_))),
            "only those two"
        );
    }

    #[test]
    fn a_refresh_reply_rebuilds_the_menu_through_its_wrappers() {
        let mut t = info_tui();
        let chat = t.core.chat_id.unwrap();
        let effects = t.update(Msg::Submit("/info".into()));
        let generation = effects
            .iter()
            .find_map(|e| match e {
                Effect::RefreshChat { generation, .. } => Some(*generation),
                _ => None,
            })
            .expect("/info refreshes the chat");
        t.composer.set_menu(vec![]);
        let mut refreshed = workspace_chat("lint");
        refreshed.id = Some(chat);
        t.update(Msg::ForChat {
            chat,
            msg: Box::new(Msg::ForRefresh {
                generation,
                msg: Box::new(Msg::ChatRefreshed(refreshed)),
            }),
        });
        t.composer.set_text("/li");
        assert!(
            t.composer
                .slash_matches()
                .iter()
                .any(|e| e.label == "/lint"),
            "the refreshed chat's skill is listed"
        );
    }

    /// A directory with `name` in it, removed when dropped.
    struct Files(std::path::PathBuf);

    impl Files {
        fn with(names: &[&str]) -> Files {
            let dir =
                std::env::temp_dir().join(format!("scuttle-mention-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            for name in names {
                std::fs::write(dir.join(name), "x").unwrap();
            }
            Files(dir)
        }

        fn path(&self, name: &str) -> String {
            self.0.join(name).to_string_lossy().into_owned()
        }
    }

    impl Drop for Files {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn type_text(t: &mut Tui, text: &str) {
        for c in text.chars() {
            t.handle(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    fn uploads(effects: &[Effect]) -> Vec<String> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::UploadFile { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn an_at_mention_becomes_a_chip_and_the_second_send_sends_it() {
        let files = Files::with(&["notes.md"]);
        let mut t = tui();
        started(&mut t);
        loaded(&mut t, json!([]));
        let text = format!("read @{}", files.path("notes.md"));
        type_text(&mut t, &text);
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            uploads(&effects).is_empty(),
            "the first send uploads nothing: {effects:?}"
        );
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::SendMessage { .. })),
            "the first send only attaches: {effects:?}"
        );
        assert_eq!(t.composer.text(), text, "the message waits in the composer");
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Info(
                "Attached 1 file from @ mentions. Send again to send it.".into()
            ))
        );
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            uploads(&effects),
            [files.path("notes.md")],
            "the second send uploads the chip, and the message waits for it"
        );
        let file = uuid::Uuid::new_v4();
        let effects = t.update(Msg::FileUploaded {
            local: t.core.chips[0].local,
            file_id: file,
            size: 1,
        });
        assert!(uploads(&effects).is_empty(), "{effects:?}");
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { text: sent, turn, .. }]
                if *sent == text && turn.files == vec![file]),
            "{effects:?}"
        );
        assert!(t.core.chips.is_empty());
    }

    #[test]
    fn a_mention_chip_removed_before_the_second_send_never_uploads() {
        let files = Files::with(&["secret.md"]);
        let mut t = tui();
        started(&mut t);
        loaded(&mut t, json!([]));
        let mut effects = Vec::new();
        type_text(&mut t, &format!("read @{}", files.path("secret.md")));
        effects.extend(t.handle(key(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(t.core.chips.len(), 1, "the mention shows as a chip");
        t.composer.set_text("");
        effects.extend(t.handle(key(KeyCode::Backspace, KeyModifiers::NONE)));
        assert!(t.core.chips.is_empty());
        type_text(&mut t, "never mind");
        effects.extend(t.handle(key(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::UploadFile { .. } | Effect::CancelUpload(_))),
            "{effects:?}"
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::SendMessage { turn, .. } if turn.files.is_empty())),
            "{effects:?}"
        );
    }

    #[test]
    fn editing_between_the_sends_keeps_the_chips_and_attaches_only_new_mentions() {
        let files = Files::with(&["a.md", "b.md"]);
        let mut t = tui();
        started(&mut t);
        loaded(&mut t, json!([]));
        type_text(&mut t, &format!("read @{}", files.path("a.md")));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(uploads(&effects).is_empty(), "{effects:?}");
        assert_eq!(t.core.chips.len(), 1);
        type_text(&mut t, &format!(" and @{}", files.path("b.md")));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            uploads(&effects).is_empty(),
            "only the new mention attaches, and the message waits again: {effects:?}"
        );
        assert_eq!(t.core.chips.len(), 2);
        t.handle(key(KeyCode::Char('!'), KeyModifiers::NONE));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            uploads(&effects),
            [files.path("a.md"), files.path("b.md")],
            "both chips upload at the send that sends the message"
        );
        assert!(t.composer.text().is_empty(), "the edited message went out");
    }

    #[test]
    fn a_mention_in_a_skill_trigger_is_attached() {
        let files = Files::with(&["main.rs"]);
        let mut t = tui();
        started(&mut t);
        loaded(&mut t, json!([]));
        t.update(Msg::SkillsLoaded(vec![scuttle_core::skills::Skill {
            name: "review".into(),
            description: "Review code".into(),
        }]));
        t.composer
            .set_text(&format!("/review @{}", files.path("main.rs")));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(uploads(&effects).is_empty(), "{effects:?}");
        assert_eq!(t.core.chips.len(), 1, "the mention shows as a chip");
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(uploads(&effects), [files.path("main.rs")]);
    }

    #[test]
    fn a_pasted_dotfile_mention_is_left_out_and_a_typed_one_attaches() {
        let files = Files::with(&[".env"]);
        let mut t = tui();
        started(&mut t);
        loaded(&mut t, json!([]));
        let pasted = format!("see @{}", files.path(".env"));
        t.handle(Event::Paste(pasted.clone()));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(
            t.composer.text(),
            pasted,
            "the message waits for a second send"
        );
        assert!(
            t.core
                .notices
                .iter()
                .any(|n| matches!(n, Notice::Info(m) if m.contains("type its leading dot"))),
            "{:?}",
            t.core.notices
        );
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(uploads(&effects).is_empty(), "{effects:?}");
        assert!(
            matches!(effects.as_slice(), [Effect::SendMessage { .. }]),
            "the second send sends it without the file: {effects:?}"
        );
        type_text(&mut t, &format!("see @{}", files.path(".env")));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(uploads(&effects).is_empty(), "{effects:?}");
        assert_eq!(t.core.chips.len(), 1, "a typed dotfile mention attaches");
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(uploads(&effects), [files.path(".env")]);
    }

    #[test]
    fn a_key_that_only_edits_the_composer_reuses_the_transcript_lines() {
        let mut t = tui();
        let mut messages: Vec<_> = (1..=30)
            .map(|i| json!({"id": i, "role": "assistant", "content": [{"type": "text", "text": format!("row {i:02}")}]}))
            .collect();
        messages.push(json!({"id": 31, "role": "assistant", "content": [
            {"type": "tool-call", "tool_call_id": "a", "tool_name": "read_file", "args": {"path": "/x"}},
            {"type": "tool-result", "tool_call_id": "a", "tool_name": "read_file", "result": {"content": "one\ntwo"}}
        ]}));
        loaded(&mut t, serde_json::Value::Array(messages));
        screen(&mut t, 60, 20);
        assert_eq!(t.view_builds, 1);
        t.handle(key(KeyCode::Char('a'), KeyModifiers::NONE));
        t.handle(key(KeyCode::PageUp, KeyModifiers::NONE));
        screen(&mut t, 60, 20);
        assert_eq!(t.view_builds, 1, "typing and scrolling keep the lines");
        t.update(Msg::StreamHealthy);
        screen(&mut t, 60, 20);
        assert_eq!(t.view_builds, 2, "a message to the core rebuilds them");
        t.handle(key(KeyCode::End, KeyModifiers::NONE));
        screen(&mut t, 60, 20);
        let row = t
            .row_of(|target| matches!(target, HitTarget::Toggle(_)))
            .expect("the tool call is on screen");
        click(&mut t, row);
        screen(&mut t, 60, 20);
        assert_eq!(
            t.view_builds, 3,
            "a click that toggles a block rebuilds them"
        );
    }

    #[test]
    fn a_refused_send_keeps_its_mentions_handled_so_a_resend_attaches_none_again() {
        let files = Files::with(&["a.md", "b.exe"]);
        let mut t = tui();
        started(&mut t);
        loaded(&mut t, json!([]));
        type_text(
            &mut t,
            &format!("read @{} and @{}", files.path("a.md"), files.path("b.exe")),
        );
        t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(t.core.chips.len(), 2);
        let Some(Notice::Info(notice)) = t.core.notices.last().cloned() else {
            panic!("{:?}", t.core.notices);
        };
        assert!(
            notice.starts_with("Attached 1 file from @ mentions.") && notice.contains("b.exe"),
            "the failed chip is not counted as attached: {notice}"
        );
        let mut effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        let local = t.core.chips[0].local;
        let file = uuid::Uuid::new_v4();
        let refused = t.update(Msg::FileUploaded {
            local,
            file_id: file,
            size: 1,
        });
        assert!(
            refused
                .iter()
                .any(|e| matches!(e, Effect::RestoreComposer(_))),
            "the failed chip refuses the send once a.md is up: {refused:?}"
        );
        show(&mut t, refused.clone());
        effects.extend(refused);
        t.update(Msg::RemoveLastChip);
        assert_eq!(t.core.chips.len(), 1, "only a.md is left");
        let resent = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            !t.core
                .chips
                .iter()
                .any(|c| matches!(c.state, ChipState::Failed(_))),
            "the refused type does not come back: {:?}",
            t.core.chips
        );
        assert!(
            matches!(resent.as_slice(), [Effect::SendMessage { turn, .. }] if turn.files == vec![file]),
            "the resend sends a.md once and attaches nothing again: {resent:?}"
        );
        effects.extend(resent);
        assert_eq!(uploads(&effects), [files.path("a.md")], "a.md uploads once");
    }

    #[test]
    fn chips_past_the_cap_are_summed_up_as_more() {
        let mut t = tui();
        started(&mut t);
        let names: Vec<String> = (0..30).map(|i| format!("file-{i:02}.md")).collect();
        for name in &names {
            t.update(Msg::Submit(format!("/attach /tmp/{name}")));
        }
        let shown = screen(&mut t, 40, 14);
        let visible = names.iter().filter(|n| shown.contains(n.as_str())).count();
        assert!(visible < names.len(), "{shown}");
        assert!(
            shown.contains(&format!("+{} more", names.len() - visible)),
            "{shown}"
        );
    }

    /// A Tui whose config file is at a fresh path under the temp directory, not yet written.
    fn settings_tui() -> (Tui, PathBuf) {
        let dir = std::env::temp_dir().join(format!("scuttle-settings-{}", uuid::Uuid::new_v4()));
        let path = dir.join("scuttle/config.toml");
        let t = Tui::new(
            scuttle_core::config::LocalConfig::default(),
            Some(path.clone()),
            Theme::terminal(true),
            Welcome {
                url: "https://x".into(),
                user: "nick".into(),
                art: vec![],
                art_accent: true,
                show: true,
                tip: true,
            },
            0,
        );
        (t, path)
    }

    fn remove_settings(path: &std::path::Path) {
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn settings_creates_the_file_and_applies_the_live_keys() {
        use crate::activity::SpinnerStyle;
        let (mut t, path) = settings_tui();
        t.edit_settings_with(|p| {
            let text = std::fs::read_to_string(p)?;
            assert!(text.contains("# mouse = true"), "{text}");
            std::fs::write(
                p,
                "mouse = false\nbusy_behavior = \"interrupt\"\ncomposer_max_lines = 4\nspinner = \"line\"\n[chats]\npin_icon = \"*\"\n[density]\nread_file = \"hidden\"\n",
            )
        });
        assert!(!t.core.mouse);
        assert_eq!(t.core.busy, scuttle_core::config::BusyBehavior::Interrupt);
        assert_eq!(t.spinner, SpinnerStyle::Line);
        assert_eq!(t.config.chats.pin_icon.as_deref(), Some("*"));
        assert_eq!(
            t.config.density.get("read_file"),
            Some(&scuttle_core::density::Density::Hidden)
        );
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Info("Settings applied.".into()))
        );
        t.composer.set_text("1\n2\n3\n4\n5\n6");
        assert_eq!(
            t.composer.height(80),
            4 + 2,
            "the composer stops at the new limit"
        );
        remove_settings(&path);
    }

    #[test]
    fn a_setting_that_waits_for_a_restart_is_named() {
        let (mut t, path) = settings_tui();
        let org = uuid::Uuid::new_v4();
        t.edit_settings_with(|p| {
            std::fs::write(
                p,
                format!("organization = \"{org}\"\n[welcome]\nshow = false\n"),
            )
        });
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Info(
                "Settings applied. welcome.show, organization apply on restart.".into()
            ))
        );
        assert!(
            t.welcome.show,
            "the welcome screen keeps its startup setting"
        );
        remove_settings(&path);
    }

    #[test]
    fn a_parse_error_names_the_line_and_keeps_the_settings_in_effect() {
        let (mut t, path) = settings_tui();
        t.edit_settings_with(|p| std::fs::write(p, "mouse = false\nbusy_behavior = 3\n"));
        assert!(t.core.mouse, "nothing applied");
        assert_eq!(t.core.busy, scuttle_core::config::BusyBehavior::Queue);
        let Some(Notice::Error(message)) = t.core.notices.last() else {
            panic!("{:?}", t.core.notices);
        };
        assert!(message.contains("line 2"), "{message}");
        assert!(
            message.contains("The previous settings stay in effect"),
            "{message}"
        );
        remove_settings(&path);
    }

    #[test]
    fn a_failed_editor_reloads_nothing() {
        let (mut t, path) = settings_tui();
        t.edit_settings_with(|p| {
            std::fs::write(p, "mouse = false\n")?;
            Err(std::io::Error::other("editor exited with exit status: 1"))
        });
        assert!(
            t.core.mouse,
            "a failed editor may have left a half-written file"
        );
        assert!(
            matches!(t.core.notices.last(), Some(Notice::Error(m)) if m.starts_with("Editor failed")),
            "{:?}",
            t.core.notices
        );
        remove_settings(&path);
    }

    #[test]
    fn settings_saved_by_commands_are_not_reported_as_changes() {
        let (mut t, path) = settings_tui();
        let (org, model) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        // What `/organization`, `/mouse`, and an effort choice save to the file.
        assert!(t.apply_ui_effect(&Effect::SaveOrganization(org)));
        assert!(t.apply_ui_effect(&Effect::SetMouse(false)));
        assert!(t.apply_ui_effect(&Effect::SaveEffort {
            model,
            effort: "high".into(),
        }));
        t.edit_settings_with(|_| Ok(()));
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Info("Settings unchanged.".into())),
            "the file holds what the commands saved, so nothing changed"
        );
        remove_settings(&path);
    }

    #[test]
    fn unchanged_settings_say_so_and_no_location_says_why() {
        let (mut t, path) = settings_tui();
        t.edit_settings_with(|_| Ok(()));
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Info("Settings unchanged.".into()))
        );
        remove_settings(&path);
        let mut t = tui();
        t.edit_settings_with(|_| panic!("no editor without a file"));
        assert!(
            matches!(t.core.notices.last(), Some(Notice::Error(m)) if m.contains("no config file location")),
            "{:?}",
            t.core.notices
        );
    }

    /// Replaces the open chat's queue with `queued`.
    fn queue(t: &mut Tui, queued: serde_json::Value) {
        t.update(stream(
            json!({"type": "queue_update", "queued_messages": queued}),
        ));
    }

    /// One queued message, ID 7.
    fn one_queued() -> serde_json::Value {
        json!([{"id": 7, "content": [{"type": "text", "text": "then run the tests"}]}])
    }

    /// A Tui on a loaded chat with one message waiting in the queue, under `shortcut`.
    fn queued_tui(shortcut: SendShortcut) -> Tui {
        let mut t = tui();
        t.core.prefs.send_shortcut = shortcut;
        started(&mut t);
        loaded(&mut t, json!([]));
        queue(&mut t, one_queued());
        t
    }

    #[test]
    fn a_dim_hint_names_the_send_now_key_only_while_messages_wait() {
        let mut t = tui();
        started(&mut t);
        loaded(&mut t, json!([]));
        assert!(!screen(&mut t, 70, 20).contains(QUEUED_HINT));
        queue(&mut t, one_queued());
        let hint = format!("{} {QUEUED_HINT}", t.send_now_key());
        let mut term = Terminal::new(TestBackend::new(70, 20)).unwrap();
        term.draw(|f| t.draw(f)).unwrap();
        let buf = term.backend().buffer().clone();
        let row = |y: u16| (0..70u16).map(|x| buf[(x, y)].symbol()).collect::<String>();
        let y = (0..20u16)
            .find(|&y| row(y).contains(&hint))
            .unwrap_or_else(|| panic!("no queued hint:\n{}", screen(&mut t, 70, 20)));
        assert!(
            row(y + 1).trim_start().starts_with('─'),
            "the hint sits just above the composer:\n{}",
            screen(&mut t, 70, 20)
        );
        let x = (0..70u16).find(|&x| buf[(x, y)].symbol() == "↵").unwrap();
        assert_eq!(buf[(x, y)].style().fg, t.theme.dim.fg, "the hint is dim");
        t.handle(key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(
            !screen(&mut t, 70, 20).contains(QUEUED_HINT),
            "typed text is what the send key sends"
        );
        t.composer.set_text("");
        t.update(Msg::Submit("/attach /tmp/shot.png".into()));
        assert!(
            !screen(&mut t, 70, 20).contains(QUEUED_HINT),
            "an attachment waits for a message, so the send key asks for one"
        );
        t.handle(key(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(t.core.chips.is_empty());
        assert!(screen(&mut t, 70, 20).contains(&hint));
        queue(&mut t, json!([]));
        assert!(
            !screen(&mut t, 70, 20).contains(QUEUED_HINT),
            "it leaves when the queue empties"
        );
    }

    #[test]
    fn in_modifier_mode_the_hint_names_cmd_on_macos_and_ctrl_elsewhere() {
        let expected = if cfg!(target_os = "macos") {
            "⌘↵"
        } else {
            "Ctrl+↵"
        };
        let mut t = queued_tui(SendShortcut::ModifierEnter);
        assert_eq!(t.send_now_key(), expected);
        let shown = screen(&mut t, 70, 20);
        assert!(
            shown.contains(&format!("{expected} {QUEUED_HINT}")),
            "{shown}"
        );
        t.show_help = true;
        let help = screen(&mut t, 200, 120);
        assert!(
            help.contains(&format!("in this terminal that is {expected}")),
            "/help names the same key:\n{help}"
        );
        for mods in [KeyModifiers::SUPER, KeyModifiers::CONTROL] {
            let mut t = queued_tui(SendShortcut::ModifierEnter);
            let effects = t.handle(key(KeyCode::Enter, mods));
            assert!(
                matches!(effects.as_slice(), [Effect::PromoteQueued { id: 7, .. }]),
                "{mods:?}: {effects:?}"
            );
        }
    }

    #[test]
    fn in_enter_mode_the_hint_names_enter_and_cmd_enter_adds_a_line() {
        let mut t = queued_tui(SendShortcut::Enter);
        assert_eq!(t.send_now_key(), "↵");
        let shown = screen(&mut t, 70, 20);
        assert!(shown.contains(&format!("↵ {QUEUED_HINT}")), "{shown}");
        assert!(!shown.contains('⌘') && !shown.contains("Ctrl+↵"), "{shown}");
        assert!(
            t.handle(key(KeyCode::Enter, KeyModifiers::SUPER))
                .is_empty()
        );
        assert_eq!(
            t.composer.text(),
            "\n",
            "Super+Enter is not the send key here"
        );
        t.composer.set_text("");
        assert!(matches!(
            t.handle(key(KeyCode::Enter, KeyModifiers::NONE)).as_slice(),
            [Effect::PromoteQueued { id: 7, .. }]
        ));
    }

    #[test]
    fn without_keyboard_enhancement_the_hint_names_the_alt_enter_fallback() {
        let alt = if cfg!(target_os = "macos") {
            "⌥↵"
        } else {
            "Alt+↵"
        };
        let mut t = queued_tui(SendShortcut::ModifierEnter);
        t.set_keyboard_enhanced(false);
        assert_eq!(t.send_now_key(), alt);
        let shown = screen(&mut t, 70, 20);
        assert!(shown.contains(&format!("{alt} {QUEUED_HINT}")), "{shown}");
        assert!(!shown.contains('⌘') && !shown.contains("Ctrl+↵"), "{shown}");
        // Ctrl+Enter and Cmd+Enter arrive as a plain Enter here, which adds a line.
        assert!(t.handle(key(KeyCode::Enter, KeyModifiers::NONE)).is_empty());
        assert_eq!(t.composer.text(), "\n");
        assert!(
            screen(&mut t, 70, 20).contains(&format!("{alt} {QUEUED_HINT}")),
            "a blank line is still nothing typed"
        );
        assert!(matches!(
            t.handle(key(KeyCode::Enter, KeyModifiers::ALT)).as_slice(),
            [Effect::PromoteQueued { id: 7, .. }]
        ));
        let mut t = queued_tui(SendShortcut::Enter);
        t.set_keyboard_enhanced(false);
        assert_eq!(t.send_now_key(), "↵", "Enter mode needs no enhancement");
        assert!(screen(&mut t, 70, 20).contains(&format!("↵ {QUEUED_HINT}")));
        assert!(matches!(
            t.handle(key(KeyCode::Enter, KeyModifiers::NONE)).as_slice(),
            [Effect::PromoteQueued { id: 7, .. }]
        ));
    }

    #[test]
    fn send_now_fires_exactly_when_the_queued_hint_shows() {
        let question = || {
            let mut t = tui();
            started(&mut t);
            loaded(
                &mut t,
                json!([{"id": 2, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "q1",
                    "tool_name": "ask_user_question", "args": {"questions": [{"header": "Scope", "question": "Which crate?",
                    "options": [{"label": "core", "description": "state"}]}]}}]}]),
            );
            queue(&mut t, one_queued());
            t
        };
        let cases: [(&str, Tui, &str); 4] = [
            ("an empty composer", queued_tui(SendShortcut::Enter), ""),
            ("only spaces", queued_tui(SendShortcut::Enter), "  "),
            ("a question menu and only spaces", question(), "  "),
            ("a question menu", question(), ""),
        ];
        for (case, mut t, typed) in cases {
            for c in typed.chars() {
                t.handle(key(KeyCode::Char(c), KeyModifiers::NONE));
            }
            let hint = screen(&mut t, 70, 24).contains(QUEUED_HINT);
            let fired = t
                .handle(key(KeyCode::Enter, KeyModifiers::NONE))
                .iter()
                .any(|e| matches!(e, Effect::PromoteQueued { .. }));
            assert_eq!(hint, fired, "{case}: hint {hint}, send now {fired}");
        }
    }

    #[test]
    fn the_queued_hint_yields_to_hidden_questions_and_a_ready_plan_but_not_a_copy() {
        // A ready plan takes the row.
        let mut t = tui();
        started(&mut t);
        plan_ready(&mut t, vec![]);
        queue(&mut t, one_queued());
        let shown = screen(&mut t, 70, 20);
        assert!(shown.contains("Implement the plan"), "{shown}");
        assert!(
            !shown.contains(QUEUED_HINT),
            "one row, the plan first:\n{shown}"
        );

        // The question menu takes the send key, so no hint shows; once Esc hides the
        // questions, their hint takes the row.
        let mut t = tui();
        started(&mut t);
        loaded(
            &mut t,
            json!([{"id": 2, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "q1",
                "tool_name": "ask_user_question", "args": {"questions": [{"header": "Scope", "question": "Which crate?",
                "options": [{"label": "core", "description": "state"}]}]}}]}]),
        );
        queue(&mut t, one_queued());
        let shown = screen(&mut t, 70, 24);
        assert!(shown.contains("Question 1 of 1: Scope"), "{shown}");
        assert!(!shown.contains(QUEUED_HINT), "{shown}");
        t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        let shown = screen(&mut t, 70, 24);
        assert!(
            shown.contains("Questions hidden: Tab shows them"),
            "{shown}"
        );
        assert!(
            !shown.contains(QUEUED_HINT),
            "one row, the questions first:\n{shown}"
        );

        // The copy notice is drawn in the composer's rule, so the queued hint stays.
        let mut t = queued_tui(SendShortcut::Enter);
        let now = Instant::now();
        assert!(screen_at(&mut t, 70, 20, now).contains(QUEUED_HINT));
        t.copy("abc".into());
        let shown = screen_at(&mut t, 70, 20, now);
        assert!(shown.contains("Copied 3 characters"), "{shown}");
        assert!(
            shown.contains(QUEUED_HINT),
            "the copy notice leaves the hint row alone:\n{shown}"
        );
        let later = screen_at(&mut t, 70, 20, now + COPY_TTL);
        assert!(later.contains(QUEUED_HINT), "{later}");
    }

    #[test]
    fn the_placeholder_names_the_send_now_key_in_modifier_mode() {
        let cmd = placeholder_text(
            SendShortcut::ModifierEnter,
            true,
            send_now_label(SendShortcut::ModifierEnter, true, true),
        );
        assert!(cmd.contains("⌘↵ to send"), "{cmd}");
        assert!(!cmd.contains("Ctrl+Enter"), "{cmd}");
        let ctrl = placeholder_text(
            SendShortcut::ModifierEnter,
            true,
            send_now_label(SendShortcut::ModifierEnter, true, false),
        );
        assert!(ctrl.contains("Ctrl+↵ to send"), "{ctrl}");
        let option = placeholder_text(
            SendShortcut::ModifierEnter,
            false,
            send_now_label(SendShortcut::ModifierEnter, false, true),
        );
        assert!(option.contains("⌥↵ to send"), "{option}");
        let mut t = tui();
        t.core.prefs.send_shortcut = SendShortcut::ModifierEnter;
        t.set_keyboard_enhanced(true);
        let shown = t.composer.widget().placeholder_text().to_owned();
        assert!(
            shown.contains(&format!("{} to send", t.send_now_key())),
            "{shown}"
        );
        if cfg!(target_os = "macos") {
            assert!(shown.contains("⌘↵ to send"), "{shown}");
            assert!(!shown.contains("Ctrl+Enter"), "{shown}");
        }
    }

    /// A Tui on a chat whose model, `Old`, is disabled, with the default `Fresh` to pick
    /// instead. Returns the Tui and `Fresh`.
    fn tui_on_a_disabled_model() -> (Tui, uuid::Uuid) {
        let mut t = tui();
        started(&mut t);
        let provider = uuid::Uuid::new_v4();
        let (old, fresh) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        t.core.providers = serde_json::from_value(json!([
            {"id": provider, "display_name": "Provider", "available": true}
        ]))
        .unwrap();
        t.update(Msg::ModelsLoaded(
            serde_json::from_value(json!([
                {"id": old, "display_name": "Old", "ai_provider_id": provider, "enabled": false, "reasoning_efforts": []},
                {"id": fresh, "display_name": "Fresh", "ai_provider_id": provider, "enabled": true, "is_default": true, "reasoning_efforts": []}
            ]))
            .unwrap(),
        ));
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "last_model_config_id": old, "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap();
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: vec![],
        });
        (t, fresh)
    }

    /// Applies the effects the Tui owns, as the main loop does, and returns the rest, which
    /// the runtime would run.
    fn apply_ui_effects(t: &mut Tui, effects: Vec<Effect>) -> Vec<Effect> {
        effects
            .into_iter()
            .filter(|e| !t.apply_ui_effect(e))
            .collect()
    }

    #[test]
    fn a_chat_on_an_unavailable_model_says_so_before_anything_is_typed() {
        let (mut t, _) = tui_on_a_disabled_model();
        let now = Instant::now();
        let first = screen_at(&mut t, 100, 20, now);
        assert!(
            first.contains("This chat's model, Old, is not available."),
            "{first}"
        );
        let later = screen_at(&mut t, 100, 20, now + NOTICE_TTL);
        assert!(
            later.contains("/model: Old (unavailable)"),
            "the footer keeps saying so:\n{later}"
        );
    }

    #[test]
    fn the_send_key_on_an_unavailable_model_opens_the_picker_and_esc_gives_the_draft_back() {
        let (mut t, _) = tui_on_a_disabled_model();
        t.composer.set_text("first line\nsecond line");
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            apply_ui_effects(&mut t, effects).is_empty(),
            "nothing goes to the server"
        );
        assert!(matches!(t.overlay, Some(Overlay::Model(_))));
        assert_eq!(t.composer.text(), "", "the core holds the draft");
        let effects = t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(apply_ui_effects(&mut t, effects).is_empty());
        assert!(t.overlay.is_none());
        assert_eq!(t.composer.text(), "first line\nsecond line");
    }

    #[test]
    fn picking_a_model_sends_the_held_draft_with_it() {
        let (mut t, fresh) = tui_on_a_disabled_model();
        t.composer.set_text("look");
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        apply_ui_effects(&mut t, effects);
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        let sent = apply_ui_effects(&mut t, effects);
        assert!(
            matches!(sent.as_slice(), [Effect::SendMessage { text, model, .. }] if text == "look" && *model == Some(fresh)),
            "{sent:?}"
        );
        assert!(t.overlay.is_none());
        assert_eq!(t.composer.text(), "");
    }

    /// The deferred-pick notice, shown when a refusal cannot open the `/model` table.
    const PICK_LATER: &str =
        "This chat's model is unavailable. Pick one with /model to send your held message.";

    /// Sends "look" on `Fresh`, which the list still offers, and returns the refusal the
    /// server would answer with.
    fn refused_send(t: &mut Tui, fresh: uuid::Uuid) -> Msg {
        t.update(Msg::ModelChosen(fresh));
        t.composer.set_text("look");
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        let seq = apply_ui_effects(t, effects)
            .iter()
            .find_map(|e| match e {
                Effect::SendMessage { seq, .. } => Some(*seq),
                _ => None,
            })
            .expect("the message goes to the server");
        Msg::ModelUnavailable {
            text: "look".into(),
            files: vec![],
            message: "Invalid model config ID.".into(),
            plan_mode: None,
            seq,
        }
    }

    fn last_notice(t: &Tui) -> String {
        match t.core.notices.last() {
            Some(Notice::Error(m) | Notice::Info(m)) => m.clone(),
            None => String::new(),
        }
    }

    #[test]
    fn a_refusal_leaves_an_open_overlay_alone_and_slash_model_sends_the_draft_later() {
        let (mut t, fresh) = tui_on_a_disabled_model();
        let refusal = refused_send(&mut t, fresh);
        t.overlay = Some(Overlay::Mcp(crate::table::TableState::default()));
        let effects = t.update(refusal);
        let rest = apply_ui_effects(&mut t, effects);
        assert!(
            matches!(rest.as_slice(), [Effect::FetchModels(_)]),
            "only the list refetch goes to the runtime: {rest:?}"
        );
        assert!(
            matches!(t.overlay, Some(Overlay::Mcp(_))),
            "the open overlay stays"
        );
        assert_eq!(last_notice(&t), PICK_LATER);
        let effects = t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        apply_ui_effects(&mut t, effects);
        assert!(t.overlay.is_none());
        t.composer.set_text("/model");
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        apply_ui_effects(&mut t, effects);
        assert!(matches!(t.overlay, Some(Overlay::Model(_))));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        let sent = apply_ui_effects(&mut t, effects);
        assert!(
            matches!(sent.as_slice(), [Effect::SendMessage { text, .. }] if text == "look"),
            "{sent:?}"
        );
    }

    #[test]
    fn a_refusal_while_the_user_types_never_takes_their_keys() {
        let (mut t, fresh) = tui_on_a_disabled_model();
        let refusal = refused_send(&mut t, fresh);
        t.composer.set_text("next");
        let effects = t.update(refusal);
        let rest = apply_ui_effects(&mut t, effects);
        assert!(
            matches!(rest.as_slice(), [Effect::FetchModels(_)]),
            "only the list refetch goes to the runtime: {rest:?}"
        );
        assert!(t.overlay.is_none(), "the picker waits");
        assert_eq!(t.composer.text(), "next");
        assert_eq!(last_notice(&t), PICK_LATER);
        // The refetched list no longer offers the refused model, so sending the next
        // message joins the held one and opens the picker.
        let spare = uuid::Uuid::new_v4();
        t.update(Msg::ModelsLoaded(
            serde_json::from_value(json!([
                {"id": fresh, "display_name": "Fresh", "enabled": false, "reasoning_efforts": []},
                {"id": spare, "display_name": "Spare", "enabled": true, "is_default": true, "reasoning_efforts": []}
            ]))
            .unwrap(),
        ));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        apply_ui_effects(&mut t, effects);
        assert!(matches!(t.overlay, Some(Overlay::Model(_))));
        let effects = t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        apply_ui_effects(&mut t, effects);
        assert_eq!(t.composer.text(), "look\n\nnext");
    }

    #[test]
    fn a_refusal_with_nothing_open_opens_the_picker() {
        let (mut t, fresh) = tui_on_a_disabled_model();
        let refusal = refused_send(&mut t, fresh);
        let effects = t.update(refusal);
        apply_ui_effects(&mut t, effects);
        assert!(matches!(t.overlay, Some(Overlay::Model(_))));
    }

    #[test]
    fn ctrl_o_copies_the_whole_draft_and_confirms_in_the_composer_rule() {
        let mut t = tui();
        t.composer.set_text("first line\nsecond line");
        let now = Instant::now();
        assert!(
            t.handle(key(KeyCode::Char('o'), KeyModifiers::CONTROL))
                .is_empty()
        );
        assert_eq!(t.last_copied.as_deref(), Some("first line\nsecond line"));
        assert_eq!(
            t.composer.text(),
            "first line\nsecond line",
            "the draft stays, and no 'o' is typed"
        );
        let shown = screen_at(&mut t, 60, 20, now);
        assert!(shown.contains("Copied 22 characters"), "{shown}");
        let later = screen_at(&mut t, 60, 20, now + COPY_TTL);
        assert!(!later.contains("Copied"), "{later}");
    }

    #[test]
    fn ctrl_o_on_a_blank_composer_copies_nothing_and_says_so() {
        let mut t = tui();
        t.composer.set_text("  \n ");
        t.handle(key(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert_eq!(t.last_copied, None);
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Info(NOTHING_TO_COPY.into()))
        );
    }

    #[test]
    fn an_open_overlay_keeps_ctrl_o() {
        let mut t = tui();
        t.composer.set_text("draft");
        t.show_help = true;
        t.handle(key(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert!(!t.show_help, "any other key closes /help, as before");
        assert_eq!(
            t.last_copied, None,
            "the key closed /help and copied nothing"
        );
    }

    #[test]
    fn ctrl_o_does_nothing_while_a_table_overlay_is_open() {
        let (mut t, _, _) = chats_tui();
        t.composer.set_text("draft");
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        let before = screen(&mut t, 80, 20);
        let effects = t.handle(key(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert!(effects.is_empty(), "{effects:?}");
        assert!(
            matches!(t.overlay, Some(Overlay::Chats(_))),
            "the overlay stays"
        );
        assert_eq!(t.last_copied, None, "the overlay covers the draft");
        assert_eq!(t.composer.text(), "draft");
        assert_eq!(
            screen(&mut t, 80, 20),
            before,
            "the filter types no 'o' and no confirmation shows"
        );
    }

    #[test]
    fn ctrl_o_leaves_the_question_menu_open() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        loaded(
            &mut t,
            json!([{"id": 2, "role": "assistant", "content": [{"type": "tool-call", "tool_call_id": "q1",
            "tool_name": "ask_user_question", "args": {"questions": [
                {"header": "Scope", "question": "Which crate?", "options": [{"label": "core", "description": "state"}]},
                {"header": "Tests", "question": "Which kind?", "options": [{"label": "unit", "description": "fast"}]}
            ]}}]}]),
        );
        assert!(screen(&mut t, 70, 24).contains("Question 1 of 2: Scope"));
        t.handle(key(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert_eq!(t.last_copied, None);
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Info(NOTHING_TO_COPY.into()))
        );
        assert!(screen(&mut t, 70, 24).contains("Question 1 of 2: Scope"));
    }

    const HELD_HINT: &str = "Held message: pick a model with /model to send it.";

    #[test]
    fn the_hint_row_says_a_message_is_held_until_it_is_sent_or_given_back() {
        let (mut t, fresh) = tui_on_a_disabled_model();
        let refusal = refused_send(&mut t, fresh);
        t.composer.set_text("next");
        let effects = t.update(refusal);
        apply_ui_effects(&mut t, effects);
        let shown = screen(&mut t, 100, 20);
        assert!(shown.contains(HELD_HINT), "even while typing:\n{shown}");
        // With messages queued too, the send key sends the queued one, so the one hint row
        // names both.
        t.composer.set_text("");
        queue(&mut t, one_queued());
        let shown = screen(&mut t, 100, 20);
        assert!(
            !shown.contains(HELD_HINT),
            "the plain held hint would hide what the send key does:\n{shown}"
        );
        assert!(
            shown.contains(&format!(
                "Held message: /model to send it. {} {QUEUED_HINT}",
                t.send_now_key()
            )),
            "{shown}"
        );
        // Typing leaves the send key to the draft, so the hint is the held one again.
        t.composer.set_text("next");
        assert!(screen(&mut t, 100, 20).contains(HELD_HINT));
        t.composer.set_text("");
        // Given back, it clears.
        t.composer.set_text("/model");
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        apply_ui_effects(&mut t, effects);
        let effects = t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        apply_ui_effects(&mut t, effects);
        assert_eq!(t.composer.text(), "look");
        assert!(!screen(&mut t, 100, 20).contains(HELD_HINT));
    }

    #[test]
    fn the_hint_row_clears_once_the_held_message_is_sent() {
        let (mut t, fresh) = tui_on_a_disabled_model();
        t.composer.set_text("look");
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        apply_ui_effects(&mut t, effects);
        let effects = t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        apply_ui_effects(&mut t, effects);
        assert!(!screen(&mut t, 100, 20).contains(HELD_HINT), "given back");
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        apply_ui_effects(&mut t, effects);
        t.overlay = None;
        assert!(screen(&mut t, 100, 20).contains(HELD_HINT), "held again");
        let sent = t.update(Msg::ModelChosen(fresh));
        assert!(
            sent.iter().any(|e| matches!(e, Effect::SendMessage { .. })),
            "{sent:?}"
        );
        assert!(!screen(&mut t, 100, 20).contains(HELD_HINT), "sent");
    }

    #[test]
    fn the_queued_hint_hides_while_an_overlay_is_open() {
        let mut t = queued_tui(SendShortcut::Enter);
        assert!(t.sends_now());
        t.overlay = Some(Overlay::Queue(crate::table::TableState::default()));
        assert!(!t.sends_now(), "Enter belongs to the overlay");
        assert!(!screen(&mut t, 70, 20).contains(QUEUED_HINT));
    }

    #[test]
    fn the_queued_hint_hides_while_the_save_question_is_open() {
        let mut t = queued_tui(SendShortcut::Enter);
        assert!(t.sends_now());
        t.core.save_conflict = Some(scuttle_core::files::SaveConflict {
            file: uuid::Uuid::new_v4(),
            name: "a.txt".into(),
            path: "/h/Downloads/a.txt".into(),
        });
        assert!(!t.sends_now(), "the question takes Enter");
        assert!(!screen(&mut t, 70, 20).contains(QUEUED_HINT));
    }

    #[test]
    fn the_footer_shows_the_configured_fields_in_their_order() {
        let config = LocalConfig {
            statusline: StatuslineConfig {
                fields: vec![StatusField::Status, StatusField::Model, StatusField::Cost],
                ..StatuslineConfig::default()
            },
            ..LocalConfig::default()
        };
        let mut t = Tui::new(
            config,
            None,
            Theme::terminal(true),
            Welcome {
                url: "https://x".into(),
                user: String::new(),
                art: vec![],
                art_accent: true,
                show: true,
                tip: true,
            },
            0,
        );
        assert!(
            t.core.cost_in_footer,
            "the core fetches cost for the footer"
        );
        t.update(Msg::ModelsLoaded(vec![serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "display_name": "Big", "is_default": true, "enabled": true, "reasoning_efforts": []})).unwrap()]));
        let shown = screen(&mut t, 60, 10);
        assert!(shown.contains("new chat · /model: Big"), "{shown}");
        assert!(
            !tui().core.cost_in_footer,
            "the default footer shows no cost"
        );
    }

    #[test]
    fn statusline_changes_the_footer_and_saves_each_change() {
        use scuttle_core::config::StatusField;
        let dir = std::env::temp_dir().join(format!("scuttle-statusline-{}", uuid::Uuid::new_v4()));
        let _cleanup = RemoveOnDrop(dir.clone());
        let path = dir.join("config.toml");
        let mut t = Tui::new(
            LocalConfig::default(),
            Some(path.clone()),
            Theme::terminal(true),
            Welcome {
                url: "https://x".into(),
                user: String::new(),
                art: vec![],
                art_accent: true,
                show: true,
                tip: true,
            },
            0,
        );
        t.update(Msg::ModelsLoaded(vec![serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "display_name": "Big", "is_default": true, "enabled": true, "reasoning_efforts": []})).unwrap()]));
        assert!(screen(&mut t, 100, 24).contains("/model: Big · new chat"));
        let effects = t.update(Msg::Submit("/statusline".into()));
        show(&mut t, effects);
        let shown = screen(&mut t, 100, 24);
        assert!(shown.contains("[x] model"), "{shown}");
        assert!(shown.contains("[ ] cost"), "{shown}");
        t.handle(key(KeyCode::Char(' '), KeyModifiers::NONE));
        let shown = screen(&mut t, 100, 24);
        assert!(shown.contains("[ ] model"), "{shown}");
        assert!(
            !shown.contains("/model: Big"),
            "the footer follows at once:\n{shown}"
        );
        let saved = config::load(&path).unwrap().statusline;
        assert!(!saved.fields.contains(&StatusField::Model), "{saved:?}");
        assert_eq!(t.config.statusline, saved, "the config follows the file");
        for _ in 0..8 {
            t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        }
        t.handle(key(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(
            config::load(&path)
                .unwrap()
                .statusline
                .fields
                .contains(&StatusField::Cost),
            "the ninth row is cost"
        );
        assert!(t.core.cost_in_footer);
        assert!(t.cost_due, "the cost is asked for at the next wakeup");
        t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(t.overlay.is_none());
    }

    /// Removes a test's temp directory when the test ends, even when an assertion fails.
    struct RemoveOnDrop(PathBuf);

    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn three_statusline_changes_to_a_read_only_config_say_so_once() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("scuttle-ro-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "mouse = true\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let mut t = Tui::new(
            LocalConfig::default(),
            Some(path.clone()),
            Theme::terminal(true),
            Welcome {
                url: "https://x".into(),
                user: String::new(),
                art: vec![],
                art_accent: true,
                show: true,
                tip: true,
            },
            0,
        );
        let now = Instant::now();
        t.sync_notice(now);
        let effects = t.update(Msg::Submit("/statusline".into()));
        show(&mut t, effects);
        for _ in 0..3 {
            t.handle_at(
                key(KeyCode::Char(' '), KeyModifiers::NONE),
                now,
                paste_time(),
            );
            t.sync_notice(now);
        }
        let first = t.active_notice().cloned();
        t.sync_notice(now + NOTICE_TTL);
        let after = t.active_notice().cloned();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        assert!(
            matches!(&first, Some(Notice::Error(m)) if m.contains("config.toml is read-only")),
            "{first:?}"
        );
        assert_eq!(after, None, "no copy of the error waits its turn");
    }

    #[test]
    fn an_error_repeating_the_active_or_a_waiting_one_is_dropped() {
        let mut t = tui();
        let now = Instant::now();
        // The first draw, so the notices below arrive while scuttle runs.
        t.sync_notice(now);
        t.core.notices.push(Notice::Error("boom".into()));
        t.sync_notice(now);
        t.core.notices.push(Notice::Error("boom".into()));
        t.sync_notice(now + Duration::from_secs(1));
        assert_eq!(t.active_notice(), Some(&Notice::Error("boom".into())));
        assert_eq!(
            t.notice_deadline(),
            Some(now + NOTICE_TTL),
            "the copy never restarts the error's time on screen"
        );
        t.core.notices.push(Notice::Info("later".into()));
        t.core.notices.push(Notice::Error("boom".into()));
        t.core.notices.push(Notice::Error("bang".into()));
        t.core.notices.push(Notice::Error("bang".into()));
        t.sync_notice(now + Duration::from_secs(2));
        assert_eq!(t.active_notice(), Some(&Notice::Error("bang".into())));
        let mut shown = Vec::new();
        for step in 1..=4 {
            t.sync_notice(now + Duration::from_secs(2) + NOTICE_TTL * step);
            shown.extend(t.active_notice().cloned());
        }
        // "later" waited its ten seconds behind the errors, so it went stale.
        assert_eq!(
            shown,
            vec![Notice::Error("boom".into())],
            "each error shows once"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_statusline_change_to_a_read_only_config_says_so() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("scuttle-ro-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "mouse = true\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let mut t = Tui::new(
            LocalConfig::default(),
            Some(path.clone()),
            Theme::terminal(true),
            Welcome {
                url: "https://x".into(),
                user: String::new(),
                art: vec![],
                art_accent: true,
                show: true,
                tip: true,
            },
            0,
        );
        let now = Instant::now();
        t.sync_notice(now);
        let effects = t.update(Msg::Submit("/statusline".into()));
        show(&mut t, effects);
        t.handle(key(KeyCode::Char(' '), KeyModifiers::NONE));
        t.sync_notice(now);
        let active = t.active_notice().cloned();
        let on_disk = std::fs::read_to_string(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        assert!(
            matches!(&active, Some(Notice::Error(m)) if m.contains("config.toml is read-only")),
            "{active:?}"
        );
        assert_eq!(on_disk, "mouse = true\n");
        assert_eq!(
            t.config.statusline,
            StatuslineConfig::default(),
            "the config keeps what the file holds"
        );
        assert!(
            !t.statusline.fields.contains(&StatusField::Model),
            "the change still applies for this session"
        );
    }

    #[test]
    fn the_footer_follows_a_reloaded_statusline() {
        let (mut t, path) = settings_tui();
        t.update(Msg::ModelsLoaded(vec![serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "display_name": "Big", "is_default": true, "enabled": true, "reasoning_efforts": []})).unwrap()]));
        let generation = t
            .update(Msg::RefreshLimits)
            .iter()
            .find_map(|e| match e {
                Effect::FetchSpend { generation } => Some(*generation),
                _ => None,
            })
            .expect("a spend fetch");
        t.update(Msg::SpendLoaded {
            spend: Box::new(
                serde_json::from_value(json!({"current_spend_micros": 9_000_000,
                    "effective_budget": {"spend_limit_micros": 10_000_000, "limit_source": "group"}}))
                .unwrap(),
            ),
            generation,
        });
        let now = Instant::now();
        let shown = screen_at(&mut t, 80, 10, now);
        assert!(
            shown.contains("/model: Big · spend $9.00/$10.00 · new chat"),
            "{shown}"
        );
        assert!(!t.core.cost_in_footer);
        t.edit_settings_with(|p| {
            std::fs::write(
                p,
                "[statusline]\nfields = [\"status\", \"model\", \"cost\"]\n",
            )
        });
        assert!(
            t.core.cost_in_footer,
            "the core fetches cost for the new list"
        );
        assert!(t.cost_due, "/settings and /statusline share one path");
        assert!(screen_at(&mut t, 80, 10, now).contains("Settings applied."));
        let shown = screen_at(&mut t, 80, 10, now + NOTICE_TTL);
        assert!(shown.contains("new chat · /model: Big"), "{shown}");
        assert!(
            !shown.contains("spend"),
            "spend is no longer listed: {shown}"
        );
        t.edit_settings_with(|p| {
            std::fs::write(
                p,
                "[statusline]\nfields = [\"model\", \"status\"]\n[statusline.thresholds]\nspend = 80\n",
            )
        });
        assert!(!t.core.cost_in_footer, "cost is no longer listed");
        screen_at(&mut t, 80, 10, now + NOTICE_TTL);
        let shown = screen_at(&mut t, 80, 10, now + NOTICE_TTL * 2);
        assert!(
            shown.contains("/model: Big · new chat · spend $9.00/$10.00"),
            "the spend past its new threshold joins the end: {shown}"
        );
        remove_settings(&path);
    }

    fn fetches_spend(effects: &[Effect]) -> bool {
        effects
            .iter()
            .any(|e| matches!(e, Effect::FetchSpend { .. }))
    }

    #[test]
    fn limits_refresh_at_once_then_every_minute() {
        let mut t = info_tui();
        let start = t.epoch;
        assert!(
            t.usage_deadline().is_some_and(|d| d <= start),
            "the first refresh is due at startup"
        );
        let effects = t.poll_usage(start);
        assert!(
            effects.contains(&Effect::FetchSpend { generation: 1 }),
            "{effects:?}"
        );
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::FetchQuota { generation: 1, .. })),
            "{effects:?}"
        );
        assert_eq!(t.usage_deadline(), Some(start + LIMITS_EVERY));
        assert!(
            t.poll_usage(start + LIMITS_EVERY - Duration::from_secs(1))
                .is_empty(),
            "not due yet"
        );
        assert_eq!(
            t.usage_deadline(),
            Some(start + LIMITS_EVERY),
            "a poll before the deadline leaves it, so the loop may poll on every iteration"
        );
        let effects = t.poll_usage(start + LIMITS_EVERY);
        assert!(
            effects.contains(&Effect::FetchSpend { generation: 2 }),
            "{effects:?}"
        );
    }

    #[test]
    fn a_finished_turn_refreshes_the_limits_at_the_next_wakeup() {
        let mut t = info_tui();
        let start = t.epoch;
        t.poll_usage(start);
        t.update(status("running"));
        assert_eq!(
            t.usage_deadline(),
            Some(start + LIMITS_EVERY),
            "a turn in progress changes nothing"
        );
        t.update(status("waiting"));
        let later = start + Duration::from_secs(10);
        assert!(
            t.usage_deadline().is_some_and(|d| d <= later),
            "the turn ended"
        );
        assert!(fetches_spend(&t.poll_usage(later)));
        assert_eq!(t.usage_deadline(), Some(later + LIMITS_EVERY));
    }

    #[test]
    fn choosing_another_organization_refreshes_its_quota_at_the_next_wakeup() {
        let mut t = tui();
        let org = |name: &str| scuttle_core::app::OrgRef {
            id: uuid::Uuid::new_v4(),
            name: name.into(),
            display_name: name.into(),
            is_default: false,
            can_create_chats: true,
        };
        let (coder, product) = (org("coder"), org("product"));
        t.update(Msg::OrganizationsLoaded(vec![
            coder.clone(),
            product.clone(),
        ]));
        t.update(Msg::Started {
            org_id: coder.id,
            open_chat: None,
        });
        let start = t.epoch;
        t.poll_usage(start);
        t.update(Msg::OrganizationChosen(product.id));
        let later = start + Duration::from_secs(5);
        assert!(t.usage_deadline().is_some_and(|d| d <= later));
        let effects = t.poll_usage(later);
        assert!(
            effects.contains(&Effect::FetchQuota {
                org: product.id,
                generation: 2
            }),
            "{effects:?}"
        );
    }

    #[test]
    fn the_chat_cost_is_fetched_after_a_turn_only_while_the_footer_shows_it() {
        let mut t = info_tui();
        let start = t.epoch;
        t.poll_usage(start);
        t.update(status("running"));
        t.update(status("waiting"));
        let effects = t.poll_usage(start);
        assert!(fetches_spend(&effects));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::FetchCost { .. })),
            "the default footer shows no cost: {effects:?}"
        );
        t.core.cost_in_footer = true;
        t.update(status("running"));
        t.update(status("waiting"));
        let effects = t.poll_usage(start);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::FetchCost { .. })),
            "{effects:?}"
        );
        assert!(
            t.usage_deadline().is_some_and(|d| d > start),
            "the deadline moves past the cost poll, so the loop never spins"
        );
    }

    #[test]
    fn a_chat_switch_asks_for_the_opened_chats_cost_at_the_next_wakeup() {
        let mut t = info_tui();
        let start = t.epoch;
        t.core.cost_in_footer = true;
        t.poll_usage(start);
        assert!(!t.cost_due);
        let other = uuid::Uuid::new_v4();
        t.update(Msg::OpenChat(other));
        assert!(t.cost_due, "leaving a chat makes the cost due");
        t.poll_usage(start);
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(
                serde_json::from_value(json!({"id": other, "children": [], "files": [],
                "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}}))
                .unwrap(),
            ),
            messages: vec![],
        });
        assert!(t.cost_due, "the opened chat's cost is due once it loads");
        assert_eq!(t.usage_deadline(), Some(t.epoch));
        let effects = t.poll_usage(start);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::FetchCost { chat, .. } if *chat == other)),
            "{effects:?}"
        );
        assert!(!t.cost_due);
    }

    #[test]
    fn nothing_left_to_refresh_leaves_no_usage_deadline() {
        use scuttle_core::usage::{Limit, Refusal};
        let refused = |t: &mut Tui, refusal: Refusal| {
            let start = t.epoch;
            t.poll_usage(start);
            for limit in [Limit::Spend, Limit::Quota] {
                t.update(Msg::LimitFailed {
                    limit,
                    refusal: refusal.clone(),
                    generation: 1,
                });
            }
        };
        let mut t = info_tui();
        refused(&mut t, Refusal::Absent);
        assert_eq!(t.usage_deadline(), None, "both limits are absent");
        t.core.cost_in_footer = true;
        t.update(status("running"));
        t.update(status("waiting"));
        assert_eq!(
            t.usage_deadline(),
            Some(t.epoch),
            "the cost is still due after a turn"
        );
        t.poll_usage(t.epoch);
        assert_eq!(t.usage_deadline(), None);

        let mut t = info_tui();
        refused(&mut t, Refusal::Unlicensed("Premium".into()));
        assert_eq!(t.usage_deadline(), None, "both limits are unlicensed");

        let mut t = info_tui();
        t.core.cost_in_footer = true;
        refused(&mut t, Refusal::Unauthorized);
        t.update(status("running"));
        t.update(status("waiting"));
        assert_eq!(t.usage_deadline(), None, "a 401 stops every refresh");
    }

    #[test]
    fn turns_and_an_organization_change_in_a_row_refresh_the_limits_once() {
        let mut t = info_tui();
        let start = t.epoch;
        t.poll_usage(start);
        for _ in 0..2 {
            t.update(status("running"));
            t.update(status("waiting"));
        }
        let other = uuid::Uuid::new_v4();
        t.update(Msg::OrganizationsLoaded(vec![scuttle_core::app::OrgRef {
            id: other,
            name: "other".into(),
            display_name: "other".into(),
            is_default: false,
            can_create_chats: true,
        }]));
        t.update(Msg::OrganizationChosen(other));
        let later = start + Duration::from_secs(3);
        let effects = t.poll_usage(later);
        assert_eq!(
            effects
                .iter()
                .filter(|e| matches!(e, Effect::FetchSpend { .. }))
                .count(),
            1,
            "{effects:?}"
        );
        assert!(
            t.poll_usage(later + Duration::from_secs(1)).is_empty(),
            "the burst was answered by one refresh"
        );
    }

    /// A paste that becomes a snippet: twelve numbered lines.
    fn big_paste() -> String {
        (1..=12).map(|i| format!("log line {i}\n")).collect()
    }

    /// When the tests' pastes arrive, in milliseconds since the Unix epoch.
    /// 1,700,000,000 seconds after the Unix epoch, on a clock five hours behind UTC.
    fn paste_time() -> crate::composer::PasteTime {
        chrono::DateTime::from_timestamp(1_700_000_000, 0)
            .unwrap()
            .with_timezone(&chrono::FixedOffset::west_opt(5 * 3600).unwrap())
    }

    /// The file a paste at `paste_time()` uploads as, named in that clock's local time.
    const PASTE_FILE: &str = "pasted-text-2023-11-14-17-13-20.txt";

    /// Pastes `big_paste()` at `at`, and at `paste_time()` on the local clock.
    fn paste_big(t: &mut Tui, at: Instant) -> Vec<Effect> {
        t.handle_at(Event::Paste(big_paste()), at, paste_time())
    }

    #[test]
    fn a_large_paste_shows_a_token_and_goes_as_a_text_file_with_the_message() {
        let mut t = tui();
        started(&mut t);
        loaded(&mut t, json!([]));
        type_text(&mut t, "why did this fail? ");
        paste_big(&mut t, Instant::now());
        assert_eq!(
            t.composer.text(),
            "why did this fail? [Pasted text #1 +12 lines]"
        );
        assert!(screen(&mut t, 80, 20).contains("[Pasted text #1 +12 lines]"));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::UploadText { name, text, .. }
                if name == PASTE_FILE && *text == big_paste())),
            "{effects:?}"
        );
        assert_eq!(t.composer.text(), "", "the message waits for its upload");
        let local = t.core.chips[0].local;
        let effects = t.update(Msg::FileUploaded {
            local,
            file_id: uuid::Uuid::new_v4(),
            size: 132,
        });
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::SendMessage { text, .. }
                if text == "why did this fail?")),
            "the token left the text: {effects:?}"
        );
    }

    #[test]
    fn a_send_with_a_token_edited_into_plain_text_is_refused_and_names_the_paste() {
        let mut t = tui();
        started(&mut t);
        loaded(&mut t, json!([]));
        type_text(&mut t, "see ");
        paste_big(&mut t, Instant::now());
        for _ in 0..5 {
            t.handle(key(KeyCode::Left, KeyModifiers::NONE));
        }
        type_text(&mut t, "x");
        let draft = t.composer.text();
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::UploadText { .. } | Effect::SendMessage { .. })),
            "{effects:?}"
        );
        assert_eq!(t.composer.text(), draft, "the draft stays");
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Error(broken_paste("[Pasted text #1 +12 lines]")))
        );
        assert!(t.core.chips.is_empty(), "no paste was attached");
    }

    #[test]
    fn a_paste_alone_sends_a_message_of_only_its_file() {
        let mut t = tui();
        started(&mut t);
        loaded(&mut t, json!([]));
        paste_big(&mut t, Instant::now());
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::UploadText { name, .. } if name == PASTE_FILE)),
            "{effects:?}"
        );
        assert_eq!(t.composer.text(), "");
        let file = uuid::Uuid::new_v4();
        let local = t.core.chips[0].local;
        let effects = t.update(Msg::FileUploaded {
            local,
            file_id: file,
            size: 132,
        });
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::SendMessage { text, turn, .. }
                if text.is_empty() && turn.files == vec![file])),
            "{effects:?}"
        );
    }

    #[test]
    fn pasting_the_same_text_again_right_away_expands_the_token() {
        let at = Instant::now();
        let mut t = tui();
        paste_big(&mut t, at);
        paste_big(&mut t, at + Duration::from_millis(500));
        assert_eq!(t.composer.text(), big_paste());
        let mut t = tui();
        paste_big(&mut t, at);
        paste_big(&mut t, at + crate::composer::PASTE_AGAIN);
        assert_eq!(
            t.composer.text(),
            "[Pasted text #1 +12 lines][Pasted text #2 +12 lines]",
            "a paste after the window adds a second token"
        );
    }

    #[test]
    fn a_draft_recalled_after_a_failed_send_attaches_its_paste_once() {
        let mut t = tui();
        started(&mut t);
        loaded(&mut t, json!([]));
        let chat = t.core.chat_id.unwrap();
        type_text(&mut t, "why? ");
        paste_big(&mut t, Instant::now());
        t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        let local = t.core.chips[0].local;
        let effects = t.update(Msg::FileUploaded {
            local,
            file_id: uuid::Uuid::new_v4(),
            size: 132,
        });
        let seq = effects
            .iter()
            .find_map(|e| match e {
                Effect::SendMessage { seq, .. } => Some(*seq),
                _ => None,
            })
            .expect("the message went out");
        let failed = t.update(Msg::ForChat {
            chat,
            msg: Box::new(Msg::SendFailed {
                text: "why?".into(),
                message: "boom".into(),
                plan_mode: None,
                seq,
                mcp_rejected: false,
            }),
        });
        for effect in &failed {
            t.apply_ui_effect(effect);
        }
        assert_eq!(t.core.chips.len(), 1, "the paste is back as a chip");
        t.composer.set_text("");
        t.handle(key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(t.composer.text(), "why? [Pasted text #1 +12 lines]");
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            t.core.chips.iter().filter(|c| c.name == PASTE_FILE).count(),
            1,
            "{:?}",
            t.core.chips
        );
        assert_eq!(
            effects
                .iter()
                .filter(|e| matches!(e, Effect::UploadText { .. }))
                .count(),
            1,
            "{effects:?}"
        );
    }

    #[test]
    fn ctrl_o_copies_a_draft_with_its_pasted_text_expanded() {
        let mut t = tui();
        type_text(&mut t, "see ");
        paste_big(&mut t, Instant::now());
        t.handle(key(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert_eq!(t.last_copied, Some(format!("see {}", big_paste())));
        assert_eq!(
            t.composer.text(),
            "see [Pasted text #1 +12 lines]",
            "the draft keeps its token"
        );
    }

    #[test]
    fn settings_keep_a_pasted_snippet_in_the_draft() {
        let (mut t, path) = settings_tui();
        type_text(&mut t, "see ");
        paste_big(&mut t, Instant::now());
        t.edit_settings_with(|p| std::fs::write(p, "composer_max_lines = 4\n"));
        assert_eq!(t.composer.text(), "see [Pasted text #1 +12 lines]");
        assert_eq!(t.composer.expanded_text(), format!("see {}", big_paste()));
        remove_settings(&path);
    }

    #[test]
    fn a_mention_hold_keeps_the_token_and_the_second_send_attaches_both() {
        let files = Files::with(&["a.md"]);
        let mut t = tui();
        started(&mut t);
        loaded(&mut t, json!([]));
        type_text(&mut t, &format!("compare @{} with ", files.path("a.md")));
        paste_big(&mut t, Instant::now());
        let held = t.composer.text();
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(uploads(&effects).is_empty(), "{effects:?}");
        assert_eq!(
            t.composer.text(),
            held,
            "the draft, token and all, waits for a second send"
        );
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(uploads(&effects), [files.path("a.md")]);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::UploadText { name, .. } if name == PASTE_FILE)),
            "{effects:?}"
        );
    }

    #[test]
    fn a_large_paste_into_the_rename_editor_stays_one_line() {
        let (mut t, _, _) = chats_tui();
        show(&mut t, vec![Effect::ShowChats(String::new())]);
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        t.handle(key(KeyCode::Char('e'), KeyModifiers::CONTROL));
        for _ in 0.."Fix the flaky watch reconnect test".len() {
            t.handle(key(KeyCode::Backspace, KeyModifiers::NONE));
        }
        paste_big(&mut t, Instant::now());
        assert_eq!(
            t.core.editor.as_ref().map(|e| e.line.text().to_owned()),
            Some(one_line_paste(&big_paste()))
        );
        assert_eq!(t.composer.text(), "", "no snippet was made");
    }

    #[test]
    fn a_paste_over_the_upload_limit_is_refused_with_its_size() {
        let mut t = tui();
        let max = scuttle_core::attachments::MAX_FILE_BYTES;
        t.handle(Event::Paste("a".repeat(max as usize + 1)));
        assert_eq!(t.composer.text(), "");
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Error(
                "The paste is 10485761 bytes, over the 10485760-byte limit for an attachment, so it was not added.".into()
            ))
        );
    }

    /// A Tui scrolled up from the bottom of thirty rows, with `typed` in the composer.
    fn scrolled_up(typed: &str) -> Tui {
        let mut t = tui();
        numbered_rows(&mut t);
        screen(&mut t, 60, 20);
        t.handle(key(KeyCode::PageUp, KeyModifiers::NONE));
        assert!(t.scroll_from_bottom > 0, "the reader scrolled up");
        t.composer.set_text(typed);
        t
    }

    #[test]
    fn end_moves_to_the_line_end_first_and_then_jumps_to_the_latest_message() {
        let mut t = scrolled_up("abc");
        t.handle(key(KeyCode::Home, KeyModifiers::NONE));
        let up = t.scroll_from_bottom;
        t.handle(key(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(t.scroll_from_bottom, up, "the first End moved the cursor");
        assert!(t.composer.at_line_end());
        t.handle(key(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(
            t.scroll_from_bottom, 0,
            "End at the end of the line jumps to the latest message"
        );
        assert_eq!(t.composer.text(), "abc");
    }

    #[test]
    fn end_on_an_empty_composer_jumps_and_ctrl_end_never_scrolls() {
        let mut t = scrolled_up("");
        t.handle(key(KeyCode::End, KeyModifiers::CONTROL));
        assert!(t.scroll_from_bottom > 0, "Ctrl+End belongs to the composer");
        t.handle(key(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(t.scroll_from_bottom, 0);
    }

    #[test]
    fn cmd_arrows_and_home_move_within_the_line_without_scrolling() {
        let mut t = scrolled_up("one\ntwo three");
        let up = t.scroll_from_bottom;
        t.handle(key(KeyCode::Left, KeyModifiers::SUPER));
        assert!(!t.composer.at_line_end());
        t.handle(key(KeyCode::Right, KeyModifiers::SUPER));
        assert!(t.composer.at_line_end());
        t.handle(key(KeyCode::Home, KeyModifiers::NONE));
        t.handle(key(KeyCode::End, KeyModifiers::CONTROL));
        assert_eq!(
            t.scroll_from_bottom, up,
            "no line key scrolled the transcript"
        );
        assert_eq!(t.composer.text(), "one\ntwo three");
    }

    #[test]
    fn the_one_line_editor_takes_every_line_key() {
        let cases = [
            (KeyCode::Home, KeyModifiers::NONE, Edit::Home),
            (KeyCode::End, KeyModifiers::NONE, Edit::End),
            (KeyCode::Left, KeyModifiers::SUPER, Edit::Home),
            (KeyCode::Right, KeyModifiers::SUPER, Edit::End),
            (KeyCode::Char('a'), KeyModifiers::CONTROL, Edit::Home),
            (KeyCode::Char('e'), KeyModifiers::CONTROL, Edit::End),
            (KeyCode::Home, KeyModifiers::CONTROL, Edit::Home),
            (KeyCode::End, KeyModifiers::CONTROL, Edit::End),
            (KeyCode::Left, KeyModifiers::NONE, Edit::Left),
            (KeyCode::Char('a'), KeyModifiers::NONE, Edit::Char('a')),
        ];
        for (code, mods, want) in cases {
            assert_eq!(
                edit_key(KeyEvent::new(code, mods)),
                Some(want),
                "{code:?} {mods:?}"
            );
        }
        assert_eq!(
            edit_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            None,
            "other Ctrl keys still do nothing in the editor"
        );
    }

    /// The row of `shown` where the composer's placeholder is.
    fn placeholder_row(shown: &str) -> usize {
        shown
            .lines()
            .position(|l| l.contains("Message the agent"))
            .unwrap_or_else(|| panic!("no composer:\n{shown}"))
    }

    #[test]
    fn the_activity_row_stays_blank_while_the_transcript_animates_the_thought() {
        use scuttle_core::live::LiveBlock;
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "user", "content": [{"type": "text", "text": "plan it"}]}]),
        );
        t.core.transcript.status = Some(coder_sdk::ChatStatus::Running);
        t.core
            .transcript
            .live
            .blocks
            .push(LiveBlock::Reasoning("hmm".into()));
        t.view_revision += 1;
        let thinking = screen(&mut t, 60, 20);
        assert_eq!(
            thinking.matches("Thinking").count(),
            1,
            "only the transcript's marker says it:\n{thinking}"
        );
        t.core
            .transcript
            .live
            .blocks
            .push(LiveBlock::Text("the answer".into()));
        t.view_revision += 1;
        let writing = screen(&mut t, 60, 20);
        assert!(
            writing.contains("Writing…"),
            "finished text animates nothing, so the row shows:\n{writing}"
        );
        assert_eq!(
            placeholder_row(&thinking),
            placeholder_row(&writing),
            "the blank row kept its height, so nothing moved"
        );
    }

    #[test]
    fn the_activity_row_stays_blank_while_parallel_tool_calls_animate() {
        let mut t = tui();
        let call = |id: &str, n: u32| {
            json!({"type": "tool-call", "tool_call_id": id, "tool_name": "execute",
                "args": {"command": format!("make {n}")}})
        };
        loaded(
            &mut t,
            json!([
                {"id": 1, "role": "user", "content": [{"type": "text", "text": "build all four"}]},
                {"id": 2, "role": "assistant", "content": [
                    call("a", 1), call("b", 2), call("c", 3), call("d", 4)
                ]}
            ]),
        );
        t.core.transcript.status = Some(coder_sdk::ChatStatus::Running);
        t.view_revision += 1;
        assert_eq!(
            t.core.activity(),
            Some(scuttle_core::app::Activity::Tool(
                "execute and 3 more".into()
            ))
        );
        let shown = screen(&mut t, 80, 24);
        for n in 1..=4 {
            assert!(shown.contains(&format!("execute(make {n})")), "{shown}");
        }
        assert_eq!(t.view.spinners.len(), 4, "each running call animates");
        assert!(
            !shown.contains("Running execute"),
            "the row does not repeat the four calls:\n{shown}"
        );
    }

    #[test]
    fn nerd_font_decides_the_icons_only_while_the_file_sets_none() {
        let mut t = tui();
        assert_eq!(t.theme.icons, IconSet::Text, "neither set means text");
        assert_eq!(t.pin_icon(), "📌");
        t.set_icon_env(Some(IconSet::Nerd));
        assert_eq!(t.theme.icons, IconSet::Nerd);
        assert_eq!(t.pin_icon(), "\u{f435} ", "the unset pin follows the icons");
        t.set_icon_env(None);
        assert_eq!(t.theme.icons, IconSet::Text);
        let mut set = Tui::new(
            LocalConfig {
                icons: Some(IconSet::Text),
                ..LocalConfig::default()
            },
            None,
            Theme::terminal(true),
            Welcome {
                url: "https://x".into(),
                user: "nick".into(),
                art: vec![],
                art_accent: true,
                show: true,
                tip: true,
            },
            0,
        );
        set.set_icon_env(Some(IconSet::Nerd));
        assert_eq!(
            set.theme.icons,
            IconSet::Text,
            "the file wins over NERD_FONT"
        );
    }

    #[test]
    fn settings_switch_the_icons_live_and_a_set_pin_wins() {
        let (mut t, path) = settings_tui();
        assert!(screen(&mut t, 80, 24).contains("better with a Nerd Font!"));
        t.edit_settings_with(|p| std::fs::write(p, "icons = \"nerd\"\n"));
        assert_eq!(t.theme.icons, IconSet::Nerd);
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Info("Settings applied.".into())),
            "icons apply without a restart"
        );
        assert!(
            !screen(&mut t, 80, 24).contains("Nerd Font"),
            "the tip goes at once"
        );
        assert_eq!(t.pin_icon(), "\u{f435} ");
        t.edit_settings_with(|p| {
            std::fs::write(p, "icons = \"nerd\"\n[chats]\npin_icon = \"*\"\n")
        });
        assert_eq!(
            t.pin_icon(),
            "*",
            "a pin in the file wins over the icon set's"
        );
        t.edit_settings_with(|p| std::fs::write(p, "icons = \"text\"\n"));
        assert_eq!(t.theme.icons, IconSet::Text);
        assert_eq!(t.pin_icon(), "📌");
        assert!(screen(&mut t, 80, 24).contains("better with a Nerd Font!"));
        remove_settings(&path);
    }

    #[test]
    fn the_nerd_font_tip_lights_up_and_opens_nerdfonts_com() {
        let mut t = tui();
        let shown = screen(&mut t, 80, 24);
        let (x, y) = find(&shown, "Nerd Font!");
        mouse(&mut t, MouseEventKind::Moved, x + 1, y);
        assert!(lit(style_at(&mut t, 80, 24, x, y)), "the link lights up");
        assert!(
            lit(style_at(&mut t, 80, 24, x + 8, y)),
            "to its last letter"
        );
        assert!(
            !lit(style_at(&mut t, 80, 24, x + 9, y)),
            "the ! after it is not the link"
        );
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x + 2, y);
        assert_eq!(
            release(&mut t, x + 2, y),
            vec![Effect::OpenLink("https://www.nerdfonts.com/".into())]
        );
    }

    #[test]
    fn nerd_icons_mark_the_composer_chips_and_a_failed_one() {
        use scuttle_core::attachments::{Chip, ChipState};
        let mut t = tui();
        started(&mut t);
        t.update(Msg::Submit("/attach /tmp/notes.md".into()));
        let shown = screen(&mut t, 60, 14);
        assert!(
            shown.contains("[notes.md"),
            "text mode keeps the chip: {shown}"
        );
        t.set_icon_env(Some(IconSet::Nerd));
        let shown = screen(&mut t, 60, 14);
        assert!(shown.contains("[\u{ec34} notes.md"), "{shown}");
        t.core.chips.push(Chip {
            local: 99,
            name: "big.zip".into(),
            size: None,
            state: ChipState::Failed("too large".into()),
            pasted: None,
        });
        let shown = screen(&mut t, 60, 14);
        assert!(shown.contains("[\u{ea87} big.zip: too large]"), "{shown}");
    }

    #[test]
    fn the_nerd_font_tip_goes_for_good_once_a_chat_opens() {
        let mut t = tui();
        started(&mut t);
        assert!(screen(&mut t, 80, 24).contains("better with a Nerd Font!"));
        loaded(
            &mut t,
            json!([{"id": 1, "role": "user", "content": [{"type": "text", "text": "hi"}]}]),
        );
        screen(&mut t, 80, 24);
        t.update(Msg::Command(scuttle_core::commands::Command::New));
        let shown = screen(&mut t, 80, 24);
        assert!(shown.contains("Type a message to start"), "{shown}");
        assert!(!shown.contains("Nerd Font"), "{shown}");
    }

    #[test]
    fn the_first_draw_that_shows_the_nerd_font_tip_remembers_it() {
        let dir = std::env::temp_dir().join(format!("scuttle-tip-{}", uuid::Uuid::new_v4()));
        let path = dir.join("scuttle/state.toml");
        let mut nerd = tui();
        nerd.set_icon_env(Some(IconSet::Nerd));
        nerd.remember_nerd_font_tip_in(path.clone());
        screen(&mut nerd, 80, 24);
        assert!(!path.exists(), "a tip never drawn is not remembered");
        let mut t = tui();
        t.remember_nerd_font_tip_in(path.clone());
        assert!(!path.exists(), "nothing is written before the tip draws");
        assert!(screen(&mut t, 80, 24).contains("better with a Nerd Font!"));
        assert!(scuttle_core::state::load(&path).nerd_font_tip_shown);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_tip_scrolled_off_screen_is_not_remembered() {
        let dir = std::env::temp_dir().join(format!("scuttle-tip-{}", uuid::Uuid::new_v4()));
        let path = dir.join("scuttle/state.toml");
        let mut t = tui();
        t.remember_nerd_font_tip_in(path.clone());
        t.scroll_from_bottom = usize::MAX;
        let shown = screen(&mut t, 80, 10);
        assert!(!shown.contains("Nerd Font"), "the tip is below: {shown}");
        assert!(!path.exists(), "a tip off screen is not remembered");
        t.scroll_from_bottom = 0;
        assert!(screen(&mut t, 80, 10).contains("Nerd Font"));
        assert!(scuttle_core::state::load(&path).nerd_font_tip_shown);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mcp_on_a_blank_chat_lists_the_organization_servers() {
        let mut t = tui();
        let org = uuid::Uuid::new_v4();
        t.core.update(Msg::Started {
            org_id: org,
            open_chat: None,
        });
        t.core.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::OrgMcpLoaded(
                serde_json::from_value(json!([
                    {"id": uuid::Uuid::new_v4(), "display_name": "GitHub", "availability": "default_on", "enabled": true, "tool_allow_list": [], "tool_deny_list": []},
                    {"id": uuid::Uuid::new_v4(), "display_name": "Docs", "availability": "force_on", "enabled": true, "tool_allow_list": [], "tool_deny_list": []}
                ]))
                .unwrap(),
            )),
        });
        let effects = t.update(Msg::Command(scuttle_core::commands::Command::Mcp));
        apply_ui_effects(&mut t, effects);
        let shown = screen(&mut t, 100, 24);
        assert!(shown.contains("GitHub"), "{shown}");
        assert!(shown.contains("on (required)"), "{shown}");
        assert!(shown.contains("first message"), "{shown}");
        assert!(!shown.contains("Start a chat first"), "{shown}");
    }

    /// A Tui on a 60-message chat whose agent attached a file in message 30.
    fn tui_with_a_file() -> (Tui, uuid::Uuid) {
        let mut t = tui();
        let file = uuid::Uuid::new_v4();
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [],
            "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}, "files": [
                {"id": file, "name": "build-logs.zip", "mime_type": "application/zip",
                    "size_bytes": 6144, "created_at": "2026-10-05T12:00:00Z"}]}))
        .unwrap();
        let mut messages: Vec<serde_json::Value> = (1..=60)
            .map(|i| {
                json!({"id": i, "role": "assistant", "content": [
                {"type": "text", "text": format!("line {i}")}]})
            })
            .collect();
        messages[29] = json!({"id": 30, "role": "assistant", "content": [
            {"type": "file", "file_id": file, "media_type": "application/zip",
                "name": "build-logs.zip", "file_name": ""}]});
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(chat),
            messages: serde_json::from_value(json!(messages)).unwrap(),
        });
        t.core.home = Some("/h".into());
        t.core.save_dir = "/h/Downloads".into();
        (t, file)
    }

    #[test]
    fn clicking_an_attached_file_saves_it() {
        let (mut t, file) = tui_with_a_file();
        t.handle(key(KeyCode::End, KeyModifiers::NONE));
        let mut term = Terminal::new(TestBackend::new(60, 80)).unwrap();
        term.draw(|f| t.draw(f)).unwrap();
        let row = t
            .row_of(|target| matches!(target, HitTarget::SaveFile(id) if *id == file))
            .expect("the attached line is on screen");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), 4, row);
        let effects = t.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 4,
            row,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(
            matches!(effects.as_slice(), [Effect::SaveFile { file: f, .. }] if *f == file),
            "{effects:?}"
        );
    }

    #[test]
    fn a_click_on_the_attached_line_while_the_save_question_is_open_does_nothing() {
        let (mut t, file) = tui_with_a_file();
        t.handle(key(KeyCode::End, KeyModifiers::NONE));
        let mut term = Terminal::new(TestBackend::new(60, 80)).unwrap();
        term.draw(|f| t.draw(f)).unwrap();
        let row = t
            .row_of(|target| matches!(target, HitTarget::SaveFile(id) if *id == file))
            .expect("the attached line is on screen");
        t.update(Msg::FileConflict {
            file,
            name: "build-logs.zip".into(),
            path: "/h/Downloads/build-logs.zip".into(),
        });
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), 4, row);
        let effects = t.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 4,
            row,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(effects.is_empty(), "{effects:?}");
        assert!(t.core.save_conflict.is_some());
    }

    #[test]
    fn dragging_across_an_attached_line_copies_the_name_without_the_click_hint() {
        let (mut t, file) = tui_with_a_file();
        t.handle(key(KeyCode::End, KeyModifiers::NONE));
        let mut term = Terminal::new(TestBackend::new(60, 80)).unwrap();
        term.draw(|f| t.draw(f)).unwrap();
        let row = t
            .row_of(|target| matches!(target, HitTarget::SaveFile(id) if *id == file))
            .expect("the attached line is on screen");
        let shown = screen(&mut t, 60, 80);
        assert!(
            shown.contains("attached build-logs.zip \u{b7} click to save"),
            "{shown}"
        );
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), 3, row);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), 58, row);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), 58, row);
        assert_eq!(t.last_copied.as_deref(), Some("attached build-logs.zip"));
    }

    #[test]
    fn dragging_across_a_wrapped_attached_line_copies_the_name_without_the_hint() {
        let (mut t, file) = tui_with_a_file();
        t.handle(key(KeyCode::End, KeyModifiers::NONE));
        // Narrow enough that the hint wraps onto its own row.
        let mut term = Terminal::new(TestBackend::new(26, 80)).unwrap();
        term.draw(|f| t.draw(f)).unwrap();
        let hits: Vec<_> = t
            .view
            .hits
            .iter()
            .filter(|h| h.target == HitTarget::SaveFile(file))
            .collect();
        let rows = hits[0].lines.clone();
        assert!(rows.len() > 1, "the line wraps: {rows:?}");
        let first = t.row_of(|x| *x == HitTarget::SaveFile(file)).unwrap();
        let last = first + (rows.len() as u16 - 1);
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), 3, first);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), 24, last);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), 24, last);
        assert_eq!(t.last_copied.as_deref(), Some("attached build-logs.zip"));
    }

    #[test]
    fn slash_copy_of_a_message_with_an_attached_file_leaves_out_the_click_hint() {
        let mut t = tui();
        t.update(Msg::ChatLoaded {
            has_more: None,
            chat: Box::new(
                serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [],
                    "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}, "files": []}))
                .unwrap(),
            ),
            messages: serde_json::from_value(json!([{"id": 1, "role": "assistant", "content": [
                {"type": "text", "text": "here you go"},
                {"type": "file", "file_id": uuid::Uuid::new_v4(), "name": "r.zip"}]}]))
            .unwrap(),
        });
        let effects = t.update(Msg::Command(scuttle_core::commands::Command::Copy(None)));
        show(&mut t, effects);
        assert_eq!(t.last_copied.as_deref(), Some("here you go"));
    }

    #[test]
    fn slash_files_opens_the_overlay() {
        let (mut t, _) = tui_with_a_file();
        let effects = t.update(Msg::Submit("/files".into()));
        show(&mut t, effects);
        assert!(matches!(t.overlay, Some(Overlay::Files(_))));
        assert!(screen(&mut t, 100, 30).contains("build-logs.zip"));
    }

    #[test]
    fn a_file_that_arrives_while_files_is_open_does_not_take_the_highlight() {
        let (mut t, file) = tui_with_a_file();
        let effects = t.update(Msg::Submit("/files".into()));
        show(&mut t, effects);
        t.core.chat.as_mut().unwrap().files.push(
            serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "name": "newer.md",
                "mime_type": "text/markdown", "size_bytes": 10,
                "created_at": "2026-10-05T13:00:00Z"}))
            .unwrap(),
        );
        assert!(screen(&mut t, 100, 30).contains("newer.md"));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(effects.as_slice(), [Effect::SaveFile { file: f, .. }] if *f == file),
            "Enter saves the file that was highlighted: {effects:?}"
        );
    }

    #[test]
    fn any_user_scroll_ends_a_pending_jump() {
        type Scroll = fn(&mut Tui);
        let scrolls: [(&str, Scroll); 4] = [
            ("PageUp", |t| {
                t.handle(key(KeyCode::PageUp, KeyModifiers::NONE));
            }),
            ("PageDown", |t| {
                t.handle(key(KeyCode::PageDown, KeyModifiers::NONE));
            }),
            ("End", |t| {
                t.handle(key(KeyCode::End, KeyModifiers::NONE));
            }),
            ("the wheel", |t| mouse(t, MouseEventKind::ScrollDown, 5, 5)),
        ];
        for (name, scroll) in scrolls {
            let mut t = tui();
            let file = uuid::Uuid::new_v4();
            let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [],
                "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}, "files": [
                    {"id": file, "name": "a.txt", "mime_type": "text/plain",
                        "size_bytes": 1, "created_at": "2026-10-05T12:00:00Z"}]}))
            .unwrap();
            let messages: Vec<serde_json::Value> = (100..=160)
                .map(|i| {
                    json!({"id": i, "role": "assistant", "content": [
                    {"type": "text", "text": format!("line {i}")}]})
                })
                .collect();
            t.update(Msg::ChatLoaded {
                has_more: Some(true),
                chat: Box::new(chat),
                messages: serde_json::from_value(json!(messages)).unwrap(),
            });
            screen(&mut t, 60, 20);
            let effects = t.update(Msg::FileAction(scuttle_core::files::FileAction::Jump(file)));
            assert!(
                effects
                    .iter()
                    .any(|e| matches!(e, Effect::LoadOlder { .. })),
                "{name}: the jump loads older pages: {effects:?}"
            );
            t.jump_to = Some(100);
            scroll(&mut t);
            assert_eq!(t.jump_to, None, "{name}: the waiting scroll is dropped");
            assert!(
                t.core
                    .notices
                    .iter()
                    .any(|n| *n == Notice::Info("Stopped looking for the file's message.".into())),
                "{name}: the core jump ended"
            );
        }
    }

    #[test]
    fn g_scrolls_the_transcript_to_the_files_message() {
        let (mut t, file) = tui_with_a_file();
        screen(&mut t, 60, 20);
        let effects = t.update(Msg::FileAction(scuttle_core::files::FileAction::Jump(file)));
        assert_eq!(effects, vec![Effect::ScrollToMessage(30)]);
        show(&mut t, effects);
        let shown = screen(&mut t, 60, 20);
        let first = shown.lines().find(|l| !l.trim().is_empty()).unwrap();
        assert!(first.contains("build-logs.zip"), "{shown}");
        assert_eq!(t.top_line(), t.view.message_rows[&30]);
    }

    #[test]
    fn the_save_question_holds_the_keyboard_and_answers_only_by_letter() {
        use scuttle_core::files::{OnConflict, SaveTo};
        let (mut t, file) = tui_with_a_file();
        let path = std::path::PathBuf::from("/h/Downloads/build-logs.zip");
        let ask = |t: &mut Tui| {
            t.update(Msg::FileConflict {
                file,
                name: "build-logs.zip".into(),
                path: path.clone(),
            })
        };
        let resave = |conflict| {
            vec![Effect::SaveFile {
                file,
                name: "build-logs.zip".into(),
                to: SaveTo::File(path.clone()),
                conflict,
            }]
        };
        ask(&mut t);
        assert!(screen(&mut t, 80, 24).contains("~/Downloads/build-logs.zip already exists"));
        assert!(
            t.handle(key(KeyCode::Char('x'), KeyModifiers::NONE))
                .is_empty()
        );
        assert_eq!(t.composer.text(), "", "the question took the key");
        // Terminals report a held key as repeated presses, at any pace.
        let start = Instant::now();
        for (i, gap) in [0u64, 1, 30, 300, 1000, 5000].into_iter().enumerate() {
            for code in [KeyCode::Up, KeyCode::Down, KeyCode::Enter] {
                let at = start + Duration::from_millis(gap * (i as u64 + 1));
                let effects = t.handle_at(
                    Event::Key(KeyEvent::new(code, KeyModifiers::NONE)),
                    at,
                    local_now(),
                );
                assert!(effects.is_empty(), "{code:?} never answers: {effects:?}");
            }
        }
        assert!(t.core.save_conflict.is_some(), "only a letter answers");
        assert_eq!(
            t.handle(key(KeyCode::Char('k'), KeyModifiers::NONE)),
            resave(OnConflict::KeepBoth)
        );
        assert!(t.core.save_conflict.is_none());
        t.update(Msg::FileFailed {
            file,
            message: "x".into(),
        });
        ask(&mut t);
        assert_eq!(
            t.handle(key(KeyCode::Char('r'), KeyModifiers::NONE)),
            resave(OnConflict::Replace)
        );
        assert!(!screen(&mut t, 80, 24).contains("already exists"));
        t.update(Msg::FileFailed {
            file,
            message: "x".into(),
        });
        ask(&mut t);
        assert!(
            t.handle(key(KeyCode::Char('c'), KeyModifiers::NONE))
                .is_empty()
        );
        assert!(t.core.save_conflict.is_none(), "c cancels");
        ask(&mut t);
        assert!(t.handle(key(KeyCode::Esc, KeyModifiers::NONE)).is_empty());
        assert!(t.core.save_conflict.is_none(), "Esc cancels");
    }

    #[test]
    fn one_ctrl_c_cancels_the_save_question() {
        let (mut t, file) = tui_with_a_file();
        t.update(Msg::FileConflict {
            file,
            name: "build-logs.zip".into(),
            path: "/h/Downloads/build-logs.zip".into(),
        });
        let effects = t.handle(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(effects.is_empty(), "{effects:?}");
        assert!(t.core.save_conflict.is_none());
        assert!(
            t.handle(key(KeyCode::Char('x'), KeyModifiers::NONE))
                .is_empty()
        );
        assert_eq!(
            t.composer.text(),
            "x",
            "the keyboard is the composer's again"
        );
    }

    #[test]
    fn a_drag_under_the_save_question_selects_nothing() {
        let (mut t, file) = tui_with_a_file();
        t.handle(key(KeyCode::End, KeyModifiers::NONE));
        screen(&mut t, 60, 40);
        t.update(Msg::FileConflict {
            file,
            name: "build-logs.zip".into(),
            path: "/h/Downloads/build-logs.zip".into(),
        });
        screen(&mut t, 60, 40);
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), 2, 2);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), 50, 6);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), 50, 6);
        assert!(t.drag.is_none() && t.selection.is_none());
        assert_eq!(t.last_copied, None, "nothing under the box was copied");
        assert!(t.core.save_conflict.is_some());
    }

    #[test]
    fn the_save_question_takes_the_keys_over_an_open_save_as_line() {
        use scuttle_core::files::{FileAction, OnConflict};
        let (mut t, file) = tui_with_a_file();
        t.update(Msg::FileAction(FileAction::SaveAs(file)));
        assert!(t.core.editor.is_some());
        t.update(Msg::FileConflict {
            file,
            name: "build-logs.zip".into(),
            path: "/h/Downloads/build-logs.zip".into(),
        });
        let line = t.core.editor.as_ref().unwrap().line.text().to_owned();
        assert!(t.handle(Event::Paste("typed".into())).is_empty());
        assert!(
            t.handle(key(KeyCode::Char('x'), KeyModifiers::NONE))
                .is_empty()
        );
        assert_eq!(
            t.core.editor.as_ref().unwrap().line.text(),
            line,
            "the box holds the keyboard, so the line under it is unchanged"
        );
        assert!(matches!(
            t.handle(key(KeyCode::Char('k'), KeyModifiers::NONE))
                .as_slice(),
            [Effect::SaveFile {
                conflict: OnConflict::KeepBoth,
                ..
            }]
        ));
        assert!(t.core.editor.is_some(), "the save-as line is still open");
    }

    #[test]
    fn a_held_k_in_files_saves_exactly_once() {
        let (mut t, file) = tui_with_a_file();
        let effects = t.update(Msg::Submit("/files".into()));
        show(&mut t, effects);
        screen(&mut t, 100, 30);
        let saves = |effects: &[Effect]| {
            effects
                .iter()
                .filter(|e| matches!(e, Effect::SaveFile { file: f, .. } if *f == file))
                .count()
        };
        let asked = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(saves(&asked), 1, "{asked:?}");
        t.update(Msg::FileConflict {
            file,
            name: "build-logs.zip".into(),
            path: "/h/Downloads/build-logs.zip".into(),
        });
        let total: usize = (0..8)
            .map(|_| saves(&t.handle(key(KeyCode::Char('k'), KeyModifiers::NONE))))
            .sum();
        assert_eq!(total, 1, "the first k answers, and closes the question");
        assert!(t.core.save_conflict.is_none());
        let again = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            saves(&again),
            0,
            "the save in flight refuses another: {again:?}"
        );
    }

    #[test]
    fn the_save_dir_follows_files_save_dir_and_a_reload() {
        let mut t = tui();
        let home = std::env::temp_dir().join(format!("scuttle-home-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(home.join("Downloads")).unwrap();
        t.set_home(Some(home.clone()));
        assert_eq!(t.core.save_dir, home.join("Downloads"));
        let mut new = t.config.clone();
        new.files.save_dir = Some("~/out".into());
        t.apply_settings(new);
        assert_eq!(t.core.save_dir, home.join("out"));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn save_as_titles_its_line() {
        let (mut t, file) = tui_with_a_file();
        t.update(Msg::FileAction(scuttle_core::files::FileAction::SaveAs(
            file,
        )));
        let shown = screen(&mut t, 80, 24);
        assert!(
            shown.contains("Save as (Enter saves, Esc cancels)"),
            "{shown}"
        );
        assert!(shown.contains("~/Downloads/build-logs.zip"), "{shown}");
    }

    /// The top-left corner and size of the box whose title holds `title` on `shown`: the `┌`
    /// before the title, and the `┐` and `└` that close the box.
    fn box_around(shown: &str, title: &str) -> (usize, usize, usize, usize) {
        let lines: Vec<Vec<char>> = shown.lines().map(|l| l.chars().collect()).collect();
        let top = lines
            .iter()
            .position(|l| l.iter().collect::<String>().contains(title))
            .unwrap_or_else(|| panic!("{title:?} shows: {shown}"));
        let row: String = lines[top].iter().collect();
        let title_col = row[..row.find(title).unwrap()].chars().count();
        let left = lines[top][..title_col]
            .iter()
            .rposition(|c| *c == '┌')
            .unwrap();
        let right = left + lines[top][left..].iter().position(|c| *c == '┐').unwrap();
        let bottom = (top + 1..lines.len())
            .find(|&y| lines[y].get(left) == Some(&'└'))
            .unwrap();
        (left, top, right - left + 1, bottom - top + 1)
    }

    #[test]
    fn the_save_question_keeps_its_place_and_size_while_keys_are_pressed() {
        for (w, h) in [(100, 24), (60, 20), (34, 14)] {
            let (mut t, file) = tui_with_a_file();
            t.update(Msg::FileConflict {
                file,
                name: "build-logs.zip".into(),
                path: "/h/Downloads/build-logs.zip".into(),
            });
            let on_keep = box_around(&screen(&mut t, w, h), " already exists");
            t.handle(key(KeyCode::Down, KeyModifiers::NONE));
            let on_replace = box_around(&screen(&mut t, w, h), " already exists");
            t.handle(key(KeyCode::Down, KeyModifiers::NONE));
            let on_cancel = box_around(&screen(&mut t, w, h), " already exists");
            assert_eq!(on_keep, on_replace, "{w}x{h}");
            assert_eq!(on_keep, on_cancel, "{w}x{h}");
        }
        let (mut t, file) = tui_with_a_file();
        t.update(Msg::FileConflict {
            file,
            name: "build-logs.zip".into(),
            path: format!("/h/Downloads/{}.zip", "build-logs-".repeat(12)).into(),
        });
        for w in [100, 44] {
            let shown = screen(&mut t, w, 24);
            let (left, top, width, _) = box_around(&shown, " already exists");
            let row: Vec<char> = shown.lines().nth(top).unwrap().chars().collect();
            let inside: String = row[left + 1..left + width - 1].iter().collect();
            assert!(
                inside.contains("\u{2026} already exists"),
                "{w}: {inside:?}"
            );
            assert_eq!(row[left + width - 1], '┐', "{w}: the corner stays");
            assert!(
                matches!(row[left + width - 2], ' ' | '─'),
                "{w}: the title stops before the border: {inside:?}"
            );
        }
    }

    #[test]
    fn the_save_question_keeps_pastes_and_the_wheel_from_what_it_covers() {
        let (mut t, file) = tui_with_a_file();
        t.core.chat.as_mut().unwrap().files.push(
            serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "name": "notes.md",
                "mime_type": "text/markdown", "size_bytes": 10,
                "created_at": "2026-10-05T11:00:00Z"}))
            .unwrap(),
        );
        t.update(Msg::FileConflict {
            file,
            name: "build-logs.zip".into(),
            path: "/h/Downloads/build-logs.zip".into(),
        });
        assert!(t.handle(Event::Paste("typed".into())).is_empty());
        assert_eq!(
            t.composer.text(),
            "",
            "the paste never reaches the composer"
        );
        let effects = t.update(Msg::Submit("/files".into()));
        show(&mut t, effects);
        screen(&mut t, 80, 24);
        let before = t.overlay.as_ref().unwrap().state().selected.clone();
        mouse(&mut t, MouseEventKind::ScrollDown, 5, 5);
        assert_eq!(
            t.overlay.as_ref().unwrap().state().selected,
            before,
            "the wheel never moves the /files selection under the question"
        );
        assert!(t.core.save_conflict.is_some());
    }

    #[test]
    fn scroll_to_latest_returns_to_the_end() {
        let (mut t, _) = tui_with_a_file();
        screen(&mut t, 60, 20);
        t.handle(key(KeyCode::PageUp, KeyModifiers::NONE));
        assert!(t.scroll_from_bottom > 0, "scrolled up first");
        show(&mut t, vec![Effect::ScrollToLatest]);
        assert_eq!(t.scroll_from_bottom, 0);
    }

    fn menu_labels(t: &Tui) -> Vec<String> {
        t.composer
            .slash_matches()
            .iter()
            .map(|e| e.label.clone())
            .collect()
    }

    fn workspace(name: &str, last_used: i64) -> scuttle_core::app::WorkspaceRef {
        scuttle_core::app::WorkspaceRef {
            id: uuid::Uuid::new_v4(),
            name: name.into(),
            template: "Docker".into(),
            status: "running".into(),
            last_used: Some(last_used),
        }
    }

    #[test]
    fn workspace_names_complete_after_workspace_and_ws_and_the_send_key_attaches_one() {
        let mut t = tui();
        started(&mut t);
        t.update(Msg::WorkspacesLoaded(vec![
            workspace("dev-2", 3),
            workspace("dev-20", 2),
            workspace("build3", 1),
        ]));
        type_text(&mut t, "/ws dev-2");
        assert_eq!(menu_labels(&t), ["dev-2", "dev-20"]);
        t.handle(key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(t.composer.text(), "/ws dev-2");
        t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(t.core.workspace_name().as_deref(), Some("dev-2"));
        type_text(&mut t, "/workspace b");
        t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(t.core.workspace_name().as_deref(), Some("build3"));
        type_text(&mut t, "/workspace n");
        t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(t.core.selected_workspace, None, "none detaches");
    }

    #[test]
    fn the_first_space_after_workspace_fetches_a_list_that_failed_once() {
        let mut t = tui();
        let org = uuid::Uuid::new_v4();
        t.update(Msg::Started {
            org_id: org,
            open_chat: None,
        });
        t.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::WorkspacesFailed {
                message: "HTTP 502".into(),
            }),
        });
        let mut effects = Vec::new();
        for c in "/workspace d".chars() {
            effects.extend(t.handle(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        assert_eq!(
            effects
                .iter()
                .filter(|e| **e == Effect::FetchWorkspaces(org))
                .count(),
            1,
            "{effects:?}"
        );
        assert_eq!(menu_labels(&t), ["Loading workspaces…"], "the dim note");
        t.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::WorkspacesLoaded(vec![workspace("dev", 1)])),
        });
        assert_eq!(menu_labels(&t), ["dev"], "the list shows in the open menu");
    }

    #[test]
    fn the_workspace_menu_keeps_its_highlight_on_a_name_when_the_list_refreshes() {
        let mut t = tui();
        started(&mut t);
        t.update(Msg::WorkspacesLoaded(vec![
            workspace("alpha", 3),
            workspace("beta", 2),
            workspace("gamma", 1),
        ]));
        type_text(&mut t, "/workspace ");
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        let highlighted = |t: &Tui| {
            t.composer.slash_matches()[t.composer.slash_selected()]
                .label
                .clone()
        };
        assert_eq!(highlighted(&t), "beta");
        t.update(Msg::WorkspacesLoaded(vec![
            workspace("delta", 9),
            workspace("gamma", 8),
            workspace("alpha", 7),
            workspace("beta", 6),
        ]));
        assert_eq!(highlighted(&t), "beta", "the name, not the row");
        t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(t.core.workspace_name().as_deref(), Some("beta"));
    }

    #[test]
    fn the_workspace_menu_follows_an_organization_switch() {
        let mut t = tui();
        let org = |name: &str, is_default: bool| scuttle_core::app::OrgRef {
            id: uuid::Uuid::new_v4(),
            name: name.into(),
            display_name: name.into(),
            is_default,
            can_create_chats: true,
        };
        let (first, second) = (org("first", true), org("second", false));
        t.update(Msg::OrganizationsLoaded(vec![
            first.clone(),
            second.clone(),
        ]));
        t.update(Msg::Started {
            org_id: first.id,
            open_chat: None,
        });
        let loaded = |t: &mut Tui, org: uuid::Uuid, name: &str| {
            t.update(Msg::ForOrg {
                org,
                msg: Box::new(Msg::WorkspacesLoaded(vec![workspace(name, 1)])),
            });
        };
        loaded(&mut t, first.id, "first-dev");
        type_text(&mut t, "/workspace ");
        assert_eq!(menu_labels(&t), ["first-dev", "none"]);
        t.update(Msg::OrganizationChosen(second.id));
        assert_eq!(
            menu_labels(&t),
            ["none", "Loading workspaces…"],
            "the old organization's names go at once"
        );
        loaded(&mut t, first.id, "first-dev");
        assert_eq!(
            menu_labels(&t),
            ["none", "Loading workspaces…"],
            "a late reply for the old organization is dropped"
        );
        loaded(&mut t, second.id, "second-dev");
        assert_eq!(menu_labels(&t), ["second-dev", "none"]);
    }

    #[test]
    fn the_workspace_menu_shows_five_rows_over_the_transcript() {
        let mut t = tui();
        started(&mut t);
        t.update(Msg::WorkspacesLoaded(
            (1..=7).map(|i| workspace(&format!("app-{i}"), i)).collect(),
        ));
        t.composer.set_text("draft");
        let plain = screen(&mut t, 80, 24);
        t.composer.set_text("");
        type_text(&mut t, "/workspace ");
        let shown = screen(&mut t, 80, 24);
        for name in ["app-7", "app-6", "app-5", "app-4", "app-3"] {
            assert!(shown.contains(name), "{name} is missing:\n{shown}");
        }
        assert!(
            !shown.contains("app-2"),
            "the sixth row is hidden:\n{shown}"
        );
        assert!(shown.contains("↓ 3 more"), "{shown}");
        assert!(shown.contains("› app-7"), "{shown}");
        assert!(shown.contains("running · Docker"), "{shown}");
        let row = |s: &str, needle: &str| s.lines().rev().position(|l| l.contains(needle));
        assert_eq!(
            row(&shown, "/workspace "),
            row(&plain, "draft"),
            "the composer stays on its row:\n{shown}"
        );
    }

    #[test]
    fn an_organization_switch_forgets_a_moved_highlight_so_the_send_key_runs_bare() {
        let mut t = tui();
        let org = |name: &str, is_default: bool| scuttle_core::app::OrgRef {
            id: uuid::Uuid::new_v4(),
            name: name.into(),
            display_name: name.into(),
            is_default,
            can_create_chats: true,
        };
        let (first, second) = (org("first", true), org("second", false));
        t.update(Msg::OrganizationsLoaded(vec![
            first.clone(),
            second.clone(),
        ]));
        t.update(Msg::Started {
            org_id: first.id,
            open_chat: None,
        });
        t.update(Msg::ForOrg {
            org: first.id,
            msg: Box::new(Msg::WorkspacesLoaded(vec![
                workspace("one", 2),
                workspace("two", 1),
            ])),
        });
        type_text(&mut t, "/workspace ");
        t.handle(key(KeyCode::Down, KeyModifiers::NONE));
        t.update(Msg::OrganizationChosen(second.id));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            effects.contains(&Effect::ShowPicker(scuttle_core::app::Picker::Workspace)),
            "the bare /workspace opens the table: {effects:?}"
        );
        assert_eq!(t.core.selected_workspace, None);
    }

    /// A started TUI whose workspace list failed to load, and its organization.
    fn tui_with_failed_workspaces() -> (Tui, uuid::Uuid) {
        let mut t = tui();
        let org = uuid::Uuid::new_v4();
        t.update(Msg::Started {
            org_id: org,
            open_chat: None,
        });
        t.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::WorkspacesFailed {
                message: "HTTP 502".into(),
            }),
        });
        (t, org)
    }

    #[test]
    fn a_workspace_command_from_the_editor_fetches_a_list_that_failed() {
        let (mut t, org) = tui_with_failed_workspaces();
        let effects = t.open_editor_with(|path| (std::fs::write(path, "/workspace d\n"), Ok(())));
        assert_eq!(t.composer.text(), "/workspace d");
        assert!(
            effects.contains(&Effect::FetchWorkspaces(org)),
            "{effects:?}"
        );
    }

    #[test]
    fn a_restored_workspace_command_fetches_a_list_that_failed_on_the_next_key() {
        let (mut t, org) = tui_with_failed_workspaces();
        type_text(&mut t, "/workspace d");
        t.composer.set_text("");
        t.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::WorkspacesFailed {
                message: "HTTP 502".into(),
            }),
        });
        show(&mut t, vec![Effect::RestoreComposer("/workspace d".into())]);
        assert_eq!(t.composer.text(), "/workspace d");
        let effects = t.handle(key(KeyCode::Char('e'), KeyModifiers::NONE));
        assert!(
            effects.contains(&Effect::FetchWorkspaces(org)),
            "{effects:?}"
        );
    }

    #[test]
    fn a_recalled_workspace_command_fetches_a_list_that_failed() {
        let mut t = tui();
        let org = uuid::Uuid::new_v4();
        t.update(Msg::Started {
            org_id: org,
            open_chat: None,
        });
        t.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::WorkspacesFailed {
                message: "HTTP 502".into(),
            }),
        });
        type_text(&mut t, "/workspace d");
        t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        t.update(Msg::ForOrg {
            org,
            msg: Box::new(Msg::WorkspacesFailed {
                message: "HTTP 502".into(),
            }),
        });
        let effects = t.handle(key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(t.composer.text(), "/workspace d");
        assert!(
            effects.contains(&Effect::FetchWorkspaces(org)),
            "{effects:?}"
        );
    }

    use scuttle_core::compaction::{Change, Save, Shown};

    /// A Tui with `/model` open on `Alpha`, whose default threshold is 70%, above `Beta`,
    /// whose default is 30% and whose override is 50%. Returns the Tui, Alpha, and Beta.
    fn tui_with_thresholds() -> (Tui, uuid::Uuid, uuid::Uuid) {
        let mut t = tui();
        started(&mut t);
        let provider = uuid::Uuid::new_v4();
        let (alpha, beta) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let effects = t.update(Msg::CatalogLoaded(Box::new(
            serde_json::from_value(json!({
                "models": [
                    {"id": alpha, "display_name": "Alpha", "ai_provider_id": provider, "enabled": true, "is_default": true, "context_limit": 200000, "compression_threshold": 70, "reasoning_efforts": ["low", "high"]},
                    {"id": beta, "display_name": "Beta", "ai_provider_id": provider, "enabled": true, "context_limit": 1000000, "compression_threshold": 30, "reasoning_efforts": []}
                ],
                "providers": [{"id": provider, "display_name": "Provider", "available": true}],
                "unsupported_providers": []
            }))
            .unwrap(),
        )));
        let generation = effects
            .iter()
            .find_map(|e| match e {
                Effect::FetchThresholds { generation } => Some(*generation),
                _ => None,
            })
            .expect("the overrides load with the model list");
        t.update(Msg::ThresholdsLoaded {
            thresholds: vec![(beta, 50)],
            generation,
        });
        let effects = t.update(Msg::Command(scuttle_core::commands::Command::Model(None)));
        assert!(apply_ui_effects(&mut t, effects).is_empty());
        assert!(matches!(t.overlay, Some(Overlay::Model(_))));
        (t, alpha, beta)
    }

    fn tap(t: &mut Tui, code: KeyCode) -> Vec<Effect> {
        t.handle(key(code, KeyModifiers::NONE))
    }

    fn threshold(t: &Tui, model: uuid::Uuid) -> Shown {
        t.core
            .model_groups("")
            .into_iter()
            .flat_map(|g| g.models)
            .find(|m| m.id == model)
            .expect("a /model row")
            .compaction
    }

    #[test]
    fn the_model_table_takes_the_area_chats_takes() {
        let t = tui();
        let model = Overlay::open(Picker::Model, &t.core).expect("a table");
        let chats = Overlay::chats(String::new(), &t.core);
        for (width, height) in [(40, 6), (80, 20), (120, 40), (200, 61)] {
            let transcript = Rect::new(0, 2, width, height);
            let area = overlay_area(&model, None, transcript);
            assert_eq!(
                area,
                overlay_area(&chats, None, transcript),
                "{width}x{height}"
            );
            assert_eq!(
                area, transcript,
                "{width}x{height}: the whole transcript area"
            );
        }
    }

    #[test]
    fn left_and_right_step_the_threshold_by_five_and_clamp() {
        let (mut t, alpha, _) = tui_with_thresholds();
        for _ in 0..7 {
            assert!(
                tap(&mut t, KeyCode::Right).is_empty(),
                "nothing is sent per press"
            );
        }
        assert_eq!(
            threshold(&t, alpha),
            Shown::Known {
                percent: 100,
                default: false
            }
        );
        for _ in 0..25 {
            assert!(tap(&mut t, KeyCode::Left).is_empty());
        }
        assert_eq!(
            threshold(&t, alpha),
            Shown::Known {
                percent: 0,
                default: false
            }
        );
        assert!(
            matches!(t.overlay, Some(Overlay::Model(ref s)) if s.filter.is_empty()),
            "the arrows never reach the filter"
        );
    }

    #[test]
    fn several_fast_presses_save_once_when_the_row_is_left() {
        let (mut t, alpha, _) = tui_with_thresholds();
        let mut sent: Vec<Effect> = (0..3).flat_map(|_| tap(&mut t, KeyCode::Right)).collect();
        sent.extend(tap(&mut t, KeyCode::Down));
        sent.extend(tap(&mut t, KeyCode::Up));
        assert!(
            matches!(
                sent.as_slice(),
                [Effect::SaveThreshold(Save { model, change: Change::Set(85), .. })] if *model == alpha
            ),
            "{sent:?}"
        );
    }

    #[test]
    fn delete_restores_the_default_and_closing_sends_the_delete() {
        let (mut t, _, beta) = tui_with_thresholds();
        tap(&mut t, KeyCode::Down);
        assert!(tap(&mut t, KeyCode::Delete).is_empty());
        assert_eq!(
            threshold(&t, beta),
            Shown::Known {
                percent: 30,
                default: true
            }
        );
        let effects = tap(&mut t, KeyCode::Esc);
        let sent = apply_ui_effects(&mut t, effects);
        assert!(
            matches!(
                sent.as_slice(),
                [Effect::SaveThreshold(Save { model, change: Change::Reset, .. })] if *model == beta
            ),
            "{sent:?}"
        );
        assert!(t.overlay.is_none());
    }

    #[test]
    fn enter_still_picks_the_model_and_saves_its_edit() {
        let (mut t, _, beta) = tui_with_thresholds();
        tap(&mut t, KeyCode::Down);
        tap(&mut t, KeyCode::Right);
        let effects = tap(&mut t, KeyCode::Enter);
        let sent = apply_ui_effects(&mut t, effects);
        assert!(
            matches!(
                sent.as_slice(),
                [Effect::SaveThreshold(Save { model, change: Change::Set(55), .. })] if *model == beta
            ),
            "{sent:?}"
        );
        assert!(t.overlay.is_none());
        assert_eq!(t.core.selected_model, Some(beta));
    }

    #[test]
    fn typing_still_filters_and_the_arrows_change_the_filtered_row() {
        let (mut t, alpha, beta) = tui_with_thresholds();
        for c in "bet".chars() {
            tap(&mut t, KeyCode::Char(c));
        }
        tap(&mut t, KeyCode::Left);
        assert!(matches!(t.overlay, Some(Overlay::Model(ref s)) if s.filter == "bet"));
        assert_eq!(
            threshold(&t, beta),
            Shown::Known {
                percent: 45,
                default: false
            }
        );
        assert_eq!(
            threshold(&t, alpha),
            Shown::Known {
                percent: 70,
                default: true
            }
        );
        let effects = tap(&mut t, KeyCode::Enter);
        let sent = apply_ui_effects(&mut t, effects);
        assert_eq!(sent.len(), 1, "the one save: {sent:?}");
        assert_eq!(t.core.selected_model, Some(beta));
    }

    #[test]
    fn the_model_table_shows_the_whole_name_and_the_threshold_at_every_width() {
        let mut t = tui();
        started(&mut t);
        let provider = uuid::Uuid::new_v4();
        let effects = t.update(Msg::CatalogLoaded(Box::new(
            serde_json::from_value(json!({
                "models": [{"id": uuid::Uuid::new_v4(), "display_name": "Claude Sonnet 4.5", "ai_provider_id": provider, "enabled": true, "is_default": true, "context_limit": 200000, "compression_threshold": 70, "reasoning_efforts": ["low", "medium", "high"]}],
                "providers": [{"id": provider, "display_name": "Provider", "available": true}],
                "unsupported_providers": []
            }))
            .unwrap(),
        )));
        let generation = effects
            .iter()
            .find_map(|e| match e {
                Effect::FetchThresholds { generation } => Some(*generation),
                _ => None,
            })
            .unwrap();
        t.update(Msg::ThresholdsLoaded {
            thresholds: vec![],
            generation,
        });
        let effects = t.update(Msg::Command(scuttle_core::commands::Command::Model(None)));
        apply_ui_effects(&mut t, effects);
        for width in [40u16, 44, 50, 60, 80] {
            let shown = screen(&mut t, width, 20);
            // The footer names the model too, so only the table's highlighted row counts.
            let row = shown
                .lines()
                .find(|l| l.contains('›'))
                .unwrap_or_else(|| panic!("{width}: no highlighted row in\n{shown}"));
            assert!(row.contains("Claude Sonnet 4.5"), "{width}: {row:?}");
            assert!(row.contains("70%"), "{width}: {row:?}");
        }
        let shown = screen(&mut t, 80, 20);
        let row = shown.lines().find(|l| l.contains('›')).unwrap();
        for cell in ["200.0k tokens", "70% (default)", "low, medium, high"] {
            assert!(row.contains(cell), "{cell}: {row:?}");
        }
    }

    #[test]
    fn a_paste_that_moves_the_model_highlight_saves_the_edit_left_behind() {
        let (mut t, alpha, _) = tui_with_thresholds();
        assert!(tap(&mut t, KeyCode::Right).is_empty());
        let sent = t.handle(Event::Paste("bet".into()));
        assert!(
            matches!(
                sent.as_slice(),
                [Effect::SaveThreshold(Save { model, change: Change::Set(75), .. })] if *model == alpha
            ),
            "{sent:?}"
        );
        assert!(matches!(t.overlay, Some(Overlay::Model(ref s)) if s.filter == "bet"));
    }
}
