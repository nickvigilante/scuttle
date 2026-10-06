//! Which transcript links may open in a browser.

/// `url` parsed, when it is an `http` or `https` link, the only kind a transcript link may
/// open. Anything else, such as a file, a `javascript:` URL, or text that starts with `-` and
/// would reach the opener as an option, could run something other than a browser.
pub fn web_link(url: &str) -> Option<url::Url> {
    url::Url::parse(url)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
}
