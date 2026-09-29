//! Warns when the server is much newer than the SDK scuttle was built with.

fn major_minor(s: &str) -> Option<(u64, u64)> {
    let rest = s.trim().strip_prefix('v')?;
    let mut parts = rest.split(|c: char| !c.is_ascii_digit());
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// A warning when the server is more than one minor version ahead of `generated_from`.
pub fn skew_warning(server: &str, generated_from: &str) -> Option<String> {
    let (s_major, s_minor) = major_minor(server)?;
    let (g_major, g_minor) = major_minor(generated_from)?;
    let ahead = s_major > g_major || (s_major == g_major && s_minor > g_minor + 1);
    ahead.then(|| {
        format!(
            "Server {} is newer than scuttle's SDK ({}); some features may not work.",
            server.trim(),
            generated_from.trim()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warns_only_when_more_than_one_minor_ahead() {
        assert_eq!(skew_warning("v2.38.0+abc", "v2.37.3 (abc)"), None);
        assert!(skew_warning("v2.39.1", "v2.37.3 (abc)").is_some());
        assert!(skew_warning("v3.0.0", "v2.37.3 (abc)").is_some());
    }

    #[test]
    fn unparseable_versions_never_warn() {
        assert_eq!(skew_warning("v2.40.0", "local (d1597a583b)"), None);
        assert_eq!(skew_warning("devel", "v2.37.3 (abc)"), None);
    }
}
