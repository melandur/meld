//! Line wrapping and the line ↔ y geometry shared by every widget that
//! lines up with a [`TextPane`](crate::text_pane::TextPane)
//! (GtkTextView's `wrap-mode` and `get_line_yrange` / `get_line_at_y`).
//!
//! Text is laid out on a grid of character cells (the pane's font is
//! monospace): ASCII characters take one cell, wide characters two
//! (Unicode East Asian width), tabs run to the next tab stop of the row.
//! A [`WrapLayout`] holds how many visual rows each line takes at a given
//! number of `columns`; [`wrap_line`] gives the rows of one line and is
//! what the pane draws, so both always agree.
//!
//! The application owns one `WrapLayout` per pane:
//!
//! ```ignore
//! layout.set_mode(WrapMode::Word);
//! // on Action::WrapColumns(n) from the pane:
//! layout.set_columns(n);
//! // after edits, loads and the above (cheap when nothing changed):
//! layout.update(&buffer);
//! text_pane(&buffer, &style).wrap(&layout) // link_map(..).wrap(..), ...
//! ```
//!
//! Without wrapping (mode [`WrapMode::None`], or columns not known yet)
//! every line is one row and the layout stores nothing.

use std::ops::Range;

use crate::buffer::Buffer;

/// How lines are wrapped (`GtkWrapMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WrapMode {
    /// No wrapping: long lines scroll horizontally.
    #[default]
    None,
    /// Wrap at any character.
    Char,
    /// Wrap between words; a word longer than a row overflows it.
    Word,
    /// Wrap between words, or inside a word that does not fit a row.
    WordChar,
}

/// Width of a character in cells (tabs excluded).
pub fn char_cells(ch: char) -> usize {
    if ch.is_ascii() {
        1
    } else {
        unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1)
    }
}

/// Character indices where the visual rows of `text` start when wrapped
/// at `columns` cells; always starts with `0`. Tabs expand to the next
/// multiple of `tab_width` within their row.
pub fn wrap_line(text: &str, tab_width: usize, columns: usize, mode: WrapMode) -> Vec<usize> {
    let tab = tab_width.max(1);
    if mode == WrapMode::None || columns == 0 || (text.len() <= columns && !text.contains('\t')) {
        return vec![0];
    }
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let word_mode = mode != WrapMode::Char;
    // `breaks[i]`: a row may start at char `i`.
    let breaks: Vec<bool> = if word_mode {
        let mut breaks = vec![false; n + 1];
        let mut char_at_byte = 0usize;
        let mut bytes = text.char_indices().map(|(b, _)| b).peekable();
        for (byte, _) in unicode_linebreak::linebreaks(text) {
            while let Some(&b) = bytes.peek() {
                if b >= byte {
                    break;
                }
                bytes.next();
                char_at_byte += 1;
            }
            if char_at_byte <= n {
                breaks[char_at_byte] = true;
            }
        }
        breaks
    } else {
        Vec::new()
    };

    let mut rows = vec![0];
    let mut start = 0;
    'rows: loop {
        let mut col = 0usize;
        let mut last_break: Option<usize> = None;
        for (i, &ch) in chars.iter().enumerate().skip(start) {
            let w = if ch == '\t' {
                tab - col % tab
            } else {
                char_cells(ch)
            };
            if i > start && word_mode && breaks[i] {
                last_break = Some(i);
            }
            // Spaces at the end of a row may hang past its edge.
            let hangs = word_mode && ch.is_whitespace();
            if !hangs && i > start && col + w > columns {
                let at = match mode {
                    WrapMode::Char => Some(i),
                    WrapMode::Word => last_break,
                    _ => last_break.or(Some(i)),
                };
                if let Some(at) = at {
                    rows.push(at);
                    start = at;
                    continue 'rows;
                }
            }
            col += w;
        }
        break;
    }
    rows
}

/// Visual rows of a buffer (see the module docs). Cheap to query; built
/// in O(text) by [`WrapLayout::update`].
#[derive(Debug, Clone, Default)]
pub struct WrapLayout {
    mode: WrapMode,
    columns: usize,
    /// What `prefix` was computed for: buffer revision, line count, tab
    /// width, columns, mode, text address.
    key: Option<(u64, usize, usize, usize, WrapMode, usize)>,
    /// Rows before each line (`lines + 1` entries) when some line
    /// wraps; empty when every line is one row.
    prefix: Vec<u32>,
}

static IDENTITY: WrapLayout = WrapLayout {
    mode: WrapMode::None,
    columns: 0,
    key: None,
    prefix: Vec::new(),
};

impl WrapLayout {
    /// A layout wrapping in `mode` (columns are set later).
    #[cfg(test)]
    pub fn new(mode: WrapMode) -> Self {
        Self {
            mode,
            ..Self::default()
        }
    }

    /// The layout without wrapping: line `i` is row `i`.
    pub fn identity() -> &'static WrapLayout {
        &IDENTITY
    }

    /// The wrap mode.
    pub fn mode(&self) -> WrapMode {
        self.mode
    }

    /// Changes the wrap mode (takes effect on the next [`update`](Self::update)).
    pub fn set_mode(&mut self, mode: WrapMode) {
        self.mode = mode;
    }

    /// Cells per row (0: unknown).
    pub fn columns(&self) -> usize {
        self.columns
    }

    /// Sets the cells per row, as reported by the pane with
    /// [`Action::WrapColumns`](crate::Action::WrapColumns).
    pub fn set_columns(&mut self, columns: usize) {
        self.columns = columns;
    }

    /// Whether lines are wrapped (a mode and a row width are set).
    pub fn is_wrapping(&self) -> bool {
        self.mode != WrapMode::None && self.columns > 0
    }

    /// Recomputes the rows for `buffer` if it, the mode or the columns
    /// changed. Returns whether the geometry changed.
    pub fn update(&mut self, buffer: &Buffer) -> bool {
        let key = (
            buffer.revision(),
            buffer.line_count(),
            buffer.tab_width(),
            self.columns,
            self.mode,
            buffer.lines().as_ptr() as usize,
        );
        if self.key == Some(key) {
            return false;
        }
        let old_wrapping = !self.prefix.is_empty();
        self.key = Some(key);
        self.prefix.clear();
        if !self.is_wrapping() {
            return old_wrapping;
        }
        let tab = buffer.tab_width();
        let mut total = 0u32;
        let mut prefix = Vec::with_capacity(buffer.line_count() + 2);
        let mut wraps = false;
        for line in buffer.lines() {
            prefix.push(total);
            let rows = wrap_line(line, tab, self.columns, self.mode).len() as u32;
            wraps |= rows > 1;
            total += rows;
        }
        if buffer.line_count() == 0 {
            prefix.push(0);
            total = 1;
        }
        prefix.push(total);
        if wraps {
            self.prefix = prefix;
        }
        true
    }

    /// Rows before `line`. Lines past the end count one row each.
    pub fn rows_before(&self, line: usize) -> usize {
        if self.prefix.is_empty() {
            return line;
        }
        let last = self.prefix.len() - 1;
        if line <= last {
            self.prefix[line] as usize
        } else {
            self.prefix[last] as usize + (line - last)
        }
    }

    /// Rows taken by `line`.
    pub fn rows(&self, line: usize) -> usize {
        self.rows_before(line + 1) - self.rows_before(line)
    }

    /// Rows taken by the first `line_count` lines (at least one line).
    pub fn total_rows(&self, line_count: usize) -> usize {
        self.rows_before(line_count.max(1))
    }

    /// The line showing visual `row`, and the row within that line.
    pub fn line_at_row(&self, row: usize) -> (usize, usize) {
        if self.prefix.is_empty() {
            return (row, 0);
        }
        let last = self.prefix.len() - 1;
        let total = self.prefix[last] as usize;
        if row >= total {
            return (last + (row - total), 0);
        }
        let line = self.prefix.partition_point(|&p| p as usize <= row) - 1;
        (line, row - self.prefix[line] as usize)
    }

    /// Top of `line` in content pixels (`line == line_count` gives the
    /// end of the text).
    pub fn y_for_line(&self, line: usize, line_height: f32) -> f32 {
        self.rows_before(line) as f32 * line_height
    }

    /// Height of `line` in pixels.
    pub fn line_height(&self, line: usize, line_height: f32) -> f32 {
        self.rows(line) as f32 * line_height
    }

    /// The line at content `y` and how far into it `y` is (0..1). Never
    /// past `line_count - 1` (at least line 0).
    pub fn line_at_y(&self, y: f32, line_height: f32, line_count: usize) -> (usize, f32) {
        let lh = line_height.max(1.0);
        let last = line_count.max(1) - 1;
        if y <= 0.0 {
            return (0, 0.0);
        }
        let (line, _) = self.line_at_row((y / lh).floor() as usize);
        if line > last {
            return (last, 1.0);
        }
        let top = self.y_for_line(line, lh);
        let h = self.line_height(line, lh).max(1.0);
        (line, ((y - top) / h).clamp(0.0, 1.0))
    }

    /// Lines (partially) visible at a scroll offset, clamped to
    /// `line_count` (at least line 0).
    pub fn visible_lines(
        &self,
        scroll_y: f32,
        viewport_height: f32,
        line_height: f32,
        line_count: usize,
    ) -> Range<usize> {
        let lh = line_height.max(1.0);
        let count = line_count.max(1);
        let first_row = (scroll_y / lh).floor().max(0.0) as usize;
        let end_row = ((scroll_y + viewport_height) / lh).ceil().max(0.0) as usize;
        let first = self.line_at_row(first_row).0.min(count);
        if end_row <= first_row {
            return first..first;
        }
        let last = (self.line_at_row(end_row - 1).0 + 1).min(count);
        first..last.max(first)
    }

    /// Scrollable content height: all rows plus the overscroll margin.
    pub fn content_height(
        &self,
        line_count: usize,
        line_height: f32,
        overscroll_lines: usize,
    ) -> f32 {
        (self.total_rows(line_count) + overscroll_lines) as f32 * line_height
    }
}

/// `layout` if it wraps, else the identity layout.
pub(crate) fn or_identity(layout: Option<&WrapLayout>) -> &WrapLayout {
    layout.unwrap_or(WrapLayout::identity())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(text: &str, cols: usize, mode: WrapMode) -> Vec<String> {
        let starts = wrap_line(text, 4, cols, mode);
        let chars: Vec<char> = text.chars().collect();
        starts
            .iter()
            .enumerate()
            .map(|(i, &s)| {
                let e = starts.get(i + 1).copied().unwrap_or(chars.len());
                chars[s..e].iter().collect()
            })
            .collect()
    }

    #[test]
    fn short_lines_are_one_row() {
        assert_eq!(wrap_line("hello", 4, 10, WrapMode::Word), vec![0]);
        assert_eq!(wrap_line("hello world", 4, 0, WrapMode::Word), vec![0]);
        assert_eq!(wrap_line("hello world", 4, 5, WrapMode::None), vec![0]);
        assert_eq!(wrap_line("", 4, 5, WrapMode::Char), vec![0]);
    }

    #[test]
    fn char_wrapping() {
        assert_eq!(
            rows("abcdefghij", 4, WrapMode::Char),
            ["abcd", "efgh", "ij"]
        );
        assert_eq!(rows("abcd", 4, WrapMode::Char), ["abcd"]);
        // Tabs run to the next stop of their row.
        assert_eq!(rows("ab\tcd", 4, WrapMode::Char), ["ab\t", "cd"]);
        // Wide characters take two cells.
        assert_eq!(
            rows("日本語です", 4, WrapMode::Char),
            ["日本", "語で", "す"]
        );
    }

    #[test]
    fn word_wrapping() {
        assert_eq!(
            rows("one two three", 8, WrapMode::Word),
            ["one two ", "three"]
        );
        // Spaces hang past the edge instead of starting a row.
        assert_eq!(rows("one    two", 4, WrapMode::Word), ["one    ", "two"]);
        // A long word overflows in word mode, and is split in word-char mode.
        assert_eq!(
            rows("a verylongword b", 6, WrapMode::Word),
            ["a ", "verylongword ", "b"]
        );
        assert_eq!(
            rows("a verylongword b", 6, WrapMode::WordChar),
            ["a ", "verylo", "ngword ", "b"]
        );
    }

    #[test]
    fn layout_maps_lines_to_rows() {
        let mut b = Buffer::new(vec![
            "short".into(),
            "one two three four".into(),
            "x".into(),
        ]);
        b.set_indentation(4, false);
        let mut w = WrapLayout::new(WrapMode::Word);
        assert!(!w.update(&b));
        assert_eq!(w.rows_before(2), 2);
        w.set_columns(8);
        assert!(w.update(&b));
        assert!(!w.update(&b));
        // "one two " / "three " / "four"
        assert_eq!(w.rows(1), 3);
        assert_eq!(w.rows_before(2), 4);
        assert_eq!(w.total_rows(3), 5);
        assert_eq!(w.line_at_row(0), (0, 0));
        assert_eq!(w.line_at_row(2), (1, 1));
        assert_eq!(w.line_at_row(4), (2, 0));
        assert_eq!(w.line_at_row(6), (4, 0));
        assert_eq!(w.y_for_line(2, 10.0), 40.0);
        assert_eq!(w.y_for_line(3, 10.0), 50.0);
        assert_eq!(w.line_at_y(25.0, 10.0, 3), (1, 0.5));
        assert_eq!(w.line_at_y(1000.0, 10.0, 3), (2, 1.0));
        assert_eq!(w.visible_lines(15.0, 20.0, 10.0, 3), 1..2);
        assert_eq!(w.visible_lines(15.0, 30.0, 10.0, 3), 1..3);
        assert_eq!(w.content_height(3, 10.0, 5), 100.0);
        // Turning wrapping off restores one row per line.
        w.set_mode(WrapMode::None);
        assert!(w.update(&b));
        assert_eq!(w.rows(1), 1);
        assert_eq!(w.total_rows(3), 3);
    }

    #[test]
    fn identity_matches_unwrapped_math() {
        let w = WrapLayout::identity();
        assert_eq!(w.visible_lines(0.0, 100.0, 20.0, 100), 0..5);
        assert_eq!(w.visible_lines(10.0, 100.0, 20.0, 100), 0..6);
        assert_eq!(w.visible_lines(1990.0, 100.0, 20.0, 100), 99..100);
        assert_eq!(w.visible_lines(0.0, 100.0, 20.0, 0), 0..1);
        assert_eq!(w.content_height(10, 20.0, 5), 300.0);
        assert_eq!(w.line_at_y(30.0, 20.0, 10), (1, 0.5));
    }

    #[test]
    fn edits_recompute() {
        let mut b = Buffer::new(vec!["aaaa bbbb".into()]);
        let mut w = WrapLayout::new(WrapMode::Word);
        w.set_columns(5);
        w.update(&b);
        assert_eq!(w.total_rows(1), 2);
        b.replace_lines(0..1, vec!["a".into(), "bbbb cccc dddd".into()]);
        assert!(w.update(&b));
        assert_eq!(w.total_rows(2), 4);
    }
}
