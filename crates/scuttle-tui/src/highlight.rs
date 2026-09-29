//! Syntax highlighting, loaded lazily on a background thread so startup is not delayed.

use std::sync::OnceLock;

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
use syntect::parsing::SyntaxSet;

struct Assets {
    syntaxes: SyntaxSet,
    theme: Theme,
}

static ASSETS: OnceLock<Assets> = OnceLock::new();

/// Starts loading syntaxes and the theme; safe to call more than once.
pub fn warm() {
    if ASSETS.get().is_some() {
        return;
    }
    std::thread::spawn(|| {
        let syntaxes = two_face::syntax::extra_newlines();
        let themes = two_face::theme::extra();
        let theme = themes
            .get(two_face::theme::EmbeddedThemeName::Base16OceanDark)
            .clone();
        let _ = ASSETS.set(Assets { syntaxes, theme });
    });
}

/// Highlighted lines, or `None` if assets are still loading or the language is unknown.
pub fn highlight(code: &str, lang: &str) -> Option<Vec<Line<'static>>> {
    let assets = ASSETS.get()?;
    let syntax = assets.syntaxes.find_syntax_by_token(lang)?;
    let mut h = HighlightLines::new(syntax, &assets.theme);
    let mut out = Vec::new();
    for line in syntect::util::LinesWithEndings::from(code) {
        let ranges = h.highlight_line(line, &assets.syntaxes).ok()?;
        let spans = ranges
            .into_iter()
            .map(|(style, text)| {
                let fg = style.foreground;
                Span::styled(
                    text.trim_end_matches('\n').to_owned(),
                    Style::new().fg(Color::Rgb(fg.r, fg.g, fg.b)),
                )
            })
            .collect::<Vec<_>>();
        out.push(Line::from(spans));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_known_languages_once_warm() {
        warm();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let lines = loop {
            if let Some(lines) = highlight("fn main() {}\n", "rust") {
                break lines;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "syntaxes never finished loading"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert_eq!(lines.len(), 1);
        assert!(highlight("x", "no-such-language-xyz").is_none());
    }
}
