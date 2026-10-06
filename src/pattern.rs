// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this file,
// You can obtain one at http://mozilla.org/MPL/2.0/.

//! Testcase patterns, as VUnit matches them with Python's `fnmatch`.
//!
//! Syntax: `*` matches any text including dots, `?` matches one character, `[seq]` matches a
//! character in `seq`, and `[!seq]` matches a character not in `seq`. Ranges like `a-z` are
//! allowed in sequences. A `[` without a closing `]` is a literal.
//!
//! Matching is case-insensitive on all platforms: pattern and name are compared after ASCII
//! lowercasing, as Python's `fnmatch` does on Windows.
//!
//! AI NOTICE: Generated, minimally reviewed.

use std::collections::BTreeMap;

/// A compiled testcase pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    tokens: Vec<Token>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Literal(char),
    AnyChar,
    AnyText,
    Set { negated: bool, items: Vec<SetItem> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetItem {
    Char(char),
    Range(char, char),
}

impl Token {
    fn matches(&self, ch: char) -> bool {
        match self {
            Self::Literal(literal) => *literal == ch,
            Self::AnyChar => true,
            Self::AnyText => false,
            Self::Set { negated, items } => {
                let found = items.iter().any(|item| match *item {
                    SetItem::Char(member) => member == ch,
                    SetItem::Range(first, last) => (first..=last).contains(&ch),
                });
                found != *negated
            },
        }
    }
}

impl Pattern {
    /// Compiles a pattern.
    ///
    /// Every string is a valid pattern.
    pub fn new(pattern: &str) -> Self {
        let chars: Vec<char> = pattern.to_ascii_lowercase().chars().collect();
        let mut tokens = Vec::new();
        let mut index = 0;
        while let Some(&ch) = chars.get(index) {
            index += 1;
            match ch {
                '*' => {
                    if tokens.last() != Some(&Token::AnyText) {
                        tokens.push(Token::AnyText);
                    }
                },
                '?' => tokens.push(Token::AnyChar),
                '[' => match parse_set(&chars, index) {
                    Some((token, next)) => {
                        tokens.push(token);
                        index = next;
                    },
                    None => tokens.push(Token::Literal('[')),
                },
                _ => tokens.push(Token::Literal(ch)),
            }
        }
        Self { tokens }
    }

    /// Whether `name` matches the whole pattern.
    pub fn matches(&self, name: &str) -> bool {
        let name: Vec<char> = name.to_ascii_lowercase().chars().collect();
        // Classic wildcard matching: on a mismatch, let the last `*` consume one more character.
        let mut token_index = 0;
        let mut name_index = 0;
        let mut backtrack: Option<(usize, usize)> = None;
        while name_index < name.len() {
            match self.tokens.get(token_index) {
                Some(Token::AnyText) => {
                    token_index += 1;
                    backtrack = Some((token_index, name_index));
                },
                Some(token) if token.matches(name[name_index]) => {
                    token_index += 1;
                    name_index += 1;
                },
                _ => match backtrack {
                    Some((after_star, consumed)) => {
                        token_index = after_star;
                        name_index = consumed + 1;
                        backtrack = Some((after_star, consumed + 1));
                    },
                    None => return false,
                },
            }
        }
        self.tokens[token_index..]
            .iter()
            .all(|token| *token == Token::AnyText)
    }
}

/// Parses a set starting after its `[`, as `fnmatch.translate` does. Returns `None` if the set
/// isn't closed, and the token and the index after the `]` otherwise.
fn parse_set(chars: &[char], start: usize) -> Option<(Token, usize)> {
    let mut end = start;
    if chars.get(end) == Some(&'!') {
        end += 1;
    }
    // A `]` right after `[` or `[!` is part of the set.
    if chars.get(end) == Some(&']') {
        end += 1;
    }
    while chars.get(end).is_some_and(|&ch| ch != ']') {
        end += 1;
    }
    if end >= chars.len() {
        return None;
    }

    let mut contents = &chars[start..end];
    let negated = contents.first() == Some(&'!');
    if negated {
        contents = &contents[1..];
    }
    let mut items = Vec::new();
    let mut index = 0;
    while let Some(&ch) = contents.get(index) {
        if contents.get(index + 1) == Some(&'-')
            && let Some(&last) = contents.get(index + 2)
        {
            // A reversed range matches nothing, as in Python.
            if ch <= last {
                items.push(SetItem::Range(ch, last));
            }
            index += 3;
        } else {
            items.push(SetItem::Char(ch));
            index += 1;
        }
    }
    Some((Token::Set { negated, items }, end + 1))
}

/// The result of matching simulation requests against the testcases.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolution {
    /// The matched testcases, sorted by name, and whether to run them in GUI mode.
    pub entries: Vec<(String, bool)>,
    /// The patterns that matched no testcase, in request order.
    pub unmatched: Vec<String>,
    /// The GUI-mode patterns that matched several testcases, with the number of matches, in
    /// request order. GUI mode needs exactly one testcase, so their matches run without it.
    pub ambiguous_gui: Vec<(String, usize)>,
}

/// Matches `(pattern, gui)` requests against testcase names.
///
/// A testcase matched by several requests runs once, in GUI mode if any of them asks for it.
/// A GUI-mode request that matches several testcases runs them without GUI mode: every paused
/// simulation holds a simulation permit, so a wildcard would block all other simulations.
pub fn resolve(
    requests: impl IntoIterator<Item = (impl AsRef<str>, bool)>,
    names: &[&str],
) -> Resolution {
    let mut entries: BTreeMap<&str, bool> = BTreeMap::new();
    let mut unmatched = Vec::new();
    let mut ambiguous_gui = Vec::new();
    for (pattern_text, gui) in requests {
        let pattern = Pattern::new(pattern_text.as_ref());
        let matched: Vec<&str> = names
            .iter()
            .copied()
            .filter(|name| pattern.matches(name))
            .collect();
        if matched.is_empty() {
            unmatched.push(pattern_text.as_ref().to_owned());
        }
        let gui = if gui && matched.len() > 1 {
            ambiguous_gui.push((pattern_text.as_ref().to_owned(), matched.len()));
            false
        } else {
            gui
        };
        for name in matched {
            *entries.entry(name).or_default() |= gui;
        }
    }
    Resolution {
        entries: entries
            .into_iter()
            .map(|(name, gui)| (name.to_owned(), gui))
            .collect(),
        unmatched,
        ambiguous_gui,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, name: &str) -> bool {
        Pattern::new(pattern).matches(name)
    }

    #[test]
    fn star_matches_any_text_including_dots() {
        assert!(matches("*", ""));
        assert!(matches("*", "lib.tb.test"));
        assert!(matches("lib.*", "lib.tb.test"));
        assert!(matches("lib.tb*", "lib.tb_a.test 1"));
        assert!(matches("*.test", "lib.tb.test"));
        assert!(matches("*tb*test*", "lib.tb.a.test"));
        assert!(matches("a**b", "ab"));
        assert!(!matches("lib.*", "other.tb.test"));
        assert!(!matches("*.test", "lib.tb.test2"));
        assert!(matches("*a*a*a*b", "aaaaaaaaaaaaaaaaaaaaaab"));
        assert!(!matches("*a*a*a*b", "aaaaaaaaaaaaaaaaaaaaaaa"));
    }

    #[test]
    fn question_mark_matches_one_character() {
        assert!(matches("lib.tb.test?", "lib.tb.test1"));
        assert!(matches("lib.tb.test?", "lib.tb.test."));
        assert!(!matches("lib.tb.test?", "lib.tb.test"));
        assert!(!matches("lib.tb.test?", "lib.tb.test12"));
    }

    #[test]
    fn sets() {
        assert!(matches("test[12]", "test1"));
        assert!(!matches("test[12]", "test3"));
        assert!(matches("test[!12]", "test3"));
        assert!(!matches("test[!12]", "test1"));
        assert!(matches("test[0-9]", "test7"));
        assert!(!matches("test[0-9]", "testx"));
        assert!(matches("test[a-]", "test-"));
        assert!(matches("test[]]", "test]"));
        assert!(matches("test[!]]", "testx"));
        assert!(!matches("test[!]]", "test]"));
        // A reversed range matches nothing.
        assert!(!matches("test[z-a]", "testm"));
        // An unclosed set is a literal `[`.
        assert!(matches("test[1", "test[1"));
        assert!(matches("test[!", "test[!"));
        // A backslash has no special meaning.
        assert!(matches(r"a\b", r"a\b"));
        assert!(matches(r"a[\]", r"a\"));
    }

    #[test]
    fn matching_ignores_ascii_case() {
        assert!(matches("LIB.TB_*", "lib.tb_Example.Test 1"));
        assert!(matches("lib.tb.[A-C]", "lib.tb.b"));
        assert!(matches("lib.tb.[a-c]", "lib.tb.B"));
        // Non-ASCII characters keep their case.
        assert!(!matches("lib.tb.Ä", "lib.tb.ä"));
    }

    #[test]
    fn resolve_merges_requests() {
        let names = ["lib.tb_b.test", "lib.tb_a.t2", "lib.tb_a.t1"];
        let resolution = resolve(
            [("lib.tb_a.*", false), ("*.t1", true), ("nothing*", true)],
            &names,
        );
        assert_eq!(
            resolution.entries,
            [
                ("lib.tb_a.t1".to_owned(), true),
                ("lib.tb_a.t2".to_owned(), false)
            ]
        );
        assert_eq!(resolution.unmatched, ["nothing*"]);
        assert!(resolution.ambiguous_gui.is_empty());
    }

    #[test]
    fn resolve_runs_ambiguous_gui_requests_without_gui() {
        let names = ["lib.tb.t1", "lib.tb.t2", "lib.tb.t3"];
        let resolution = resolve([("lib.tb.*", true), ("lib.tb.t3", true)], &names);
        assert_eq!(
            resolution.entries,
            [
                ("lib.tb.t1".to_owned(), false),
                ("lib.tb.t2".to_owned(), false),
                ("lib.tb.t3".to_owned(), true)
            ]
        );
        assert_eq!(resolution.ambiguous_gui, [("lib.tb.*".to_owned(), 3)]);
    }
}
