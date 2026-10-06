//! The terminal palette: ANSI colors, so the user's terminal color scheme applies, and the
//! Coder brand accent for the footer's commands and the link under the pointer.

use ratatui::style::{Color, Modifier, Style};
use scuttle_core::chat_list::PrState;
use scuttle_core::config::IconSet;

/// How many colors the terminal shows, which decides how the brand accent is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colors {
    /// 24-bit color.
    TrueColor,
    /// The 256 indexed colors.
    Ansi256,
    /// `NO_COLOR` is set, so the brand accent is plain text.
    None,
}

impl Colors {
    /// Reads the environment through `env`: `NO_COLOR` set to anything but an empty string
    /// turns color off, as no-color.org asks, and a `COLORTERM` of `truecolor` or `24bit` says
    /// the terminal takes 24-bit color.
    pub fn detect(env: impl Fn(&str) -> Option<String>) -> Colors {
        if env("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return Colors::None;
        }
        match env("COLORTERM").as_deref() {
            Some("truecolor" | "24bit") => Colors::TrueColor,
            _ => Colors::Ansi256,
        }
    }
}

/// The Coder brand accent, Tailwind sky: sky-500 (`#0ea5e9`) on a dark background and sky-600
/// (`#0284c7`) on a light one. Without 24-bit color it is the nearest indexed color, 38
/// (`#00afd7`) or 32 (`#0087d7`), and under `NO_COLOR` there is none.
pub(crate) fn brand_color(dark: bool, colors: Colors) -> Option<Color> {
    match (colors, dark) {
        (Colors::None, _) => None,
        (Colors::TrueColor, true) => Some(Color::Rgb(0x0e, 0xa5, 0xe9)),
        (Colors::TrueColor, false) => Some(Color::Rgb(0x02, 0x84, 0xc7)),
        (Colors::Ansi256, true) => Some(Color::Indexed(38)),
        (Colors::Ansi256, false) => Some(Color::Indexed(32)),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub accent: Style,
    pub dim: Style,
    pub user: Style,
    pub error: Style,
    pub ok: Style,
    pub warn: Style,
    /// Background behind the user's own messages: a gray one step off the usual background.
    /// Fixed 256-color grays, since blending the real background would need truecolor.
    pub user_tint: Style,
    /// The rule between a turn's work and its answer, in the terminal's own foreground.
    pub rule: Style,
    /// Text selected with the mouse.
    pub selection: Style,
    /// The Coder brand accent, for the slash commands in the footer.
    pub brand: Style,
    /// The link under the mouse pointer: the brand accent, bold and underlined.
    pub link_hover: Style,
    /// A pull request's state in `/chats`, as the web UI colors it: open green, draft dim,
    /// merged magenta, and closed red. Under `NO_COLOR` none has a color.
    pub pr_open: Style,
    pub pr_draft: Style,
    pub pr_merged: Style,
    pub pr_closed: Style,
    /// How many colors the terminal shows; under `Colors::None` an icon's glyph is plain.
    pub colors: Colors,
    /// Which icons to draw. A new theme draws text; `Tui` sets it from `icons` and
    /// `NERD_FONT`.
    pub icons: IconSet,
}

impl Theme {
    /// The palette with the brand accent in 256 colors, as the tests draw it.
    #[cfg(test)]
    pub fn terminal(dark: bool) -> Theme {
        Theme::terminal_with(dark, Colors::Ansi256)
    }

    /// The palette for a dark or light terminal that shows `colors`.
    pub fn terminal_with(dark: bool, colors: Colors) -> Theme {
        let dim = if dark { Color::DarkGray } else { Color::Gray };
        let tint = if dark {
            Color::Indexed(236)
        } else {
            Color::Indexed(254)
        };
        let brand = brand_color(dark, colors).map_or(Style::new(), |c| Style::new().fg(c));
        let lit = Style::new().add_modifier(Modifier::UNDERLINED | Modifier::BOLD);
        let link_hover = brand_color(dark, colors).map_or(lit, |c| lit.fg(c));
        let state = |c: Color| {
            if colors == Colors::None {
                Style::new()
            } else {
                Style::new().fg(c)
            }
        };
        Theme {
            accent: Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            dim: Style::new().fg(dim),
            user: Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
            error: Style::new().fg(Color::Red),
            ok: Style::new().fg(Color::Green),
            warn: Style::new().fg(Color::Yellow),
            user_tint: Style::new().bg(tint),
            rule: Style::new().add_modifier(Modifier::BOLD),
            selection: Style::new().add_modifier(Modifier::REVERSED),
            brand,
            link_hover,
            pr_open: state(Color::Green),
            pr_draft: state(dim),
            pr_merged: state(Color::Magenta),
            pr_closed: state(Color::Red),
            colors,
            icons: IconSet::Text,
        }
    }

    /// The style of a pull request in `state`.
    pub fn pr(&self, state: PrState) -> Style {
        match state {
            PrState::Open => self.pr_open,
            PrState::Draft => self.pr_draft,
            PrState::Merged => self.pr_merged,
            PrState::Closed => self.pr_closed,
        }
    }

    /// `style` for an icon's glyph: as it is, or without color under `NO_COLOR`.
    pub fn icon(&self, style: Style) -> Style {
        if self.colors == Colors::None {
            Style::new()
        } else {
            style
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn colors_follow_no_color_then_colorterm() {
        assert_eq!(Colors::detect(env(&[])), Colors::Ansi256);
        assert_eq!(
            Colors::detect(env(&[("COLORTERM", "truecolor")])),
            Colors::TrueColor
        );
        assert_eq!(
            Colors::detect(env(&[("COLORTERM", "24bit")])),
            Colors::TrueColor
        );
        assert_eq!(
            Colors::detect(env(&[("NO_COLOR", "1"), ("COLORTERM", "truecolor")])),
            Colors::None
        );
        assert_eq!(
            Colors::detect(env(&[("NO_COLOR", "")])),
            Colors::Ansi256,
            "an empty NO_COLOR does not count"
        );
    }

    #[test]
    fn the_link_hover_is_the_brand_accent_bold_and_underlined() {
        let lit = Modifier::UNDERLINED | Modifier::BOLD;
        assert_eq!(
            Theme::terminal_with(true, Colors::Ansi256).link_hover,
            Style::new().fg(Color::Indexed(38)).add_modifier(lit)
        );
        assert_eq!(
            Theme::terminal_with(true, Colors::None).link_hover,
            Style::new().add_modifier(lit),
            "without color it is still bold and underlined"
        );
    }

    #[test]
    fn the_brand_accent_is_sky_in_true_color_and_its_nearest_index_otherwise() {
        let brand = |dark, colors| Theme::terminal_with(dark, colors).brand;
        assert_eq!(
            brand(true, Colors::TrueColor),
            Style::new().fg(Color::Rgb(0x0e, 0xa5, 0xe9))
        );
        assert_eq!(
            brand(false, Colors::TrueColor),
            Style::new().fg(Color::Rgb(0x02, 0x84, 0xc7))
        );
        assert_eq!(
            brand(true, Colors::Ansi256),
            Style::new().fg(Color::Indexed(38))
        );
        assert_eq!(
            brand(false, Colors::Ansi256),
            Style::new().fg(Color::Indexed(32))
        );
        assert_eq!(brand(true, Colors::None), Style::new());
    }

    #[test]
    fn a_new_theme_draws_text_icons_and_an_icon_is_plain_under_no_color() {
        let t = Theme::terminal_with(true, Colors::Ansi256);
        assert_eq!(
            t.icons,
            IconSet::Text,
            "a theme draws text until the Tui picks the icons"
        );
        assert_eq!(t.colors, Colors::Ansi256);
        assert_eq!(t.icon(t.accent), t.accent);
        let plain = Theme::terminal_with(true, Colors::None);
        assert_eq!(plain.icon(plain.accent), Style::new());
        assert_eq!(plain.icon(plain.error), Style::new());
    }

    #[test]
    fn pull_request_states_take_the_web_uis_colors_and_none_under_no_color() {
        use scuttle_core::chat_list::PrState;
        let t = Theme::terminal_with(true, Colors::Ansi256);
        assert_eq!(t.pr(PrState::Open).fg, Some(Color::Green));
        assert_eq!(t.pr(PrState::Draft).fg, Some(Color::DarkGray));
        assert_eq!(t.pr(PrState::Merged).fg, Some(Color::Magenta));
        assert_eq!(t.pr(PrState::Closed).fg, Some(Color::Red));
        let plain = Theme::terminal_with(true, Colors::None);
        for state in [
            PrState::Open,
            PrState::Draft,
            PrState::Merged,
            PrState::Closed,
        ] {
            assert_eq!(plain.pr(state), Style::new(), "{state:?}");
        }
    }
}
