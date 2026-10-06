//! Fuzzy ranking for list filters, over nucleo-matcher.

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// The `items` whose `text` matches `query`, best first, with equal scores in their input
/// order. Each whitespace-separated word of the query must appear in the text as a
/// subsequence, ignoring case; every character is literal, so `!`, `^`, `$`, `'`, and `\`
/// carry no pattern meaning. An empty query keeps every item.
pub fn rank<T>(query: &str, items: Vec<T>, text: impl Fn(&T) -> String) -> Vec<T> {
    if query.trim().is_empty() {
        return items;
    }
    let mut pattern = Pattern::default();
    pattern.atoms = query
        .split_whitespace()
        .map(|word| {
            Atom::new(
                word,
                CaseMatching::Ignore,
                Normalization::Smart,
                AtomKind::Fuzzy,
                false,
            )
        })
        .collect();
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, T)> = items
        .into_iter()
        .filter_map(|item| {
            let haystack = text(&item);
            let score = pattern.score(Utf32Str::new(&haystack, &mut buf), &mut matcher)?;
            Some((score, item))
        })
        .collect();
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored.into_iter().map(|(_, item)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TITLES: [&str; 3] = [
        "Fix the flaky watch reconnect test",
        "Draft the M2 design",
        "Old reconnect spike",
    ];

    fn ranked(query: &str) -> Vec<&'static str> {
        rank(query, TITLES.to_vec(), |t| t.to_string())
    }

    #[test]
    fn an_empty_query_keeps_every_item_in_order() {
        assert_eq!(ranked(""), TITLES);
        assert_eq!(ranked("  "), TITLES);
    }

    #[test]
    fn every_word_must_match_and_case_is_ignored() {
        assert_eq!(ranked("fix watch"), ["Fix the flaky watch reconnect test"]);
        assert_eq!(ranked("DRAFT"), ["Draft the M2 design"]);
        let reconnect = ranked("reconnect");
        assert_eq!(reconnect.len(), 2);
        assert!(ranked("zzz").is_empty());
    }

    #[test]
    fn query_characters_are_plain_text_not_pattern_syntax() {
        let titles = vec!["!urgent: fix login", "^caret parsing", "a$b", "plain title"];
        let ranked = |q: &str| rank(q, titles.clone(), |t| t.to_string());
        assert_eq!(ranked("!urgent"), ["!urgent: fix login"]);
        assert_eq!(ranked("^car"), ["^caret parsing"]);
        assert_eq!(
            ranked("!"),
            ["!urgent: fix login"],
            "a lone ! is a character"
        );
        assert_eq!(ranked("^"), ["^caret parsing"]);
        assert_eq!(ranked("$"), ["a$b"]);
        assert!(
            ranked("'").is_empty(),
            "a lone ' matches only a title with one"
        );
    }
}
