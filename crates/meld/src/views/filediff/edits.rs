//! Keeping the widget buffers (what the panes show and edit) and the
//! diff engine's buffers in sync.
//!
//! Both hold the same lines: `meld_widgets::Buffer` without terminators,
//! `meld_diff::TextBuffer` with per-line terminators (GtkTextBuffer
//! semantics, so it always has at least one line). Columns differ:
//! widget positions count characters, diff positions count bytes.
//!
//! - A user edit in a pane is a line-level [`Edit`]; [`to_text_edits`]
//!   turns it into the minimal primitive insert/delete [`TextEdit`]s the
//!   differ updates incrementally from.
//! - Edits made by the diff engine (chunk actions, merges) are applied
//!   primitive edits; [`widget_replacement`] gives the equivalent widget
//!   line replacement, replayed one edit at a time.

use std::ops::Range;

use meld_diff::{Position, TextEdit};
use meld_widgets::Edit;

/// The primitive edits that turn the diff buffer (whose lines equal the
/// widget buffer's lines before `edit`) into the widget buffer after it.
///
/// `line_count` is the number of lines before the edit (at least one,
/// as in a `TextBuffer`); `newline` is the terminator for new line
/// breaks.
pub fn to_text_edits(edit: &Edit, line_count: usize, newline: &str) -> Vec<TextEdit> {
    let n = line_count.max(1);
    let start = edit.start_line;
    let removed = &edit.removed_lines;
    let inserted = &edit.inserted_lines;
    let nl = |s: String| -> String {
        if newline == "\n" {
            s
        } else {
            s.replace('\n', newline)
        }
    };

    match (removed.is_empty(), inserted.is_empty()) {
        (true, true) => Vec::new(),
        // Whole lines inserted before `start`.
        (true, false) => {
            let joined = inserted.join("\n");
            if start < n {
                vec![TextEdit::Insert {
                    at: Position::new(start, 0),
                    text: nl(joined + "\n"),
                }]
            } else {
                // Appending after the last line; the edit was made against
                // the widget buffer, whose last line is line n - 1.
                vec![TextEdit::Insert {
                    at: Position::new(n - 1, usize::MAX),
                    text: nl(format!("\n{joined}")),
                }]
            }
        }
        // Whole lines removed.
        (false, true) => {
            let end = start + removed.len();
            if end < n {
                vec![TextEdit::Delete {
                    start: Position::new(start, 0),
                    end: Position::new(end, 0),
                }]
            } else if start > 0 {
                vec![TextEdit::Delete {
                    start: Position::new(start - 1, usize::MAX),
                    end: Position::new(end - 1, usize::MAX),
                }]
            } else {
                vec![TextEdit::Delete {
                    start: Position::new(0, 0),
                    end: Position::new(end - 1, usize::MAX),
                }]
            }
        }
        (false, false) => {
            let old = removed.join("\n");
            let new = inserted.join("\n");
            let (prefix, old_suffix, new_suffix) = common_affixes(&old, &new);
            let mut edits = Vec::new();
            let del_start = offset_to_position(&old, start, prefix);
            if prefix < old_suffix {
                edits.push(TextEdit::Delete {
                    start: del_start,
                    end: offset_to_position(&old, start, old_suffix),
                });
            }
            if prefix < new_suffix {
                edits.push(TextEdit::Insert {
                    at: del_start,
                    text: nl(new[prefix..new_suffix].to_owned()),
                });
            }
            edits
        }
    }
}

/// Byte lengths of the common prefix of `a` and `b`, and the start of the
/// common suffix in each (not overlapping the prefix), on char
/// boundaries.
fn common_affixes(a: &str, b: &str) -> (usize, usize, usize) {
    let prefix = a
        .char_indices()
        .zip(b.chars())
        .take_while(|((_, x), y)| x == y)
        .last()
        .map_or(0, |((i, c), _)| i + c.len_utf8());
    let max_suffix = (a.len() - prefix).min(b.len() - prefix);
    let suffix = a[prefix..]
        .chars()
        .rev()
        .zip(b[prefix..].chars().rev())
        .take_while(|(x, y)| x == y)
        .map(|(c, _)| c.len_utf8())
        .scan(0, |acc, l| {
            *acc += l;
            Some(*acc)
        })
        .take_while(|&s| s <= max_suffix)
        .last()
        .unwrap_or(0);
    (prefix, a.len() - suffix, b.len() - suffix)
}

/// Position of byte `offset` of `text` (lines joined with `\n`) whose
/// first line is buffer line `first_line`.
fn offset_to_position(text: &str, first_line: usize, offset: usize) -> Position {
    let before = &text[..offset];
    let line = before.matches('\n').count();
    let col = match before.rfind('\n') {
        Some(i) => offset - i - 1,
        None => offset,
    };
    Position::new(first_line + line, col)
}

/// The widget line replacement equivalent to a primitive diff edit (as
/// applied, with clamped positions): the line range to replace and the
/// new lines.
pub fn widget_replacement(edit: &TextEdit, lines: &[String]) -> (Range<usize>, Vec<String>) {
    let line = |i: usize| lines.get(i).map_or("", String::as_str);
    let clamp = |l: &str, col: usize| {
        let mut col = col.min(l.len());
        while !l.is_char_boundary(col) {
            col -= 1;
        }
        col
    };
    let range_end = |end: usize| if lines.is_empty() { 0 } else { end + 1 };
    match edit {
        TextEdit::Insert { at, text } => {
            let l = line(at.line);
            let col = clamp(l, at.col);
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            let joined = format!("{}{}{}", &l[..col], text, &l[col..]);
            let start = at.line.min(lines.len());
            (
                start..range_end(start),
                joined.split('\n').map(str::to_owned).collect(),
            )
        }
        TextEdit::Delete { start, end } => {
            let (start, end) = if start <= end {
                (start, end)
            } else {
                (end, start)
            };
            let first = line(start.line);
            let last = line(end.line);
            let joined = format!(
                "{}{}",
                &first[..clamp(first, start.col)],
                &last[clamp(last, end.col)..]
            );
            let s = start.line.min(lines.len());
            (
                s..range_end(end.line.max(s)).min(lines.len()).max(s),
                vec![joined],
            )
        }
    }
}

/// Character column of byte offset `byte` in `line`.
pub fn char_column(line: &str, byte: usize) -> usize {
    let byte = byte.min(line.len());
    line.char_indices().take_while(|(i, _)| *i < byte).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use meld_diff::TextBuffer;
    use meld_widgets::Position as WPos;
    use meld_widgets::{Action, Buffer, EditAction, Motion};

    fn sync_check(widget: &Buffer, diff: &TextBuffer) {
        let mut w: Vec<String> = widget.lines().to_vec();
        if w.is_empty() {
            w.push(String::new());
        }
        assert_eq!(w, diff.lines());
    }

    fn apply(widget: &mut Buffer, diff: &mut TextBuffer, action: Action, newline: &str) {
        let count = widget.line_count();
        if let Some(edit) = widget.perform(action) {
            for e in to_text_edits(&edit, count, newline) {
                diff.apply(&e);
            }
        }
        sync_check(widget, diff);
    }

    #[test]
    fn typing_and_line_breaks_stay_in_sync() {
        let text = "héllo\nwörld\n\tx";
        let mut diff = TextBuffer::from_text(text);
        let mut widget = Buffer::new(diff.lines().to_vec());
        let ins = |c| Action::Edit(EditAction::Insert(c));
        apply(&mut widget, &mut diff, Action::Click(WPos::new(0, 2)), "\n");
        apply(&mut widget, &mut diff, ins('ü'), "\n");
        apply(
            &mut widget,
            &mut diff,
            Action::Edit(EditAction::Enter),
            "\n",
        );
        apply(
            &mut widget,
            &mut diff,
            Action::Edit(EditAction::Backspace),
            "\n",
        );
        apply(
            &mut widget,
            &mut diff,
            Action::Edit(EditAction::Backspace),
            "\n",
        );
        apply(
            &mut widget,
            &mut diff,
            Action::Move(Motion::DocumentEnd),
            "\n",
        );
        apply(
            &mut widget,
            &mut diff,
            Action::Edit(EditAction::Enter),
            "\n",
        );
        apply(&mut widget, &mut diff, ins('z'), "\n");
        apply(&mut widget, &mut diff, Action::SelectAll, "\n");
        apply(
            &mut widget,
            &mut diff,
            Action::Edit(EditAction::Paste(std::sync::Arc::new("a\nb\nc".into()))),
            "\n",
        );
        apply(&mut widget, &mut diff, Action::SelectAll, "\n");
        apply(
            &mut widget,
            &mut diff,
            Action::Edit(EditAction::Delete),
            "\n",
        );
        assert_eq!(diff.text(), "");
    }

    #[test]
    fn crlf_newlines_are_used_for_new_breaks() {
        let mut diff = TextBuffer::from_text("a\r\nb");
        let mut widget = Buffer::new(diff.lines().to_vec());
        apply(
            &mut widget,
            &mut diff,
            Action::Click(WPos::new(0, 1)),
            "\r\n",
        );
        apply(
            &mut widget,
            &mut diff,
            Action::Edit(EditAction::Enter),
            "\r\n",
        );
        assert_eq!(diff.text(), "a\r\n\r\nb");
    }

    #[test]
    fn whole_line_edits() {
        let mut diff = TextBuffer::from_text("a\nb\nc");
        let mut widget = Buffer::new(diff.lines().to_vec());
        for (range, lines) in [
            (1..1, vec!["x".to_string(), "y".to_string()]),
            (5..5, vec!["end".to_string()]),
            (0..2, vec![]),
            (2..4, vec![]),
            (0..1, vec!["only".to_string()]),
        ] {
            let count = widget.line_count();
            let edit = widget.replace_lines(range, lines);
            for e in to_text_edits(&edit, count, "\n") {
                diff.apply(&e);
            }
            sync_check(&widget, &diff);
            // The inverse restores the previous state too.
            let undo = edit.inverse();
            let count = widget.line_count();
            widget.apply(&undo).unwrap();
            for e in to_text_edits(&undo, count, "\n") {
                diff.apply(&e);
            }
            sync_check(&widget, &diff);
            let count = widget.line_count();
            widget.apply(&edit).unwrap();
            for e in to_text_edits(&edit, count, "\n") {
                diff.apply(&e);
            }
            sync_check(&widget, &diff);
        }
    }

    #[test]
    fn minimal_edits() {
        let edit = Edit {
            start_line: 3,
            removed_lines: vec!["abcdef".into()],
            inserted_lines: vec!["abXdef".into()],
            cursor_before: WPos::default(),
            cursor_after: WPos::default(),
        };
        assert_eq!(
            to_text_edits(&edit, 10, "\n"),
            vec![
                TextEdit::Delete {
                    start: Position::new(3, 2),
                    end: Position::new(3, 3)
                },
                TextEdit::Insert {
                    at: Position::new(3, 2),
                    text: "X".into()
                }
            ]
        );
        // Repeated characters: the suffix must not overlap the prefix.
        let edit = Edit {
            start_line: 0,
            removed_lines: vec!["aa".into()],
            inserted_lines: vec!["aaa".into()],
            cursor_before: WPos::default(),
            cursor_after: WPos::default(),
        };
        assert_eq!(
            to_text_edits(&edit, 1, "\n"),
            vec![TextEdit::Insert {
                at: Position::new(0, 2),
                text: "a".into()
            }]
        );
    }

    #[test]
    fn diff_edits_map_back_to_widget_lines() {
        let src = TextBuffer::from_text("1\n2\n3\n4");
        let mut dst = TextBuffer::from_text("1\nx\n4");
        let mut widget = Buffer::new(dst.lines().to_vec());
        let chunk = meld_diff::Chunk::new(meld_diff::Tag::Replace, 1, 3, 1, 2);
        let edits = meld_diff::replace_chunk(&src, &mut dst, &chunk);
        for applied in &edits {
            let (range, lines) = widget_replacement(&applied.edit, widget.lines());
            widget.replace_lines(range, lines);
        }
        assert_eq!(widget.lines(), dst.lines());
        // Deleting the last lines and CRLF insertions.
        let mut dst = TextBuffer::from_text("a\r\nb\r\nc");
        let mut widget = Buffer::new(dst.lines().to_vec());
        let chunk = meld_diff::Chunk::new(meld_diff::Tag::Delete, 1, 3, 1, 1);
        for applied in meld_diff::delete_chunk(&mut dst, &chunk) {
            let (range, lines) = widget_replacement(&applied.edit, widget.lines());
            widget.replace_lines(range, lines);
        }
        assert_eq!(widget.lines(), dst.lines());
        let applied = dst.insert(Position::new(0, 1), "x\r\ny\rz");
        let (range, lines) = widget_replacement(&applied.edit, widget.lines());
        widget.replace_lines(range, lines);
        assert_eq!(widget.lines(), dst.lines());
    }

    #[test]
    fn columns() {
        assert_eq!(char_column("héllo", 3), 2);
    }
}
