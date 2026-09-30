//! The terminal palette: ANSI colors, so the user's terminal color scheme applies.

use ratatui::style::{Color, Modifier, Style};

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
}

impl Theme {
    pub fn terminal(dark: bool) -> Theme {
        let dim = if dark { Color::DarkGray } else { Color::Gray };
        let tint = if dark {
            Color::Indexed(236)
        } else {
            Color::Indexed(254)
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
        }
    }
}
