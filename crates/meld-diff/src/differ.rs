//! Two- and three-pane chunk bookkeeping (port of `meld/matchers/diffutil.py`
//! plus `AutoMergeDiffer` from `meld/matchers/merge.py`).
//!
//! Internally, diffs are stored from pane 1 to pane 0 and from pane 1 to
//! pane 2: in a [`MergedChunk`] `[left, right]`, the `a` side of each chunk
//! is the middle pane (pane 1 in three-way comparisons; in two-way
//! comparisons `a` is pane 1 and `b` is pane 0, as in Python).

use std::collections::{HashSet, VecDeque};

use crate::chunk::{Chunk, Tag, py_slice, py_splice};
use crate::error::DiffError;
use crate::myers::MyersSequenceMatcher;

/// One entry of the merge cache: the change between pane 1 and pane 0 and
/// the change between pane 1 and pane 2 at the same place. At least one is
/// present; two-way comparisons only use the first.
pub type MergedChunk = [Option<Chunk>; 2];

/// The `start_a` of a merged chunk, for sorting (`merged_chunk_order`).
pub fn merged_chunk_order(merged: &MergedChunk) -> usize {
    merged[0].or(merged[1]).map_or(0, |c| c.start_a)
}

/// What changed in the chunk set; replaces the `diffs-changed` signal.
///
/// The inline highlighter clears `removed` and highlights `added` and
/// `modified` (the chunk where the edit happened, if it survived).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffsChanged {
    pub removed: Vec<MergedChunk>,
    pub added: Vec<MergedChunk>,
    pub modified: Option<MergedChunk>,
}

/// Current, previous and next chunk index for a line (`locate_chunk`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ChunkLocation {
    /// The merge-cache index of the chunk containing the line, if any.
    pub current: Option<usize>,
    /// The closest chunk before the line.
    pub prev: Option<usize>,
    /// The closest chunk after the line.
    pub next: Option<usize>,
}

/// A chunk whose coordinates may be negative: the expected positions of
/// old chunks after an edit, which Python computes without clamping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct SignedChunk {
    tag: Tag,
    start_a: isize,
    end_a: isize,
    start_b: isize,
    end_b: isize,
}

impl From<Chunk> for SignedChunk {
    fn from(c: Chunk) -> Self {
        SignedChunk {
            tag: c.tag,
            start_a: c.start_a as isize,
            end_a: c.end_a as isize,
            start_b: c.start_b as isize,
            end_b: c.end_b as isize,
        }
    }
}

impl SignedChunk {
    fn clamped(&self) -> Chunk {
        let z = |v: isize| v.max(0) as usize;
        Chunk::new(
            self.tag,
            z(self.start_a),
            z(self.end_a),
            z(self.start_b),
            z(self.end_b),
        )
    }
}

type SignedMerged = [Option<SignedChunk>; 2];

fn signed_merged(m: &MergedChunk) -> SignedMerged {
    [m[0].map(SignedChunk::from), m[1].map(SignedChunk::from)]
}

fn clamped_merged(m: &SignedMerged) -> MergedChunk {
    [m[0].map(|c| c.clamped()), m[1].map(|c| c.clamped())]
}

/// Trim blank lines from both ends of a chunk (`consume_blank_lines`).
///
/// Returns `None` if the chunk consisted only of blank lines.
pub fn consume_blank_lines<S: AsRef<[String]>>(
    chunk: Option<Chunk>,
    texts: &[S],
    pane1: usize,
    pane2: usize,
) -> Option<Chunk> {
    let chunk = chunk?;
    fn find_blank_lines(txt: Option<&[String]>, mut lo: usize, mut hi: usize) -> (usize, usize) {
        let blank = |i: usize| txt.and_then(|t| t.get(i)).is_some_and(|l| l.is_empty());
        while lo < hi && blank(lo) {
            lo += 1;
        }
        while lo < hi && blank(hi - 1) {
            hi -= 1;
        }
        (lo, hi)
    }
    let mut tag = chunk.tag;
    let (c1, c2) = find_blank_lines(
        texts.get(pane1).map(|t| t.as_ref()),
        chunk.start_a,
        chunk.end_a,
    );
    let (c3, c4) = find_blank_lines(
        texts.get(pane2).map(|t| t.as_ref()),
        chunk.start_b,
        chunk.end_b,
    );
    if c1 == c2 && c3 == c4 {
        return None;
    }
    if c1 == c2 && tag == Tag::Replace {
        tag = Tag::Insert;
    } else if c3 == c4 && tag == Tag::Replace {
        tag = Tag::Delete;
    }
    Some(Chunk::new(tag, c1, c2, c3, c4))
}

/// Holds the chunks of a two- or three-way comparison (`diffutil.Differ`).
///
/// Build it with [`Differ::set_sequences`] (synchronous; run it off the UI
/// thread for big files), then keep it current with
/// [`Differ::change_sequence`] after each edit. With `auto_merge` enabled it
/// behaves like Python's `AutoMergeDiffer(auto_merge=True)`, resolving more
/// three-way conflicts automatically; it also tracks unresolved conflict
/// lines (the `(??)` markers) of an automatic merge.
#[derive(Clone, Debug)]
pub struct Differ {
    num_sequences: usize,
    seqlength: Vec<usize>,
    diffs: [Vec<Chunk>; 2],
    syncpoints: Vec<Vec<usize>>,
    conflicts: Vec<usize>,
    old_merge_cache: Vec<SignedMerged>,
    changed_chunks: Option<SignedMerged>,
    merge_cache: Vec<MergedChunk>,
    line_cache: [Vec<ChunkLocation>; 3],
    /// Ignore changes consisting only of blank lines (`ignore-blank-lines`).
    pub ignore_blanks: bool,
    /// Resolve more conflicts automatically (`AutoMergeDiffer.auto_merge`).
    pub auto_merge: bool,
    initialised: bool,
    has_mergeable_changes: [bool; 4],
    unresolved: Vec<usize>,
}

impl Default for Differ {
    fn default() -> Self {
        Self::new()
    }
}

impl Differ {
    pub fn new() -> Self {
        Differ {
            num_sequences: 0,
            seqlength: vec![0, 0, 0],
            diffs: [Vec::new(), Vec::new()],
            syncpoints: Vec::new(),
            conflicts: Vec::new(),
            old_merge_cache: Vec::new(),
            changed_chunks: None,
            merge_cache: Vec::new(),
            line_cache: [Vec::new(), Vec::new(), Vec::new()],
            ignore_blanks: false,
            auto_merge: false,
            initialised: false,
            has_mergeable_changes: [false; 4],
            unresolved: Vec::new(),
        }
    }

    /// Number of sequences being compared (0 before the first comparison).
    pub fn num_sequences(&self) -> usize {
        self.num_sequences
    }

    /// Set sync points: each point lists one line per pane (`[l0, l1]` or
    /// `[l0, l1, l2]`). They take effect on the next [`Differ::set_sequences`].
    /// Meld disables live updates ([`Differ::change_sequence`]) while sync
    /// points are set.
    pub fn set_syncpoints(&mut self, points: Vec<Vec<usize>>) {
        self.syncpoints = points;
    }

    pub fn syncpoints(&self) -> &[Vec<usize>] {
        &self.syncpoints
    }

    /// Compare `sequences` (2 or 3 line lists) from scratch.
    pub fn set_sequences<S: AsRef<[String]>>(
        &mut self,
        sequences: &[S],
    ) -> Result<DiffsChanged, DiffError> {
        self.set_sequences_cancellable(sequences, &|| false)
    }

    /// Like [`Differ::set_sequences`], polling `cancel` during the diff. On
    /// cancellation the differ keeps its previous state.
    pub fn set_sequences_cancellable<S: AsRef<[String]>>(
        &mut self,
        sequences: &[S],
        cancel: &dyn Fn() -> bool,
    ) -> Result<DiffsChanged, DiffError> {
        if sequences.len() > 3 {
            return Err(DiffError::TooManySequences(sequences.len()));
        }
        let mut diffs = [Vec::new(), Vec::new()];
        for (i, diff) in diffs
            .iter_mut()
            .enumerate()
            .take(sequences.len().saturating_sub(1))
        {
            let middle = sequences[1].as_ref();
            let other = sequences[i * 2].as_ref();
            let mut matcher = if self.syncpoints.is_empty() {
                MyersSequenceMatcher::new(middle, other)
            } else {
                let points = self
                    .syncpoints
                    .iter()
                    .filter_map(|p| Some((*p.get(1)?, *p.get(i * 2)?)))
                    .collect();
                MyersSequenceMatcher::with_syncpoints(middle, other, points)
            };
            matcher.run(cancel)?;
            *diff = matcher.difference_opcodes();
        }
        self.diffs = diffs;
        self.num_sequences = sequences.len();
        self.seqlength = sequences.iter().map(|s| s.as_ref().len()).collect();
        self.initialised = true;
        Ok(self.update_merge_cache(sequences))
    }

    /// Forget all chunks (`clear`).
    pub fn clear(&mut self) -> DiffsChanged {
        self.diffs = [Vec::new(), Vec::new()];
        self.seqlength = vec![0; self.num_sequences];
        self.initialised = false;
        self.old_merge_cache.clear();
        let empty: Vec<Vec<String>> = vec![Vec::new(); self.num_sequences];
        self.update_merge_cache(&empty)
    }

    /// Update the chunks after an edit in pane `sequence`: `sizechange`
    /// lines were added (positive) or removed (negative) starting at line
    /// `startidx`. `texts` are the (filtered) lines of all panes *after* the
    /// edit. Port of `Differ.change_sequence` / `AutoMergeDiffer.change_sequence`.
    pub fn change_sequence<S: AsRef<[String]>>(
        &mut self,
        sequence: usize,
        startidx: usize,
        sizechange: isize,
        texts: &[S],
    ) -> Result<DiffsChanged, DiffError> {
        if sequence > 2 || sequence >= self.seqlength.len() {
            return Err(DiffError::InvalidPane(sequence));
        }
        if sequence == 1 {
            self.update_unresolved(startidx, sizechange);
        }
        if sequence == 0 || sequence == 1 {
            self.change_one_diff(0, sequence, startidx, sizechange, texts);
        }
        if sequence == 2 || (sequence == 1 && self.num_sequences == 3) {
            self.change_one_diff(1, sequence, startidx, sizechange, texts);
        }
        self.seqlength[sequence] = add_signed(self.seqlength[sequence], sizechange);

        // Offset a chunk by o1/o2 if it's after the inserted lines. As in
        // Python this may go negative for chunks inside deleted lines.
        let offset = |c: Option<Chunk>, start: usize, o1: isize, o2: isize| {
            c.map(|c| {
                let shift = |v: usize, o: isize| v as isize + if v > start { o } else { 0 };
                SignedChunk {
                    tag: c.tag,
                    start_a: shift(c.start_a, o1),
                    end_a: shift(c.end_a, o1),
                    start_b: shift(c.start_b, o2),
                    end_b: shift(c.end_b, o2),
                }
            })
        };

        // Calculate the expected differences in the chunk set if no cascading
        // changes occur, making sure to not include the changed chunk itself
        self.old_merge_cache.clear();
        self.changed_chunks = None;
        for &[c1, c2] in &self.merge_cache {
            let chunk_changed;
            let (mut s1, mut s2) = (c1.map(SignedChunk::from), c2.map(SignedChunk::from));
            if sequence == 0 {
                chunk_changed = c1.is_some_and(|c| c.start_b <= startidx && startidx < c.end_b);
                s1 = offset(c1, startidx, 0, sizechange);
            } else if sequence == 2 {
                chunk_changed = c2.is_some_and(|c| c.start_b <= startidx && startidx < c.end_b);
                s2 = offset(c2, startidx, 0, sizechange);
            } else {
                // Middle sequence changes alter both chunks
                chunk_changed = c1.is_some_and(|c| c.start_a <= startidx && startidx < c.end_a);
                s1 = offset(c1, startidx, sizechange, 0);
                if self.num_sequences == 3 {
                    s2 = offset(c2, startidx, sizechange, 0);
                }
            }
            let (c1, c2) = (s1, s2);
            if chunk_changed && self.changed_chunks.is_none() {
                self.changed_chunks = Some([c1, c2]);
            }
            self.old_merge_cache.push([c1, c2]);
        }

        Ok(self.update_merge_cache(texts))
    }

    /// `AutoMergeDiffer.change_sequence`'s bookkeeping of unresolved lines.
    fn update_unresolved(&mut self, startidx: usize, sizechange: isize) {
        let unresolved = &mut self.unresolved;
        let lo = unresolved.iter().take_while(|&&c| startidx > c).count();
        if lo >= unresolved.len() {
            return;
        }
        let mut hi = lo;
        if sizechange < 0 {
            let limit = startidx + sizechange.unsigned_abs();
            hi += unresolved[lo..].iter().take_while(|&&c| limit > c).count();
        } else if sizechange == 0 && startidx == unresolved[lo] {
            hi += 1;
        }
        for c in unresolved.iter_mut().skip(hi) {
            *c = add_signed(*c, sizechange);
        }
        unresolved.drain(lo..hi);
    }

    fn change_one_diff<S: AsRef<[String]>>(
        &mut self,
        which: usize,
        sequence: usize,
        startidx: usize,
        sizechange: isize,
        texts: &[S],
    ) {
        let mut lines_added = [0isize; 3];
        lines_added[sequence] = sizechange;
        let mut loidx = self.locate_chunk_in_diff(which, sequence, startidx);
        let mut hiidx = if sizechange < 0 {
            self.locate_chunk_in_diff(which, sequence, startidx + sizechange.unsigned_abs())
        } else {
            loidx
        };
        let diffs = &self.diffs[which];
        let lorange = if loidx > 0 {
            loidx -= 1;
            (diffs[loidx].start_b, diffs[loidx].start_a)
        } else {
            (0, 0)
        };
        let x = which * 2;
        let hirange = if hiidx < diffs.len() {
            hiidx += 1;
            (diffs[hiidx - 1].end_b, diffs[hiidx - 1].end_a)
        } else {
            (
                self.seqlength.get(x).copied().unwrap_or(0),
                self.seqlength.get(1).copied().unwrap_or(0),
            )
        };
        let rangex = (lorange.0, add_signed(hirange.0, lines_added[x]));
        let range1 = (lorange.1, add_signed(hirange.1, lines_added[1]));
        let empty: &[String] = &[];
        let textx = texts.get(x).map_or(empty, |t| t.as_ref());
        let text1 = texts.get(1).map_or(empty, |t| t.as_ref());
        let linesx = py_slice(textx, rangex.0, rangex.1);
        let lines1 = py_slice(text1, range1.0, range1.1);

        let newdiffs: Vec<Chunk> = MyersSequenceMatcher::new(lines1, linesx)
            .difference_opcodes()
            .into_iter()
            .map(|c| c.offset(range1.0, rangex.0))
            .collect();

        let diffs = &mut self.diffs[which];
        for c in diffs.iter_mut().skip(hiidx) {
            *c = Chunk::new(
                c.tag,
                add_signed(c.start_a, lines_added[1]),
                add_signed(c.end_a, lines_added[1]),
                add_signed(c.start_b, lines_added[x]),
                add_signed(c.end_b, lines_added[x]),
            );
        }
        py_splice(diffs, loidx, hiidx, newdiffs);
    }

    /// Index of the first chunk of `diffs[which]` ending after `line`.
    fn locate_chunk_in_diff(&self, which: usize, sequence: usize, line: usize) -> usize {
        let diffs = &self.diffs[which];
        diffs
            .iter()
            .position(|c| {
                let high = if sequence != 1 { c.end_b } else { c.end_a };
                line < high
            })
            .unwrap_or(diffs.len())
    }

    fn update_merge_cache<S: AsRef<[String]>>(&mut self, texts: &[S]) -> DiffsChanged {
        let mut merge_cache = if self.num_sequences == 3 {
            self.merge_diffs(texts)
        } else {
            self.diffs[0].iter().map(|&c| [Some(c), None]).collect()
        };

        if self.ignore_blanks {
            // We don't handle altering the chunk-type of conflicts in
            // three-way comparisons where e.g., pane 1 and 3 differ in blank
            // lines
            merge_cache = merge_cache
                .into_iter()
                .map(|[c0, c1]| {
                    [
                        consume_blank_lines(c0, texts, 1, 0),
                        consume_blank_lines(c1, texts, 1, 2),
                    ]
                })
                .filter(|m| m.iter().any(Option::is_some))
                .collect();
        }
        self.merge_cache = merge_cache;

        // Chunks that were added (in the new but not the old merge cache),
        // removed (in the old but not the new) and changed (where the edit
        // actually occurred, *and* the chunk is still around).
        // The comparison happens in signed space, like Python's; reported
        // chunks are clamped at line 0.
        let new_set: HashSet<SignedMerged> = self.merge_cache.iter().map(signed_merged).collect();
        let old_set: HashSet<SignedMerged> = self.old_merge_cache.iter().copied().collect();
        let mut seen = HashSet::new();
        let removed_signed: Vec<SignedMerged> = self
            .old_merge_cache
            .iter()
            .filter(|c| !new_set.contains(*c) && seen.insert(**c))
            .copied()
            .collect();
        let mut removed: Vec<MergedChunk> = removed_signed.iter().map(clamped_merged).collect();
        removed.sort_by_key(merged_chunk_order);
        let mut seen = HashSet::new();
        let added: Vec<MergedChunk> = self
            .merge_cache
            .iter()
            .filter(|c| !old_set.contains(&signed_merged(c)) && seen.insert(**c))
            .copied()
            .collect();
        let modified = self
            .changed_chunks
            .filter(|m| !removed_signed.contains(m))
            .map(|m| clamped_merged(&m));
        let changes = DiffsChanged {
            removed,
            added,
            modified,
        };

        let mut mergeable = [false, false];
        for [c0, c1] in &self.merge_cache {
            mergeable[0] = mergeable[0] || c0.is_some_and(|c| c.tag != Tag::Conflict);
            mergeable[1] = mergeable[1] || c1.is_some_and(|c| c.tag != Tag::Conflict);
            if mergeable[0] && mergeable[1] {
                break;
            }
        }
        self.has_mergeable_changes = [false, mergeable[0], mergeable[1], false];

        // Conflicts can only occur when there are three panes, and will
        // always involve the middle pane.
        self.conflicts = self
            .merge_cache
            .iter()
            .enumerate()
            .filter(|(_, m)| m.iter().flatten().any(|c| c.tag == Tag::Conflict))
            .map(|(i, _)| i)
            .collect();

        self.update_line_cache();
        changes
    }

    /// Cache a mapping from line index to per-pane chunk indices, so that
    /// current/next/previous chunk queries are cheap.
    fn update_line_cache(&mut self) {
        for (i, &length) in self.seqlength.iter().enumerate().take(3) {
            // seqlength + 1 for after-last-line requests
            self.line_cache[i] = vec![ChunkLocation::default(); length + 1];
        }

        let merge_cache = &self.merge_cache;
        let last_chunk = merge_cache.len();
        let find_next = |diff: usize, seq: usize, current: Option<usize>| -> Option<usize> {
            let from = current.map_or(0, |c| c + 1);
            if seq == 1 && from < last_chunk {
                return Some(from);
            }
            (from..last_chunk).find(|&j| merge_cache[j][diff].is_some())
        };

        let mut prev: [Option<usize>; 3] = [None; 3];
        let mut next = [
            find_next(0, 0, None),
            find_next(0, 1, None),
            find_next(1, 2, None),
        ];
        let mut old_end = [0usize; 3];
        // (diff, seq, use the b side)
        const SEQ_PARAMS: [(usize, usize, bool); 3] = [(0, 0, true), (0, 1, false), (1, 2, true)];

        for (i, c) in merge_cache.iter().enumerate() {
            for &(diff, seq, side_b) in &SEQ_PARAMS {
                let mut diff = diff;
                if c[diff].is_none() {
                    if seq == 1 {
                        diff = 1;
                    } else {
                        continue;
                    }
                }
                let Some(chunk) = c[diff] else { continue };
                let (start, mut end) = if side_b {
                    (chunk.start_b, chunk.end_b)
                } else {
                    (chunk.start_a, chunk.end_a)
                };
                let last = old_end[seq];
                let cache = &mut self.line_cache[seq];
                if start > last {
                    let loc = ChunkLocation {
                        current: None,
                        prev: prev[seq],
                        next: next[seq],
                    };
                    py_splice(cache, last, start, std::iter::repeat_n(loc, start - last));
                }
                // For insert chunks, claim the subsequent line.
                if start == end {
                    end += 1;
                }
                next[seq] = find_next(diff, seq, Some(i));
                let loc = ChunkLocation {
                    current: Some(i),
                    prev: prev[seq],
                    next: next[seq],
                };
                py_splice(
                    cache,
                    start,
                    end,
                    std::iter::repeat_n(loc, end.saturating_sub(start)),
                );
                prev[seq] = Some(i);
                old_end[seq] = end;
            }
        }

        for seq in 0..3 {
            let cache = &mut self.line_cache[seq];
            let (last, end) = (old_end[seq], cache.len());
            if last < end {
                let loc = ChunkLocation {
                    current: None,
                    prev: prev[seq],
                    next: next[seq],
                };
                py_splice(cache, last, end, std::iter::repeat_n(loc, end - last));
            }
        }
    }

    /// Chunk containing `line` in `pane`, with previous/next chunk indices
    /// into [`Differ::all_changes`]. Lines without a cache entry give all
    /// `None`.
    pub fn locate_chunk(&self, pane: usize, line: usize) -> ChunkLocation {
        self.line_cache
            .get(pane)
            .and_then(|c| c.get(line))
            .copied()
            .unwrap_or_default()
    }

    /// Whether `chunk` (as passed to a chunk action towards `to_pane`) is
    /// still part of the comparison (`has_chunk`).
    pub fn has_chunk(&self, to_pane: usize, chunk: &Chunk) -> bool {
        let sequence = usize::from(to_pane == 2);
        let Some(index) = self.locate_chunk(1, chunk.start_a).current else {
            return false;
        };
        self.merge_cache
            .get(index)
            .is_some_and(|m| m[sequence].as_ref() == Some(chunk))
    }

    /// The `index`-th change as seen from `from_pane` (`get_chunk`).
    ///
    /// If `to_pane` is given, only changes between `from_pane` and `to_pane`
    /// are considered, otherwise all changes starting at `from_pane`. The
    /// `a` side of the result is `from_pane`.
    pub fn get_chunk(
        &self,
        index: usize,
        from_pane: usize,
        to_pane: Option<usize>,
    ) -> Option<Chunk> {
        let sequence = usize::from(from_pane == 2 || to_pane == Some(2));
        let merged = self.merge_cache.get(index)?;
        let chunk = merged[sequence];
        if from_pane == 0 || from_pane == 2 {
            chunk.map(|c| c.reversed())
        } else if to_pane.is_none() && chunk.is_none() {
            merged[1]
        } else {
            chunk
        }
    }

    /// Starting lines of all chunks at `index`, per pane (`get_chunk_starts`).
    pub fn get_chunk_starts(&self, index: usize) -> [Option<usize>; 3] {
        let Some([c0, c1]) = self.merge_cache.get(index) else {
            return [None; 3];
        };
        [
            c0.map(|c| c.start_b),
            c0.map(|c| c.start_a),
            c1.map(|c| c.start_b),
        ]
    }

    /// Number of entries in the merge cache (`diff_count`).
    pub fn diff_count(&self) -> usize {
        self.merge_cache.len()
    }

    /// Merge-cache indices of conflicting chunks.
    pub fn conflicts(&self) -> &[usize] {
        &self.conflicts
    }

    /// Whether non-conflicting changes can be merged into `pane` from its
    /// left and right neighbour (`has_mergeable_changes`).
    pub fn has_mergeable_changes(&self, pane: usize) -> (bool, bool) {
        let get = |i: usize| self.has_mergeable_changes.get(i).copied().unwrap_or(false);
        (get(pane), get(pane + 1))
    }

    /// The whole merge cache (`all_changes`).
    pub fn all_changes(&self) -> &[MergedChunk] {
        &self.merge_cache
    }

    /// The raw diffs from pane 1 to pane 0 (`0`) and to pane 2 (`1`).
    pub fn diffs(&self, which: usize) -> &[Chunk] {
        self.diffs.get(which).map_or(&[], |d| d.as_slice())
    }

    fn range_from_lines(
        &self,
        textindex: usize,
        lo: usize,
        hi: usize,
    ) -> (Option<usize>, Option<usize>) {
        let top = self.locate_chunk(textindex, lo);
        let bottom = self.locate_chunk(textindex, hi);
        (top.current.or(top.next), bottom.current.or(bottom.prev))
    }

    /// Changes between `fromindex` and `toindex`, oriented with `a` on
    /// `fromindex` (`pair_changes`). With `lines = Some([lo0, hi0, lo1, hi1])`
    /// only chunks touching those line ranges (in `fromindex` and `toindex`
    /// respectively) are returned.
    pub fn pair_changes(
        &self,
        fromindex: usize,
        toindex: usize,
        lines: Option<[usize; 4]>,
    ) -> Vec<Chunk> {
        let merge_cache: &[MergedChunk] = match lines {
            Some([lo0, hi0, lo1, hi1]) => {
                let (start1, end1) = self.range_from_lines(fromindex, lo0, hi0);
                let (start2, end2) = self.range_from_lines(toindex, lo1, hi1);
                if (start1.is_none() || end1.is_none()) && (start2.is_none() || end2.is_none()) {
                    return Vec::new();
                }
                let start = start1.into_iter().chain(start2).min().unwrap_or(0);
                let end = end1.into_iter().chain(end2).max().unwrap_or(0);
                py_slice(&self.merge_cache, start, end + 1)
            }
            None => &self.merge_cache,
        };
        Self::oriented(merge_cache, fromindex, toindex)
    }

    fn oriented(merge_cache: &[MergedChunk], fromindex: usize, toindex: usize) -> Vec<Chunk> {
        if fromindex == 1 {
            let seq = (toindex / 2).min(1);
            merge_cache.iter().filter_map(|c| c[seq]).collect()
        } else {
            let seq = (fromindex / 2).min(1);
            merge_cache
                .iter()
                .filter_map(|c| c[seq])
                .map(|c| c.reversed())
                .collect()
        }
    }

    /// All changes between `fromindex` and `toindex` (`paired_all_single_changes`).
    pub fn paired_all_single_changes(&self, fromindex: usize, toindex: usize) -> Vec<Chunk> {
        Self::oriented(&self.merge_cache, fromindex, toindex)
    }

    /// Changes of a single pane, oriented with `a` on that pane; no `equal`
    /// chunks (`single_changes`). For the middle pane, each entry is its
    /// change against pane 0, or against pane 2 if there is none.
    pub fn single_changes(&self, textindex: usize, lines: Option<(usize, usize)>) -> Vec<Chunk> {
        let merge_cache: &[MergedChunk] = match lines {
            Some((lo, hi)) => {
                let (start, end) = self.range_from_lines(textindex, lo, hi);
                let (Some(start), Some(end)) = (start, end) else {
                    return Vec::new();
                };
                py_slice(&self.merge_cache, start, end + 1)
            }
            None => &self.merge_cache,
        };
        if textindex == 0 || textindex == 2 {
            let seq = textindex / 2;
            merge_cache
                .iter()
                .filter_map(|c| c[seq])
                .map(|c| c.reversed())
                .collect()
        } else {
            merge_cache.iter().filter_map(|c| c[0].or(c[1])).collect()
        }
    }

    /// Whether the compared sequences are identical. An uninitialised
    /// comparison is never identical.
    pub fn sequences_identical(&self) -> bool {
        self.diffs[0].is_empty() && self.diffs[1].is_empty() && self.initialised
    }

    /// Line numbers (in the middle pane) of unresolved `(??)` conflict
    /// markers left by an automatic merge.
    pub fn unresolved(&self) -> &[usize] {
        &self.unresolved
    }

    /// Number of unresolved conflict lines (`get_unresolved_count`).
    pub fn unresolved_count(&self) -> usize {
        self.unresolved.len()
    }

    /// Set the unresolved lines, e.g. from [`crate::merge::Merged::unresolved`].
    pub fn set_unresolved(&mut self, unresolved: Vec<usize>) {
        self.unresolved = unresolved;
    }

    /// Combine pane1->pane0 and pane1->pane2 diffs into the three-way merge
    /// cache (`_merge_diffs`).
    fn merge_diffs<S: AsRef<[String]>>(&self, texts: &[S]) -> Vec<MergedChunk> {
        let mut seq: [VecDeque<Chunk>; 2] = [
            self.diffs[0].iter().copied().collect(),
            self.diffs[1].iter().copied().collect(),
        ];
        let mut out = Vec::new();
        while !seq[0].is_empty() || !seq[1].is_empty() {
            let mut high_seq = match (seq[0].front(), seq[1].front()) {
                (None, _) => 1,
                (_, None) => 0,
                (Some(c0), Some(c1)) => {
                    let mut h = usize::from(c0.start_a > c1.start_a);
                    if c0.start_a == c1.start_a {
                        if c0.tag == Tag::Insert {
                            h = 0;
                        } else if c1.tag == Tag::Insert {
                            h = 1;
                        }
                    }
                    h
                }
            };
            let Some(high_diff) = seq[high_seq].pop_front() else {
                break;
            };
            let mut high_mark = high_diff.end_a;
            let mut other_seq = 1 - high_seq;

            let mut using: [Vec<Chunk>; 2] = [Vec::new(), Vec::new()];
            using[high_seq].push(high_diff);

            while let Some(&other_diff) = seq[other_seq].front() {
                if high_mark < other_diff.start_a {
                    break;
                }
                if high_mark == other_diff.start_a
                    && !(high_diff.tag == Tag::Insert && other_diff.tag == Tag::Insert)
                {
                    break;
                }
                using[other_seq].push(other_diff);
                seq[other_seq].pop_front();

                if high_mark < other_diff.end_a {
                    std::mem::swap(&mut high_seq, &mut other_seq);
                    high_mark = other_diff.end_a;
                }
            }

            if using[0].is_empty() {
                out.push([None, using[1].first().copied()]);
            } else if using[1].is_empty() {
                out.push([using[0].first().copied(), None]);
            } else {
                self.auto_merge_blocks(using, texts, &mut out);
            }
        }
        out
    }

    /// `_merge_blocks`: the combined ranges `(l0, h0, l1, h1, l2, h2)`.
    fn merge_blocks(using: &[Vec<Chunk>; 2]) -> [usize; 6] {
        let lowc = using[0][0].start_a.min(using[1][0].start_a);
        let highc = using[0][using[0].len() - 1]
            .end_a
            .max(using[1][using[1].len() - 1].end_a);
        let mut low = [0; 2];
        let mut high = [0; 2];
        for i in 0..2 {
            let d = using[i][0];
            low[i] = (lowc + d.start_b).saturating_sub(d.start_a);
            let d = using[i][using[i].len() - 1];
            high[i] = (highc + d.end_b).saturating_sub(d.end_a);
        }
        [low[0], high[0], lowc, highc, low[1], high[1]]
    }

    /// `Differ._auto_merge`, extended by `AutoMergeDiffer._auto_merge` when
    /// `auto_merge` is set.
    fn auto_merge_blocks<S: AsRef<[String]>>(
        &self,
        mut using: [Vec<Chunk>; 2],
        texts: &[S],
        out: &mut Vec<MergedChunk>,
    ) {
        let [l0, h0, l1, h1, l2, h2] = Self::merge_blocks(&using);
        let empty: &[String] = &[];
        let text0 = texts.first().map_or(empty, |t| t.as_ref());
        let text2 = texts.get(2).map_or(empty, |t| t.as_ref());
        let len0 = h0.saturating_sub(l0);
        let len1 = h1.saturating_sub(l1);
        let len2 = h2.saturating_sub(l2);
        let tag = if len0 == len2 && py_slice(text0, l0, h0) == py_slice(text2, l2, h2) {
            if l1 != h1 && l0 == h0 {
                Tag::Delete
            } else if l1 != h1 {
                Tag::Replace
            } else {
                Tag::Insert
            }
        } else {
            Tag::Conflict
        };
        let out0 = Chunk::new(tag, l1, h1, l0, h0);
        let out1 = Chunk::new(tag, l1, h1, l2, h2);

        if !(self.auto_merge && tag == Tag::Conflict) {
            out.push([Some(out0), Some(out1)]);
            return;
        }

        // Try to resolve more complex conflicts automatically
        if (len0 > 0 && len2 > 0) && (len0 == len1 || len2 == len1 || len1 == 0) {
            let a = py_slice(text0, l0, h0);
            let b = py_slice(text2, l2, h2);
            for chunk in MyersSequenceMatcher::new(a, b).opcodes() {
                let (mut s1, mut e1) = (l1, l1);
                if len0 == len1 {
                    s1 += chunk.start_a;
                    e1 += chunk.end_a;
                } else if len2 == len1 {
                    s1 += chunk.start_b;
                    e1 += chunk.end_b;
                }
                let tag = if chunk.tag == Tag::Equal {
                    Tag::Replace
                } else {
                    Tag::Conflict
                };
                out.push([
                    Some(Chunk::new(
                        tag,
                        s1,
                        e1,
                        l0 + chunk.start_a,
                        l0 + chunk.end_a,
                    )),
                    Some(Chunk::new(
                        tag,
                        s1,
                        e1,
                        l2 + chunk.start_b,
                        l2 + chunk.end_b,
                    )),
                ]);
            }
            return;
        }

        // Some tricks to resolve even more conflicts automatically. The
        // resulting chunks cannot be used to highlight changes, but they are
        // good enough to merge the resulting file.
        let chunktype = using[0][0].tag;
        let all_same = using.iter().flatten().all(|c| c.tag == chunktype);
        if !(all_same && chunktype == Tag::Delete) {
            out.push([Some(out0), Some(out1)]);
            return;
        }

        // delete + delete -> split into delete/conflict
        let mut queues: [VecDeque<Chunk>; 2] =
            [using[0].drain(..).collect(), using[1].drain(..).collect()];
        let mut seq0: Option<Chunk> = None;
        let mut seq1: Option<Chunk> = None;
        let (mut i0, mut end0, mut i1, mut end1) = (0usize, 0usize, 0usize, 0usize);
        loop {
            if seq0.is_none() {
                let Some(c) = queues[0].pop_front() else {
                    break;
                };
                i0 = c.start_a;
                end0 = c.end_b;
                seq0 = Some(c);
            }
            if seq1.is_none() {
                let Some(c) = queues[1].pop_front() else {
                    break;
                };
                i1 = c.start_a;
                end1 = c.end_b;
                seq1 = Some(c);
            }
            let (Some(s0), Some(s1)) = (seq0, seq1) else {
                break;
            };
            let highstart = i0.max(i1);
            if i0 != i1 {
                out.push([
                    Some(Chunk::new(
                        Tag::Conflict,
                        (i0 + i1).saturating_sub(highstart),
                        highstart,
                        (s0.start_b + i1).saturating_sub(highstart),
                        s0.start_b,
                    )),
                    Some(Chunk::new(
                        Tag::Conflict,
                        (i1 + i0).saturating_sub(highstart),
                        highstart,
                        (s1.start_b + i0).saturating_sub(highstart),
                        s1.start_b,
                    )),
                ]);
            }
            let lowend = s0.end_a.min(s1.end_a);
            if highstart != lowend {
                out.push([
                    Some(Chunk::new(
                        Tag::Delete,
                        highstart,
                        lowend,
                        s0.start_b,
                        s0.end_b,
                    )),
                    Some(Chunk::new(
                        Tag::Delete,
                        highstart,
                        lowend,
                        s1.start_b,
                        s1.end_b,
                    )),
                ]);
            }
            i0 = lowend;
            i1 = lowend;
            if lowend == s0.end_a {
                seq0 = None;
            }
            if lowend == s1.end_a {
                seq1 = None;
            }
        }

        if let Some(s0) = seq0 {
            out.push([
                Some(Chunk::new(
                    Tag::Conflict,
                    i0,
                    s0.end_a,
                    s0.start_b,
                    s0.end_b,
                )),
                Some(Chunk::new(
                    Tag::Conflict,
                    i0,
                    s0.end_a,
                    end1,
                    (end1 + s0.end_a).saturating_sub(i0),
                )),
            ]);
        } else if let Some(s1) = seq1 {
            out.push([
                Some(Chunk::new(
                    Tag::Conflict,
                    i1,
                    s1.end_a,
                    end0,
                    (end0 + s1.end_a).saturating_sub(i1),
                )),
                Some(Chunk::new(
                    Tag::Conflict,
                    i1,
                    s1.end_a,
                    s1.start_b,
                    s1.end_b,
                )),
            ]);
        }
    }
}

/// `v + delta`, clamped at zero.
pub(crate) fn add_signed(v: usize, delta: isize) -> usize {
    if delta >= 0 {
        v + delta as usize
    } else {
        v.saturating_sub(delta.unsigned_abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.chars().map(|c| c.to_string()).collect()
    }

    #[test]
    fn two_way_basics() {
        let mut d = Differ::new();
        assert!(!d.sequences_identical());
        let texts = [lines("abcd"), lines("abxd")];
        let changes = d.set_sequences(&texts).unwrap();
        assert_eq!(changes.added.len(), 1);
        assert_eq!(
            d.all_changes(),
            &[[Some(Chunk::new(Tag::Replace, 2, 3, 2, 3)), None]]
        );
        assert_eq!(d.locate_chunk(1, 2).current, Some(0));
        assert_eq!(
            d.locate_chunk(1, 0),
            ChunkLocation {
                current: None,
                prev: None,
                next: Some(0)
            }
        );
        assert_eq!(
            d.locate_chunk(1, 3),
            ChunkLocation {
                current: None,
                prev: Some(0),
                next: None
            }
        );
        assert_eq!(d.locate_chunk(1, 99), ChunkLocation::default());
        assert_eq!(
            d.get_chunk(0, 0, Some(1)),
            Some(Chunk::new(Tag::Replace, 2, 3, 2, 3))
        );
        assert_eq!(d.has_mergeable_changes(0), (false, true));
        assert!(!d.sequences_identical());

        let same = [lines("ab"), lines("ab")];
        d.set_sequences(&same).unwrap();
        assert!(d.sequences_identical());
        d.clear();
        assert!(!d.sequences_identical());
    }

    #[test]
    fn too_many_sequences() {
        let t = vec![Vec::<String>::new(); 4];
        assert_eq!(
            Differ::new().set_sequences(&t),
            Err(DiffError::TooManySequences(4))
        );
    }

    #[test]
    fn ignore_blank_lines() {
        let a: Vec<String> = ["x", "", "y"].map(String::from).to_vec();
        let b: Vec<String> = ["x", "y"].map(String::from).to_vec();
        let mut d = Differ::new();
        d.ignore_blanks = true;
        d.set_sequences(&[a.clone(), b.clone()]).unwrap();
        assert!(d.all_changes().is_empty());
        d.ignore_blanks = false;
        d.set_sequences(&[a, b]).unwrap();
        assert_eq!(d.diff_count(), 1);
    }

    #[test]
    fn consume_blank_lines_retags() {
        let texts = [
            vec!["".to_string(), "".to_string()],
            vec!["a".to_string(), "".to_string()],
        ];
        let c = Chunk::new(Tag::Replace, 0, 2, 0, 2);
        assert_eq!(
            consume_blank_lines(Some(c), &texts, 1, 0),
            Some(Chunk::new(Tag::Delete, 0, 1, 2, 2))
        );
        let blank = Chunk::new(Tag::Insert, 1, 1, 0, 2);
        assert_eq!(consume_blank_lines(Some(blank), &texts, 1, 0), None);
    }

    #[test]
    fn unresolved_bookkeeping() {
        let mut d = Differ::new();
        d.set_unresolved(vec![2, 5, 9]);
        d.update_unresolved(3, 2);
        assert_eq!(d.unresolved(), &[2, 7, 11]);
        d.update_unresolved(7, 0);
        assert_eq!(d.unresolved(), &[2, 11]);
        d.update_unresolved(1, -3);
        assert_eq!(d.unresolved(), &[8]);
    }
}
