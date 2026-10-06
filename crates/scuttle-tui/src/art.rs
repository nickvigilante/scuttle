//! The art the welcome screen shows when `welcome.art_file` names none, and its credit.

/// The Coder wordmark in ASCII, 28 columns wide, so it sits well in an 80-column terminal.
pub const CODER_WORDMARK: &str = r"  ____          _
 / ___|___   __| | ___ _ __
| |   / _ \ / _` |/ _ \ '__|
| |__| (_) | (_| |  __/ |
 \____\___/ \__,_|\___|_|";

/// The line under the art, since scuttle is not a Coder product.
pub const CREDIT: &str = "Unofficial. Not supported by Coder Technologies, Inc.";

/// The wordmark's lines, as `Welcome::art` holds them.
pub fn wordmark_lines() -> Vec<String> {
    CODER_WORDMARK.lines().map(str::to_owned).collect()
}

/// Whether `art` is the bundled wordmark, which alone draws in the brand accent.
pub fn is_wordmark(art: &[String]) -> bool {
    art.iter().map(String::as_str).eq(CODER_WORDMARK.lines())
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn the_wordmark_is_modest_and_has_no_trailing_spaces() {
        let lines = wordmark_lines();
        assert_eq!(lines.len(), 5);
        assert!(lines.iter().all(|l| l.width() <= 40), "{lines:#?}");
        assert!(lines.iter().all(|l| !l.ends_with(' ')), "{lines:#?}");
        assert!(CREDIT.width() <= 80);
    }
}
