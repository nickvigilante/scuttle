//! The TUI: owns UI state, turns terminal events into core messages, and draws.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use scuttle_core::app::{App, CopyTarget, Effect, Msg, Notice};
use scuttle_core::commands::COMMANDS;
use scuttle_core::config::{self, LocalConfig};
use scuttle_core::density::SendShortcut;

use crate::activity::{SPINNER_INTERVAL, activity_line};
use crate::clipboard::{Clipboard, CopyOutcome};
use crate::composer::{Composer, ComposerAction};
use crate::footer::footer_line;
use crate::picker::{PickerChoice, PickerState};
use crate::theme::Theme;
use crate::transcript_view::{self, BlockId, HitTarget, View, Welcome};

/// How long a notice replaces the status footer when no key is pressed.
pub const NOTICE_TTL: Duration = Duration::from_secs(5);

/// Screens narrower or shorter than this keep every cell for content instead of a margin.
const MIN_PADDED: (u16, u16) = (20, 8);

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
const PLACEHOLDER_MODIFIER: &str =
    "Message the agent. Ctrl+Enter to send, Enter for a new line, /help for commands.";
const PLACEHOLDER_MODIFIER_ALT: &str =
    "Message the agent. Alt+Enter to send, Enter for a new line, /help for commands.";

/// Leaves the terminal, runs the editor, and resumes. Resume is attempted even when leaving
/// failed, but the editor only runs on a terminal that was fully left. Returns the editor's
/// outcome and the resume's outcome.
fn editor_round_trip(
    leave: impl FnOnce() -> std::io::Result<()>,
    run: impl FnOnce() -> std::io::Result<()>,
    resume: impl FnOnce() -> std::io::Result<()>,
) -> (std::io::Result<()>, std::io::Result<()>) {
    let edited = leave().and_then(|()| run());
    let resumed = resume();
    (edited, resumed)
}

pub struct Tui {
    pub core: App,
    pub composer: Composer,
    pub picker: Option<PickerState>,
    pub last_copied: Option<String>,
    config: LocalConfig,
    config_path: Option<PathBuf>,
    theme: Theme,
    welcome: Welcome,
    toggles: HashSet<BlockId>,
    view: View,
    area: Rect,
    scroll_from_bottom: usize,
    last_ctrl_c: Option<Instant>,
    show_help: bool,
    /// Created on the first copy: opening the native clipboard can block on a stale display.
    clipboard: Option<Clipboard>,
    /// The footer's notice: an index into `core.notices` and when it became active.
    active_notice: Option<(usize, Instant)>,
    /// How many of `core.notices` the footer has already considered.
    notices_seen: usize,
    /// Indices into `core.notices` that arrived but were never active, oldest first.
    unshown_notices: VecDeque<usize>,
    /// Set when the screen may hold foreign output, for example after the external editor.
    needs_full_redraw: bool,
    /// Whether keyboard enhancement flags are active, which decides the send and newline keys.
    keyboard_enhanced: bool,
    /// Set when the app must quit with an error, for example when the terminal cannot be restored.
    fatal: Option<String>,
    /// When the Tui was made; the spinner frame is a function of the time since.
    epoch: Instant,
    /// Set by `tick` for a timer wakeup, where only the clock changed, so `draw_at` reuses the
    /// transcript lines instead of rebuilding them. Cleared by every draw.
    reuse_view: bool,
    /// The transcript width `view` was built for.
    view_width: u16,
    /// How many times `draw_at` rebuilt the transcript lines; tests check timer frames reuse them.
    view_builds: usize,
}

impl Tui {
    pub fn new(
        config: LocalConfig,
        config_path: Option<PathBuf>,
        theme: Theme,
        welcome: Welcome,
    ) -> Tui {
        let mut composer = Composer::new(config.composer_max_lines);
        composer.set_gutter_style(theme.dim);
        Tui {
            core: App::new(config.busy_behavior, config.mouse),
            composer,
            picker: None,
            last_copied: None,
            config,
            config_path,
            theme,
            welcome,
            toggles: HashSet::new(),
            view: View::default(),
            area: Rect::default(),
            scroll_from_bottom: 0,
            last_ctrl_c: None,
            show_help: false,
            clipboard: None,
            active_notice: None,
            notices_seen: 0,
            unshown_notices: VecDeque::new(),
            needs_full_redraw: false,
            keyboard_enhanced: true,
            fatal: None,
            epoch: Instant::now(),
            reuse_view: false,
            view_width: 0,
            view_builds: 0,
        }
    }

    /// Records whether the terminal reports modified Enter keys distinctly, which decides the
    /// composer's send and newline keys and its hint.
    pub fn set_keyboard_enhanced(&mut self, enhanced: bool) {
        self.keyboard_enhanced = enhanced;
        self.composer.set_enhanced(enhanced);
        self.refresh_placeholder();
    }

    /// Picks the composer hint from the send preference and the keyboard enhancement state.
    fn refresh_placeholder(&mut self) {
        let text = match (self.core.prefs.send_shortcut, self.keyboard_enhanced) {
            (SendShortcut::Enter, true) => PLACEHOLDER_SHIFT,
            (SendShortcut::Enter, false) => PLACEHOLDER_ALT,
            (SendShortcut::ModifierEnter, true) => PLACEHOLDER_MODIFIER,
            (SendShortcut::ModifierEnter, false) => PLACEHOLDER_MODIFIER_ALT,
        };
        if self.composer.widget().placeholder_text() != text {
            self.composer.set_placeholder(text);
        }
    }

    /// The reason the app must quit with an error, if any. Resets it.
    pub fn take_fatal(&mut self) -> Option<String> {
        self.fatal.take()
    }

    /// Feeds a message to the core and keeps UI state consistent with the result.
    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        let effects = self.core.update(msg);
        self.prune_live_toggles();
        self.refresh_placeholder();
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
    /// no notice is active, the oldest notice that was never shown takes its turn.
    pub fn sync_notice(&mut self, now: Instant) {
        let count = self.core.notices.len();
        if count > self.notices_seen {
            self.unshown_notices.extend(self.notices_seen..count);
            self.active_notice = self.unshown_notices.pop_back().map(|i| (i, now));
        }
        self.notices_seen = count;
        if self
            .active_notice
            .is_some_and(|(_, since)| now.duration_since(since) >= NOTICE_TTL)
        {
            self.active_notice = None;
        }
        if self.active_notice.is_none() {
            self.active_notice = self.unshown_notices.pop_front().map(|i| (i, now));
        }
    }

    /// The notice the footer shows instead of the status line, if any.
    pub fn active_notice(&self) -> Option<&Notice> {
        self.active_notice
            .and_then(|(i, _)| self.core.notices.get(i))
    }

    /// When the active notice expires, so the loop can redraw then.
    pub fn notice_deadline(&self) -> Option<Instant> {
        self.active_notice.map(|(_, since)| since + NOTICE_TTL)
    }

    /// Whether the next draw must repaint every cell. Resets the request.
    pub fn take_full_redraw(&mut self) -> bool {
        std::mem::take(&mut self.needs_full_redraw)
    }

    /// When the spinner needs its next frame, or `None` while the agent is idle.
    pub fn animation_deadline(&self, now: Instant) -> Option<Instant> {
        self.core.activity().map(|_| now + SPINNER_INTERVAL)
    }

    /// Marks the next draw as a timer wakeup: nothing but the clock changed since the last one.
    pub fn tick(&mut self) {
        self.reuse_view = true;
    }

    fn copy(&mut self, text: String) {
        self.last_copied = Some(text.clone());
        if cfg!(test) {
            return;
        }
        let outcome = self
            .clipboard
            .get_or_insert_with(Clipboard::new)
            .copy(&text);
        self.report_copy(outcome);
    }

    fn report_copy(&mut self, outcome: CopyOutcome) {
        let notice = match outcome {
            CopyOutcome::Copied => Notice::Info("Copied".into()),
            CopyOutcome::CopiedWithWarning(w) => Notice::Info(format!("Copied. {w}")),
            CopyOutcome::Failed(why) => Notice::Error(format!("Copy failed: {why}")),
        };
        self.notice(notice);
    }

    /// Handles effects the UI owns. Returns false for effects the runtime should run.
    pub fn apply_ui_effect(&mut self, effect: &Effect) -> bool {
        match effect {
            Effect::ShowPicker(kind) => self.picker = Some(PickerState::open(*kind, &self.core)),
            Effect::ShowHelp => self.show_help = true,
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
                if let Some(path) = self.config_path.as_ref()
                    && let Err(e) = config::set_mouse(path, *enabled)
                {
                    self.notice(Notice::Error(e.to_string()));
                }
                let note = if *enabled {
                    "Mouse capture on. Hold Shift (Option in iTerm2 or Terminal.app) to select text."
                } else {
                    "Mouse capture off."
                };
                self.notice(Notice::Info(note.into()));
            }
            Effect::RestoreComposer(text) => {
                // Keep anything typed since the failed request, after the restored text.
                let current = self.composer.text();
                if current.trim().is_empty() {
                    self.composer.set_text(text);
                } else {
                    self.composer.set_text(&format!("{text}\n\n{current}"));
                }
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

    fn top_line(&self) -> usize {
        self.max_scroll().saturating_sub(self.scroll_from_bottom)
    }

    fn scroll_up(&mut self, lines: usize) {
        self.scroll_from_bottom = (self.scroll_from_bottom + lines).min(self.max_scroll());
    }

    fn scroll_down(&mut self, lines: usize) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(lines);
    }

    fn click(&mut self, column: u16, row: u16) {
        let inside = row >= self.area.y
            && row < self.area.y + self.area.height
            && column >= self.area.x
            && column < self.area.x + self.area.width;
        if !inside {
            return;
        }
        let line = self.top_line() + (row - self.area.y) as usize;
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
            }
            None => {}
        }
    }

    pub fn handle(&mut self, event: Event) -> Vec<Effect> {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.key(key),
            Event::Paste(text) => {
                self.composer.paste(&text);
                vec![]
            }
            Event::Mouse(m) => {
                match m.kind {
                    MouseEventKind::Down(MouseButton::Left) if !self.overlay_showing() => {
                        self.click(m.column, m.row)
                    }
                    MouseEventKind::ScrollUp => self.scroll_up(3),
                    MouseEventKind::ScrollDown => self.scroll_down(3),
                    _ => {}
                }
                vec![]
            }
            _ => vec![],
        }
    }

    /// Whether the help box, a picker, or the slash menu covers the transcript.
    fn overlay_showing(&self) -> bool {
        self.show_help || self.picker.is_some() || !self.composer.slash_matches().is_empty()
    }

    fn key(&mut self, key: KeyEvent) -> Vec<Effect> {
        self.active_notice = None;
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
            self.notice(Notice::Info("Press Ctrl+C again to quit.".into()));
            return vec![];
        }
        if self.show_help {
            self.show_help = false;
            return vec![];
        }
        if let Some(picker) = self.picker.as_mut() {
            let choice = picker.handle_key(key);
            return match choice {
                Some(PickerChoice::Cancel) => {
                    self.picker = None;
                    vec![]
                }
                Some(PickerChoice::Model(id)) => {
                    self.picker = None;
                    self.update(Msg::ModelChosen(id))
                }
                Some(PickerChoice::Workspace(ws)) => {
                    self.picker = None;
                    self.update(Msg::WorkspaceChosen(ws))
                }
                None => vec![],
            };
        }
        let page = self.area.height.max(1) as usize;
        match key.code {
            KeyCode::PageUp => {
                self.scroll_up(page);
                return vec![];
            }
            KeyCode::PageDown => {
                self.scroll_down(page);
                return vec![];
            }
            KeyCode::End => {
                self.scroll_from_bottom = 0;
                return vec![];
            }
            _ => {}
        }
        match self.composer.handle_key(key, self.core.prefs.send_shortcut) {
            ComposerAction::Submit(text) => {
                self.scroll_from_bottom = 0;
                self.update(Msg::Submit(text))
            }
            ComposerAction::Interrupt => self.update(Msg::Interrupt),
            ComposerAction::OpenEditor => self.open_editor(),
            ComposerAction::None => vec![],
        }
    }

    fn open_editor(&mut self) -> Vec<Effect> {
        let editor = std::env::var("EDITOR")
            .or_else(|_| std::env::var("VISUAL"))
            .ok()
            .filter(|e| !e.trim().is_empty())
            .unwrap_or_else(|| "vi".into());
        let mouse = self.core.mouse;
        let mut resumed = Ok(());
        let result = self.edit_with(|path| {
            let (edited, resume) = editor_round_trip(
                crate::terminal::leave,
                || {
                    // `$1` keeps the path out of the shell parse while still allowing
                    // `EDITOR="code -w"`.
                    let status = std::process::Command::new("sh")
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
            resumed = resume;
            edited
        });
        self.set_keyboard_enhanced(crate::terminal::keyboard_enhanced());
        if let Err(e) = result {
            self.notice(Notice::Error(format!("Editor failed: {e}")));
        }
        self.finish_editor(resumed)
    }

    /// Quits with an error when the terminal could not be restored after the editor, since
    /// drawing on a terminal in an unknown mode would garble the screen.
    fn finish_editor(&mut self, resumed: std::io::Result<()>) -> Vec<Effect> {
        match resumed {
            Ok(()) => vec![],
            Err(e) => {
                self.fatal = Some(format!(
                    "could not restore the terminal after the editor: {e}"
                ));
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
        let outer = padded(f.area());
        let activity = self.core.activity();
        let activity_height = u16::from(activity.is_some());
        let composer_height = self
            .composer
            .height(outer.width)
            .min(outer.height.saturating_sub(2 + activity_height).max(3));
        let [transcript, activity_row, composer, footer] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(activity_height),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ])
        .areas(outer);
        self.area = transcript;
        let reuse = std::mem::take(&mut self.reuse_view) && self.view_width == transcript.width;
        if !reuse {
            self.view = transcript_view::build(
                &self.core,
                &self.config.density,
                &self.toggles,
                &self.welcome,
                &self.theme,
                transcript.width,
            );
            self.view_width = transcript.width;
            self.view_builds += 1;
        }
        self.scroll_from_bottom = self.scroll_from_bottom.min(self.max_scroll());
        let top = self.top_line();
        let visible: Vec<Line> = self
            .view
            .lines
            .iter()
            .skip(top)
            .take(transcript.height as usize)
            .cloned()
            .collect();
        f.render_widget(Paragraph::new(visible), transcript);
        if let Some(activity) = activity.as_ref() {
            let elapsed = now.saturating_duration_since(self.epoch);
            f.render_widget(
                Paragraph::new(activity_line(activity, elapsed, &self.theme)),
                activity_row,
            );
        }
        let frame = Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(self.theme.dim);
        let inner = frame.inner(composer);
        f.render_widget(frame, composer);
        f.render_widget(self.composer.widget(), inner);
        f.render_widget(
            Paragraph::new(footer_line(
                &self.core,
                self.active_notice(),
                &self.theme,
                footer.width,
            )),
            footer,
        );
        let matches = self.composer.slash_matches();
        if !matches.is_empty() {
            let h = (matches.len() as u16 + 2).min(transcript.height);
            let area = Rect {
                y: transcript.y + transcript.height - h,
                height: h,
                ..transcript
            };
            let lines: Vec<Line> = matches
                .iter()
                .map(|c| {
                    Line::from(vec![
                        Span::styled(c.usage, self.theme.accent),
                        Span::raw("  "),
                        Span::styled(c.description, self.theme.dim),
                    ])
                })
                .collect();
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(lines).block(Block::default().borders(Borders::ALL)),
                area,
            );
        }
        if let Some(picker) = self.picker.as_ref() {
            let h = 10.min(transcript.height);
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
        if self.show_help {
            let mut lines: Vec<Line> = COMMANDS
                .iter()
                .map(|c| {
                    Line::from(vec![
                        Span::styled(c.usage, self.theme.accent),
                        Span::raw("  "),
                        Span::raw(c.description),
                    ])
                })
                .collect();
            lines.push(Line::default());
            lines.push(Line::from(
                "Esc interrupts · Ctrl+G opens $EDITOR · Ctrl+C twice quits · PageUp/PageDown/End scroll · click tool calls to expand",
            ));
            let h = (lines.len() as u16 + 2).min(transcript.height);
            let area = Rect {
                y: transcript.y,
                height: h,
                ..transcript
            };
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Help ")),
                area,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
                show: true,
            },
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
        let mut t = tui();
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap();
        t.core.update(Msg::ChatLoaded {
            chat: Box::new(chat),
            messages: serde_json::from_value(json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": "```\necho hi\n```"}]}])).unwrap(),
        });
        let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
        term.draw(|f| t.draw(f)).unwrap();
        let row = t
            .row_of(|target| matches!(target, HitTarget::CopyCode(_)))
            .expect("code block on screen");
        t.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(t.last_copied.as_deref(), Some("echo hi\n"));
    }

    #[test]
    fn click_hit_testing_accounts_for_the_margin_offset() {
        let mut t = tui();
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap();
        t.core.update(Msg::ChatLoaded {
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
        t.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(t.last_copied, None, "the margin row is not a hit target");
        // The code block's actual screen row, shifted down by the margin, still resolves.
        t.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: code_row,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(t.last_copied.as_deref(), Some("echo hi\n"));
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
        assert!(t.picker.is_some());
        t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(t.picker.is_none());
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
            "the composer border starts at column 0: {rows:?}"
        );
        let padded = screen(&mut t, 40, 10);
        assert!(padded.lines().all(|r| r.starts_with(' ')), "{padded}");
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
    fn copy_warnings_are_shown_as_info() {
        let mut t = tui();
        t.report_copy(CopyOutcome::CopiedWithWarning("check tmux".into()));
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Info("Copied. check tmux".into()))
        );
        t.report_copy(CopyOutcome::Failed("no display".into()));
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Error("Copy failed: no display".into()))
        );
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
        assert_eq!(
            t.composer.widget().placeholder_text(),
            "Message the agent. Alt+Enter to send, Enter for a new line, /help for commands."
        );
        t.set_keyboard_enhanced(true);
        assert!(
            t.composer
                .widget()
                .placeholder_text()
                .contains("Ctrl+Enter to send")
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
        assert!(
            t.composer
                .widget()
                .placeholder_text()
                .contains("Alt+Enter to send")
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
    fn ctrl_c_closes_a_picker_and_a_second_press_quits() {
        let mut t = tui();
        t.core.update(Msg::ModelsLoaded(vec![]));
        t.apply_ui_effect(&Effect::ShowPicker(scuttle_core::app::Picker::Model));
        assert!(t.picker.is_some());
        assert!(
            t.handle(key(KeyCode::Char('c'), KeyModifiers::CONTROL))
                .is_empty()
        );
        assert!(t.picker.is_none());
        assert_eq!(
            t.handle(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            vec![Effect::Quit]
        );
    }

    fn tui_with_a_code_block() -> (Tui, u16) {
        let mut t = tui();
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap();
        t.core.update(Msg::ChatLoaded {
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
        t.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row,
            modifiers: KeyModifiers::NONE,
        }));
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
        t.picker = None;
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
    fn the_editor_round_trip_resumes_even_when_leaving_fails() {
        let mut ran = false;
        let mut resumed = false;
        let (edited, resume) = editor_round_trip(
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
        assert!(edited.is_err());
        assert!(resume.is_ok());
        assert!(!ran, "the editor must not run on a half-restored terminal");
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

    #[test]
    fn the_spinner_keeps_going_while_the_agent_streams() {
        let mut t = tui();
        loaded(&mut t, json!([]));
        t.update(stream(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        assert!(screen(&mut t, 60, 16).contains("Working…"));
        t.update(live_part(1, json!({"type": "reasoning", "text": "hmm"})));
        assert!(screen(&mut t, 60, 16).contains("Thinking…"));
        t.update(live_part(2, json!({"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args_delta": "{"})));
        assert!(screen(&mut t, 60, 16).contains("Running execute…"));
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
}
