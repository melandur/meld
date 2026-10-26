//! `meld-diff`: Meld's GUI-free diff and merge engine.
//!
//! Replaces `meld/matchers/myers.py`, `meld/matchers/diffutil.py`,
//! `meld/matchers/merge.py`, the cache of `meld/matchers/helpers.py`,
//! `meld/undo.py`, the model of `meld/syncpoints.py`, the patch output of
//! `meld/patchdialog.py` (with the parts of Python's `difflib` it relies on),
//! `misc.apply_text_filters`/`merge_intervals`, the buffer logic of
//! `MeldBuffer`, and the buffer/differ glue of `meld/filediff.py`
//! (edits, chunk actions, merge actions, inline highlighting).
//!
//! Conventions: lines are `String`s without terminators; line numbers are
//! 0-based and ranges half-open. Every type is `Clone + Debug + Send`.
//!
//! # Public API
//!
//! - Chunks: [`Tag`] (`Equal`, `Replace`, `Delete`, `Insert`, `Conflict`),
//!   [`Chunk`] `{ tag, start_a, end_a, start_b, end_b }`, [`MatchingBlock`],
//!   [`opcodes_from_blocks`].
//! - Matchers ([`myers`]): [`MyersSequenceMatcher`] (`new`, `inline`,
//!   `with_syncpoints`, `run(cancel)`, `matching_blocks`, `opcodes`,
//!   `difference_opcodes`), and the shortcuts [`myers::opcodes`],
//!   [`myers::difference_opcodes`], [`inline_opcodes`] (char offsets).
//! - [`Differ`]: `set_sequences(&[lines])`, `set_sequences_cancellable`,
//!   `change_sequence(pane, start_line, size_change, &texts)`, all
//!   returning [`DiffsChanged`] (replaces the `diffs-changed` signal);
//!   queries `all_changes` (`&[MergedChunk]`), `locate_chunk(pane, line)`
//!   ([`ChunkLocation`] current/prev/next), `get_chunk(index, from, to)`,
//!   `get_chunk_starts`, `pair_changes`, `single_changes`, `conflicts`,
//!   `has_mergeable_changes`, `sequences_identical`, `diff_count`,
//!   `unresolved`;
//!   settings `ignore_blanks`, `auto_merge`, `set_syncpoints`.
//!   [`consume_blank_lines`], [`merged_chunk_order`].
//! - Merging ([`merge`]): [`merge_3_files`], [`merge_2_files`],
//!   [`auto_merge`] returning [`Merged`] `{ lines, unresolved }`.
//! - Inline highlighting ([`inline`]): [`inline_highlights`],
//!   [`highlight_ranges`], [`InlineCache`] (LRU by text pair),
//!   [`INLINE_LIMIT`].
//! - Buffers ([`buffer`]): [`TextBuffer`] (GtkTextBuffer line semantics,
//!   `insert`/`delete`/`apply` returning [`AppliedEdit`] with inverse and
//!   [`LineChange`]), [`Position`], [`TextEdit`], [`splitlines`].
//! - Chunk actions ([`actions`]): [`ChunkAction`], [`replace_chunk`],
//!   [`delete_chunk`] (and `*_with` variants reporting each edit, including
//!   `copy_chunk_with`).
//! - [`FileComparison`]: buffers + filters + differ kept in sync:
//!   `new_cancellable`, `new_auto_merge`, `edit`, `set_text`,
//!   `chunk_action`, `merge_all_non_conflicting`,
//!   `pull_all_non_conflicting`, `rediff_cancellable`,
//!   `inline_highlights`, returning [`EditOutcome`]s.
//! - Text filters ([`filters`]): [`apply_text_filters`], [`filter_line`],
//!   [`merge_intervals`].
//! - Patches: [`make_patch`] (patch dialog output), [`patch_lines`],
//!   [`patch_names`], and [`difflib`]'s [`SequenceMatcher`],
//!   [`unified_diff`]/[`unified_diff_lines`] (byte-identical to Python).
//! - [`UndoSequence<A: UndoAction>`](UndoSequence): `add_action`,
//!   `begin_group`/`end_group`, `undo`/`redo` (return the actions to
//!   revert/re-apply), `checkpoint`, `checkpointed`, `take_events`
//!   ([`UndoEvent`]).
//! - [`Syncpoints`]: `add`, `move_unmatched`, `remove`, `clear`,
//!   `valid_points`, `action` ([`SyncpointAction`]), `pane_state`
//!   ([`PaneState`]), `adjust_for_edit`.
//! - Errors: [`DiffError`].

pub mod actions;
pub mod buffer;
pub mod chunk;
pub mod differ;
pub mod difflib;
pub mod error;
pub mod filters;
pub mod inline;
pub mod merge;
pub mod myers;
pub mod patch;
pub mod session;
pub mod syncpoints;
pub mod undo;

pub use actions::{ChunkAction, delete_chunk, replace_chunk};
pub use buffer::{AppliedEdit, LineChange, Position, TextBuffer, TextEdit, splitlines};
pub use chunk::{Chunk, MatchingBlock, Tag, opcodes_from_blocks};
pub use differ::{
    ChunkLocation, Differ, DiffsChanged, MergedChunk, consume_blank_lines, merged_chunk_order,
};
pub use difflib::{SequenceMatcher, UnifiedDiffOptions, unified_diff, unified_diff_lines};
pub use error::DiffError;
pub use filters::{FilteredText, apply_text_filters, filter_line, merge_intervals};
pub use inline::{
    INLINE_LIMIT, InlineCache, exceeds_inline_limit, highlight_ranges, inline_highlights,
};
pub use merge::{CONFLICT_MARKER, Merged, auto_merge, merge_2_files, merge_3_files};
pub use myers::{MatcherKind, MyersSequenceMatcher, inline_opcodes};
pub use patch::{PatchSide, make_patch, patch_lines, patch_names};
pub use session::{EditOutcome, FileComparison, InlineHighlight, InlineHighlights};
pub use syncpoints::{PaneState, SyncpointAction, Syncpoints};
pub use undo::{UndoAction, UndoEvent, UndoSequence};

#[cfg(test)]
mod send_tests {
    fn assert_traits<T: Send + Sync + Clone + std::fmt::Debug>() {}

    #[test]
    fn types_are_send() {
        assert_traits::<crate::Differ>();
        assert_traits::<crate::FileComparison>();
        assert_traits::<crate::TextBuffer>();
        assert_traits::<crate::InlineCache>();
        assert_traits::<crate::Syncpoints>();
        assert_traits::<crate::Merged>();
        assert_traits::<crate::DiffsChanged>();
        assert_traits::<crate::UndoSequence<PaneEdit>>();
    }

    #[derive(Clone, Debug)]
    struct PaneEdit(usize);

    impl crate::UndoAction for PaneEdit {
        fn buffer(&self) -> usize {
            self.0
        }
    }
}
