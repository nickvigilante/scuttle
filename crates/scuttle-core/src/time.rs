//! Times as the overlays show them.

/// The time from `then_unix` to `now_unix` in one short unit: `now`, `4m`, `1h`, or `3d`.
pub fn relative(then_unix: i64, now_unix: i64) -> String {
    let secs = now_unix.saturating_sub(then_unix);
    match secs {
        ..60 => "now".into(),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// `when` at `offset`, as `2026-09-30 14:05`.
pub fn local(when: chrono::DateTime<chrono::Utc>, offset: chrono::FixedOffset) -> String {
    when.with_timezone(&offset)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

/// `now` or `4m ago`.
pub fn ago(then_unix: i64, now_unix: i64) -> String {
    match relative(then_unix, now_unix).as_str() {
        "now" => "now".into(),
        short => format!("{short} ago"),
    }
}

/// `in 4m` until `when_unix`, or `now` once it is less than a minute away or past.
pub fn until(when_unix: i64, now_unix: i64) -> String {
    match relative(now_unix, when_unix).as_str() {
        "now" => "now".into(),
        short => format!("in {short}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_times_are_short() {
        let now = 1_000_000;
        assert_eq!(relative(now - 20, now), "now");
        assert_eq!(relative(now - 4 * 60, now), "4m");
        assert_eq!(relative(now - 3600, now), "1h");
        assert_eq!(relative(now - 3 * 86_400, now), "3d");
        assert_eq!(
            relative(now + 90, now),
            "now",
            "a clock ahead of ours is now"
        );
    }

    #[test]
    fn local_times_have_a_date_and_a_minute() {
        let offset = chrono::FixedOffset::east_opt(2 * 3600).unwrap();
        let shown = local("2026-09-30T14:05:00Z".parse().unwrap(), offset);
        assert_eq!(shown.len(), 16, "{shown}");
        assert_eq!(&shown[4..5], "-");
        assert_eq!(&shown[13..14], ":");
        assert_eq!(shown, "2026-09-30 16:05", "the offset applies");
        assert_eq!(ago(100, 100), "now");
        assert_eq!(ago(100, 400), "5m ago");
    }

    #[test]
    fn a_time_ahead_reads_as_how_long_until_it() {
        let now = 1_000_000;
        assert_eq!(until(now + 240, now), "in 4m");
        assert_eq!(until(now + 29 * 86_400 + 3600, now), "in 29d");
        assert_eq!(until(now + 20, now), "now");
        assert_eq!(until(now - 5, now), "now", "a time already past is now");
    }
}
