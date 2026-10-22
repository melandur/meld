//! A plain text buffer with GtkTextBuffer line semantics, primitive edits,
//! and the filtered-line cache of `meld/meldbuffer.py:BufferLines`.
//!
//! Like a GtkTextBuffer, a [`TextBuffer`] always has at least one line, and
//! the last line never has a terminator: `"a\nb\n"` is the three lines
//! `["a", "b", ""]`. Those lines are exactly what the differ compares.
//! Line terminators (`\n`, `\r\n`, `\r`) are kept per line, so mixed line
//! endings survive edits.

use std::ops::Range;

use crate::chunk::py_slice;
use crate::differ::add_signed;

/// A position in a buffer: line, and byte offset within the line's text
/// (excluding its terminator).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Position {
    pub line: usize,
    pub col: usize,
}

impl Position {
    pub const fn new(line: usize, col: usize) -> Self {
        Position { line, col }
    }
}

/// A primitive buffer edit (the two GtkTextBuffer signals Meld reacts to).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TextEdit {
    Insert { at: Position, text: String },
    Delete { start: Position, end: Position },
}

/// The line-level effect of an edit, as passed to
/// [`crate::Differ::change_sequence`]: `size_change` lines were added
/// (positive) or removed (negative) starting at `start_line`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct LineChange {
    pub start_line: usize,
    pub size_change: isize,
}

/// An edit that was applied to a buffer, with its inverse (for undo) and
/// its line-level effect.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AppliedEdit {
    pub edit: TextEdit,
    pub inverse: TextEdit,
    pub change: LineChange,
}

const LF: &str = "\n";
const CRLF: &str = "\r\n";
const CR: &str = "\r";

/// Split text into `(line, terminator)` pairs on `\n`, `\r\n` and `\r`; the
/// last pair has an empty terminator (and may be an empty line).
fn split_terminated(text: &str) -> Vec<(&str, &'static str)> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                out.push((&text[start..i], LF));
                i += 1;
                start = i;
            }
            b'\r' => {
                if bytes.get(i + 1) == Some(&b'\n') {
                    out.push((&text[start..i], CRLF));
                    i += 2;
                } else {
                    out.push((&text[start..i], CR));
                    i += 1;
                }
                start = i;
            }
            _ => i += 1,
        }
    }
    out.push((&text[start..], ""));
    out
}

/// Python's `str.splitlines(keepends)`, including its full set of line
/// boundaries (`\v`, `\f`, `\x1c`-`\x1e`, `\x85`, U+2028, U+2029).
pub fn splitlines(text: &str, keepends: bool) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut iter = text.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        let end_of_break = match c {
            '\r' => {
                if let Some(&(j, '\n')) = iter.peek() {
                    iter.next();
                    j + 1
                } else {
                    i + 1
                }
            }
            '\n' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}'
            | '\u{2029}' => i + c.len_utf8(),
            _ => continue,
        };
        out.push(if keepends {
            &text[start..end_of_break]
        } else {
            &text[start..i]
        });
        start = end_of_break;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// A text buffer with GtkTextBuffer line semantics.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TextBuffer {
    lines: Vec<String>,
    /// Terminator after each line; the last is always empty.
    endings: Vec<&'static str>,
}

impl Default for TextBuffer {
    fn default() -> Self {
        TextBuffer {
            lines: vec![String::new()],
            endings: vec![""],
        }
    }
}

impl TextBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_text(text: &str) -> Self {
        let (lines, endings) = split_terminated(text)
            .into_iter()
            .map(|(l, e)| (l.to_owned(), e))
            .unzip();
        TextBuffer { lines, endings }
    }

    /// The whole text, with the original line terminators.
    pub fn text(&self) -> String {
        let mut s = String::new();
        for (line, ending) in self.lines.iter().zip(&self.endings) {
            s.push_str(line);
            s.push_str(ending);
        }
        s
    }

    /// Number of lines (`get_line_count`); always at least one.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// All lines without terminators; feed these to the differ.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Line `i` without its terminator.
    pub fn line(&self, i: usize) -> Option<&str> {
        self.lines.get(i).map(String::as_str)
    }

    /// The terminator of line `i` (`""` for the last line).
    pub fn line_ending(&self, i: usize) -> Option<&'static str> {
        self.endings.get(i).copied()
    }

    /// Lines `lo..hi`, clipped to the buffer like a Python slice.
    pub fn slice(&self, lo: usize, hi: usize) -> &[String] {
        py_slice(&self.lines, lo, hi)
    }

    /// The end of the buffer.
    pub fn end(&self) -> Position {
        let last = self.lines.len() - 1;
        Position::new(last, self.lines[last].len())
    }

    /// The start of `line`, or the end of the buffer if `line` is past the
    /// last line (`get_iter_at_line_or_eof`).
    pub fn iter_at_line_or_eof(&self, line: usize) -> Position {
        if line >= self.line_count() {
            self.end()
        } else {
            Position::new(line, 0)
        }
    }

    /// Clamp a position into the buffer, onto a char boundary.
    pub fn clamp(&self, pos: Position) -> Position {
        if pos.line >= self.lines.len() {
            return self.end();
        }
        let line = &self.lines[pos.line];
        let mut col = pos.col.min(line.len());
        while !line.is_char_boundary(col) {
            col -= 1;
        }
        Position::new(pos.line, col)
    }

    /// The text between two positions, including line terminators.
    pub fn text_range(&self, start: Position, end: Position) -> String {
        let (start, end) = self.ordered(start, end);
        let mut s = String::new();
        for l in start.line..=end.line {
            let line = &self.lines[l];
            let from = if l == start.line { start.col } else { 0 };
            let to = if l == end.line { end.col } else { line.len() };
            s.push_str(&line[from..to]);
            if l != end.line {
                s.push_str(self.endings[l]);
            }
        }
        s
    }

    /// The text of lines `start..end` including terminators, as Meld takes
    /// it for chunk actions and inline highlighting (from the start of
    /// `start`, or EOF, to the start of `end`, or EOF).
    pub fn lines_text(&self, start: usize, end: usize) -> String {
        self.text_range(
            self.iter_at_line_or_eof(start),
            self.iter_at_line_or_eof(end),
        )
    }

    fn ordered(&self, a: Position, b: Position) -> (Position, Position) {
        let (a, b) = (self.clamp(a), self.clamp(b));
        if a <= b { (a, b) } else { (b, a) }
    }

    /// Advance `pos` by `chars` characters, counting terminators as their
    /// length in characters like GtkTextIter offsets (`\r\n` is two).
    /// Stops at the end of the buffer.
    pub fn forward_chars(&self, pos: Position, chars: usize) -> Position {
        let mut pos = self.clamp(pos);
        let mut remaining = chars;
        loop {
            let line = &self.lines[pos.line];
            let rest = &line[pos.col..];
            let rest_chars = rest.chars().count();
            if remaining <= rest_chars {
                let col = pos.col
                    + rest
                        .char_indices()
                        .nth(remaining)
                        .map_or(rest.len(), |(i, _)| i);
                return Position::new(pos.line, col);
            }
            remaining -= rest_chars;
            let ending = self.endings[pos.line];
            if ending.is_empty() {
                return Position::new(pos.line, line.len());
            }
            // Landing inside a "\r\n" terminator is reported as the line end.
            if remaining < ending.len() {
                return Position::new(pos.line, line.len());
            }
            remaining -= ending.len();
            pos = Position::new(pos.line + 1, 0);
        }
    }

    /// The previous cursor position (`backward_cursor_position`): one
    /// character back, or to the end of the previous line's text. Returns
    /// `pos` unchanged at the start of the buffer.
    pub fn backward_cursor_position(&self, pos: Position) -> Position {
        let pos = self.clamp(pos);
        if pos.col > 0 {
            let line = &self.lines[pos.line];
            let col = line[..pos.col]
                .char_indices()
                .next_back()
                .map_or(0, |(i, _)| i);
            Position::new(pos.line, col)
        } else if pos.line > 0 {
            Position::new(pos.line - 1, self.lines[pos.line - 1].len())
        } else {
            pos
        }
    }

    /// Insert `text` at `at`.
    pub fn insert(&mut self, at: Position, text: &str) -> AppliedEdit {
        let at = self.clamp(at);
        let pieces = split_terminated(text);
        let added = pieces.len() - 1;
        let line = std::mem::take(&mut self.lines[at.line]);
        let (head, tail) = line.split_at(at.col);
        let original_ending = self.endings[at.line];

        let mut new_lines = Vec::with_capacity(pieces.len());
        let mut new_endings = Vec::with_capacity(pieces.len());
        let last = pieces.len() - 1;
        let mut end = at;
        for (i, (piece, ending)) in pieces.into_iter().enumerate() {
            let mut l = String::new();
            if i == 0 {
                l.push_str(head);
            }
            l.push_str(piece);
            if i == last {
                end = Position::new(at.line + i, l.len());
                l.push_str(tail);
                new_endings.push(original_ending);
            } else {
                new_endings.push(ending);
            }
            new_lines.push(l);
        }
        self.lines.splice(at.line..=at.line, new_lines);
        self.endings.splice(at.line..=at.line, new_endings);
        AppliedEdit {
            edit: TextEdit::Insert {
                at,
                text: text.to_owned(),
            },
            inverse: TextEdit::Delete { start: at, end },
            change: LineChange {
                start_line: at.line,
                size_change: added as isize,
            },
        }
    }

    /// Delete the text between two positions.
    pub fn delete(&mut self, start: Position, end: Position) -> AppliedEdit {
        let (start, end) = self.ordered(start, end);
        let removed_text = self.text_range(start, end);
        let removed = end.line - start.line;
        let tail = self.lines[end.line][end.col..].to_owned();
        let end_ending = self.endings[end.line];
        let first = &mut self.lines[start.line];
        first.truncate(start.col);
        first.push_str(&tail);
        self.endings[start.line] = end_ending;
        let range: Range<usize> = start.line + 1..end.line + 1;
        self.lines.drain(range.clone());
        self.endings.drain(range);
        AppliedEdit {
            edit: TextEdit::Delete { start, end },
            inverse: TextEdit::Insert {
                at: start,
                text: removed_text,
            },
            change: LineChange {
                start_line: start.line,
                size_change: -(removed as isize),
            },
        }
    }

    /// Apply a primitive edit.
    pub fn apply(&mut self, edit: &TextEdit) -> AppliedEdit {
        match edit {
            TextEdit::Insert { at, text } => self.insert(*at, text),
            TextEdit::Delete { start, end } => self.delete(*start, *end),
        }
    }

    /// Insert `text` at the start of `line`; past the last line, insert at
    /// the end of the buffer after a `"\n"` (`MeldBuffer.insert_at_line`).
    pub fn insert_at_line(&mut self, line: usize, text: &str) -> AppliedEdit {
        let mut text = text.to_owned();
        if line >= self.line_count() {
            // TODO (as in Meld): we can't be certain which line break to use.
            text.insert(0, '\n');
        }
        let at = self.iter_at_line_or_eof(line);
        self.insert(at, &text)
    }

    /// Replace the whole text as GtkTextBuffer.set_text does: a deletion of
    /// everything followed by an insertion.
    pub fn set_text(&mut self, text: &str) -> Vec<AppliedEdit> {
        let delete = self.delete(Position::new(0, 0), self.end());
        let insert = self.insert(Position::new(0, 0), text);
        vec![delete, insert]
    }
}

/// Lazily filtered copy of a buffer's lines (`BufferLines`).
///
/// Entries are `None` until requested; edits invalidate exactly the cache
/// entries GtkTextBuffer signals would. The raw lines and the filter are
/// passed in on access.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilteredLines {
    cache: Vec<Option<String>>,
}

impl FilteredLines {
    /// An empty cache for a buffer with `line_count` lines.
    pub fn new(line_count: usize) -> Self {
        FilteredLines {
            cache: vec![None; line_count],
        }
    }

    /// The cached entries (`BufferLines.lines`).
    pub fn cache(&self) -> &[Option<String>] {
        &self.cache
    }

    /// Drop all cached entries (`clear_cache`).
    pub fn clear(&mut self, line_count: usize) {
        self.cache = vec![None; line_count];
    }

    /// Invalidate the entries touched by an edit (`after_insert_text` /
    /// `on_delete_range`): an insertion of `n` lines at line `s` replaces
    /// entry `s` with `n + 1` empty entries; a deletion of `n` lines from
    /// line `s` replaces entries `s..=s + n` with one empty entry.
    pub fn invalidate(&mut self, change: LineChange) {
        let start = change.start_line.min(self.cache.len());
        if change.size_change >= 0 {
            let end = (start + 1).min(self.cache.len());
            let n = change.size_change as usize + 1;
            self.cache.splice(start..end, std::iter::repeat_n(None, n));
        } else {
            let end = add_signed(start + 1, -change.size_change).min(self.cache.len());
            self.cache.splice(start..end, [None]);
        }
    }

    /// Whether the cache length matches the buffer (`_check_cache_invariant`).
    pub fn is_consistent(&self, line_count: usize) -> bool {
        self.cache.len() == line_count
    }

    /// Filtered line `i`, or `None` past the end (Python's `IndexError`).
    pub fn get<S: AsRef<str>>(
        &mut self,
        raw: &[S],
        i: usize,
        filter: &dyn Fn(&str) -> String,
    ) -> Option<String> {
        let line = raw.get(i)?;
        if self.cache.len() < raw.len() {
            self.cache.resize(raw.len(), None);
        }
        let entry = &mut self.cache[i];
        Some(entry.get_or_insert_with(|| filter(line.as_ref())).clone())
    }

    /// Filtered lines `lo..hi`, clipped like a Python slice.
    pub fn slice<S: AsRef<str>>(
        &mut self,
        raw: &[S],
        lo: usize,
        hi: usize,
        filter: &dyn Fn(&str) -> String,
    ) -> Vec<String> {
        let hi = hi.min(raw.len());
        (lo.min(hi)..hi)
            .filter_map(|i| self.get(raw, i, filter))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "0\n1\n2\n3\n4\n5\n6\n7\n8\n9\n10";

    fn ident(s: &str) -> String {
        s.to_owned()
    }

    // Ported from test/test_buffer_lines.py

    #[test]
    fn meld_buffer_slicing() {
        let buf = TextBuffer::from_text(TEXT);
        let cases: &[(usize, usize, &[&str])] = &[
            (0, 1, &["0"]),
            (0, 2, &["0", "1"]),
            // zero-sized slice
            (9, 9, &[]),
            (9, 10, &["9"]),
            (9, 11, &["9", "10"]),
            // Past the end of the buffer
            (9, 12, &["9", "10"]),
            // Waaaay past the end of the buffer
            (9, 9999, &["9", "10"]),
            // And sidling towards past-the-end start indices
            (10, 12, &["10"]),
            (11, 12, &[]),
        ];
        for &(lo, hi, expected) in cases {
            let mut lines = FilteredLines::new(buf.line_count());
            assert_eq!(
                lines.slice(buf.lines(), lo, hi, &ident),
                expected,
                "{lo}..{hi}"
            );
            assert_eq!(buf.slice(lo, hi), expected);
        }
    }

    #[test]
    fn meld_buffer_index_out_of_range() {
        let buf = TextBuffer::from_text(TEXT);
        let mut lines = FilteredLines::new(buf.line_count());
        assert_eq!(lines.get(buf.lines(), 11, &ident), None);
        assert_eq!(buf.line(11), None);
    }

    #[test]
    fn meld_buffer_cached_contents() {
        let buf = TextBuffer::from_text(TEXT);
        let text_lines: Vec<&str> = TEXT.lines().collect();
        let mut lines = FilteredLines::new(buf.line_count());
        assert_eq!(lines.cache().len(), buf.line_count());
        assert_eq!(buf.line_count(), text_lines.len());
        assert!(lines.cache().iter().all(Option::is_none));
        lines.slice(buf.lines(), 0, usize::MAX, &ident);
        let cached: Vec<&str> = lines
            .cache()
            .iter()
            .map(|l| l.as_deref().unwrap())
            .collect();
        assert_eq!(cached, text_lines);
    }

    fn cache_window(lines: &FilteredLines, lo: usize, hi: usize) -> Vec<Option<&str>> {
        lines.cache()[lo..hi].iter().map(|l| l.as_deref()).collect()
    }

    #[test]
    fn meld_buffer_insert_text() {
        let mut buf = TextBuffer::from_text(TEXT);
        let mut lines = FilteredLines::new(buf.line_count());
        lines.slice(buf.lines(), 0, usize::MAX, &ident);
        assert_eq!(
            cache_window(&lines, 4, 8),
            [Some("4"), Some("5"), Some("6"), Some("7")]
        );

        let edit = buf.insert(Position::new(5, 0), "hey\nthings");
        lines.invalidate(edit.change);
        assert_eq!(
            cache_window(&lines, 4, 8),
            [Some("4"), None, None, Some("6")]
        );

        assert_eq!(lines.slice(buf.lines(), 5, 7, &ident), ["hey", "things5"]);
        assert_eq!(
            cache_window(&lines, 4, 8),
            [Some("4"), Some("hey"), Some("things5"), Some("6")]
        );
        assert!(lines.is_consistent(buf.line_count()));
    }

    #[test]
    fn meld_buffer_delete_range() {
        let mut buf = TextBuffer::from_text(TEXT);
        let mut lines = FilteredLines::new(buf.line_count());
        lines.slice(buf.lines(), 0, usize::MAX, &ident);
        assert_eq!(
            cache_window(&lines, 4, 8),
            [Some("4"), Some("5"), Some("6"), Some("7")]
        );

        // Delete from the start of line 5 to the start of line 7,
        // invalidating line 7 but leaving its contents intact.
        let edit = buf.delete(Position::new(5, 0), Position::new(7, 0));
        lines.invalidate(edit.change);
        assert_eq!(cache_window(&lines, 4, 7), [Some("4"), None, Some("8")]);

        assert_eq!(lines.get(buf.lines(), 5, &ident).as_deref(), Some("7"));
        assert_eq!(
            cache_window(&lines, 4, 7),
            [Some("4"), Some("7"), Some("8")]
        );
        assert!(lines.is_consistent(buf.line_count()));
    }

    #[test]
    fn meld_buffer_cache_debug() {
        let buf = TextBuffer::from_text(TEXT);
        let mut lines = FilteredLines::new(buf.line_count() + 1);
        assert!(!lines.is_consistent(buf.line_count()));
        lines.clear(buf.line_count());
        assert!(lines.is_consistent(buf.line_count()));
    }

    #[test]
    fn filter_applies_once() {
        let buf = TextBuffer::from_text("a b\nc");
        let mut lines = FilteredLines::new(buf.line_count());
        let strip = |s: &str| s.replace(' ', "");
        assert_eq!(lines.slice(buf.lines(), 0, 2, &strip), ["ab", "c"]);
    }

    #[test]
    fn gtk_line_semantics() {
        let buf = TextBuffer::from_text("a\r\nb\n");
        assert_eq!(buf.lines(), ["a", "b", ""]);
        assert_eq!(buf.text(), "a\r\nb\n");
        assert_eq!(buf.lines_text(0, 1), "a\r\n");
        assert_eq!(buf.lines_text(1, 9), "b\n");
        assert_eq!(TextBuffer::from_text("").lines(), [""]);
        assert_eq!(TextBuffer::from_text("x\ry").lines(), ["x", "y"]);
        assert_eq!(
            buf.forward_chars(Position::new(0, 0), 3),
            Position::new(1, 0)
        );
        assert_eq!(
            buf.forward_chars(Position::new(0, 0), 2),
            Position::new(0, 1)
        );
        assert_eq!(
            buf.forward_chars(Position::new(0, 0), 99),
            Position::new(2, 0)
        );
        assert_eq!(
            buf.backward_cursor_position(Position::new(1, 0)),
            Position::new(0, 1)
        );
    }

    #[test]
    fn edits_and_inverses() {
        let mut buf = TextBuffer::from_text("one\r\ntwo\nthree");
        let original = buf.clone();
        let e = buf.insert(Position::new(1, 1), "X\nY\r\nZ");
        assert_eq!(buf.text(), "one\r\ntX\nY\r\nZwo\nthree");
        assert_eq!(
            e.change,
            LineChange {
                start_line: 1,
                size_change: 2
            }
        );
        buf.apply(&e.inverse);
        assert_eq!(buf, original);

        let e = buf.delete(Position::new(0, 2), Position::new(2, 1));
        assert_eq!(buf.text(), "onhree");
        assert_eq!(
            e.change,
            LineChange {
                start_line: 0,
                size_change: -2
            }
        );
        buf.apply(&e.inverse);
        assert_eq!(buf, original);

        let e = buf.insert_at_line(9, "tail");
        assert_eq!(buf.text(), "one\r\ntwo\nthree\ntail");
        assert_eq!(
            e.change,
            LineChange {
                start_line: 2,
                size_change: 1
            }
        );

        let edits = buf.set_text("new");
        assert_eq!(buf.text(), "new");
        assert_eq!(edits[0].change.size_change, -3);
        assert_eq!(edits[1].change.size_change, 0);
    }

    #[test]
    fn python_splitlines() {
        assert_eq!(
            splitlines("a\nb\r\nc\rd", true),
            ["a\n", "b\r\n", "c\r", "d"]
        );
        assert_eq!(splitlines("a\nb\n", false), ["a", "b"]);
        assert_eq!(splitlines("", true), Vec::<&str>::new());
        assert_eq!(splitlines("x\u{2028}y\x0cz", false), ["x", "y", "z"]);
    }
}
