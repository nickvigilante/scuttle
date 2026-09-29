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
}

impl Theme {
    pub fn terminal(dark: bool) -> Theme {
        let dim = if dark { Color::DarkGray } else { Color::Gray };
        Theme {
            accent: Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            dim: Style::new().fg(dim),
            user: Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
            error: Style::new().fg(Color::Red),
            ok: Style::new().fg(Color::Green),
            warn: Style::new().fg(Color::Yellow),
        }
    }
}
