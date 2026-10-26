//! [`FileComparison`]: buffers, filters and a [`Differ`] kept in sync — the
//! non-GTK core of `meld/filediff.py`.

use regex::Regex;

use crate::actions::{ChunkAction, copy_chunk_with, delete_chunk_with, replace_chunk_with};
use crate::buffer::{AppliedEdit, LineChange, Position, TextBuffer, TextEdit};
use crate::chunk::{Chunk, Tag};
use crate::differ::{Differ, DiffsChanged, MergedChunk};
use crate::error::DiffError;
use crate::filters::filter_line;
use crate::inline::{InlineCache, exceeds_inline_limit, highlight_ranges};
use crate::merge::{auto_merge_cancellable, merge_2_files, merge_3_files};

/// The edits made to one pane by an operation, and the chunk changes after
/// each edit (process them in order, e.g. for inline highlighting).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditOutcome {
    pub pane: usize,
    /// Applied edits in order; their inverses (in reverse order) undo the
    /// operation. Record them as one undo group.
    pub edits: Vec<AppliedEdit>,
    pub changes: Vec<DiffsChanged>,
}

/// One inline (character-level) highlight range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InlineHighlight {
    pub pane: usize,
    pub start: Position,
    pub end: Position,
}

/// Inline highlights for one merged chunk.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InlineHighlights {
    pub ranges: Vec<InlineHighlight>,
    /// Some replace chunk exceeded [`crate::INLINE_LIMIT`]; it is highlighted
    /// whole (Meld then offers to force a proper highlight).
    pub too_long: bool,
}

/// A two- or three-way file comparison: the pane buffers, their filtered
/// lines and the differ, updated together after every edit.
///
/// All methods are synchronous. Construction runs the full diff; build the
/// comparison on a background task for large files.
#[derive(Clone, Debug)]
pub struct FileComparison {
    buffers: Vec<TextBuffer>,
    filtered: Vec<Vec<String>>,
    filters: Vec<Regex>,
    differ: Differ,
}

impl FileComparison {
    /// Compare `buffers` (1 to 3 panes), filtering lines with `filters`.
    #[cfg(test)]
    pub fn new(
        buffers: Vec<TextBuffer>,
        filters: Vec<Regex>,
        ignore_blanks: bool,
    ) -> Result<(Self, DiffsChanged), DiffError> {
        Self::new_cancellable(buffers, filters, ignore_blanks, &|| false)
    }

    /// Compare `buffers` (1 to 3 panes), filtering lines with `filters`,
    /// polling `cancel` during the diff.
    pub fn new_cancellable(
        buffers: Vec<TextBuffer>,
        filters: Vec<Regex>,
        ignore_blanks: bool,
        cancel: &dyn Fn() -> bool,
    ) -> Result<(Self, DiffsChanged), DiffError> {
        if buffers.len() > 3 {
            return Err(DiffError::TooManySequences(buffers.len()));
        }
        let mut differ = Differ::new();
        differ.ignore_blanks = ignore_blanks;
        let mut cmp = FileComparison {
            filtered: Vec::new(),
            buffers,
            filters,
            differ,
        };
        cmp.refilter();
        let changes = cmp
            .differ
            .set_sequences_cancellable(&cmp.filtered, cancel)?;
        Ok((cmp, changes))
    }

    /// Meld's auto-merge mode: merge panes 0 and 2 into pane 1 (marking
    /// conflicts with `(??)`), then compare the result.
    pub fn new_auto_merge(
        buffers: Vec<TextBuffer>,
        filters: Vec<Regex>,
        ignore_blanks: bool,
        cancel: &dyn Fn() -> bool,
    ) -> Result<(Self, DiffsChanged), DiffError> {
        if buffers.len() != 3 {
            return Err(DiffError::InvalidPane(buffers.len()));
        }
        let filtered: Vec<Vec<String>> = buffers
            .iter()
            .map(|b| b.lines().iter().map(|l| filter_line(l, &filters)).collect())
            .collect();
        let raw: Vec<&[String]> = buffers.iter().map(TextBuffer::lines).collect();
        let merged = auto_merge_cancellable(&filtered, &raw, cancel)?;
        let mut buffers = buffers;
        buffers[1] = TextBuffer::from_text(&merged.text());
        let (mut cmp, changes) = Self::new_cancellable(buffers, filters, ignore_blanks, cancel)?;
        cmp.differ.set_unresolved(merged.unresolved);
        Ok((cmp, changes))
    }

    fn refilter(&mut self) {
        let filters = &self.filters;
        self.filtered = self
            .buffers
            .iter()
            .map(|b| b.lines().iter().map(|l| filter_line(l, filters)).collect())
            .collect();
    }

    pub fn buffers(&self) -> &[TextBuffer] {
        &self.buffers
    }

    pub fn buffer(&self, pane: usize) -> Option<&TextBuffer> {
        self.buffers.get(pane)
    }

    pub fn differ(&self) -> &Differ {
        &self.differ
    }

    pub fn differ_mut(&mut self) -> &mut Differ {
        &mut self.differ
    }

    /// Re-run the whole comparison (Meld's refresh).
    pub fn rediff_cancellable(
        &mut self,
        cancel: &dyn Fn() -> bool,
    ) -> Result<DiffsChanged, DiffError> {
        self.refilter();
        self.differ
            .set_sequences_cancellable(&self.filtered, cancel)
    }

    /// Apply a user edit to `pane` and update the comparison.
    pub fn edit(&mut self, pane: usize, edit: &TextEdit) -> Result<EditOutcome, DiffError> {
        let mut outcome = EditOutcome {
            pane,
            ..Default::default()
        };
        let Self {
            buffers,
            filtered,
            filters,
            differ,
        } = self;
        let buf = buffers.get_mut(pane).ok_or(DiffError::InvalidPane(pane))?;
        let applied = buf.apply(edit);
        Self::after_edit(differ, filtered, filters, pane, buf, &applied, &mut outcome);
        Ok(outcome)
    }

    /// Replace the whole text of `pane` (a deletion and an insertion).
    pub fn set_text(&mut self, pane: usize, text: &str) -> Result<EditOutcome, DiffError> {
        let end = self.buffer(pane).ok_or(DiffError::InvalidPane(pane))?.end();
        let mut outcome = self.edit(
            pane,
            &TextEdit::Delete {
                start: Position::new(0, 0),
                end,
            },
        )?;
        let insert = self.edit(
            pane,
            &TextEdit::Insert {
                at: Position::new(0, 0),
                text: text.to_owned(),
            },
        )?;
        outcome.edits.extend(insert.edits);
        outcome.changes.extend(insert.changes);
        Ok(outcome)
    }

    /// Update filtered lines and the differ after an applied edit (the
    /// `insert-text` / `delete-range` handlers of FileDiff).
    fn after_edit(
        differ: &mut Differ,
        filtered: &mut [Vec<String>],
        filters: &[Regex],
        pane: usize,
        buf: &TextBuffer,
        applied: &AppliedEdit,
        outcome: &mut EditOutcome,
    ) {
        let LineChange {
            start_line,
            size_change,
        } = applied.change;
        let lines = &mut filtered[pane];
        let start = start_line.min(lines.len());
        let (old_end, new_end) = if size_change >= 0 {
            (start + 1, start + 1 + size_change as usize)
        } else {
            (start + 1 + size_change.unsigned_abs(), start + 1)
        };
        let replacement: Vec<String> = buf
            .slice(start, new_end)
            .iter()
            .map(|l| filter_line(l, filters))
            .collect();
        lines.splice(start..old_end.min(lines.len()), replacement);

        outcome.edits.push(applied.clone());
        if filtered.len() > 1
            && differ.syncpoints().is_empty()
            && let Ok(changes) = differ.change_sequence(pane, start_line, size_change, filtered)
        {
            outcome.changes.push(changes);
        }
    }

    /// Run a gutter chunk action. `chunk` is oriented from `from_pane` (`a`)
    /// to `to_pane` (`b`), as returned by [`Differ::get_chunk`].
    pub fn chunk_action(
        &mut self,
        action: ChunkAction,
        from_pane: usize,
        to_pane: usize,
        chunk: &Chunk,
    ) -> Result<EditOutcome, DiffError> {
        let n = self.buffers.len();
        for pane in [from_pane, to_pane] {
            if pane >= n {
                return Err(DiffError::InvalidPane(pane));
            }
        }
        let edited = if action == ChunkAction::Delete {
            from_pane
        } else {
            to_pane
        };
        if action != ChunkAction::Delete && from_pane == to_pane {
            return Err(DiffError::InvalidPane(to_pane));
        }
        let mut outcome = EditOutcome {
            pane: edited,
            ..Default::default()
        };
        let Self {
            buffers,
            filtered,
            filters,
            differ,
        } = self;
        let mut sink = |buf: &TextBuffer, applied: &AppliedEdit| {
            Self::after_edit(
                differ,
                filtered,
                filters,
                edited,
                buf,
                applied,
                &mut outcome,
            );
        };
        if action == ChunkAction::Delete {
            delete_chunk_with(&mut buffers[from_pane], chunk, &mut sink);
        } else {
            let (src, dst) = pair_mut(buffers, from_pane, to_pane);
            match action {
                ChunkAction::Replace => replace_chunk_with(src, dst, chunk, &mut sink),
                ChunkAction::CopyUp => copy_chunk_with(src, dst, chunk, true, &mut sink),
                ChunkAction::CopyDown => copy_chunk_with(src, dst, chunk, false, &mut sink),
                ChunkAction::Delete => {}
            }
        }
        Ok(outcome)
    }

    /// Merge all non-conflicting changes from panes 0 and 2 into pane 1
    /// (three-way "Merge All").
    pub fn merge_all_non_conflicting(&mut self) -> Result<EditOutcome, DiffError> {
        if self.buffers.len() != 3 {
            return Err(DiffError::InvalidPane(2));
        }
        let raw: Vec<&[String]> = self.buffers.iter().map(TextBuffer::lines).collect();
        let merged = merge_3_files(&self.differ, &raw, false);
        self.set_text(1, &merged.text())
    }

    /// Apply all non-conflicting changes of `src` onto `dst` ("Pull all
    /// non-conflicting changes from the left/right").
    pub fn pull_all_non_conflicting(
        &mut self,
        src: usize,
        dst: usize,
    ) -> Result<EditOutcome, DiffError> {
        for pane in [src, dst] {
            if pane >= self.buffers.len() {
                return Err(DiffError::InvalidPane(pane));
            }
        }
        let raw: Vec<&[String]> = self.buffers.iter().map(TextBuffer::lines).collect();
        let merged = merge_2_files(&self.differ, &raw, src, dst);
        self.set_text(dst, &merged.text())
    }

    /// Inline highlights of the replace chunks in `merged` (an entry of
    /// [`Differ::all_changes`]). Above [`crate::INLINE_LIMIT`] the chunk is
    /// highlighted whole unless `force` is set.
    pub fn inline_highlights(
        &self,
        cache: &mut InlineCache,
        merged: &MergedChunk,
        force: bool,
    ) -> InlineHighlights {
        let mut result = InlineHighlights::default();
        for (side, chunk) in merged.iter().enumerate() {
            let Some(chunk) = chunk.filter(|c| c.tag == Tag::Replace) else {
                continue;
            };
            let other_pane = if side == 1 { 2 } else { 0 };
            let (Some(middle), Some(other)) = (self.buffers.get(1), self.buffers.get(other_pane))
            else {
                continue;
            };
            let starts = [
                middle.iter_at_line_or_eof(chunk.start_a),
                other.iter_at_line_or_eof(chunk.start_b),
            ];
            let text1 = middle.lines_text(chunk.start_a, chunk.end_a);
            let textn = other.lines_text(chunk.start_b, chunk.end_b);
            if exceeds_inline_limit(&text1, &textn) && !force {
                result.too_long = true;
                result.ranges.push(InlineHighlight {
                    pane: 1,
                    start: starts[0],
                    end: middle.iter_at_line_or_eof(chunk.end_a),
                });
                result.ranges.push(InlineHighlight {
                    pane: other_pane,
                    start: starts[1],
                    end: other.iter_at_line_or_eof(chunk.end_b),
                });
                continue;
            }
            let opcodes = cache.get_or_compute(&text1, &textn);
            for m in highlight_ranges(&text1, &textn, &opcodes) {
                let spans = [
                    (1, middle, starts[0], m.start_a, m.end_a),
                    (other_pane, other, starts[1], m.start_b, m.end_b),
                ];
                for (pane, buf, start, lo, hi) in spans {
                    result.ranges.push(InlineHighlight {
                        pane,
                        start: buf.forward_chars(start, lo),
                        end: buf.forward_chars(start, hi),
                    });
                }
            }
        }
        result
    }
}

/// Mutable references to two distinct elements.
fn pair_mut(v: &mut [TextBuffer], a: usize, b: usize) -> (&TextBuffer, &mut TextBuffer) {
    if a < b {
        let (lo, hi) = v.split_at_mut(b);
        (&lo[a], &mut hi[0])
    } else {
        let (lo, hi) = v.split_at_mut(a);
        (&hi[0], &mut lo[b])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tb(lines: &[&str]) -> TextBuffer {
        TextBuffer::from_text(&lines.join("\n"))
    }

    fn fresh_diff(cmp: &FileComparison) -> Vec<MergedChunk> {
        let mut d = Differ::new();
        d.ignore_blanks = cmp.differ().ignore_blanks;
        d.set_sequences(&cmp.filtered).unwrap();
        d.all_changes().to_vec()
    }

    #[test]
    fn edits_keep_differ_in_sync() {
        let (mut cmp, changes) = FileComparison::new(
            vec![tb(&["a", "b", "c"]), tb(&["a", "x", "c"])],
            vec![],
            false,
        )
        .unwrap();
        assert_eq!(changes.added.len(), 1);
        let out = cmp
            .edit(
                1,
                &TextEdit::Insert {
                    at: Position::new(2, 0),
                    text: "new\n".into(),
                },
            )
            .unwrap();
        assert_eq!(out.changes.len(), 1);
        assert_eq!(cmp.buffer(1).unwrap().lines(), ["a", "x", "new", "c"]);
        assert_eq!(cmp.differ().all_changes(), fresh_diff(&cmp));

        // Pull the change from pane 0 into pane 1
        let chunk = cmp.differ().get_chunk(0, 0, Some(1)).unwrap();
        let out = cmp
            .chunk_action(ChunkAction::Replace, 0, 1, &chunk)
            .unwrap();
        assert_eq!(out.pane, 1);
        assert_eq!(out.edits.len(), 2);
        assert_eq!(cmp.buffer(1).unwrap().lines(), ["a", "b", "c"]);
        assert!(cmp.differ().all_changes().is_empty());

        // Undo by applying the inverses in reverse order
        for e in out.edits.iter().rev() {
            cmp.edit(1, &e.inverse).unwrap();
        }
        assert_eq!(cmp.buffer(1).unwrap().lines(), ["a", "x", "new", "c"]);
        assert_eq!(cmp.differ().all_changes(), fresh_diff(&cmp));
    }

    #[test]
    fn three_way_merge_all_and_conflicts() {
        let buffers = vec![
            tb(&["a", "L", "c", "x", "e"]),
            tb(&["a", "b", "c", "d", "e"]),
            tb(&["a", "b", "c", "y", "e", "R"]),
        ];
        let (mut cmp, _) = FileComparison::new(buffers.clone(), vec![], false).unwrap();
        assert_eq!(cmp.differ().conflicts().len(), 1);
        cmp.merge_all_non_conflicting().unwrap();
        assert_eq!(
            cmp.buffer(1).unwrap().lines(),
            ["a", "L", "c", "d", "e", "R"]
        );
        assert_eq!(cmp.differ().all_changes(), fresh_diff(&cmp));

        let (cmp, _) = FileComparison::new_auto_merge(buffers, vec![], false, &|| false).unwrap();
        assert_eq!(
            cmp.buffer(1).unwrap().lines(),
            ["a", "L", "c", "(??)d", "e", "R"]
        );
        assert_eq!(cmp.differ().unresolved(), &[3]);
    }

    #[test]
    fn filters_and_inline() {
        let filters = vec![Regex::new(r"#.*").unwrap()];
        let (mut cmp, _) = FileComparison::new(
            vec![tb(&["x = 1 # one", "y"]), tb(&["x = 1 # uno", "y"])],
            filters,
            false,
        )
        .unwrap();
        assert!(cmp.differ().all_changes().is_empty());
        cmp.filters = vec![];
        cmp.rediff_cancellable(&|| false).unwrap();
        let merged = cmp.differ().all_changes()[0];
        let mut cache = InlineCache::new();
        let hl = cmp.inline_highlights(&mut cache, &merged, false);
        assert!(!hl.too_long);
        assert!(
            hl.ranges
                .iter()
                .all(|r| r.start.line == 0 && r.end.line == 0)
        );
        assert!(hl.ranges.iter().any(|r| r.pane == 1) && hl.ranges.iter().any(|r| r.pane == 0));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn invalid_panes() {
        let (mut cmp, _) =
            FileComparison::new(vec![tb(&["a"]), tb(&["b"])], vec![], false).unwrap();
        let c = Chunk::new(Tag::Replace, 0, 1, 0, 1);
        assert_eq!(
            cmp.chunk_action(ChunkAction::Replace, 0, 2, &c),
            Err(DiffError::InvalidPane(2))
        );
        assert!(cmp.merge_all_non_conflicting().is_err());
        cmp.pull_all_non_conflicting(0, 1).unwrap();
        assert_eq!(cmp.buffer(1).unwrap().lines(), ["a"]);
    }
}
