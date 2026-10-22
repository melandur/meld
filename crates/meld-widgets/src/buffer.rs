//! The text model behind a [`TextPane`](crate::text_pane::TextPane).
//!
//! The application owns a [`Buffer`]; the pane widget only reads it and
//! emits [`Action`]s. [`Buffer::perform`] applies an action and returns the
//! line-level [`Edit`] it made, which the application forwards to its diff
//! engine (incremental update), undo stack and highlighter.

use std::cell::Cell;
use std::ops::Range;
use std::sync::Arc;

use iced::{Point, Size};

/// A cursor position: 0-based line and character (not byte) column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Position {
    /// Line index.
    pub line: usize,
    /// Character index within the line.
    pub column: usize,
}

impl Position {
    /// Creates a position.
    pub const fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }
}

/// A cursor movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Motion {
    /// One character left.
    Left,
    /// One character right.
    Right,
    /// One line up.
    Up,
    /// One line down.
    Down,
    /// To the previous word start.
    WordLeft,
    /// To the next word end.
    WordRight,
    /// To the start of the line.
    Home,
    /// To the end of the line.
    End,
    /// To the start of the document.
    DocumentStart,
    /// To the end of the document.
    DocumentEnd,
    /// Up by the given number of lines (a page).
    PageUp(usize),
    /// Down by the given number of lines (a page).
    PageDown(usize),
}

/// A text-modifying action.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EditAction {
    /// Insert a character, replacing the selection.
    Insert(char),
    /// Insert text (paste, IME commit), replacing the selection.
    /// `\r\n` and `\r` are normalised to line breaks.
    Paste(Arc<String>),
    /// Break the line.
    Enter,
    /// Delete the selection or the character before the cursor.
    Backspace,
    /// Delete the selection or the character after the cursor.
    Delete,
    /// Delete the selection or back to the previous word start.
    DeleteWordBackward,
    /// Delete the selection or forward to the next word end.
    DeleteWordForward,
    /// Insert a tab (or spaces), or indent all selected lines when the
    /// selection spans several lines.
    Tab,
    /// Unindent the cursor line or all selected lines.
    Unindent,
}

/// An action emitted by the text pane.
///
/// Pass everything except [`Action::Scrolled`], [`Action::Resized`],
/// [`Action::Focused`] and [`Action::ContextMenu`] (which concern the
/// application, not the text) to [`Buffer::perform`]; those are ignored by
/// it anyway.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Move the cursor, clearing the selection.
    Move(Motion),
    /// Move the cursor, extending the selection.
    Select(Motion),
    /// Place the cursor (single click).
    Click(Position),
    /// Extend the selection to a position (drag, shift-click).
    ExtendTo(Position),
    /// Select the word at a position (double click).
    SelectWord(Position),
    /// Select the line at a position (triple click, line-number click).
    SelectLine(Position),
    /// Select everything.
    SelectAll,
    /// Modify the text. Not emitted for read-only buffers.
    Edit(EditAction),
    /// The user scrolled; `x`/`y` are the new, clamped scroll offsets in
    /// pixels. The application should store them (and sync other panes).
    Scrolled {
        /// Horizontal offset.
        x: f32,
        /// Vertical offset.
        y: f32,
    },
    /// The text area (excluding gutter) changed size.
    Resized(Size),
    /// The pane gained (`true`) or lost (`false`) keyboard focus.
    Focused(bool),
    /// Secondary click: the line/column under the pointer and the pointer
    /// position in window coordinates (for placing a popup menu).
    ContextMenu(Position, Point),
}

/// A line-level description of a change: lines
/// `start_line..start_line + removed_lines.len()` were replaced by
/// `inserted_lines`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Edit {
    /// First affected line.
    pub start_line: usize,
    /// Previous contents of the affected lines.
    pub removed_lines: Vec<String>,
    /// New contents of the affected lines.
    pub inserted_lines: Vec<String>,
    /// Cursor before the edit.
    pub cursor_before: Position,
    /// Cursor after the edit.
    pub cursor_after: Position,
}

impl Edit {
    /// The replaced line range in the old text.
    pub fn removed_range(&self) -> Range<usize> {
        self.start_line..self.start_line + self.removed_lines.len()
    }

    /// The line range of the new lines in the new text.
    pub fn inserted_range(&self) -> Range<usize> {
        self.start_line..self.start_line + self.inserted_lines.len()
    }

    /// Change in line count.
    pub fn line_delta(&self) -> isize {
        self.inserted_lines.len() as isize - self.removed_lines.len() as isize
    }

    /// The edit that undoes this one.
    pub fn inverse(&self) -> Edit {
        Edit {
            start_line: self.start_line,
            removed_lines: self.inserted_lines.clone(),
            inserted_lines: self.removed_lines.clone(),
            cursor_before: self.cursor_after,
            cursor_after: self.cursor_before,
        }
    }
}

/// Error from [`Buffer::apply`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApplyError {
    /// The edit's removed range is past the end of the buffer.
    #[error("edit range {start}..{end} is out of bounds for {len} lines")]
    OutOfBounds {
        /// Range start.
        start: usize,
        /// Range end.
        end: usize,
        /// Buffer line count.
        len: usize,
    },
    /// The buffer's lines don't match the edit's `removed_lines`.
    #[error("buffer contents at line {line} do not match the edit")]
    Mismatch {
        /// First mismatching line.
        line: usize,
    },
}

/// Editable lines plus cursor and selection.
///
/// Lines carry no terminators. An empty buffer has zero lines; the pane
/// still shows one empty line, and typing into it inserts line 0.
#[derive(Debug, Clone)]
pub struct Buffer {
    lines: Vec<String>,
    cursor: Position,
    anchor: Option<Position>,
    preferred_column: Option<usize>,
    read_only: bool,
    tab_width: usize,
    insert_spaces: bool,
    revision: u64,
    cursor_revision: u64,
    widest: Cell<Option<usize>>,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl Buffer {
    /// Creates a buffer from lines.
    pub fn new(lines: Vec<String>) -> Self {
        Self {
            lines,
            cursor: Position::default(),
            anchor: None,
            preferred_column: None,
            read_only: false,
            tab_width: 8,
            insert_spaces: false,
            revision: 0,
            cursor_revision: 0,
            widest: Cell::new(None),
        }
    }

    /// Creates a buffer by splitting `text` on `\n` (a trailing `\r` is
    /// stripped from each line). A trailing newline does not produce an
    /// extra empty line.
    pub fn from_text(text: &str) -> Self {
        Self::new(split_text(text))
    }

    /// The lines.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Number of lines (0 for an empty buffer).
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Number of lines displayed (at least 1).
    pub fn display_line_count(&self) -> usize {
        self.lines.len().max(1)
    }

    /// Line `i`, or `""` past the end.
    pub fn line(&self, i: usize) -> &str {
        self.lines.get(i).map_or("", String::as_str)
    }

    /// The text joined with `\n`.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Takes the lines out of the buffer.
    pub fn into_lines(self) -> Vec<String> {
        self.lines
    }

    /// Replaces all content without producing an [`Edit`]; resets the
    /// cursor and selection.
    pub fn set_lines(&mut self, lines: Vec<String>) {
        self.lines = lines;
        self.cursor = Position::default();
        self.anchor = None;
        self.preferred_column = None;
        self.widest.set(None);
        self.bump();
        self.bump_cursor();
    }

    /// Whether editing actions are ignored.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Sets the read-only flag.
    pub fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
    }

    /// Tab width in columns.
    pub fn tab_width(&self) -> usize {
        self.tab_width
    }

    /// Whether Tab inserts spaces.
    pub fn insert_spaces(&self) -> bool {
        self.insert_spaces
    }

    /// Sets the indentation settings (`tab-width`,
    /// `insert-spaces-instead-of-tabs`).
    pub fn set_indentation(&mut self, tab_width: usize, insert_spaces: bool) {
        let tab_width = tab_width.max(1);
        if tab_width != self.tab_width {
            self.widest.set(None);
        }
        self.tab_width = tab_width;
        self.insert_spaces = insert_spaces;
    }

    /// Content revision; increases on every change.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Cursor revision; increases whenever cursor or selection change.
    /// The pane scrolls the cursor into view when this changes.
    pub fn cursor_revision(&self) -> u64 {
        self.cursor_revision
    }

    /// The cursor.
    pub fn cursor(&self) -> Position {
        self.cursor
    }

    /// The selection anchor, if a selection is active.
    pub fn anchor(&self) -> Option<Position> {
        self.anchor
    }

    /// The ordered selection range, if non-empty.
    pub fn selection(&self) -> Option<(Position, Position)> {
        let anchor = self.anchor?;
        if anchor == self.cursor {
            return None;
        }
        Some((anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    /// The selected text joined with `\n`.
    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection()?;
        Some(self.text_range(start, end))
    }

    /// Text between two ordered positions.
    pub fn text_range(&self, start: Position, end: Position) -> String {
        if start.line == end.line {
            let l = self.line(start.line);
            return l[byte_at(l, start.column)..byte_at(l, end.column)].to_owned();
        }
        let mut out = String::new();
        let first = self.line(start.line);
        out.push_str(&first[byte_at(first, start.column)..]);
        for i in start.line + 1..end.line {
            out.push('\n');
            out.push_str(self.line(i));
        }
        out.push('\n');
        let last = self.line(end.line);
        out.push_str(&last[..byte_at(last, end.column)]);
        out
    }

    /// Moves the cursor (clamped), optionally extending the selection.
    pub fn set_cursor(&mut self, position: Position, extend: bool) {
        let position = self.clamp(position);
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
        } else {
            self.anchor = None;
        }
        self.cursor = position;
        self.preferred_column = None;
        self.bump_cursor();
    }

    /// Selects `start..end`, leaving the cursor at `end`.
    pub fn select(&mut self, start: Position, end: Position) {
        self.anchor = Some(self.clamp(start));
        self.cursor = self.clamp(end);
        self.preferred_column = None;
        self.bump_cursor();
    }

    /// Clamps a position to the buffer.
    pub fn clamp(&self, p: Position) -> Position {
        if self.lines.is_empty() {
            return Position::default();
        }
        let line = p.line.min(self.lines.len() - 1);
        let column = p.column.min(char_len(&self.lines[line]));
        Position { line, column }
    }

    /// Position at the end of the buffer.
    pub fn end_position(&self) -> Position {
        match self.lines.last() {
            Some(last) => Position::new(self.lines.len() - 1, char_len(last)),
            None => Position::default(),
        }
    }

    /// Display column (tabs expanded) of a character column.
    pub fn display_column(&self, p: Position) -> usize {
        display_column(self.line(p.line), p.column, self.tab_width)
    }

    /// Widest line in display columns (cached).
    pub fn max_display_columns(&self) -> usize {
        if let Some(w) = self.widest.get() {
            return w;
        }
        let w = self
            .lines
            .iter()
            .map(|l| line_display_width(l, self.tab_width))
            .max()
            .unwrap_or(0);
        self.widest.set(Some(w));
        w
    }

    /// Performs an action, returning the edit if the text changed.
    pub fn perform(&mut self, action: Action) -> Option<Edit> {
        match action {
            Action::Move(motion) => {
                self.motion(motion, false);
                None
            }
            Action::Select(motion) => {
                self.motion(motion, true);
                None
            }
            Action::Click(p) => {
                self.set_cursor(p, false);
                None
            }
            Action::ExtendTo(p) => {
                self.set_cursor(p, true);
                None
            }
            Action::SelectWord(p) => {
                let p = self.clamp(p);
                let (s, e) = word_bounds(self.line(p.line), p.column);
                self.select(Position::new(p.line, s), Position::new(p.line, e));
                None
            }
            Action::SelectLine(p) => {
                let p = self.clamp(p);
                let start = Position::new(p.line, 0);
                let end = if p.line + 1 < self.lines.len() {
                    Position::new(p.line + 1, 0)
                } else {
                    Position::new(p.line, char_len(self.line(p.line)))
                };
                self.select(start, end);
                None
            }
            Action::SelectAll => {
                self.select(Position::default(), self.end_position());
                None
            }
            Action::Edit(edit) => {
                if self.read_only {
                    None
                } else {
                    self.edit(edit)
                }
            }
            Action::Scrolled { .. }
            | Action::Resized(_)
            | Action::Focused(_)
            | Action::ContextMenu(..) => None,
        }
    }

    /// Applies an edit (e.g. undo/redo, or a change made by the diff
    /// view). Verifies that the removed lines match. The cursor moves to
    /// `edit.cursor_after` and the selection is cleared.
    pub fn apply(&mut self, edit: &Edit) -> Result<(), ApplyError> {
        let range = edit.removed_range();
        if range.end > self.lines.len() {
            return Err(ApplyError::OutOfBounds {
                start: range.start,
                end: range.end,
                len: self.lines.len(),
            });
        }
        if let Some(i) = (range.clone())
            .zip(&edit.removed_lines)
            .find(|(i, l)| &self.lines[*i] != *l)
            .map(|(i, _)| i)
        {
            return Err(ApplyError::Mismatch { line: i });
        }
        self.splice(range, edit.inserted_lines.clone());
        self.anchor = None;
        self.cursor = self.clamp(edit.cursor_after);
        self.preferred_column = None;
        self.bump_cursor();
        Ok(())
    }

    /// Replaces a line range programmatically and returns the [`Edit`].
    /// The cursor keeps its place relative to unaffected text; if it was
    /// inside the range it moves to the range start.
    pub fn replace_lines(&mut self, range: Range<usize>, lines: Vec<String>) -> Edit {
        let start = range.start.min(self.lines.len());
        let end = range.end.clamp(start, self.lines.len());
        let removed: Vec<String> = self.lines[start..end].to_vec();
        let delta = lines.len() as isize - removed.len() as isize;
        let cursor_before = self.cursor;
        let adjust = |p: Position| {
            if p.line >= end {
                Position::new((p.line as isize + delta).max(0) as usize, p.column)
            } else if p.line >= start {
                Position::new(start, 0)
            } else {
                p
            }
        };
        let cursor_after = adjust(self.cursor);
        let anchor_after = self.anchor.map(adjust);
        let edit = Edit {
            start_line: start,
            removed_lines: removed,
            inserted_lines: lines.clone(),
            cursor_before,
            cursor_after,
        };
        self.splice(start..end, lines);
        self.cursor = self.clamp(cursor_after);
        self.anchor = anchor_after.map(|a| self.clamp(a));
        self.bump_cursor();
        Edit {
            cursor_after: self.cursor,
            ..edit
        }
    }

    /// Replaces the text between two positions with `text`.
    pub fn replace_range(&mut self, start: Position, end: Position, text: &str) -> Edit {
        let (start, end) = (self.clamp(start.min(end)), self.clamp(start.max(end)));
        let cursor_before = self.cursor;
        let first = self.line(start.line);
        let last = self.line(end.line);
        let prefix = &first[..byte_at(first, start.column)];
        let suffix = &last[byte_at(last, end.column)..];
        let mut new_text = String::with_capacity(prefix.len() + text.len() + suffix.len());
        new_text.push_str(prefix);
        new_text.push_str(text);
        let cursor_line_offset = text.matches('\n').count();
        let cursor_column = match text.rfind('\n') {
            Some(i) => char_len(&text[i + 1..]),
            None => char_len(prefix) + char_len(text),
        };
        new_text.push_str(suffix);
        let inserted: Vec<String> = new_text.split('\n').map(str::to_owned).collect();
        let removed_end = if self.lines.is_empty() {
            0
        } else {
            end.line + 1
        };
        let removed = self.lines[start.line.min(removed_end)..removed_end].to_vec();
        let cursor_after = Position::new(start.line + cursor_line_offset, cursor_column);
        let edit = Edit {
            start_line: start.line,
            removed_lines: removed,
            inserted_lines: inserted.clone(),
            cursor_before,
            cursor_after,
        };
        self.splice(edit.removed_range(), inserted);
        self.anchor = None;
        self.cursor = cursor_after;
        self.preferred_column = None;
        self.bump_cursor();
        edit
    }

    fn splice(&mut self, range: Range<usize>, lines: Vec<String>) {
        if let Some(w) = self.widest.get() {
            let removed_widest = self.lines[range.clone()]
                .iter()
                .any(|l| line_display_width(l, self.tab_width) >= w);
            if removed_widest {
                self.widest.set(None);
            } else {
                let new_max = lines
                    .iter()
                    .map(|l| line_display_width(l, self.tab_width))
                    .max()
                    .unwrap_or(0);
                self.widest.set(Some(w.max(new_max)));
            }
        }
        self.lines.splice(range, lines);
        self.bump();
    }

    fn bump(&mut self) {
        self.revision += 1;
    }

    fn bump_cursor(&mut self) {
        self.cursor_revision += 1;
    }

    fn motion(&mut self, motion: Motion, extend: bool) {
        // Without shift, horizontal moves collapse an existing selection.
        if !extend && let Some((start, end)) = self.selection() {
            match motion {
                Motion::Left => {
                    self.set_cursor(start, false);
                    return;
                }
                Motion::Right => {
                    self.set_cursor(end, false);
                    return;
                }
                _ => {}
            }
        }

        let c = self.cursor;
        let vertical = matches!(
            motion,
            Motion::Up | Motion::Down | Motion::PageUp(_) | Motion::PageDown(_)
        );
        let target = match motion {
            Motion::Left => {
                if c.column > 0 {
                    Position::new(c.line, c.column - 1)
                } else if c.line > 0 {
                    Position::new(c.line - 1, char_len(self.line(c.line - 1)))
                } else {
                    c
                }
            }
            Motion::Right => {
                if c.column < char_len(self.line(c.line)) {
                    Position::new(c.line, c.column + 1)
                } else if c.line + 1 < self.lines.len() {
                    Position::new(c.line + 1, 0)
                } else {
                    c
                }
            }
            Motion::Up => self.vertical(-1),
            Motion::Down => self.vertical(1),
            Motion::PageUp(n) => self.vertical(-(n.max(1) as isize)),
            Motion::PageDown(n) => self.vertical(n.max(1) as isize),
            Motion::WordLeft => {
                if c.column == 0 {
                    if c.line > 0 {
                        Position::new(c.line - 1, char_len(self.line(c.line - 1)))
                    } else {
                        c
                    }
                } else {
                    Position::new(c.line, word_left(self.line(c.line), c.column))
                }
            }
            Motion::WordRight => {
                let len = char_len(self.line(c.line));
                if c.column >= len {
                    if c.line + 1 < self.lines.len() {
                        Position::new(c.line + 1, 0)
                    } else {
                        c
                    }
                } else {
                    Position::new(c.line, word_right(self.line(c.line), c.column))
                }
            }
            Motion::Home => Position::new(c.line, 0),
            Motion::End => Position::new(c.line, char_len(self.line(c.line))),
            Motion::DocumentStart => Position::default(),
            Motion::DocumentEnd => self.end_position(),
        };
        let preferred = self.preferred_column;
        self.set_cursor(target, extend);
        if vertical {
            self.preferred_column = preferred.or(Some(self.display_column(c)));
        }
    }

    fn vertical(&mut self, delta: isize) -> Position {
        let c = self.cursor;
        let want = self
            .preferred_column
            .unwrap_or_else(|| self.display_column(c));
        self.preferred_column = Some(want);
        let last = self.lines.len().saturating_sub(1) as isize;
        let line = c.line as isize + delta;
        if line < 0 {
            return Position::new(0, 0);
        }
        if line > last {
            return self.end_position();
        }
        let line = line as usize;
        let column = column_for_display(self.line(line), want, self.tab_width);
        Position::new(line, column)
    }

    fn edit(&mut self, action: EditAction) -> Option<Edit> {
        let selection = self.selection();
        let c = self.cursor;
        match action {
            EditAction::Insert(ch) => {
                let mut buf = [0u8; 4];
                Some(self.replace_selection(ch.encode_utf8(&mut buf)))
            }
            EditAction::Paste(text) => {
                let text = normalize_newlines(&text);
                if text.is_empty() && selection.is_none() {
                    return None;
                }
                Some(self.replace_selection(&text))
            }
            EditAction::Enter => Some(self.replace_selection("\n")),
            EditAction::Backspace => {
                if let Some((s, e)) = selection {
                    return Some(self.replace_range(s, e, ""));
                }
                if c.column == 0 {
                    if c.line == 0 {
                        return None;
                    }
                    let prev = Position::new(c.line - 1, char_len(self.line(c.line - 1)));
                    return Some(self.replace_range(prev, c, ""));
                }
                let line = self.line(c.line);
                let mut start = c.column - 1;
                // Smart backspace: in leading spaces with insert-spaces,
                // delete back to the previous indentation stop.
                if self.insert_spaces {
                    let before: String = line.chars().take(c.column).collect();
                    if before.chars().all(|ch| ch == ' ') {
                        let stop = (c.column - 1) / self.tab_width * self.tab_width;
                        start = stop;
                    }
                }
                Some(self.replace_range(Position::new(c.line, start), c, ""))
            }
            EditAction::Delete => {
                if let Some((s, e)) = selection {
                    return Some(self.replace_range(s, e, ""));
                }
                let len = char_len(self.line(c.line));
                let end = if c.column < len {
                    Position::new(c.line, c.column + 1)
                } else if c.line + 1 < self.lines.len() {
                    Position::new(c.line + 1, 0)
                } else {
                    return None;
                };
                Some(self.replace_range(c, end, ""))
            }
            EditAction::DeleteWordBackward => {
                if let Some((s, e)) = selection {
                    return Some(self.replace_range(s, e, ""));
                }
                self.motion(Motion::WordLeft, false);
                let start = self.cursor;
                self.cursor = c;
                if start == c {
                    return None;
                }
                Some(self.replace_range(start, c, ""))
            }
            EditAction::DeleteWordForward => {
                if let Some((s, e)) = selection {
                    return Some(self.replace_range(s, e, ""));
                }
                self.motion(Motion::WordRight, false);
                let end = self.cursor;
                self.cursor = c;
                if end == c {
                    return None;
                }
                Some(self.replace_range(c, end, ""))
            }
            EditAction::Tab => match selection {
                Some((s, e)) if s.line != e.line => Some(self.indent_lines(s, e, true)),
                _ => {
                    let text = if self.insert_spaces {
                        let col = self.display_column(selection.map_or(c, |s| s.0));
                        " ".repeat(self.tab_width - col % self.tab_width)
                    } else {
                        "\t".to_owned()
                    };
                    Some(self.replace_selection(&text))
                }
            },
            EditAction::Unindent => {
                let (s, e) = selection.unwrap_or((c, c));
                let edit = self.indent_lines(s, e, false);
                if edit.removed_lines == edit.inserted_lines {
                    // Nothing to unindent; undo bookkeeping is pointless.
                    return None;
                }
                Some(edit)
            }
        }
    }

    fn replace_selection(&mut self, text: &str) -> Edit {
        let (s, e) = self.selection().unwrap_or((self.cursor, self.cursor));
        self.replace_range(s, e, text)
    }

    fn indent_lines(&mut self, s: Position, e: Position, indent: bool) -> Edit {
        // A selection ending at column 0 doesn't include that line.
        let last = if e.line > s.line && e.column == 0 {
            e.line - 1
        } else {
            e.line
        };
        let first = s.line;
        let unit = if self.insert_spaces {
            " ".repeat(self.tab_width)
        } else {
            "\t".to_owned()
        };
        let mut shifts = Vec::new();
        let new_lines: Vec<String> = (first..=last)
            .map(|i| {
                let line = self.line(i);
                if indent {
                    if line.is_empty() {
                        shifts.push(0isize);
                        String::new()
                    } else {
                        shifts.push(char_len(&unit) as isize);
                        format!("{unit}{line}")
                    }
                } else {
                    let n = if line.starts_with('\t') {
                        1
                    } else {
                        line.chars()
                            .take(self.tab_width)
                            .take_while(|c| *c == ' ')
                            .count()
                    };
                    shifts.push(-(n as isize));
                    line.chars().skip(n).collect()
                }
            })
            .collect();
        let shift = |p: Position| {
            if p.line < first || p.line > last {
                return p;
            }
            let d = shifts[p.line - first];
            let col = (p.column as isize + d).max(0) as usize;
            Position::new(p.line, if p.column == 0 && d > 0 { 0 } else { col })
        };
        let anchor = self.anchor.map(shift);
        let cursor = shift(self.cursor);
        let mut edit = self.replace_lines(first..last + 1, new_lines);
        self.anchor = anchor.map(|a| self.clamp(a));
        self.cursor = self.clamp(cursor);
        edit.cursor_after = self.cursor;
        edit
    }
}

/// Splits text into lines (see [`Buffer::from_text`]).
pub fn split_text(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let text = text.strip_suffix('\n').unwrap_or(text);
    text.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_owned())
        .collect()
}

fn normalize_newlines(text: &str) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text.to_owned()
    }
}

/// Number of characters in a string.
pub fn char_len(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.chars().count()
    }
}

/// Byte offset of character column `col` (clamped to the end).
pub fn byte_at(s: &str, col: usize) -> usize {
    if s.is_ascii() {
        return col.min(s.len());
    }
    s.char_indices().nth(col).map_or(s.len(), |(i, _)| i)
}

/// Display column of character column `col`, expanding tabs.
pub fn display_column(line: &str, col: usize, tab_width: usize) -> usize {
    let mut d = 0;
    for ch in line.chars().take(col) {
        d += if ch == '\t' {
            tab_width - d % tab_width
        } else {
            1
        };
    }
    d
}

fn line_display_width(line: &str, tab_width: usize) -> usize {
    if !line.contains('\t') {
        return char_len(line);
    }
    display_column(line, usize::MAX, tab_width)
}

/// Character column closest to display column `want`.
pub fn column_for_display(line: &str, want: usize, tab_width: usize) -> usize {
    let mut d = 0;
    for (i, ch) in line.chars().enumerate() {
        let w = if ch == '\t' {
            tab_width - d % tab_width
        } else {
            1
        };
        if d + w > want {
            // Pick the nearer edge of this character.
            return if want - d > w / 2 { i + 1 } else { i };
        }
        d += w;
    }
    char_len(line)
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum CharClass {
    Space,
    Word,
    Other,
}

fn class(c: char) -> CharClass {
    if c.is_whitespace() {
        CharClass::Space
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Other
    }
}

fn word_left(line: &str, col: usize) -> usize {
    let chars: Vec<char> = line.chars().take(col).collect();
    let mut i = chars.len();
    while i > 0 && class(chars[i - 1]) == CharClass::Space {
        i -= 1;
    }
    if i == 0 {
        return 0;
    }
    let k = class(chars[i - 1]);
    while i > 0 && class(chars[i - 1]) == k {
        i -= 1;
    }
    i
}

fn word_right(line: &str, col: usize) -> usize {
    let chars: Vec<char> = line.chars().collect();
    let mut i = col;
    while i < chars.len() && class(chars[i]) == CharClass::Space {
        i += 1;
    }
    if i == chars.len() {
        return i;
    }
    let k = class(chars[i]);
    while i < chars.len() && class(chars[i]) == k {
        i += 1;
    }
    i
}

/// Bounds of the run of same-class characters around `col`.
pub(crate) fn word_bounds(line: &str, col: usize) -> (usize, usize) {
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return (0, 0);
    }
    let at = if col < chars.len() {
        col
    } else {
        chars.len() - 1
    };
    // Prefer the word before the cursor when on a boundary to whitespace.
    let at = if col > 0
        && col <= chars.len()
        && (col == chars.len() || class(chars[col]) == CharClass::Space)
        && class(chars[col - 1]) != CharClass::Space
    {
        col - 1
    } else {
        at
    };
    let k = class(chars[at]);
    let mut s = at;
    while s > 0 && class(chars[s - 1]) == k {
        s -= 1;
    }
    let mut e = at + 1;
    while e < chars.len() && class(chars[e]) == k {
        e += 1;
    }
    (s, e)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(text: &str) -> Buffer {
        Buffer::new(text.split('\n').map(str::to_owned).collect())
    }

    fn p(line: usize, column: usize) -> Position {
        Position::new(line, column)
    }

    fn ins(c: char) -> Action {
        Action::Edit(EditAction::Insert(c))
    }

    #[test]
    fn split_text_handles_terminators() {
        assert_eq!(split_text(""), Vec::<String>::new());
        assert_eq!(split_text("a\nb\n"), vec!["a", "b"]);
        assert_eq!(split_text("a\r\nb"), vec!["a", "b"]);
        assert_eq!(split_text("\n"), vec![""]);
    }

    #[test]
    fn insert_char_produces_single_line_edit() {
        let mut b = buf("hello\nworld");
        b.set_cursor(p(0, 5), false);
        let e = b.perform(ins('!')).unwrap();
        assert_eq!(e.start_line, 0);
        assert_eq!(e.removed_lines, vec!["hello"]);
        assert_eq!(e.inserted_lines, vec!["hello!"]);
        assert_eq!(e.cursor_before, p(0, 5));
        assert_eq!(e.cursor_after, p(0, 6));
        assert_eq!(b.cursor(), p(0, 6));
        assert_eq!(e.line_delta(), 0);
    }

    #[test]
    fn enter_splits_line() {
        let mut b = buf("abcd");
        b.set_cursor(p(0, 2), false);
        let e = b.perform(Action::Edit(EditAction::Enter)).unwrap();
        assert_eq!(e.removed_lines, vec!["abcd"]);
        assert_eq!(e.inserted_lines, vec!["ab", "cd"]);
        assert_eq!(b.lines(), &["ab", "cd"]);
        assert_eq!(b.cursor(), p(1, 0));
        assert_eq!(e.line_delta(), 1);
    }

    #[test]
    fn backspace_joins_lines() {
        let mut b = buf("ab\ncd");
        b.set_cursor(p(1, 0), false);
        let e = b.perform(Action::Edit(EditAction::Backspace)).unwrap();
        assert_eq!(e.start_line, 0);
        assert_eq!(e.removed_lines, vec!["ab", "cd"]);
        assert_eq!(e.inserted_lines, vec!["abcd"]);
        assert_eq!(b.cursor(), p(0, 2));
        // At the very start nothing happens.
        b.set_cursor(p(0, 0), false);
        assert!(b.perform(Action::Edit(EditAction::Backspace)).is_none());
    }

    #[test]
    fn delete_joins_and_stops_at_end() {
        let mut b = buf("ab\ncd");
        b.set_cursor(p(0, 2), false);
        let e = b.perform(Action::Edit(EditAction::Delete)).unwrap();
        assert_eq!(e.inserted_lines, vec!["abcd"]);
        b.set_cursor(p(0, 4), false);
        assert!(b.perform(Action::Edit(EditAction::Delete)).is_none());
    }

    #[test]
    fn multiline_paste_replaces_selection() {
        let mut b = buf("one\ntwo\nthree");
        b.select(p(0, 1), p(2, 2));
        let e = b
            .perform(Action::Edit(EditAction::Paste(Arc::new("X\r\nY".into()))))
            .unwrap();
        assert_eq!(e.start_line, 0);
        assert_eq!(e.removed_lines, vec!["one", "two", "three"]);
        assert_eq!(e.inserted_lines, vec!["oX", "Yree"]);
        assert_eq!(b.cursor(), p(1, 1));
        assert!(b.selection().is_none());
    }

    #[test]
    fn inverse_restores() {
        let mut b = buf("one\ntwo\nthree");
        let orig = b.lines().to_vec();
        b.select(p(0, 1), p(1, 1));
        let e = b.perform(Action::Edit(EditAction::Enter)).unwrap();
        assert_ne!(b.lines(), orig.as_slice());
        b.apply(&e.inverse()).unwrap();
        assert_eq!(b.lines(), orig.as_slice());
        assert_eq!(b.cursor(), e.cursor_before);
        // Redo
        b.apply(&e).unwrap();
        assert_eq!(b.cursor(), e.cursor_after);
    }

    #[test]
    fn apply_checks_contents() {
        let mut b = buf("a\nb");
        let e = Edit {
            start_line: 1,
            removed_lines: vec!["zzz".into()],
            inserted_lines: vec![],
            cursor_before: p(0, 0),
            cursor_after: p(0, 0),
        };
        assert_eq!(b.apply(&e), Err(ApplyError::Mismatch { line: 1 }));
        let e = Edit {
            start_line: 1,
            removed_lines: vec!["b".into(), "c".into()],
            ..e
        };
        assert!(matches!(b.apply(&e), Err(ApplyError::OutOfBounds { .. })));
    }

    #[test]
    fn typing_into_empty_buffer() {
        let mut b = Buffer::default();
        assert_eq!(b.display_line_count(), 1);
        let e = b.perform(ins('x')).unwrap();
        assert_eq!(e.start_line, 0);
        assert!(e.removed_lines.is_empty());
        assert_eq!(e.inserted_lines, vec!["x"]);
        assert_eq!(b.lines(), &["x"]);
        b.apply(&e.inverse()).unwrap();
        assert_eq!(b.line_count(), 0);
    }

    #[test]
    fn replace_lines_adjusts_cursor() {
        let mut b = buf("a\nb\nc\nd");
        b.set_cursor(p(3, 1), false);
        let e = b.replace_lines(1..2, vec!["x".into(), "y".into(), "z".into()]);
        assert_eq!(e.removed_lines, vec!["b"]);
        assert_eq!(b.lines(), &["a", "x", "y", "z", "c", "d"]);
        assert_eq!(b.cursor(), p(5, 1));
        b.set_cursor(p(2, 1), false);
        b.replace_lines(1..4, vec![]);
        assert_eq!(b.cursor(), p(1, 0));
        assert_eq!(b.lines(), &["a", "c", "d"]);
        // Insertion at the end.
        b.replace_lines(3..3, vec!["e".into()]);
        assert_eq!(b.lines(), &["a", "c", "d", "e"]);
    }

    #[test]
    fn read_only_ignores_edits() {
        let mut b = buf("abc");
        b.set_read_only(true);
        assert!(b.perform(ins('x')).is_none());
        assert_eq!(b.lines(), &["abc"]);
        b.perform(Action::Move(Motion::End));
        assert_eq!(b.cursor(), p(0, 3));
    }

    #[test]
    fn motions() {
        let mut b = buf("foo bar_baz  qux\n\tx\nend");
        b.perform(Action::Move(Motion::WordRight));
        assert_eq!(b.cursor(), p(0, 3));
        b.perform(Action::Move(Motion::WordRight));
        assert_eq!(b.cursor(), p(0, 11));
        b.perform(Action::Move(Motion::WordRight));
        assert_eq!(b.cursor(), p(0, 16));
        b.perform(Action::Move(Motion::WordRight));
        assert_eq!(b.cursor(), p(1, 0));
        b.perform(Action::Move(Motion::WordLeft));
        assert_eq!(b.cursor(), p(0, 16));
        b.perform(Action::Move(Motion::WordLeft));
        assert_eq!(b.cursor(), p(0, 13));
        b.perform(Action::Move(Motion::Home));
        assert_eq!(b.cursor(), p(0, 0));
        b.perform(Action::Move(Motion::Left));
        assert_eq!(b.cursor(), p(0, 0));
        b.perform(Action::Move(Motion::DocumentEnd));
        assert_eq!(b.cursor(), p(2, 3));
        b.perform(Action::Move(Motion::Right));
        assert_eq!(b.cursor(), p(2, 3));
        b.perform(Action::Move(Motion::Left));
        assert_eq!(b.cursor(), p(2, 2));
    }

    #[test]
    fn vertical_motion_keeps_display_column() {
        let mut b = buf("0123456789\n\tab\n0123456789\nx");
        b.set_cursor(p(0, 9), false);
        b.perform(Action::Move(Motion::Down));
        // Tab spans display columns 0..8, 'a' at 8, 'b' at 9.
        assert_eq!(b.cursor(), p(1, 2));
        b.perform(Action::Move(Motion::Down));
        assert_eq!(b.cursor(), p(2, 9));
        b.perform(Action::Move(Motion::Down));
        assert_eq!(b.cursor(), p(3, 1));
        b.perform(Action::Move(Motion::Up));
        assert_eq!(b.cursor(), p(2, 9));
        b.perform(Action::Move(Motion::PageUp(10)));
        assert_eq!(b.cursor(), p(0, 0));
        b.perform(Action::Move(Motion::PageDown(10)));
        assert_eq!(b.cursor(), p(3, 1));
    }

    #[test]
    fn selection_with_shift_and_collapse() {
        let mut b = buf("hello world");
        b.set_cursor(p(0, 2), false);
        b.perform(Action::Select(Motion::WordRight));
        assert_eq!(b.selection(), Some((p(0, 2), p(0, 5))));
        assert_eq!(b.selected_text().as_deref(), Some("llo"));
        b.perform(Action::Move(Motion::Left));
        assert_eq!(b.cursor(), p(0, 2));
        assert!(b.selection().is_none());
        b.perform(Action::SelectAll);
        assert_eq!(b.selected_text().as_deref(), Some("hello world"));
        let e = b.perform(ins('X')).unwrap();
        assert_eq!(e.inserted_lines, vec!["X"]);
    }

    #[test]
    fn word_and_line_selection() {
        let mut b = buf("let foo_bar = 1;\nnext");
        b.perform(Action::SelectWord(p(0, 6)));
        assert_eq!(b.selected_text().as_deref(), Some("foo_bar"));
        b.perform(Action::SelectWord(p(0, 11)));
        assert_eq!(b.selected_text().as_deref(), Some("foo_bar"));
        b.perform(Action::SelectLine(p(0, 3)));
        assert_eq!(b.selection(), Some((p(0, 0), p(1, 0))));
        assert_eq!(b.selected_text().as_deref(), Some("let foo_bar = 1;\n"));
        b.perform(Action::SelectLine(p(1, 0)));
        assert_eq!(b.selected_text().as_deref(), Some("next"));
    }

    #[test]
    fn click_and_extend() {
        let mut b = buf("abc\ndef");
        b.perform(Action::Click(p(0, 1)));
        b.perform(Action::ExtendTo(p(1, 2)));
        assert_eq!(b.selected_text().as_deref(), Some("bc\nde"));
        b.perform(Action::Click(p(9, 9)));
        assert_eq!(b.cursor(), p(1, 3));
    }

    #[test]
    fn tab_and_spaces() {
        let mut b = buf("ab");
        b.set_cursor(p(0, 1), false);
        let e = b.perform(Action::Edit(EditAction::Tab)).unwrap();
        assert_eq!(e.inserted_lines, vec!["a\tb"]);
        let mut b = buf("ab");
        b.set_indentation(4, true);
        b.set_cursor(p(0, 1), false);
        b.perform(Action::Edit(EditAction::Tab)).unwrap();
        assert_eq!(b.lines(), &["a   b"]);
        assert_eq!(b.cursor(), p(0, 4));
        // Smart backspace removes back to the indent stop only in leading
        // whitespace.
        let mut b = buf("        x");
        b.set_indentation(4, true);
        b.set_cursor(p(0, 8), false);
        b.perform(Action::Edit(EditAction::Backspace)).unwrap();
        assert_eq!(b.lines(), &["    x"]);
        assert_eq!(b.cursor(), p(0, 4));
    }

    #[test]
    fn indent_and_unindent_selection() {
        let mut b = buf("a\n\nb\nc");
        b.set_indentation(4, true);
        b.select(p(0, 0), p(3, 0));
        let e = b.perform(Action::Edit(EditAction::Tab)).unwrap();
        assert_eq!(e.removed_range(), 0..3);
        assert_eq!(b.lines(), &["    a", "", "    b", "c"]);
        assert_eq!(b.selection(), Some((p(0, 0), p(3, 0))));
        b.perform(Action::Edit(EditAction::Unindent)).unwrap();
        assert_eq!(b.lines(), &["a", "", "b", "c"]);
        assert!(b.perform(Action::Edit(EditAction::Unindent)).is_none());
        let mut b = buf("\tx");
        b.set_cursor(p(0, 2), false);
        b.perform(Action::Edit(EditAction::Unindent)).unwrap();
        assert_eq!(b.lines(), &["x"]);
        assert_eq!(b.cursor(), p(0, 1));
    }

    #[test]
    fn delete_words() {
        let mut b = buf("foo bar baz");
        b.set_cursor(p(0, 7), false);
        let e = b
            .perform(Action::Edit(EditAction::DeleteWordBackward))
            .unwrap();
        assert_eq!(e.inserted_lines, vec!["foo  baz"]);
        assert_eq!(b.cursor(), p(0, 4));
        b.perform(Action::Edit(EditAction::DeleteWordForward))
            .unwrap();
        assert_eq!(b.lines(), &["foo "]);
    }

    #[test]
    fn unicode_columns() {
        let mut b = buf("héllo wörld");
        b.set_cursor(p(0, 2), false);
        b.perform(ins('X'));
        assert_eq!(b.lines(), &["héXllo wörld"]);
        b.select(p(0, 8), p(0, 10));
        assert_eq!(b.selected_text().as_deref(), Some("ör"));
        b.perform(Action::Move(Motion::End));
        assert_eq!(b.cursor(), p(0, 12));
    }

    #[test]
    fn widest_line_tracking() {
        let mut b = buf("ab\nabcd\n\tx");
        b.set_indentation(4, false);
        assert_eq!(b.max_display_columns(), 5);
        b.set_cursor(p(0, 2), false);
        b.perform(Action::Edit(EditAction::Paste(Arc::new("123456".into()))));
        assert_eq!(b.max_display_columns(), 8);
        b.perform(Action::SelectLine(p(0, 0)));
        b.perform(Action::Edit(EditAction::Delete));
        assert_eq!(b.max_display_columns(), 5);
    }

    #[test]
    fn revisions_bump() {
        let mut b = buf("x");
        let (r, c) = (b.revision(), b.cursor_revision());
        b.perform(Action::Move(Motion::End));
        assert_eq!(b.revision(), r);
        assert!(b.cursor_revision() > c);
        b.perform(ins('y'));
        assert!(b.revision() > r);
    }
}
