//! The find and replace bar (`meld/ui/findbar.py`, `findbar.ui`).
//!
//! Searches the active pane line by line with GtkSourceSearchSettings'
//! options: case sensitivity, whole words, regular expressions, and
//! always wrapping around.

use meld_widgets::Position;
use regex::Regex;

#[derive(Debug, Clone, Default)]
pub struct FindBar {
    pub visible: bool,
    pub replace_mode: bool,
    pub text: String,
    pub replacement: String,
    pub match_case: bool,
    pub whole_word: bool,
    pub regex: bool,
    /// The last search wrapped around the end of the text.
    pub wrapped: bool,
    /// The search text has no match in the active pane.
    pub no_match: bool,
    /// The pattern is not a valid regular expression.
    pub invalid: bool,
}

#[derive(Debug, Clone)]
pub enum FindMessage {
    Text(String),
    Replacement(String),
    MatchCase(bool),
    WholeWord(bool),
    Regex(bool),
    Next,
    Previous,
    Replace,
    ReplaceAll,
    ToggleReplace,
    Close,
}

/// A match: start and end positions (character columns).
pub type Match = (Position, Position);

impl FindBar {
    /// The compiled search, or `None` for an empty or invalid pattern.
    pub fn compile(&self) -> Option<Regex> {
        build_regex(&self.text, self.match_case, self.whole_word, self.regex)
    }
}

pub fn build_regex(text: &str, match_case: bool, whole_word: bool, is_regex: bool) -> Option<Regex> {
    if text.is_empty() {
        return None;
    }
    let mut pattern = if is_regex { text.to_owned() } else { regex::escape(text) };
    if whole_word {
        pattern = format!(r"\b(?:{pattern})\b");
    }
    regex::RegexBuilder::new(&pattern)
        .case_insensitive(!match_case)
        .build()
        .ok()
}

fn byte_to_col(line: &str, byte: usize) -> usize {
    line[..byte].chars().count()
}

fn col_to_byte(line: &str, col: usize) -> usize {
    line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
}

/// All non-empty matches in `line`.
fn line_matches(re: &Regex, line: &str) -> Vec<(usize, usize)> {
    re.find_iter(line)
        .filter(|m| m.start() != m.end())
        .map(|m| (byte_to_col(line, m.start()), byte_to_col(line, m.end())))
        .collect()
}

/// The first match at or after `from` (or, backwards, the last match
/// ending at or before `from`), wrapping around. Returns the match and
/// whether the search wrapped.
pub fn search(re: &Regex, lines: &[String], from: Position, backwards: bool) -> Option<(Match, bool)> {
    let n = lines.len();
    if n == 0 {
        return None;
    }
    let from_line = from.line.min(n - 1);
    if !backwards {
        for step in 0..=n {
            let l = (from_line + step) % n;
            let wrapped = from_line + step >= n;
            for (s, e) in line_matches(re, &lines[l]) {
                if step == 0 && s < from.column {
                    continue;
                }
                if step == n && s >= from.column {
                    break;
                }
                return Some(((Position::new(l, s), Position::new(l, e)), wrapped));
            }
        }
    } else {
        for step in 0..=n {
            let l = (from_line + n * 2 - step) % n;
            let wrapped = step > from_line;
            for (s, e) in line_matches(re, &lines[l]).into_iter().rev() {
                if step == 0 && e > from.column {
                    continue;
                }
                if step == n && e <= from.column {
                    break;
                }
                return Some(((Position::new(l, s), Position::new(l, e)), wrapped));
            }
        }
    }
    None
}

/// Whether the text has any match.
pub fn has_match(re: &Regex, lines: &[String]) -> bool {
    lines.iter().any(|l| re.find_iter(l).any(|m| m.start() != m.end()))
}

/// The replacement text for a match (with `$1` expansion in regex mode).
pub fn expand_replacement(re: &Regex, matched: &str, replacement: &str, is_regex: bool) -> String {
    if !is_regex {
        return replacement.to_owned();
    }
    match re.captures(matched) {
        Some(caps) => {
            let mut out = String::new();
            caps.expand(replacement, &mut out);
            out
        }
        None => replacement.to_owned(),
    }
}

/// The text of `line` between two columns.
pub fn slice(line: &str, start: usize, end: usize) -> &str {
    &line[col_to_byte(line, start)..col_to_byte(line, end)]
}

/// Every line with all matches replaced, for lines that changed, as
/// `(line index, new text)`.
pub fn replace_all(re: &Regex, lines: &[String], replacement: &str, is_regex: bool) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if !re.is_match(line) {
            continue;
        }
        let new = if is_regex {
            re.replace_all(line, replacement).into_owned()
        } else {
            re.replace_all(line, regex::NoExpand(replacement)).into_owned()
        };
        if &new != line {
            out.push((i, new));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &[&str]) -> Vec<String> {
        s.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn forward_and_backward_with_wrap() {
        let text = lines(&["foo bar", "baz foo", "qux"]);
        let re = build_regex("foo", false, false, false).unwrap();
        let ((s, e), wrapped) = search(&re, &text, Position::new(0, 1), false).unwrap();
        assert_eq!((s, e, wrapped), (Position::new(1, 4), Position::new(1, 7), false));
        let ((s, _), wrapped) = search(&re, &text, Position::new(2, 0), false).unwrap();
        assert_eq!((s, wrapped), (Position::new(0, 0), true));
        let ((s, _), wrapped) = search(&re, &text, Position::new(1, 4), true).unwrap();
        assert_eq!((s, wrapped), (Position::new(0, 0), false));
        let ((s, _), wrapped) = search(&re, &text, Position::new(0, 0), true).unwrap();
        assert_eq!((s, wrapped), (Position::new(1, 4), true));
    }

    #[test]
    fn options() {
        let text = lines(&["Foo foobar"]);
        assert!(
            search(
                &build_regex("foo", true, false, false).unwrap(),
                &text,
                Position::default(),
                false
            )
            .is_some_and(|((s, _), _)| s.column == 4)
        );
        let re = build_regex("foo", false, true, false).unwrap();
        let ((s, e), _) = search(&re, &text, Position::new(0, 1), false).unwrap();
        assert_eq!((s.column, e.column), (0, 3));
        assert!(build_regex("(", false, false, true).is_none());
        assert!(build_regex("(", false, false, false).is_some());
        let re = build_regex(r"f(o+)", false, false, true).unwrap();
        assert_eq!(replace_all(&re, &text, "<$1>", true), vec![(0, "<oo> <oo>bar".into())]);
        assert_eq!(replace_all(&re, &text, "$1", false), vec![(0, "$1 $1bar".into())]);
    }

    #[test]
    fn unicode_columns() {
        let text = lines(&["äöü foo"]);
        let re = build_regex("foo", false, false, false).unwrap();
        let ((s, e), _) = search(&re, &text, Position::default(), false).unwrap();
        assert_eq!((s.column, e.column), (4, 7));
        assert_eq!(slice(&text[0], 4, 7), "foo");
    }
}
