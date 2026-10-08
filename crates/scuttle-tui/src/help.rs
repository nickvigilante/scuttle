//! The `/help` overlay: every slash command with its usage, then every key.

use ratatui::text::{Line, Span};
use scuttle_core::commands::COMMANDS;
use unicode_width::UnicodeWidthStr;

use crate::icons::IconSet;
use crate::theme::Theme;
use crate::wrap::{hang, wrap_line};

/// One key binding as `/help` shows it.
pub struct KeyInfo {
    pub keys: &'static str,
    pub action: &'static str,
}

/// Every key the TUI handles, in the order `/help` lists them.
pub const KEYS: &[KeyInfo] = &[
    KeyInfo {
        keys: "Enter",
        action: "Send, or add a line, per your Coder send-key preference",
    },
    KeyInfo {
        keys: "Shift+Enter, Alt+Enter, Ctrl+J",
        action: "Add a line (Alt+Enter sends when Ctrl+Enter is the send key and the terminal cannot tell them apart)",
    },
    KeyInfo {
        keys: "Esc",
        action: "Interrupt the agent, or close the slash menu",
    },
    KeyInfo {
        keys: "Esc in an idle subagent",
        action: "Return to the parent chat, when nothing is typed",
    },
    KeyInfo {
        keys: "Ctrl+R",
        action: "Find and open a chat or subagent (/chats)",
    },
    KeyInfo {
        keys: "Ctrl+G",
        action: "Edit the message in $EDITOR",
    },
    KeyInfo {
        keys: "Ctrl+O",
        action: "Copy everything typed in the composer, every line of it",
    },
    KeyInfo {
        keys: "In Zellij",
        action: "Its default keymap takes Ctrl+O and Ctrl+G before scuttle sees them",
    },
    KeyInfo {
        keys: "Up, Down",
        action: "Recall sent messages",
    },
    KeyInfo {
        keys: "Home, End",
        action: "Move to the start or end of the line you are typing, here and in the rename and answer boxes; Cmd+Left and Cmd+Right, and Ctrl+A and Ctrl+E, do the same",
    },
    KeyInfo {
        keys: "Ctrl+Home, Ctrl+End",
        action: "Move to the start or end of the whole message",
    },
    KeyInfo {
        keys: "Up, Down in the slash menu",
        action: "Move through the commands, or the workspace names after /workspace; Tab completes the highlighted one",
    },
    KeyInfo {
        keys: "Send key in the slash menu",
        action: "Run the highlighted command or skill, or complete one that needs an argument; after a space, send the text",
    },
    KeyInfo {
        keys: "Send key after /workspace",
        action: "Attach the highlighted workspace once part of its name is typed or Up or Down moved the highlight; with nothing after the space, run /workspace",
    },
    KeyInfo {
        keys: "Tab",
        action: "Complete the slash command or skill being typed",
    },
    KeyInfo {
        keys: "Tab after @",
        action: "Complete a file path to attach",
    },
    KeyInfo {
        keys: "Tab after /workspace",
        action: "Complete a workspace name, or none, from the organization's workspaces; /ws works the same",
    },
    KeyInfo {
        keys: "Tab on a pasted text token",
        action: "Expand the large paste it stands for, which is otherwise sent as a text file; pasting the same text again right away does the same, and Backspace after it removes it",
    },
    KeyInfo {
        keys: "Tab on an empty composer",
        action: "Show the questions Esc hid",
    },
    KeyInfo {
        keys: "With nothing typed",
        action: "The question menu takes its keys first, then Ctrl+Enter implements a ready plan, then the send key sends the first queued message now",
    },
    KeyInfo {
        keys: "Up, Down, Enter, Left, Esc",
        action: "Answer a plan-mode question while the composer is empty; Left goes back a question, Esc hides them",
    },
    KeyInfo {
        keys: "Ctrl+Enter on an empty composer",
        action: "Implement a proposed plan",
    },
    KeyInfo {
        keys: "Send key on an empty composer",
        action: "Send the first queued message now, interrupting a running turn",
    },
    KeyInfo {
        keys: "Backspace on an empty composer",
        action: "Remove the last attachment",
    },
    KeyInfo {
        keys: "Left, Right",
        action: "Move the /effort slider; Enter saves, Esc cancels",
    },
    KeyInfo {
        keys: "PageUp, PageDown",
        action: "Scroll the transcript; PageUp at the top loads older messages",
    },
    KeyInfo {
        keys: "Wheel",
        action: "Scroll the transcript while mouse capture is on (/mouse); at the top it loads older messages; over /chats and the other tables, it moves like Up and Down",
    },
    KeyInfo {
        keys: "End at a line's end",
        action: "Jump to the latest message; in a draft, press End twice, since the first goes to the line's end; with nothing typed, End alone jumps",
    },
    KeyInfo {
        keys: "Click",
        action: "Open a link, expand a tool call or thinking, copy a code block, or save an attached file",
    },
    KeyInfo {
        keys: "Pointer over a link",
        action: "Underlines and brightens it while mouse capture is on",
    },
    KeyInfo {
        keys: "Drag",
        action: "Select text and copy it on release",
    },
    KeyInfo {
        keys: "In /chats",
        action: "Type to filter, Tab switches All, Active, Unread, Archived, Right and Left show and hide subagents",
    },
    KeyInfo {
        keys: "Ctrl keys in /chats",
        action: "Ctrl+A archives, and asks whether to delete the workspace too, or unarchives at once; Ctrl+E renames, Ctrl+P pins, Ctrl+U marks read or unread",
    },
    KeyInfo {
        keys: "In /subagents",
        action: "Up and Down preview, Enter opens, PageUp and PageDown scroll the preview",
    },
    KeyInfo {
        keys: "In /queue",
        action: "Enter sends now, interrupting a running turn; Delete or Backspace removes",
    },
    KeyInfo {
        keys: "In /files",
        action: "Enter saves to files.save_dir; s saves as; v views text in scuttle's pager; g goes to its message; a taken name asks: k keeps both, r replaces, c cancels",
    },
    KeyInfo {
        keys: "In /mcp",
        action: "Enter or Space turns an organization server on or off for the next message",
    },
    KeyInfo {
        keys: "In /git",
        action: "Enter opens the pull request or shows the diff in your git pager",
    },
    KeyInfo {
        keys: "In /workspace",
        action: "Enter copies the SSH command, opens it in the web UI, detaches, or switches",
    },
    KeyInfo {
        keys: "In /statusline",
        action: "Space or Enter shows or hides a field; [ and ] move it, as do Alt+Up and Alt+Down or Shift+Up and Shift+Down; Left and Right warn on context, spend, and quota",
    },
    KeyInfo {
        keys: "In /model, /workspace",
        action: "Type to filter /model and /workspace; Enter chooses, and Enter in the /workspace table attaches the workspace",
    },
    KeyInfo {
        keys: "Left, Right in /model",
        action: "Step the model's compaction threshold by 5%, from 0% (compacts after every turn) to 100% (never compacts); saves when you move to another model or close /model",
    },
    KeyInfo {
        keys: "Delete in /model",
        action: "Restore the model's default compaction threshold, which saves the same way",
    },
    KeyInfo {
        keys: "Send on an unavailable model",
        action: "Hold the message and open /model: Enter sends it with the model you pick, Esc puts it back in the composer",
    },
    KeyInfo {
        keys: "Held message",
        action: "If an overlay was open or you were typing, the picker waits: run /model to send it",
    },
    KeyInfo {
        keys: "Esc or Ctrl+C",
        action: "Close the open overlay, such as /chats, /info, or /help",
    },
    KeyInfo {
        keys: "Ctrl+C twice",
        action: "Quit",
    },
];

/// The keys that read differently when the terminal cannot tell Ctrl+Enter from Enter, by
/// their `KEYS` entry: in their place `/help` offers `/implement`.
const CTRL_ENTER_KEYS: &str = "Ctrl+Enter on an empty composer";
const NOTHING_TYPED_KEYS: &str = "With nothing typed";
/// The `KEYS` entry for "Send now", which also names the key the queued hint names.
const SEND_NOW_KEYS: &str = "Send key on an empty composer";
/// The `KEYS` entry for the line keys, which names Cmd+Left and Cmd+Right only where the
/// terminal reports Super, with keyboard enhancement.
const LINE_KEYS: &str = "Home, End";

/// `KEYS` as this terminal handles them. Without keyboard enhancement, Ctrl+Enter arrives as
/// Enter, so `/implement` replaces it, and Cmd+Left and Cmd+Right never arrive, so the line
/// keys leave them out. `send_now` is the key the queued hint above the
/// composer names, and the "Send now" entry names the same one.
fn keys_for(enhanced: bool, send_now: &str) -> Vec<(&'static str, String)> {
    KEYS.iter()
        .map(|k| match k.keys {
            CTRL_ENTER_KEYS if !enhanced => (
                "/implement",
                "Implement a proposed plan; this terminal reports Ctrl+Enter as Enter".to_owned(),
            ),
            NOTHING_TYPED_KEYS if !enhanced => (
                k.keys,
                "The question menu takes its keys first, then the send key sends the first queued message now".to_owned(),
            ),
            LINE_KEYS if !enhanced => (
                k.keys,
                "Move to the start or end of the line you are typing, here and in the rename and answer boxes; Ctrl+A and Ctrl+E do the same".to_owned(),
            ),
            SEND_NOW_KEYS => (
                k.keys,
                format!(
                    "{}; in this terminal that is {send_now}, which the row above the composer names while messages wait",
                    k.action
                ),
            ),
            _ => (k.keys, k.action.to_owned()),
        })
        .collect()
}

/// How skills appear in the slash menu, which `/help` notes below the commands.
pub const SKILLS_NOTE: &str = "Your personal and workspace skills follow the commands in the slash menu; a personal skill named like a command shows as /<username>:<name>.";

/// How `/chats` searches, which `/help` explains last. The operators are the ones
/// `scuttle_core::chat_list::SEARCH_KEYS` passes through.
pub const CHATS_SEARCH: &[&str] = &[
    "Typing in /chats filters the loaded chats by title.",
    "The last row, Search all chats, asks the server, which also searches messages and pull request titles.",
    "That search takes these operators as typed: status:running, archived:true, has_unread:true, pr_status:open, pr:123, pr_title:<text>, title:<text>, repo:<text>, source:created_by_me, source:shared_with_me, and diff_url:<url>.",
    "Other words are searched as text, except next to title:, pr_title:, or pr:, which the server does not combine with a text search.",
    "Quote a value that has spaces, as in title:\"fix ci\".",
];

/// What the columns before a `/chats` title show, which `/help` explains after the keys and
/// before the search, so the search notes still end the help. The order is
/// `overlay::status_cell`'s.
pub const CHATS_MARKERS: &[&str] = &[
    "Each row in /chats starts, after the › that marks the selected row, with two fixed columns, so every title lines up: the pin, from chats.pin_icon, then the chat's status.",
    "The status shows the most important of these: a spinner while the agent works, ! after an error, ? while it waits on you, then 🔵 for unread messages.",
    "A working chat that is also unread keeps its spinner, and the 🔵 shows once it stops.",
    "+N after a title counts its subagents, followed by the busiest one's marker, and a subagent is indented under its parent.",
    "From 120 columns, a column after the age shows the chat's pull request and its state, such as coder/coder#123 merged, for open, draft, merged, or closed.",
    "The pull request reads as its forge writes it: owner/repo#123 on GitHub, Gitea, Forgejo, and Bitbucket, group/project!123 on GitLab, and project/repo!123 on Azure DevOps, or #123 when the URL names no forge.",
    "When the column is tight, the owner is dropped first, then the repository is cut short.",
    "When a chat other than the open one finishes, fails, or needs an answer, a toast in the top-right corner names it for five seconds, unless toast = false in config.toml.",
    "While scuttle is not the focused window, any chat ending that way, the open one included, also raises a desktop notification or rings the bell, as notifications in config.toml says.",
];

/// `CHATS_MARKERS` for Nerd Font icons, naming the glyphs `/chats` draws in their place.
pub const CHATS_MARKERS_NERD: &[&str] = &[
    "Each row in /chats starts, after the › that marks the selected row, with two fixed columns, so every title lines up: the pin, from chats.pin_icon or else the Octicons pin \u{f435} by default, then the chat's status.",
    "The status shows the most important of these: a spinner while the agent works, \u{f421} after an error, \u{f420} while it waits on you, then \u{f111} for unread messages.",
    "A working chat that is also unread keeps its spinner, and the \u{f111} shows once it stops.",
    "\u{f411} after the title marks an archived chat.",
    "+N after a title counts its subagents, followed by the busiest one's marker, and a subagent is indented under its parent.",
    "From 120 columns, a column after the age shows the chat's pull request, such as \u{f09b} coder/coder#123, then its state: \u{f407} open, \u{f4dd} draft, \u{f419} merged, or \u{f4dc} closed.",
    "The glyph before it names the forge: \u{f09b} GitHub, \u{f296} GitLab, \u{f339} Gitea or Forgejo, \u{f171} Bitbucket, or \u{ebe8} Azure DevOps.",
    "The pull request reads as its forge writes it: owner/repo#123 on GitHub, Gitea, Forgejo, and Bitbucket, group/project!123 on GitLab, and project/repo!123 on Azure DevOps, or #123 when the URL names no forge.",
    "When the column is tight, the owner is dropped first, then the repository is cut short.",
    "When a chat other than the open one finishes, fails, or needs an answer, a toast in the top-right corner names it for five seconds, unless toast = false in config.toml.",
    "While scuttle is not the focused window, any chat ending that way, the open one included, also raises a desktop notification or rings the bell, as notifications in config.toml says.",
];

/// Where to get a Nerd Font and how to turn icons on, which `/help` shows in text mode
/// between the `/chats` markers and the search, since the welcome screen's tip is not always
/// on screen.
pub const ICONS_HELP: &[&str] = &[
    "scuttle works better with a Nerd Font! https://www.nerdfonts.com/",
    "Install one, set it as your terminal font, then set icons = \"nerd\" in config.toml or NERD_FONT=1 in the environment. A Mono variant keeps every icon inside its cell.",
];

/// The help text for a terminal with keyboard enhancement and Enter as the send key.
#[cfg(test)]
pub fn help_lines(theme: &Theme, width: u16) -> Vec<Line<'static>> {
    help_lines_for(theme, width, true, "↵")
}

/// The help text, wrapped to `width` so a narrow screen scrolls instead of cutting lines off.
/// Each command's description and each key's action wraps under its own column. `enhanced`
/// says whether the terminal tells Ctrl+Enter from Enter, and `send_now` is the key that
/// sends a queued message now, from `composer::send_now_label`.
pub fn help_lines_for(
    theme: &Theme,
    width: u16,
    enhanced: bool,
    send_now: &str,
) -> Vec<Line<'static>> {
    let usage_width = COMMANDS
        .iter()
        .map(|c| c.display_usage().width())
        .max()
        .unwrap_or(0);
    let keys = keys_for(enhanced, send_now);
    let keys_width = keys.iter().map(|(k, _)| k.width()).max().unwrap_or(0);
    let entry = |name: String, column: usize, text: String| {
        hang(
            vec![
                Span::styled(format!("{name:<column$}"), theme.accent),
                Span::raw("  "),
            ],
            &Line::from(text),
            width,
        )
    };
    let mut lines = vec![Line::from(Span::styled("Commands", theme.accent))];
    for c in COMMANDS {
        let description = if enhanced {
            c.description.to_owned()
        } else {
            c.description.replace(" (Ctrl+Enter)", "")
        };
        lines.extend(entry(c.display_usage(), usage_width, description));
    }
    lines.extend(wrap_line(
        &Line::from(Span::styled(SKILLS_NOTE, theme.dim)),
        width,
    ));
    lines.push(Line::default());
    lines.push(Line::from(Span::styled("Keys", theme.accent)));
    for (k, action) in keys {
        lines.extend(entry(k.to_owned(), keys_width, action));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled("Markers in /chats", theme.accent)));
    let markers = match theme.icons {
        IconSet::Nerd => CHATS_MARKERS_NERD,
        IconSet::Text => CHATS_MARKERS,
    };
    for sentence in markers {
        lines.extend(wrap_line(&Line::from(*sentence), width));
    }
    if theme.icons == IconSet::Text {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled("Icons", theme.accent)));
        for sentence in ICONS_HELP {
            lines.extend(wrap_line(&Line::from(*sentence), width));
        }
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled("Searching /chats", theme.accent)));
    for sentence in CHATS_SEARCH {
        lines.extend(wrap_line(&Line::from(*sentence), width));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn help_names_the_statusline_keys() {
        let lines = text(&help_lines(&Theme::terminal(true), 200));
        let line = lines
            .iter()
            .find(|l| l.starts_with("In /statusline"))
            .unwrap_or_else(|| panic!("no /statusline line in {lines:?}"));
        for needle in [
            "Space or Enter shows or hides",
            "Alt+Up and Alt+Down",
            "Shift+Up and Shift+Down",
            "Left and Right",
            "[ and ]",
            "on context, spend, and quota",
        ] {
            assert!(line.contains(needle), "{needle} is missing from {line:?}");
        }
    }

    #[test]
    fn help_explains_the_chats_search_and_every_operator() {
        let shown = text(&help_lines(&Theme::terminal(true), 300)).join("\n");
        for needle in [
            "Searching /chats",
            "Search all chats",
            "status:running",
            "archived:true",
            "has_unread:true",
            "pr_status:open",
            "pr:123",
            "repo:<text>",
            "source:created_by_me",
            "title:\"fix ci\"",
        ] {
            assert!(shown.contains(needle), "{needle} is missing from:\n{shown}");
        }
        for key in scuttle_core::chat_list::SEARCH_KEYS {
            if key != "search" {
                assert!(
                    shown.contains(&format!("{key}:")),
                    "{key} is not documented"
                );
            }
        }
    }

    #[test]
    fn help_lists_every_command_and_key() {
        let lines = text(&help_lines(&Theme::terminal(true), 200));
        for c in COMMANDS {
            assert!(
                lines
                    .iter()
                    .any(|l| l.contains(&c.display_usage()) && l.contains(c.description)),
                "{} missing from {lines:?}",
                c.usage
            );
        }
        for k in KEYS {
            assert!(
                lines
                    .iter()
                    .any(|l| l.contains(k.keys) && l.contains(k.action)),
                "{} missing",
                k.keys
            );
        }
    }

    #[test]
    fn help_shows_aliases_on_the_command_line() {
        let lines = text(&help_lines(&Theme::terminal(true), 200));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("/quit (/exit)") && l.contains("Exit scuttle"))
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("/organization [name] (/org)"))
        );
    }

    #[test]
    fn help_names_the_m2_keys_and_commands() {
        let theme = Theme::terminal(true);
        let shown = text(&help_lines(&theme, 200)).join("\n");
        for needle in [
            "Ctrl+R",
            "/chats [query] (/resume)",
            "/parent (/back)",
            "/info (/chat-info)",
            "Return to the parent chat",
            "Send the first queued message now",
            "Implement a proposed plan",
            "Remove the last attachment",
            "Complete a file path to attach",
            "Ctrl+A archives",
        ] {
            assert!(shown.contains(needle), "{needle} is missing from:\n{shown}");
        }
    }

    /// The facts `/help` states about how keys behave, each named after the code that decides
    /// it, so a change to the behavior has to change this text too.
    #[test]
    fn help_states_how_the_m2_keys_behave() {
        let lines = text(&help_lines(&Theme::terminal(true), 200));
        let line_with = |keys: &str, facts: &[&str]| {
            let line = lines
                .iter()
                .find(|l| l.starts_with(keys))
                .unwrap_or_else(|| panic!("no help line for {keys} in {lines:?}"));
            for fact in facts {
                assert!(
                    line.contains(fact),
                    "{keys}: {fact:?} is missing from {line:?}"
                );
            }
        };
        // `Tui::key`: with nothing typed, the question menu, then Ctrl+Enter's plan, then the
        // send key's "Send now".
        line_with(
            "With nothing typed",
            &[
                "The question menu takes its keys first",
                "then Ctrl+Enter implements a ready plan",
                "then the send key sends the first queued message now",
            ],
        );
        line_with(
            "Ctrl+Enter on an empty composer",
            &["Implement a proposed plan"],
        );
        line_with(
            "Send key on an empty composer",
            &[
                "Send the first queued message now",
                "interrupting a running turn",
            ],
        );
        line_with(
            "Up, Down, Enter, Left, Esc",
            &["while the composer is empty", "Left goes back", "Esc hides"],
        );
        line_with("Click", &["save an attached file"]);
        line_with(
            "Backspace on an empty composer",
            &["Remove the last attachment"],
        );
        // `Composer::key_action`'s Tab: a slash entry first, else an `@path`; `Tui::key` takes
        // Tab on an empty composer to show hidden questions.
        line_with("Tab ", &["Complete the slash command or skill"]);
        // `Composer::key_action`'s send key on a bare command prefix.
        line_with(
            "Send key in the slash menu",
            &[
                "Run the highlighted command or skill",
                "after a space, send the text",
            ],
        );
        line_with("Tab after @", &["Complete a file path to attach"]);
        // `Composer::slash_matches` and `key_action`: after `/workspace ` or `/ws `, the menu
        // lists workspace names, and the send key takes one only once one is typed or picked.
        line_with(
            "Up, Down in the slash menu",
            &["the workspace names after /workspace"],
        );
        line_with(
            "Tab after /workspace",
            &["Complete a workspace name, or none", "/ws works the same"],
        );
        line_with(
            "Send key after /workspace",
            &[
                "Attach the highlighted workspace",
                "with nothing after the space, run /workspace",
            ],
        );
        line_with("Tab on an empty composer", &["Show the questions Esc hid"]);
        line_with("Esc in an idle subagent", &["Return to the parent chat"]);
        line_with(
            "PageUp, PageDown",
            &["PageUp at the top loads older messages"],
        );
        line_with(
            "Wheel",
            &[
                "while mouse capture is on (/mouse)",
                "loads older messages",
                "moves like Up and Down",
            ],
        );
        line_with("Drag", &["copy it on release"]);
        line_with(
            "In /chats",
            &[
                "Tab switches All, Active, Unread, Archived",
                "Right and Left show and hide subagents",
            ],
        );
        line_with(
            "Ctrl keys in /chats",
            &[
                "Ctrl+A archives",
                "asks whether to delete the workspace too",
                "unarchives at once",
                "Ctrl+E renames",
                "Ctrl+P pins",
                "Ctrl+U marks read or unread",
            ],
        );
        line_with(
            "In /subagents",
            &[
                "Up and Down preview",
                "Enter opens",
                "PageUp and PageDown scroll",
            ],
        );
        line_with(
            "In /queue",
            &["Enter sends now", "Delete or Backspace removes"],
        );
        line_with(
            "In /files",
            &[
                "Enter saves",
                "s saves as",
                "v views text",
                "g goes to its message",
                "a taken name asks: k keeps both, r replaces, c cancels",
            ],
        );
        let files = lines
            .iter()
            .find(|l| l.starts_with("In /files"))
            .expect("a /files line");
        assert!(!files.contains("opens"), "{files}");
        line_with(
            "In /mcp",
            &["Enter or Space turns an organization server on or off for the next message"],
        );
        line_with("In /git", &["Enter", "pull request", "your git pager"]);
        line_with("In /workspace", &["Enter", "SSH", "detaches", "switches"]);
        // `runtime::page` follows git's pager order.
        line_with("/diff ", &["your git pager"]);
        let shown = lines.join("\n");
        for command in [
            "/attach <path>",
            "@path",
            "/info (/chat-info)",
            "/workspace [name|none] (/ws)",
            "/git",
            "/diff",
            "/mcp",
            "/queue",
            "/files",
            "/subagents",
            "/parent (/back)",
            "/<username>:<name>",
        ] {
            assert!(
                shown.contains(command),
                "{command} is missing from:\n{shown}"
            );
        }
    }

    #[test]
    fn help_wraps_to_narrow_widths() {
        let lines = help_lines(&Theme::terminal(true), 30);
        assert!(lines.iter().all(|l| l.width() <= 30));
    }

    #[test]
    fn without_keyboard_enhancement_help_offers_implement_instead_of_ctrl_enter() {
        let lines = text(&help_lines_for(&Theme::terminal(true), 200, false, "Alt+↵"));
        assert!(
            !lines.iter().any(|l| l.contains("Ctrl+Enter implements")
                || l.contains("(Ctrl+Enter)")
                || l.starts_with("Ctrl+Enter on")),
            "{lines:#?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("/implement") && l.contains("Implement a proposed plan")),
            "{lines:#?}"
        );
        let nothing_typed = lines
            .iter()
            .find(|l| l.starts_with("With nothing typed"))
            .unwrap();
        assert!(
            nothing_typed.contains("then the send key sends the first queued message now"),
            "{nothing_typed}"
        );
    }

    #[test]
    fn help_wraps_actions_with_a_hanging_indent_and_names_the_overlay_keys() {
        let theme = Theme::terminal(true);
        let keys_width = KEYS.iter().map(|k| k.keys.width()).max().unwrap();
        let lines = text(&help_lines(&theme, 76));
        let wheel = lines.iter().position(|l| l.starts_with("Wheel")).unwrap();
        let next = &lines[wheel + 1];
        assert!(
            next.starts_with(&" ".repeat(keys_width + 2)) && !next.trim().is_empty(),
            "the action continues under its column: {:#?}",
            &lines[wheel..wheel + 2]
        );
        let wide = text(&help_lines(&theme, 300)).join("\n");
        for fact in [
            "Esc or Ctrl+C",
            "Close the open overlay",
            "Type to filter /model and /workspace",
            "Enter in the /workspace table attaches the workspace",
        ] {
            assert!(wide.contains(fact), "{fact:?} is missing from:\n{wide}");
        }
    }

    #[test]
    fn help_names_the_send_now_key_the_queued_hint_names() {
        for label in ["↵", "⌘↵", "Ctrl+↵", "⌥↵", "Alt+↵"] {
            let lines = text(&help_lines_for(&Theme::terminal(true), 200, true, label));
            let line = lines
                .iter()
                .find(|l| l.starts_with("Send key on an empty composer"))
                .unwrap_or_else(|| panic!("no send-now line in {lines:?}"));
            let names_the_key = format!("in this terminal that is {label},");
            for fact in [
                "Send the first queued message now, interrupting a running turn",
                names_the_key.as_str(),
                "which the row above the composer names while messages wait",
            ] {
                assert!(
                    line.contains(fact),
                    "{label}: {fact:?} is missing from {line:?}"
                );
            }
        }
    }

    #[test]
    fn help_says_how_a_model_pick_sends_a_held_message() {
        let lines = text(&help_lines(&Theme::terminal(true), 200));
        let line = lines
            .iter()
            .find(|l| l.starts_with("Send on an unavailable model"))
            .unwrap_or_else(|| panic!("no unavailable-model line in {lines:?}"));
        for fact in [
            "Hold the message and open /model",
            "Enter sends it with the model you pick",
            "Esc puts it back in the composer",
        ] {
            assert!(line.contains(fact), "{fact:?} is missing from {line:?}");
        }
    }

    #[test]
    fn help_names_the_key_that_copies_the_draft() {
        let lines = text(&help_lines(&Theme::terminal(true), 200));
        let line = lines
            .iter()
            .find(|l| l.starts_with("Ctrl+O"))
            .unwrap_or_else(|| panic!("no Ctrl+O line in {lines:?}"));
        assert!(
            line.contains("Copy everything typed in the composer, every line of it"),
            "{line:?}"
        );
    }

    #[test]
    fn help_says_how_to_send_a_held_message_later_and_what_zellij_takes() {
        let lines = text(&help_lines(&Theme::terminal(true), 300));
        let held = lines
            .iter()
            .find(|l| l.starts_with("Held message"))
            .unwrap_or_else(|| panic!("no held-message line in {lines:?}"));
        assert!(
            held.contains(
                "If an overlay was open or you were typing, the picker waits: run /model to send it"
            ),
            "{held:?}"
        );
        let zellij = lines
            .iter()
            .find(|l| l.starts_with("In Zellij"))
            .unwrap_or_else(|| panic!("no Zellij line in {lines:?}"));
        assert!(
            zellij.contains("Its default keymap takes Ctrl+O and Ctrl+G before scuttle sees them"),
            "{zellij:?}"
        );
    }

    #[test]
    fn help_explains_the_chats_markers_in_their_order() {
        let shown = text(&help_lines(&Theme::terminal(true), 300)).join("\n");
        for needle in [
            "Markers in /chats",
            "chats.pin_icon",
            "a spinner while the agent works, ! after an error, ? while it waits on you, then 🔵 for unread messages",
            "keeps its spinner, and the 🔵 shows once it stops",
            "+N",
        ] {
            assert!(shown.contains(needle), "{needle} is missing from:\n{shown}");
        }
        assert!(!shown.contains('•'), "the old unread dot is gone:\n{shown}");
        for (markers, icons) in [
            (CHATS_MARKERS, IconSet::Text),
            (CHATS_MARKERS_NERD, IconSet::Nerd),
        ] {
            let theme = Theme {
                icons,
                ..Theme::terminal(true)
            };
            let shown = text(&help_lines(&theme, 1000)).join("\n");
            let at: Vec<usize> = markers
                .iter()
                .map(|m| {
                    shown
                        .find(m)
                        .unwrap_or_else(|| panic!("{m:?} is missing from:\n{shown}"))
                })
                .collect();
            assert!(at.is_sorted(), "{icons:?} markers out of order: {at:?}");
            let selection = shown.find('\u{203a}');
            assert!(
                selection.is_some_and(|i| i < at[1]),
                "{icons:?}: the selection column is named first:\n{shown}"
            );
        }
        let status = CHATS_MARKERS[1];
        let order: Vec<usize> = ["a spinner", "! after", "? while", "🔵 for"]
            .iter()
            .map(|m| status.find(m).unwrap())
            .collect();
        assert!(
            order.is_sorted(),
            "the status priority reads in order: {status}"
        );
    }

    #[test]
    fn help_says_how_a_pasted_text_token_expands() {
        let lines = text(&help_lines(&Theme::terminal(true), 300));
        let line = lines
            .iter()
            .find(|l| l.starts_with("Tab on a pasted text token"))
            .unwrap_or_else(|| panic!("no pasted-text line in {lines:?}"));
        for fact in [
            "Expand the large paste it stands for",
            "pasting the same text again right away",
            "Backspace after it removes it",
            "sent as a text file",
        ] {
            assert!(line.contains(fact), "{fact:?} is missing from {line:?}");
        }
    }

    #[test]
    fn help_names_the_line_keys_and_the_jump_to_the_latest_message() {
        let lines = text(&help_lines(&Theme::terminal(true), 300));
        let line = |start: &str| {
            lines
                .iter()
                .find(|l| l.starts_with(start))
                .unwrap_or_else(|| panic!("no {start:?} line in {lines:?}"))
                .clone()
        };
        let home = line("Home, End");
        for fact in [
            "start or end of the line",
            "Cmd+Left and Cmd+Right",
            "Ctrl+A and Ctrl+E",
        ] {
            assert!(home.contains(fact), "{fact:?} is missing from {home:?}");
        }
        assert!(line("Ctrl+Home, Ctrl+End").contains("whole message"));
        let jump = line("End at a line's end");
        assert!(jump.contains("Jump to the latest message"), "{jump:?}");
        assert!(jump.contains("with nothing typed"), "{jump:?}");
    }

    #[test]
    fn text_help_says_where_to_get_a_nerd_font_and_how_to_turn_icons_on() {
        let shown = text(&help_lines(&Theme::terminal(true), 300));
        let at = |title: &str| {
            shown
                .iter()
                .position(|l| l == title)
                .unwrap_or_else(|| panic!("no {title} section in {shown:#?}"))
        };
        let (markers, icons, search) =
            (at("Markers in /chats"), at("Icons"), at("Searching /chats"));
        assert!(
            markers < icons && icons < search,
            "after the markers, and the search stays last"
        );
        let section = shown[icons..search].join("\n");
        for needle in [
            "scuttle works better with a Nerd Font!",
            crate::icons::NERD_FONTS_URL,
            "icons = \"nerd\"",
            "NERD_FONT=1",
            "Mono",
        ] {
            assert!(
                section.contains(needle),
                "{needle} is missing from:\n{section}"
            );
        }
    }

    #[test]
    fn nerd_help_describes_the_glyphs_it_shows_and_drops_the_tip() {
        let nerd = Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal(true)
        };
        let lines = text(&help_lines(&nerd, 300));
        assert!(!lines.iter().any(|l| l == "Icons"), "{lines:#?}");
        let shown = lines.join("\n");
        assert!(!shown.contains("nerdfonts.com"), "{shown}");
        for needle in [
            "Markers in /chats",
            "chats.pin_icon",
            "\u{f421} after an error",
            "\u{f420} while it waits on you",
            "\u{f111} for unread messages",
            "\u{f411} after the title marks an archived chat",
            "+N",
        ] {
            assert!(shown.contains(needle), "{needle} is missing from:\n{shown}");
        }
        assert!(
            !shown.contains('🔵'),
            "nerd mode names the glyph it draws, not the emoji"
        );
    }

    #[test]
    fn help_explains_the_pull_request_column_in_both_icon_sets() {
        let shown = text(&help_lines(&Theme::terminal(true), 300)).join("\n");
        for needle in [
            "From 120 columns",
            "coder/coder#123 merged",
            "open, draft, merged, or closed",
            "group/project!123",
            "the owner is dropped first",
        ] {
            assert!(shown.contains(needle), "{needle} is missing from:\n{shown}");
        }
        let nerd = Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal(true)
        };
        let shown = text(&help_lines(&nerd, 300)).join("\n");
        for needle in [
            "From 120 columns",
            "\u{f407} open",
            "\u{f4dd} draft",
            "\u{f419} merged",
            "\u{f4dc} closed",
            "\u{f09b} coder/coder#123",
            "\u{f296} GitLab",
            "the owner is dropped first",
        ] {
            assert!(shown.contains(needle), "{needle} is missing from:\n{shown}");
        }
    }

    #[test]
    fn help_names_cmd_arrows_only_where_the_terminal_can_send_them() {
        let home = |enhanced: bool| {
            text(&help_lines_for(&Theme::terminal(true), 300, enhanced, "↵"))
                .into_iter()
                .find(|l| l.starts_with("Home, End"))
                .unwrap_or_else(|| panic!("no Home, End line"))
        };
        let with = home(true);
        assert!(with.contains("Cmd+Left and Cmd+Right"), "{with:?}");
        let without = home(false);
        assert!(!without.contains("Cmd"), "{without:?}");
        for fact in ["start or end of the line", "Ctrl+A and Ctrl+E"] {
            assert!(
                without.contains(fact),
                "{fact:?} is missing from {without:?}"
            );
        }
    }

    #[test]
    fn help_says_end_twice_jumps_from_a_draft() {
        let lines = text(&help_lines(&Theme::terminal(true), 300));
        let jump = lines
            .iter()
            .find(|l| l.starts_with("End at a line's end"))
            .unwrap_or_else(|| panic!("no End line in {lines:?}"));
        assert!(jump.contains("in a draft, press End twice"), "{jump:?}");
    }

    #[test]
    fn every_glyph_in_nerd_help_has_a_space_after_it() {
        let nerd = Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal(true)
        };
        for line in text(&help_lines(&nerd, 1000)) {
            let chars: Vec<char> = line.chars().collect();
            for (i, c) in chars.iter().enumerate() {
                if matches!(c, '\u{e000}'..='\u{f8ff}') {
                    assert_eq!(chars.get(i + 1), Some(&' '), "{c:?} in {line:?}");
                }
            }
        }
    }

    #[test]
    fn help_says_how_model_changes_the_compaction_threshold() {
        let lines = text(&help_lines(&Theme::terminal(true), 300));
        let line = |keys: &str| {
            lines
                .iter()
                .find(|l| l.starts_with(keys))
                .unwrap_or_else(|| panic!("no {keys:?} line in {lines:?}"))
                .clone()
        };
        for (keys, fact) in [
            ("Left, Right in /model", "compaction threshold by 5%"),
            ("Left, Right in /model", "0% (compacts after every turn)"),
            ("Left, Right in /model", "100% (never compacts)"),
            (
                "Left, Right in /model",
                "saves when you move to another model or close /model",
            ),
            ("Delete in /model", "Restore the model's default"),
            ("Delete in /model", "saves the same way"),
        ] {
            let line = line(keys);
            assert!(line.contains(fact), "{fact:?} is missing from {line:?}");
        }
    }
}
