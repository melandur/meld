//! Chunk actions on [`TextBuffer`]s (the buffer logic of
//! `FileDiff.replace_chunk`, `copy_chunk` and `delete_chunk`).
//!
//! Each function applies its edits and returns them in order, so the
//! caller can update the differ after each one (see
//! [`crate::FileComparison`], which does this) and record undo actions.

use crate::buffer::{AppliedEdit, TextBuffer};
use crate::chunk::Chunk;

/// The chunk actions offered in the gutter (`meld.const.ChunkAction`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChunkAction {
    /// Delete the chunk from the source pane.
    Delete,
    /// Replace the destination lines with the source lines.
    Replace,
    /// Insert the source lines after the destination lines.
    CopyDown,
    /// Insert the source lines before the destination lines.
    CopyUp,
}

/// Called after each primitive edit with the edited buffer's new state.
pub type EditSink<'f> = &'f mut dyn FnMut(&TextBuffer, &AppliedEdit);

fn collect(f: impl FnOnce(EditSink<'_>)) -> Vec<AppliedEdit> {
    let mut edits = Vec::new();
    f(&mut |_, e| edits.push(e.clone()));
    edits
}

fn apply_insert_at_line(buf: &mut TextBuffer, line: usize, text: &str, sink: EditSink<'_>) {
    let edit = buf.insert_at_line(line, text);
    sink(buf, &edit);
}

/// Replace `chunk`'s `b` lines in `dst` with its `a` lines from `src`.
pub fn replace_chunk(src: &TextBuffer, dst: &mut TextBuffer, chunk: &Chunk) -> Vec<AppliedEdit> {
    collect(|sink| replace_chunk_with(src, dst, chunk, sink))
}

/// [`replace_chunk`], reporting each edit to `sink` as it is applied.
pub fn replace_chunk_with(
    src: &TextBuffer,
    dst: &mut TextBuffer,
    chunk: &Chunk,
    sink: EditSink<'_>,
) {
    let t0 = src.lines_text(chunk.start_a, chunk.end_a);
    let dst_start = dst.iter_at_line_or_eof(chunk.start_b);
    let dst_end = dst.iter_at_line_or_eof(chunk.end_b);
    let delete = dst.delete(dst_start, dst_end);
    sink(dst, &delete);
    apply_insert_at_line(dst, chunk.start_b, &t0, sink);
}

/// Copy `chunk`'s `a` lines from `src` into `dst`, before (`copy_up`) or
/// after the chunk's `b` lines.
pub fn copy_chunk(
    src: &TextBuffer,
    dst: &mut TextBuffer,
    chunk: &Chunk,
    copy_up: bool,
) -> Vec<AppliedEdit> {
    collect(|sink| copy_chunk_with(src, dst, chunk, copy_up, sink))
}

/// [`copy_chunk`], reporting each edit to `sink` as it is applied.
pub fn copy_chunk_with(
    src: &TextBuffer,
    dst: &mut TextBuffer,
    chunk: &Chunk,
    copy_up: bool,
    sink: EditSink<'_>,
) {
    let mut t0 = src.lines_text(chunk.start_a, chunk.end_a);
    if copy_up {
        if chunk.end_a >= src.line_count() && chunk.start_b < dst.line_count() {
            // TODO (as in Meld): we can't be certain which line break to use.
            t0.push('\n');
        }
        apply_insert_at_line(dst, chunk.start_b, &t0, sink);
    } else {
        apply_insert_at_line(dst, chunk.end_b, &t0, sink);
    }
}

/// Delete `chunk`'s `a` lines from `buf`. At the end of the buffer the
/// preceding line break goes too, since the last line has none.
pub fn delete_chunk(buf: &mut TextBuffer, chunk: &Chunk) -> Vec<AppliedEdit> {
    collect(|sink| delete_chunk_with(buf, chunk, sink))
}

/// [`delete_chunk`], reporting the edit to `sink`.
pub fn delete_chunk_with(buf: &mut TextBuffer, chunk: &Chunk, sink: EditSink<'_>) {
    let mut start = buf.iter_at_line_or_eof(chunk.start_a);
    if chunk.end_a >= buf.line_count() {
        start = buf.backward_cursor_position(start);
    }
    let end = buf.iter_at_line_or_eof(chunk.end_a);
    let edit = buf.delete(start, end);
    sink(buf, &edit);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::Tag;

    // Ported from test/test_chunk_actions.py. The newline type of the
    // buffer is irrelevant: deleting the last line removes the preceding
    // line break, whatever it is.
    #[test]
    fn delete_last_line_crlf() {
        let cases = [
            ("ree\r\neee", "ree"),
            ("ree\r\neee\r\n", "ree\r\neee"),
            ("ree\neee", "ree"),
            ("ree\neee\n", "ree\neee"),
            ("ree\reee", "ree"),
            ("ree\reee\r", "ree\reee"),
            ("ree\r\neee\n", "ree\r\neee"),
            ("ree\r\neee\nqqq", "ree\r\neee"),
            ("ree\r\neee\nqqq\r\n", "ree\r\neee\nqqq"),
        ];
        for (text, expected) in cases {
            let mut buf = TextBuffer::from_text(text);
            let end = buf.line_count();
            let last = end - 1;
            delete_chunk(&mut buf, &Chunk::new(Tag::Delete, last, end, last, end));
            assert_eq!(buf.text(), expected, "{text:?}");
        }
    }

    fn buf(lines: &[&str]) -> TextBuffer {
        TextBuffer::from_text(&lines.join("\n"))
    }

    #[test]
    fn replace_and_copy() {
        let src = buf(&["a", "B", "c"]);
        let mut dst = buf(&["a", "X", "Y", "c"]);
        replace_chunk(&src, &mut dst, &Chunk::new(Tag::Replace, 1, 2, 1, 3));
        assert_eq!(dst.lines(), ["a", "B", "c"]);

        let mut dst = buf(&["a", "X", "c"]);
        copy_chunk(&src, &mut dst, &Chunk::new(Tag::Replace, 1, 2, 1, 2), true);
        assert_eq!(dst.lines(), ["a", "B", "X", "c"]);
        let mut dst = buf(&["a", "X", "c"]);
        copy_chunk(&src, &mut dst, &Chunk::new(Tag::Replace, 1, 2, 1, 2), false);
        assert_eq!(dst.lines(), ["a", "X", "B", "c"]);

        // Copying the last line (without a line break) up adds one
        let mut dst = buf(&["X", "c"]);
        copy_chunk(&src, &mut dst, &Chunk::new(Tag::Replace, 2, 3, 0, 1), true);
        assert_eq!(dst.lines(), ["c", "X", "c"]);

        // Replacing at the end of the buffer
        let mut dst = buf(&["a", "B"]);
        replace_chunk(&src, &mut dst, &Chunk::new(Tag::Insert, 2, 3, 2, 2));
        assert_eq!(dst.lines(), ["a", "B", "c"]);

        // Deleting in the middle
        let mut b = buf(&["a", "B", "c"]);
        delete_chunk(&mut b, &Chunk::new(Tag::Delete, 1, 2, 1, 1));
        assert_eq!(b.lines(), ["a", "c"]);
    }
}
