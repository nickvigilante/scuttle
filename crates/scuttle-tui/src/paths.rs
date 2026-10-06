//! Local file paths for `@path`: completion and the files a message names.

use std::path::{Path, PathBuf};

/// `partial` with a leading `~/` resolved against `home`.
pub(crate) fn expand(partial: &str, home: Option<&Path>) -> PathBuf {
    match (partial.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(partial),
    }
}

/// Completes `partial` to the longest prefix every matching entry shares, with a slash after
/// a single matching directory. `None` when nothing matches. A `~/` prefix is kept as typed.
/// The scan is capped by [`MAX_SCANNED`] and [`MAX_MATCHES`]; a scan cut short by either
/// returns `partial` as typed, since entries it never read may share less of the stem. It
/// leaves out dotfiles unless the typed name starts with `.`.
pub fn complete(partial: &str, home: Option<&Path>) -> Option<String> {
    complete_within(partial, home, MAX_SCANNED, MAX_MATCHES)
}

/// Directory entries `complete` reads before it stops, so Tab in a huge directory never
/// stalls the key loop.
const MAX_SCANNED: usize = 2000;
/// Matching entries `complete` collects before it stops.
const MAX_MATCHES: usize = 200;

/// `complete` with the scan stopping after `max_scanned` entries or `max_matches` matches,
/// keeping `partial` as typed when it stops early. Dotfiles match only a stem that starts
/// with `.`.
fn complete_within(
    partial: &str,
    home: Option<&Path>,
    max_scanned: usize,
    max_matches: usize,
) -> Option<String> {
    let (dir_part, stem) = match partial.rfind('/') {
        Some(i) => (&partial[..=i], &partial[i + 1..]),
        None => ("", partial),
    };
    let dir = if dir_part.is_empty() {
        PathBuf::from(".")
    } else {
        expand(dir_part, home)
    };
    let hidden_ok = stem.starts_with('.');
    let mut names: Vec<(String, bool)> = Vec::new();
    // One entry past a cap shows that the scan would have been cut short.
    for (scanned, entry) in std::fs::read_dir(&dir).ok()?.enumerate() {
        if scanned == max_scanned {
            return Some(partial.to_owned());
        }
        let Ok(entry) = entry else { continue };
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !name.starts_with(stem) || (name.starts_with('.') && !hidden_ok) {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if names.len() == max_matches {
            return Some(partial.to_owned());
        }
        names.push((name, kind.is_dir()));
    }
    names.sort();
    let first = names.first()?.0.clone();
    let shared = names.iter().fold(first, |acc, (name, _)| {
        acc.chars()
            .zip(name.chars())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a)
            .collect()
    });
    let slash = if names.len() == 1 && names[0].1 {
        "/"
    } else {
        ""
    };
    Some(format!("{dir_part}{shared}{slash}"))
}

/// Punctuation that can follow a mention in prose, as in "see @main.rs." or "(@a.rs, @b.rs)".
const TRAILING: &[char] = &[',', '.', ';', ':', '!', '?', ')'];

/// The `@path` tokens in `text` that name existing files, resolved, in order. A token that
/// names no file is tried again without the punctuation that ends it. An `@` inside a word,
/// as in an email address, is not a path.
pub fn at_paths(text: &str, home: Option<&Path>) -> Vec<String> {
    text.split_whitespace()
        .filter_map(|word| word.strip_prefix('@'))
        .filter_map(|p| {
            [p, p.trim_end_matches(TRAILING)]
                .into_iter()
                .filter(|p| !p.is_empty())
                .map(|p| expand(p, home))
                .find(|p| p.is_file())
        })
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

/// The path parts in `text` that name a dotfile or dot-directory, such as `.env` in
/// `@~/.env,`, without the punctuation that may follow them. `.` and `..` are not dotfiles.
pub fn dot_parts(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| c.is_whitespace() || c == '/' || c == '@')
        .map(|part| part.trim_end_matches(TRAILING))
        .filter(|part| part.starts_with('.') && *part != "." && *part != "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temporary directory, removed when the guard drops, even after a failed assertion.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("scuttle-paths-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tree() -> TempDir {
        let tmp = TempDir::new();
        let dir = &tmp.0;
        std::fs::create_dir_all(dir.join("docs/specs")).unwrap();
        std::fs::write(dir.join("docs/design.md"), "x").unwrap();
        std::fs::write(dir.join("docs/deploy.md"), "x").unwrap();
        tmp
    }

    #[test]
    fn completion_extends_to_the_shared_prefix_and_marks_a_lone_directory() {
        let tmp = tree();
        let dir = tmp.0.clone();
        let base = dir.to_string_lossy();
        assert_eq!(
            complete(&format!("{base}/do"), None),
            Some(format!("{base}/docs/"))
        );
        assert_eq!(
            complete(&format!("{base}/docs/de"), None),
            Some(format!("{base}/docs/de"))
        );
        assert_eq!(
            complete(&format!("{base}/docs/des"), None),
            Some(format!("{base}/docs/design.md"))
        );
        assert_eq!(complete(&format!("{base}/nothing"), None), None);
        assert_eq!(complete("~/do", Some(&dir)), Some("~/docs/".into()));
    }

    #[test]
    fn at_tokens_that_name_files_are_attached() {
        let tmp = tree();
        let dir = tmp.0.clone();
        let base = dir.to_string_lossy();
        let text = format!(
            "compare @{base}/docs/design.md with @{base}/docs/gone.md and mail@example.com"
        );
        assert_eq!(at_paths(&text, None), [format!("{base}/docs/design.md")]);
        assert_eq!(
            at_paths("see @~/docs/deploy.md", Some(&dir)),
            [format!("{}/docs/deploy.md", base)]
        );
    }

    #[test]
    fn the_tree_is_removed_when_its_guard_drops() {
        let tmp = tree();
        let dir = tmp.0.clone();
        assert!(dir.is_dir());
        drop(tmp);
        assert!(!dir.exists());
    }

    #[test]
    fn dotfiles_match_only_a_stem_that_starts_with_a_dot() {
        let tmp = TempDir::new();
        let base = tmp.0.to_string_lossy().into_owned();
        std::fs::write(tmp.0.join(".env"), "x").unwrap();
        std::fs::write(tmp.0.join("notes.md"), "x").unwrap();
        assert_eq!(
            complete(&format!("{base}/"), None),
            Some(format!("{base}/notes.md"))
        );
        assert_eq!(
            complete(&format!("{base}/."), None),
            Some(format!("{base}/.env"))
        );
    }

    #[test]
    fn the_scan_stops_at_its_entry_and_match_caps() {
        let tmp = TempDir::new();
        let base = tmp.0.to_string_lossy().into_owned();
        let names = ["log1.txt", "log2.txt", "log3.txt"];
        for name in names {
            std::fs::write(tmp.0.join(name), "x").unwrap();
        }
        let partial = format!("{base}/lo");
        let uncapped = complete_within(&partial, None, MAX_SCANNED, MAX_MATCHES);
        assert_eq!(uncapped, Some(format!("{base}/log")));
        assert_eq!(
            complete_within(&partial, None, 1, MAX_MATCHES),
            Some(partial.clone()),
            "a scan cut short at its entry cap keeps the typed stem"
        );
        assert_eq!(
            complete_within(&partial, None, MAX_SCANNED, 1),
            Some(partial.clone()),
            "a scan cut short at its match cap keeps the typed stem"
        );
        assert_eq!(
            complete_within(&partial, None, 3, 3),
            Some(format!("{base}/log")),
            "caps that every entry fits within cut nothing short"
        );
    }

    #[test]
    fn a_token_followed_by_punctuation_names_the_file_before_it() {
        let tmp = tree();
        let base = tmp.0.to_string_lossy().into_owned();
        let text = format!(
            "check @{base}/docs/design.md. Then @{base}/docs/deploy.md, (see @{base}/docs/design.md)!?"
        );
        assert_eq!(
            at_paths(&text, None),
            [
                format!("{base}/docs/design.md"),
                format!("{base}/docs/deploy.md"),
                format!("{base}/docs/design.md"),
            ]
        );
    }
}
