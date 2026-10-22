//! The find and replace bar (`meld/ui/findbar.py`, `findbar.ui`).
//!
//! Searches the active pane with GtkSourceSearchSettings' options: case
//! sensitivity, whole words, regular expressions, and always wrapping
//! around. Like GtkSourceSearchContext, regular expressions see the text
//! with its line breaks, so `\n` or `\s` can match across lines; plain
//! text (from a one-line entry) is searched line by line. All matches
//! around the view are highlighted in every pane ([`matches_in`]).

use std::ops::Range;

use meld_widgets::{InlineHighlight, Position};
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

pub fn build_regex(
    text: &str,
    match_case: bool,
    whole_word: bool,
    is_regex: bool,
) -> Option<Regex> {
    if text.is_empty() {
        return None;
    }
    let mut pattern = if is_regex {
        text.to_owned()
    } else {
        regex::escape(text)
    };
    if whole_word {
        pattern = format!(r"\b(?:{pattern})\b");
    }
    regex::RegexBuilder::new(&pattern)
        .case_insensitive(!match_case)
        // `^` and `$` match at line breaks of the joined text.
        .multi_line(true)
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

/// Lines joined with `\n`, with the byte offset of each line.
struct Joined<'a> {
    lines: &'a [String],
    text: String,
    starts: Vec<usize>,
}

impl<'a> Joined<'a> {
    fn new(lines: &'a [String]) -> Self {
        let mut text = String::with_capacity(lines.iter().map(|l| l.len() + 1).sum());
        let mut starts = Vec::with_capacity(lines.len());
        for (i, line) in lines.iter().enumerate() {
            if i > 0 {
                text.push('\n');
            }
            starts.push(text.len());
            text.push_str(line);
        }
        Joined {
            lines,
            text,
            starts,
        }
    }

    fn offset(&self, p: Position) -> usize {
        let line = p.line.min(self.lines.len().saturating_sub(1));
        match self.lines.get(line) {
            Some(l) => self.starts[line] + col_to_byte(l, p.column),
            None => 0,
        }
    }

    fn position(&self, byte: usize) -> Position {
        let line = self
            .starts
            .partition_point(|&s| s <= byte)
            .saturating_sub(1);
        let text = self.lines.get(line).map_or("", String::as_str);
        let rel = (byte - self.starts.get(line).copied().unwrap_or(0)).min(text.len());
        Position::new(line, byte_to_col(text, rel))
    }

    /// Non-empty matches as byte ranges.
    fn matches<'b>(&'b self, re: &'b Regex) -> impl Iterator<Item = Range<usize>> + 'b {
        re.find_iter(&self.text)
            .filter(|m| m.start() != m.end())
            .map(|m| m.range())
    }
}

/// The first match at or after `from` (or, backwards, the last match
/// ending at or before `from`), wrapping around. Returns the match and
/// whether the search wrapped. `multiline` searches the joined text, so
/// matches may span lines.
pub fn search(
    re: &Regex,
    lines: &[String],
    from: Position,
    backwards: bool,
    multiline: bool,
) -> Option<(Match, bool)> {
    if multiline {
        return search_joined(re, lines, from, backwards);
    }
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

fn search_joined(
    re: &Regex,
    lines: &[String],
    from: Position,
    backwards: bool,
) -> Option<(Match, bool)> {
    if lines.is_empty() {
        return None;
    }
    let joined = Joined::new(lines);
    let at = joined.offset(from);
    let found = if !backwards {
        let mut first = None;
        let mut after = None;
        for m in joined.matches(re) {
            if m.start >= at {
                after = Some(m);
                break;
            }
            first.get_or_insert(m);
        }
        after
            .map(|m| (m, false))
            .or_else(|| first.map(|m| (m, true)))
    } else {
        let mut before = None;
        let mut last = None;
        for m in joined.matches(re) {
            if m.end <= at {
                before = Some(m);
            } else {
                last = Some(m);
            }
        }
        before
            .map(|m| (m, false))
            .or_else(|| last.map(|m| (m, true)))
    };
    found.map(|(m, wrapped)| ((joined.position(m.start), joined.position(m.end)), wrapped))
}

/// Whether the text has any match.
pub fn has_match(re: &Regex, lines: &[String], multiline: bool) -> bool {
    if multiline {
        return Joined::new(lines).matches(re).next().is_some();
    }
    lines
        .iter()
        .any(|l| re.find_iter(l).any(|m| m.start() != m.end()))
}

/// The matches in `lines[range]`, split per line, sorted.
pub fn matches_in(
    re: &Regex,
    lines: &[String],
    range: Range<usize>,
    multiline: bool,
) -> Vec<InlineHighlight> {
    let range = range.start.min(lines.len())..range.end.min(lines.len());
    let mut out = Vec::new();
    if !multiline {
        for line in range {
            for (s, e) in line_matches(re, &lines[line]) {
                out.push(InlineHighlight {
                    line,
                    columns: s..e,
                });
            }
        }
        return out;
    }
    let first = range.start;
    let joined = Joined::new(&lines[range]);
    for m in joined.matches(re) {
        let (s, e) = (joined.position(m.start), joined.position(m.end));
        for line in s.line..=e.line {
            let a = if line == s.line { s.column } else { 0 };
            let b = if line == e.line {
                e.column
            } else {
                joined.lines[line].chars().count()
            };
            if a < b {
                out.push(InlineHighlight {
                    line: first + line,
                    columns: a..b,
                });
            }
        }
    }
    out
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
#[cfg(test)]
pub fn slice(line: &str, start: usize, end: usize) -> &str {
    &line[col_to_byte(line, start)..col_to_byte(line, end)]
}

/// The result of replacing all matches.
#[derive(Debug, PartialEq, Eq)]
pub enum Replaced {
    /// Changed lines, as `(line index, new text)`.
    Lines(Vec<(usize, String)>),
    /// Some match spanned lines: the whole new text.
    Text(Vec<String>),
}

impl Replaced {
    pub fn is_empty(&self) -> bool {
        match self {
            Replaced::Lines(lines) => lines.is_empty(),
            Replaced::Text(_) => false,
        }
    }
}

/// Every match replaced: per line, or over the whole text when a regular
/// expression matches across lines.
pub fn replace_all(re: &Regex, lines: &[String], replacement: &str, is_regex: bool) -> Replaced {
    if is_regex {
        let joined = Joined::new(lines);
        if joined.matches(re).any(|m| joined.text[m].contains('\n')) {
            let text = re.replace_all(&joined.text, replacement);
            return Replaced::Text(text.split('\n').map(str::to_owned).collect());
        }
    }
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if !re.is_match(line) {
            continue;
        }
        let new = if is_regex {
            re.replace_all(line, replacement).into_owned()
        } else {
            re.replace_all(line, regex::NoExpand(replacement))
                .into_owned()
        };
        if &new != line {
            out.push((i, new));
        }
    }
    Replaced::Lines(out)
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
        for multiline in [false, true] {
            let ((s, e), wrapped) =
                search(&re, &text, Position::new(0, 1), false, multiline).unwrap();
            assert_eq!(
                (s, e, wrapped),
                (Position::new(1, 4), Position::new(1, 7), false)
            );
            let ((s, _), wrapped) =
                search(&re, &text, Position::new(2, 0), false, multiline).unwrap();
            assert_eq!((s, wrapped), (Position::new(0, 0), true));
            let ((s, _), wrapped) =
                search(&re, &text, Position::new(1, 4), true, multiline).unwrap();
            assert_eq!((s, wrapped), (Position::new(0, 0), false));
            let ((s, _), wrapped) =
                search(&re, &text, Position::new(0, 0), true, multiline).unwrap();
            assert_eq!((s, wrapped), (Position::new(1, 4), true));
        }
    }

    #[test]
    fn options() {
        let text = lines(&["Foo foobar"]);
        assert!(
            search(
                &build_regex("foo", true, false, false).unwrap(),
                &text,
                Position::default(),
                false,
                false
            )
            .is_some_and(|((s, _), _)| s.column == 4)
        );
        let re = build_regex("foo", false, true, false).unwrap();
        let ((s, e), _) = search(&re, &text, Position::new(0, 1), false, false).unwrap();
        assert_eq!((s.column, e.column), (0, 3));
        assert!(build_regex("(", false, false, true).is_none());
        assert!(build_regex("(", false, false, false).is_some());
        let re = build_regex(r"f(o+)", false, false, true).unwrap();
        assert_eq!(
            replace_all(&re, &text, "<$1>", true),
            Replaced::Lines(vec![(0, "<oo> <oo>bar".into())])
        );
        assert_eq!(
            replace_all(&re, &text, "$1", false),
            Replaced::Lines(vec![(0, "$1 $1bar".into())])
        );
    }

    #[test]
    fn unicode_columns() {
        let text = lines(&["äöü foo"]);
        let re = build_regex("foo", false, false, false).unwrap();
        let ((s, e), _) = search(&re, &text, Position::default(), false, false).unwrap();
        assert_eq!((s.column, e.column), (4, 7));
        assert_eq!(slice(&text[0], 4, 7), "foo");
    }

    #[test]
    fn regex_matches_across_lines() {
        let text = lines(&["let a = 1;", "let b = 2;", "äb = 3;"]);
        let re = build_regex(r";\nlet", false, false, true).unwrap();
        let ((s, e), wrapped) = search(&re, &text, Position::default(), false, true).unwrap();
        assert_eq!(
            (s, e, wrapped),
            (Position::new(0, 9), Position::new(1, 3), false)
        );
        assert!(has_match(&re, &text, true));
        assert!(!has_match(&re, &text, false));
        // `^` anchors at line starts of the joined text.
        let re = build_regex(r"^let", false, false, true).unwrap();
        let hl = matches_in(&re, &text, 0..3, true);
        assert_eq!(hl.len(), 2);
        assert_eq!((hl[1].line, hl[1].columns.clone()), (1, 0..3));
        // A match over a line break is highlighted on both lines.
        let re = build_regex(r"2;\s+ä", false, false, true).unwrap();
        let hl = matches_in(&re, &text, 1..3, true);
        assert_eq!(
            hl,
            vec![
                InlineHighlight {
                    line: 1,
                    columns: 8..10
                },
                InlineHighlight {
                    line: 2,
                    columns: 0..1
                },
            ]
        );
        // Replacing such matches rewrites the text.
        let re = build_regex(r";\n", false, false, true).unwrap();
        assert_eq!(
            replace_all(&re, &text, "; ", true),
            Replaced::Text(vec!["let a = 1; let b = 2; äb = 3;".into()])
        );
    }

    #[test]
    fn plain_matches_per_line() {
        let text = lines(&["a foo foo", "", "foo"]);
        let re = build_regex("foo", false, false, false).unwrap();
        let hl = matches_in(&re, &text, 0..10, false);
        let got: Vec<_> = hl.iter().map(|h| (h.line, h.columns.clone())).collect();
        assert_eq!(got, vec![(0, 2..5), (0, 6..9), (2, 0..3)]);
    }
}
