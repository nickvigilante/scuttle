//! The icons scuttle draws: each one as a Nerd Font glyph, or as the text scuttle showed in
//! its place before icons, whichever `Theme::icons` picks.
//!
//! A glyph always takes a slot of two cells, the glyph and a space, counted here and never
//! measured. A Nerd Font glyph is one cell wide by `width`, the measure ratatui places cells
//! by, but a non-Mono font draws many of them wider, and the space takes what spills.
//! `width_cjk` would count a glyph as two cells and put scuttle out of step with its own
//! buffer, so nothing here uses it. The codepoints were read from the `cmap` and `post`
//! tables of patched Nerd Fonts.

use ratatui::style::Style;
use ratatui::text::Span;
use scuttle_core::chat_list::PrState;

pub use scuttle_core::config::IconSet;

use crate::theme::Theme;
use crate::wrap::cells_width;

/// Where the Nerd Font tip sends the reader.
pub const NERD_FONTS_URL: &str = "https://www.nerdfonts.com/";

/// Something scuttle marks with an icon. The transcript, the composer, the footer,
/// `/workspace`, and `/mcp` use Codicons (nf-cod), and `/chats` and `/subagents` use
/// Octicons (nf-oct), so each surface keeps to one family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    /// A shell call: `execute` and the `process_*` tools. nf-cod-terminal.
    Terminal,
    /// A read: `read_file`, `read_template`, and the skill reads. nf-cod-file.
    File,
    /// A write: `write_file` and `edit_files`. nf-cod-edit.
    Edit,
    /// A search: `find_tools`, and any other tool named for searching. nf-cod-search.
    Search,
    /// The web: `web_search` and `web_fetch`. nf-cod-globe.
    Web,
    /// An MCP server's tool, named `server__tool`. nf-cod-mcp.
    Mcp,
    /// A subagent call. nf-cod-agent.
    Agent,
    /// `propose_plan`. nf-cod-checklist.
    Plan,
    /// `ask_user_question`. nf-cod-question.
    Question,
    /// A workspace or template call. nf-cod-vm.
    Workspace,
    /// Any other tool, so every tool head lines up. nf-cod-tools.
    Tool,
    /// An error: the transcript's error line, an error notice, a failed chip, a failed
    /// workspace, or a failed MCP server. nf-cod-error.
    Error,
    /// A queued message, in place of `queued ·`. nf-cod-history.
    Queued,
    /// A file sent with a message, in place of `attached`. nf-cod-attach.
    Attached,
    /// A file chip above the composer, inside its brackets. nf-cod-attach.
    Chip,
    /// The footer while the stream connects. nf-cod-plug.
    Connecting,
    /// The footer while the stream reconnects. nf-cod-debug_disconnect.
    Reconnecting,
    /// A pinned chat, unless `chats.pin_icon` names another marker. nf-oct-pin.
    Pin,
    /// A chat waiting on the user, in place of `?`. nf-oct-question.
    Asking,
    /// A chat that stopped on an error, in place of `!`. nf-oct-alert.
    Failed,
    /// A chat with unread messages, in place of 🔵. nf-fa-circle, larger than a dot glyph.
    Unread,
    /// An archived chat, in place of `archived`. nf-oct-archive.
    Archived,
    /// A running workspace. nf-cod-vm_running.
    WorkspaceRunning,
    /// A stopped, canceled, or deleted workspace. nf-cod-vm_outline.
    WorkspaceStopped,
    /// A workspace between states, such as starting. nf-cod-loading.
    WorkspaceBusy,
    /// The workspace picker's "none" row. nf-cod-circle_slash.
    NoWorkspace,
    /// An MCP server that is on. nf-cod-pass_filled.
    ServerOn,
    /// An MCP server that is off. nf-cod-circle_large.
    ServerOff,
    /// An open pull request. nf-oct-git_pull_request.
    PrOpen,
    /// A draft pull request. nf-oct-git_pull_request_draft.
    PrDraft,
    /// A merged pull request. nf-oct-git_merge.
    PrMerged,
    /// A closed pull request. nf-oct-git_pull_request_closed.
    PrClosed,
}

impl Icon {
    /// Every icon, in declaration order.
    pub const ALL: [Icon; 32] = [
        Icon::Terminal,
        Icon::File,
        Icon::Edit,
        Icon::Search,
        Icon::Web,
        Icon::Mcp,
        Icon::Agent,
        Icon::Plan,
        Icon::Question,
        Icon::Workspace,
        Icon::Tool,
        Icon::Error,
        Icon::Queued,
        Icon::Attached,
        Icon::Chip,
        Icon::Connecting,
        Icon::Reconnecting,
        Icon::Pin,
        Icon::Asking,
        Icon::Failed,
        Icon::Unread,
        Icon::Archived,
        Icon::WorkspaceRunning,
        Icon::WorkspaceStopped,
        Icon::WorkspaceBusy,
        Icon::NoWorkspace,
        Icon::ServerOn,
        Icon::ServerOff,
        Icon::PrOpen,
        Icon::PrDraft,
        Icon::PrMerged,
        Icon::PrClosed,
    ];

    /// The glyph and the space that ends its slot.
    fn nerd(self) -> &'static str {
        match self {
            Icon::Terminal => "\u{ea85} ",
            Icon::File => "\u{ea7b} ",
            Icon::Edit => "\u{ea73} ",
            Icon::Search => "\u{ea6d} ",
            Icon::Web => "\u{eb01} ",
            Icon::Mcp => "\u{ec47} ",
            Icon::Agent => "\u{ec67} ",
            Icon::Plan => "\u{eab3} ",
            Icon::Question => "\u{eb32} ",
            Icon::Workspace => "\u{ea7a} ",
            Icon::Tool => "\u{eb6d} ",
            Icon::Error => "\u{ea87} ",
            Icon::Queued => "\u{ea82} ",
            Icon::Attached | Icon::Chip => "\u{ec34} ",
            Icon::Connecting => "\u{eb2d} ",
            Icon::Reconnecting => "\u{ead0} ",
            Icon::Pin => "\u{f435} ",
            Icon::Asking => "\u{f420} ",
            Icon::Failed => "\u{f421} ",
            Icon::Unread => "\u{f111} ",
            Icon::Archived => "\u{f411} ",
            Icon::WorkspaceRunning => "\u{eb7b} ",
            Icon::WorkspaceStopped => "\u{eb7a} ",
            Icon::WorkspaceBusy => "\u{eb19} ",
            Icon::NoWorkspace => "\u{eabd} ",
            Icon::ServerOn => "\u{ebb3} ",
            Icon::ServerOff => "\u{ebb5} ",
            Icon::PrOpen => "\u{f407} ",
            Icon::PrDraft => "\u{f4dd} ",
            Icon::PrMerged => "\u{f419} ",
            Icon::PrClosed => "\u{f4dc} ",
        }
    }

    /// What text mode shows in the icon's place: exactly what scuttle showed there before
    /// icons, which for most icons is nothing.
    fn text(self) -> &'static str {
        match self {
            Icon::Pin => "📌",
            Icon::Asking => "?",
            Icon::Failed => "!",
            // U+1F535, two cells wide under both width rules.
            Icon::Unread => "\u{1f535}",
            Icon::Archived => "archived",
            Icon::Queued => "queued · ",
            Icon::Attached => "attached ",
            _ => "",
        }
    }
}

/// What text mode shows in place of the Nerd Font glyph `glyph`, the first grapheme of an
/// icon's slot, or `None` when it is no icon's glyph. A copy uses it, so a glyph never reaches
/// a reader without the font, and the words a glyph stands for, such as `queued`, come back.
pub fn text_for_glyph(glyph: &str) -> Option<&'static str> {
    Icon::ALL
        .iter()
        .find(|icon| icon.nerd().strip_suffix(' ') == Some(glyph))
        .map(|icon| icon.text())
}

/// What an icon takes on screen: its text, and the cells it is counted as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub text: &'static str,
    pub width: u16,
}

/// The slot `icon` takes in `set`. A Nerd Font glyph's slot is always two cells, the glyph and
/// a space; a text fallback is as wide as `wrap.rs` measures it.
pub fn slot(set: IconSet, icon: Icon) -> Slot {
    match set {
        IconSet::Nerd => Slot {
            text: icon.nerd(),
            width: 2,
        },
        IconSet::Text => {
            let text = icon.text();
            Slot {
                text,
                width: cells_width(text) as u16,
            }
        }
    }
}

/// The style an icon in `base` takes: a glyph loses its color under `NO_COLOR`, and a text
/// fallback keeps `base`, as scuttle drew it before icons.
pub fn style(theme: &Theme, base: Style) -> Style {
    match theme.icons {
        IconSet::Nerd => theme.icon(base),
        IconSet::Text => base,
    }
}

/// `icon` as a span in `base`, or `None` when `theme`'s set draws nothing for it.
pub fn lead(theme: &Theme, icon: Icon, base: Style) -> Option<Span<'static>> {
    let slot = slot(theme.icons, icon);
    (!slot.text.is_empty()).then(|| Span::styled(slot.text, style(theme, base)))
}

/// `before`, then `icon`, then `after`, all in `base`. Text mode keeps the one span scuttle
/// drew before icons; nerd mode gives the glyph its own span, so `NO_COLOR` can leave it plain.
pub fn line_with(
    theme: &Theme,
    before: &str,
    icon: Icon,
    after: &str,
    base: Style,
) -> Vec<Span<'static>> {
    match theme.icons {
        IconSet::Text => vec![Span::styled(
            format!("{before}{}{after}", icon.text()),
            base,
        )],
        IconSet::Nerd => {
            let mut spans = Vec::new();
            if !before.is_empty() {
                spans.push(Span::styled(before.to_owned(), base));
            }
            spans.push(Span::styled(icon.nerd(), theme.icon(base)));
            spans.push(Span::styled(after.to_owned(), base));
            spans
        }
    }
}

/// The kind of tool `name` calls, which picks the glyph after its state marker. Tool names
/// are the ones `coderd/x/chatd` registers.
pub fn tool_icon(name: &str) -> Icon {
    match name {
        "execute" | "process_output" | "process_list" | "process_signal" => Icon::Terminal,
        "read_file" | "read_template" | "read_skill" | "read_skill_file" => Icon::File,
        "write_file" | "edit_files" => Icon::Edit,
        "web_search" | "web_fetch" => Icon::Web,
        "propose_plan" => Icon::Plan,
        "ask_user_question" => Icon::Question,
        "create_workspace" | "start_workspace" | "stop_workspace" | "list_templates" => {
            Icon::Workspace
        }
        _ if crate::subagent::action(name).is_some() => Icon::Agent,
        // An MCP tool is named `server__tool`, so the server's words come first.
        _ if name.contains("__") => Icon::Mcp,
        _ if name == "find_tools" || name.contains("search") => Icon::Search,
        _ => Icon::Tool,
    }
}

/// The icon for a pull request in `state`, in `/chats` and `/git`.
pub fn pr_icon(state: PrState) -> Icon {
    match state {
        PrState::Open => Icon::PrOpen,
        PrState::Draft => Icon::PrDraft,
        PrState::Merged => Icon::PrMerged,
        PrState::Closed => Icon::PrClosed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Colors;
    use ratatui::style::{Color, Modifier};
    use unicode_width::UnicodeWidthStr;

    /// Whether `c` is in the Basic Multilingual Plane's Private Use Area, where every glyph
    /// this set draws sits.
    fn private_use(c: char) -> bool {
        ('\u{e000}'..='\u{f8ff}').contains(&c)
    }

    #[test]
    fn every_icon_is_a_two_cell_nerd_slot_with_a_text_fallback() {
        for icon in Icon::ALL {
            let nerd = slot(IconSet::Nerd, icon);
            let chars: Vec<char> = nerd.text.chars().collect();
            assert_eq!(chars.len(), 2, "{icon:?} is a glyph and a space");
            assert!(private_use(chars[0]), "{icon:?} is a Nerd Font glyph");
            assert_eq!(chars[1], ' ', "{icon:?} ends its slot with a space");
            assert_eq!(nerd.width, 2, "{icon:?}");
            assert_eq!(cells_width(nerd.text), 2, "{icon:?} is drawn in two cells");
            assert_eq!(
                nerd.text.width_cjk(),
                3,
                "{icon:?}: width_cjk would count three cells, so the slot never uses it"
            );
            let text = slot(IconSet::Text, icon);
            assert!(
                !text.text.chars().any(private_use),
                "{icon:?} falls back to text any font has"
            );
            assert_eq!(usize::from(text.width), cells_width(text.text), "{icon:?}");
        }
    }

    #[test]
    fn text_falls_back_to_what_scuttle_showed_before_icons() {
        let text = |icon| slot(IconSet::Text, icon).text;
        let shown = [
            (Icon::Pin, "📌"),
            (Icon::Asking, "?"),
            (Icon::Failed, "!"),
            (Icon::Unread, "\u{1f535}"),
            (Icon::Archived, "archived"),
            (Icon::Queued, "queued · "),
            (Icon::Attached, "attached "),
        ];
        for (icon, fallback) in shown {
            assert_eq!(text(icon), fallback, "{icon:?}");
        }
        for icon in Icon::ALL
            .into_iter()
            .filter(|i| !shown.iter().any(|(s, _)| s == i))
        {
            assert_eq!(text(icon), "", "{icon:?} adds nothing in text mode");
        }
    }

    #[test]
    fn a_glyph_is_plain_under_no_color_and_a_fallback_keeps_its_style() {
        let accent = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
        let nerd = Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal(true)
        };
        let plain = Theme {
            icons: IconSet::Nerd,
            ..Theme::terminal_with(true, Colors::None)
        };
        let text = Theme::terminal_with(true, Colors::None);
        assert_eq!(
            lead(&nerd, Icon::Unread, accent),
            Some(Span::styled("\u{f111} ", accent))
        );
        assert_eq!(
            lead(&plain, Icon::Unread, accent),
            Some(Span::raw("\u{f111} "))
        );
        assert_eq!(
            lead(&text, Icon::Failed, accent),
            Some(Span::styled("!", accent)),
            "a text fallback keeps the style it had"
        );
        assert_eq!(
            lead(&text, Icon::Error, accent),
            None,
            "nothing to draw is no span"
        );
        assert_eq!(
            line_with(&text, "  ", Icon::Queued, "next", accent),
            vec![Span::styled("  queued · next", accent)],
            "text mode keeps the one span it drew before icons"
        );
        assert_eq!(
            line_with(&nerd, "  ", Icon::Queued, "next", accent),
            vec![
                Span::styled("  ", accent),
                Span::styled("\u{ea82} ", accent),
                Span::styled("next", accent),
            ]
        );
        assert_eq!(
            line_with(&plain, "", Icon::Error, "Error: x", accent),
            vec![Span::raw("\u{ea87} "), Span::styled("Error: x", accent)]
        );
        assert_eq!(style(&plain, accent), Style::new());
        assert_eq!(style(&text, accent), accent);
    }

    #[test]
    fn each_tool_name_maps_to_its_kind() {
        for (name, icon) in [
            ("execute", Icon::Terminal),
            ("process_output", Icon::Terminal),
            ("process_list", Icon::Terminal),
            ("process_signal", Icon::Terminal),
            ("read_file", Icon::File),
            ("read_template", Icon::File),
            ("read_skill", Icon::File),
            ("read_skill_file", Icon::File),
            ("write_file", Icon::Edit),
            ("edit_files", Icon::Edit),
            ("web_search", Icon::Web),
            ("web_fetch", Icon::Web),
            ("find_tools", Icon::Search),
            ("search_docs", Icon::Search),
            ("github__create_issue", Icon::Mcp),
            ("github__search_issues", Icon::Mcp),
            ("spawn_agent", Icon::Agent),
            ("spawn_explore_agent", Icon::Agent),
            ("wait_agent", Icon::Agent),
            ("message_agent", Icon::Agent),
            ("interrupt_agent", Icon::Agent),
            ("close_agent", Icon::Agent),
            ("propose_plan", Icon::Plan),
            ("ask_user_question", Icon::Question),
            ("create_workspace", Icon::Workspace),
            ("start_workspace", Icon::Workspace),
            ("stop_workspace", Icon::Workspace),
            ("list_templates", Icon::Workspace),
            ("attach_file", Icon::Tool),
            ("computer", Icon::Tool),
            ("advisor", Icon::Tool),
            ("something_new", Icon::Tool),
        ] {
            assert_eq!(tool_icon(name), icon, "{name}");
        }
    }

    #[test]
    fn each_pull_request_state_has_its_octicon_and_no_text_icon() {
        use scuttle_core::chat_list::PrState;
        let glyph = |state| slot(IconSet::Nerd, pr_icon(state)).text;
        assert_eq!(glyph(PrState::Open), "\u{f407} ");
        assert_eq!(glyph(PrState::Draft), "\u{f4dd} ");
        assert_eq!(glyph(PrState::Merged), "\u{f419} ");
        assert_eq!(glyph(PrState::Closed), "\u{f4dc} ");
        for state in [
            PrState::Open,
            PrState::Draft,
            PrState::Merged,
            PrState::Closed,
        ] {
            assert_eq!(
                slot(IconSet::Text, pr_icon(state)).text,
                "",
                "text mode spells {state:?} out in the cell instead"
            );
        }
    }
}
