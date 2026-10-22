//! Myers O(NP) sequence matchers (port of `meld/matchers/myers.py`).
//!
//! [`MyersSequenceMatcher`] is used for line diffs; with
//! [`MatcherKind::Inline`] it becomes `InlineMyersSequenceMatcher` (k-mer
//! based discarding, used for character diffs inside replace chunks), and
//! with sync points it becomes `SyncPointMyersSequenceMatcher`.

use std::collections::HashSet;
use std::hash::Hash;

use crate::chunk::{Chunk, MatchingBlock, Tag, opcodes_from_blocks, push_opcodes, py_slice};
use crate::error::DiffError;

/// Length of the common prefix of `a` and `b` (`find_common_prefix`).
pub fn find_common_prefix<T: PartialEq>(a: &[T], b: &[T]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

/// Length of the common suffix of `a` and `b` (`find_common_suffix`).
pub fn find_common_suffix<T: PartialEq>(a: &[T], b: &[T]) -> usize {
    a.iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(x, y)| x == y)
        .count()
}

/// Which non-matching-element discarding heuristic a matcher uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MatcherKind {
    /// `MyersSequenceMatcher`: discard elements absent from the other side.
    #[default]
    Line,
    /// `InlineMyersSequenceMatcher`: discard elements not covered by any
    /// 3-element run (k-mer) that also occurs in the other side.
    Inline,
}

/// Myers diff of `a` against `b`, with difflib-compatible results.
#[derive(Clone, Debug)]
pub struct MyersSequenceMatcher<'s, T> {
    a: &'s [T],
    b: &'s [T],
    kind: MatcherKind,
    syncpoints: Vec<(usize, usize)>,
    matching_blocks: Option<Vec<MatchingBlock>>,
    /// Per sync-point section blocks, each terminated by a sentinel.
    split_matching_blocks: Option<Vec<Vec<MatchingBlock>>>,
}

/// State left by preprocessing, needed to map snakes back to `a`/`b`.
struct Preprocessed<'s, T> {
    a: Vec<&'s T>,
    b: Vec<&'s T>,
    common_prefix: usize,
    common_suffix: usize,
    aindex: Vec<usize>,
    bindex: Vec<usize>,
    lines_discarded: bool,
}

impl<'s, T: Eq + Hash> MyersSequenceMatcher<'s, T> {
    /// A line matcher (`MyersSequenceMatcher(None, a, b)`).
    pub fn new(a: &'s [T], b: &'s [T]) -> Self {
        Self::with_kind(a, b, MatcherKind::Line)
    }

    /// An inline matcher (`InlineMyersSequenceMatcher(None, a, b)`).
    pub fn inline(a: &'s [T], b: &'s [T]) -> Self {
        Self::with_kind(a, b, MatcherKind::Inline)
    }

    pub fn with_kind(a: &'s [T], b: &'s [T], kind: MatcherKind) -> Self {
        MyersSequenceMatcher {
            a,
            b,
            kind,
            syncpoints: Vec::new(),
            matching_blocks: None,
            split_matching_blocks: None,
        }
    }

    /// A sync point matcher (`SyncPointMyersSequenceMatcher`). Each sync
    /// point `(ai, bi)` forces `a[..ai]` to be compared only with `b[..bi]`.
    /// With no sync points this is a plain line matcher.
    pub fn with_syncpoints(a: &'s [T], b: &'s [T], syncpoints: Vec<(usize, usize)>) -> Self {
        let mut matcher = Self::new(a, b);
        matcher.syncpoints = syncpoints;
        matcher
    }

    /// Run the comparison, polling `cancel` periodically (every 100 Myers
    /// iterations, where Python yields). Does nothing if already run.
    pub fn run(&mut self, cancel: &dyn Fn() -> bool) -> Result<(), DiffError> {
        if self.matching_blocks.is_some() {
            return Ok(());
        }
        if self.syncpoints.is_empty() {
            self.matching_blocks = Some(self.compute_blocks(self.a, self.b, cancel)?);
            return Ok(());
        }

        let mut sections = Vec::new();
        let (mut ai, mut bi) = (0, 0);
        for &(aj, bj) in &self.syncpoints {
            sections.push((ai, bi, py_slice(self.a, ai, aj), py_slice(self.b, bi, bj)));
            ai = aj;
            bi = bj;
        }
        if ai < self.a.len() || bi < self.b.len() {
            sections.push((
                ai,
                bi,
                py_slice(self.a, ai, self.a.len()),
                py_slice(self.b, bi, self.b.len()),
            ));
        }

        let mut split = Vec::new();
        let mut all_blocks = Vec::new();
        for (ai, bi, a, b) in sections {
            // Sub-matchers are always plain line matchers, as in Python.
            let sub = MyersSequenceMatcher::new(a, b);
            let blocks = sub.compute_blocks(a, b, cancel)?;
            // Python carries code to merge a section's first block with the
            // previous section's last one, but its list is always empty at
            // that point, so sections are never merged.
            let mut section_blocks: Vec<MatchingBlock> = blocks[..blocks.len() - 1]
                .iter()
                .map(|m| MatchingBlock::new(ai + m.a, bi + m.b, m.size))
                .collect();
            all_blocks.extend_from_slice(&section_blocks);
            section_blocks.push(MatchingBlock::new(ai + a.len(), bi + b.len(), 0));
            split.push(section_blocks);
        }
        all_blocks.push(MatchingBlock::new(self.a.len(), self.b.len(), 0));
        self.matching_blocks = Some(all_blocks);
        self.split_matching_blocks = Some(split);
        Ok(())
    }

    /// Matching blocks, terminated by the `(len(a), len(b), 0)` sentinel.
    pub fn matching_blocks(&mut self) -> &[MatchingBlock] {
        if self.matching_blocks.is_none() {
            // Cannot fail without a cancel callback.
            let _ = self.run(&|| false);
        }
        self.matching_blocks.as_deref().unwrap_or_default()
    }

    /// All opcodes, including `equal` ones (`get_opcodes`).
    pub fn opcodes(&mut self) -> Vec<Chunk> {
        self.matching_blocks();
        match &self.split_matching_blocks {
            Some(split) => {
                let (mut i, mut j) = (0, 0);
                let mut opcodes = Vec::new();
                for blocks in split {
                    push_opcodes(blocks, &mut i, &mut j, &mut opcodes);
                }
                opcodes
            }
            None => opcodes_from_blocks(self.matching_blocks.as_deref().unwrap_or_default()),
        }
    }

    /// Opcodes other than `equal` (`get_difference_opcodes`).
    pub fn difference_opcodes(&mut self) -> Vec<Chunk> {
        self.opcodes()
            .into_iter()
            .filter(|c| c.tag != Tag::Equal)
            .collect()
    }

    fn preprocess<'x>(&self, a: &'x [T], b: &'x [T]) -> Preprocessed<'x, T> {
        // Remove common prefix and suffix
        let common_prefix = find_common_prefix(a, b);
        let (mut a, mut b) = (&a[common_prefix..], &b[common_prefix..]);
        let mut common_suffix = 0;
        if !a.is_empty() && !b.is_empty() {
            common_suffix = find_common_suffix(a, b);
            a = &a[..a.len() - common_suffix];
            b = &b[..b.len() - common_suffix];
        }

        let mut pre = Preprocessed {
            a: a.iter().collect(),
            b: b.iter().collect(),
            common_prefix,
            common_suffix,
            aindex: Vec::new(),
            bindex: Vec::new(),
            lines_discarded: false,
        };

        // Discard elements that do not match anything in the other sequence
        let (indexed_b, bindex, indexed_a, aindex) = match self.kind {
            MatcherKind::Line => {
                if a.is_empty() || b.is_empty() {
                    return pre;
                }
                let (ib, bi) = index_matching(a, b);
                let (ia, ai) = index_matching(b, a);
                (ib, bi, ia, ai)
            }
            MatcherKind::Inline => {
                if a.len() <= 2 && b.len() <= 2 {
                    return pre;
                }
                let (ib, bi) = index_matching_kmers(a, b);
                let (ia, ai) = index_matching_kmers(b, a);
                (ib, bi, ia, ai)
            }
        };
        // Only use the optimised result if it's worthwhile; the constant is
        // a heuristic for how many lines constitute 'worthwhile'.
        pre.lines_discarded = b.len() - indexed_b.len() > 10 || a.len() - indexed_a.len() > 10;
        if pre.lines_discarded {
            pre.a = indexed_a;
            pre.b = indexed_b;
        }
        pre.aindex = aindex;
        pre.bindex = bindex;
        pre
    }

    fn compute_blocks(
        &self,
        full_a: &[T],
        full_b: &[T],
        cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<MatchingBlock>, DiffError> {
        let pre = self.preprocess(full_a, full_b);
        let (snakes, lastsnake) = myers_snakes(&pre.a, &pre.b, cancel)?;
        let blocks = build_matching_blocks(&pre, &snakes, lastsnake, full_a.len(), full_b.len());
        Ok(postprocess(full_a, full_b, blocks))
    }
}

fn index_matching<'x, T: Eq + Hash>(a: &[T], b: &'x [T]) -> (Vec<&'x T>, Vec<usize>) {
    let aset: HashSet<&T> = a.iter().collect();
    let mut matches = Vec::new();
    let mut index = Vec::new();
    for (i, line) in b.iter().enumerate() {
        if aset.contains(line) {
            matches.push(line);
            index.push(i);
        }
    }
    (matches, index)
}

fn index_matching_kmers<'x, T: Eq + Hash>(a: &[T], b: &'x [T]) -> (Vec<&'x T>, Vec<usize>) {
    let aset: HashSet<&[T]> = a.windows(3).collect();
    let mut matches = Vec::new();
    let mut index = Vec::new();
    let mut next_poss_match = 0;
    // Start from where we can get a valid triple
    for i in 2..b.len() {
        if !aset.contains(&b[i - 2..=i]) {
            continue;
        }
        // Make sure we don't re-record matches from overlapping kmers
        let from = next_poss_match.max(i - 2);
        matches.extend(&b[from..=i]);
        index.extend(from..=i);
        next_poss_match = i + 1;
    }
    (matches, index)
}

/// A snake in the Myers path: `(previous snake, x, y, length)`.
type Snake = (Option<usize>, usize, usize, usize);

/// The O(NP) algorithm by Wu, Manber, Myers and Miller ("An O(NP) Sequence
/// Comparison Algorithm", 1989), as optimised in Meld. Returns the snake
/// arena and the index of the last snake on the path.
fn myers_snakes<X: PartialEq>(
    a: &[X],
    b: &[X],
    cancel: &dyn Fn() -> bool,
) -> Result<(Vec<Snake>, Option<usize>), DiffError> {
    let m = a.len() as isize;
    let n = b.len() as isize;
    let mut snakes: Vec<Snake> = Vec::new();
    if n == 0 || m == 0 {
        return Ok((snakes, None));
    }
    let middle = m + 1;
    let delta = n - m + middle;
    let dmin = middle.min(delta);
    let dmax = middle.max(delta);
    let size = (n + m + 2) as usize;
    let mut fp: Vec<(isize, Option<usize>)> = vec![(-1, None); size];

    // Follow a diagonal from (x, y) while elements match; returns the end.
    let follow = |mut x: isize, mut y: isize| -> (isize, isize) {
        while x >= 0 && x < m && y < n && a[x as usize] == b[y as usize] {
            x += 1;
            y += 1;
        }
        (x, y)
    };

    let mut p: isize = -1;
    loop {
        p += 1;
        if p % 100 == 0 && cancel() {
            return Err(DiffError::Cancelled);
        }
        // move along vertical edge
        let mut yv: isize = -1;
        let mut node: Option<usize> = None;
        for km in (dmin - p)..delta {
            let t = fp[(km + 1) as usize];
            if yv < t.0 {
                (yv, node) = t;
            } else {
                yv += 1;
            }
            let x = yv - km + middle;
            let (ex, ey) = follow(x, yv);
            if ex != x {
                let snake = (ex - x) as usize;
                snakes.push((node, x as usize, yv as usize, snake));
                node = Some(snakes.len() - 1);
            }
            yv = ey;
            fp[km as usize] = (yv, node);
        }
        // move along horizontal edge
        let mut yh: isize = -1;
        node = None;
        let mut km = dmax + p;
        while km > delta {
            let t = fp[(km - 1) as usize];
            if yh <= t.0 {
                (yh, node) = t;
                yh += 1;
            }
            let x = yh - km + middle;
            let (ex, ey) = follow(x, yh);
            if ex != x {
                let snake = (ex - x) as usize;
                snakes.push((node, x as usize, yh as usize, snake));
                node = Some(snakes.len() - 1);
            }
            yh = ey;
            fp[km as usize] = (yh, node);
            km -= 1;
        }
        // point on the diagonal that leads to the sink
        let (mut y, mut node) = if yv < yh {
            fp[(delta + 1) as usize]
        } else {
            let t = fp[(delta - 1) as usize];
            (t.0 + 1, t.1)
        };
        let x = y - delta + middle;
        let (ex, ey) = follow(x, y);
        if ex != x {
            let snake = (ex - x) as usize;
            snakes.push((node, x as usize, y as usize, snake));
            node = Some(snakes.len() - 1);
        }
        y = ey;
        fp[delta as usize] = (y, node);
        if y >= n {
            return Ok((snakes, node));
        }
    }
}

/// Build the list of matching blocks from the snakes, re-adding the common
/// prefix/suffix and splitting snakes around discarded lines.
fn build_matching_blocks<T>(
    pre: &Preprocessed<'_, T>,
    snakes: &[Snake],
    mut lastsnake: Option<usize>,
    len_a: usize,
    len_b: usize,
) -> Vec<MatchingBlock> {
    let cp = pre.common_prefix;
    let mut reversed = Vec::new();
    while let Some(idx) = lastsnake {
        let (prev, x, y, snake) = snakes[idx];
        lastsnake = prev;
        if pre.lines_discarded {
            // split snakes if needed because of discarded lines
            let mut x = x + snake - 1;
            let mut y = y + snake - 1;
            let mut xprev = pre.aindex[x] + cp;
            let mut yprev = pre.bindex[y] + cp;
            if snake > 1 {
                let mut newsnake = 1;
                for _ in 1..snake {
                    x -= 1;
                    y -= 1;
                    let xnext = pre.aindex[x] + cp;
                    let ynext = pre.bindex[y] + cp;
                    if xprev - xnext != 1 || yprev - ynext != 1 {
                        reversed.push(MatchingBlock::new(xprev, yprev, newsnake));
                        newsnake = 0;
                    }
                    xprev = xnext;
                    yprev = ynext;
                    newsnake += 1;
                }
                reversed.push(MatchingBlock::new(xprev, yprev, newsnake));
            } else {
                reversed.push(MatchingBlock::new(xprev, yprev, snake));
            }
        } else {
            reversed.push(MatchingBlock::new(x + cp, y + cp, snake));
        }
    }
    let mut blocks = Vec::with_capacity(reversed.len() + 3);
    if cp > 0 {
        blocks.push(MatchingBlock::new(0, 0, cp));
    }
    blocks.extend(reversed.into_iter().rev());
    let cs = pre.common_suffix;
    if cs > 0 {
        blocks.push(MatchingBlock::new(len_a - cs, len_b - cs, cs));
    }
    blocks.push(MatchingBlock::new(len_a, len_b, 0));
    blocks
}

/// Merge blocks that can be combined after backward scanning, reducing
/// the 'chaff' that the greedy algorithm leaves behind.
fn postprocess<T: PartialEq>(a: &[T], b: &[T], blocks: Vec<MatchingBlock>) -> Vec<MatchingBlock> {
    let Some(&last) = blocks.last() else {
        return blocks;
    };
    let mut mb = vec![last];
    let mut i = blocks.len() as isize - 2;
    while i >= 0 {
        let MatchingBlock {
            a: mut cur_a,
            b: mut cur_b,
            size: mut cur_len,
        } = blocks[i as usize];
        i -= 1;
        while i >= 0 {
            let prev = blocks[i as usize];
            if prev.b + prev.size == cur_b || prev.a + prev.size == cur_a {
                let prev_slice_a = py_slice(a, cur_a.saturating_sub(prev.size), cur_a);
                let prev_slice_b = py_slice(b, cur_b.saturating_sub(prev.size), cur_b);
                if cur_a >= prev.size && cur_b >= prev.size && prev_slice_a == prev_slice_b {
                    cur_b -= prev.size;
                    cur_a -= prev.size;
                    cur_len += prev.size;
                    i -= 1;
                    continue;
                }
            }
            break;
        }
        mb.push(MatchingBlock::new(cur_a, cur_b, cur_len));
    }
    mb.reverse();
    mb
}

/// Line diff opcodes (including `equal`) of `a` against `b`.
pub fn opcodes<T: Eq + Hash>(a: &[T], b: &[T]) -> Vec<Chunk> {
    MyersSequenceMatcher::new(a, b).opcodes()
}

/// Line diff opcodes other than `equal`.
pub fn difference_opcodes<T: Eq + Hash>(a: &[T], b: &[T]) -> Vec<Chunk> {
    MyersSequenceMatcher::new(a, b).difference_opcodes()
}

/// Character diff opcodes of two strings (`InlineMyersSequenceMatcher`),
/// with offsets in `char`s (Unicode scalar values, like Python `str`).
pub fn inline_opcodes(a: &str, b: &str) -> Vec<Chunk> {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    MyersSequenceMatcher::inline(&a, &b).opcodes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    fn blocks(v: &[(usize, usize, usize)]) -> Vec<MatchingBlock> {
        v.iter().copied().map(MatchingBlock::from).collect()
    }

    // Ported from test/test_matchers.py

    #[test]
    fn basic_matcher() {
        let (a, b) = (chars("abcbdefgabcdefg"), chars("gfabcdefcd"));
        let mut m = MyersSequenceMatcher::new(&a, &b);
        assert_eq!(
            m.matching_blocks(),
            blocks(&[(0, 2, 3), (4, 5, 3), (10, 8, 2), (15, 10, 0)])
        );
    }

    #[test]
    fn postprocessing_cleanup() {
        let (a, b) = (chars("abcfabgcd"), chars("afabcgabgcabcd"));
        let mut m = MyersSequenceMatcher::new(&a, &b);
        assert_eq!(
            m.matching_blocks(),
            blocks(&[(0, 2, 3), (4, 6, 3), (7, 12, 2), (9, 14, 0)])
        );
    }

    #[test]
    fn inline_matcher() {
        let (a, b) = (
            chars("red, blue, yellow, white"),
            chars("black green, hue, white"),
        );
        let mut m = MyersSequenceMatcher::inline(&a, &b);
        assert_eq!(m.matching_blocks(), blocks(&[(17, 16, 7), (24, 23, 0)]));
    }

    #[test]
    fn sync_point_matcher0() {
        let (a, b) = (chars("012a3456c789"), chars("0a3412b5678"));
        let mut m = MyersSequenceMatcher::with_syncpoints(&a, &b, vec![]);
        assert_eq!(
            m.matching_blocks(),
            blocks(&[(0, 0, 1), (3, 1, 3), (6, 7, 2), (9, 9, 2), (12, 11, 0)])
        );
    }

    #[test]
    fn sync_point_matcher2() {
        let (a, b) = (chars("012a3456c789"), chars("0a3412b5678"));
        let mut m = MyersSequenceMatcher::with_syncpoints(&a, &b, vec![(3, 6)]);
        assert_eq!(
            m.matching_blocks(),
            blocks(&[(0, 0, 1), (1, 4, 2), (6, 7, 2), (9, 9, 2), (12, 11, 0)])
        );
    }

    #[test]
    fn sync_point_matcher3() {
        let (a, b) = (chars("012a3456c789"), chars("02a341b5678"));
        let mut m = MyersSequenceMatcher::with_syncpoints(&a, &b, vec![(3, 2), (8, 6)]);
        assert_eq!(
            m.matching_blocks(),
            blocks(&[(0, 0, 1), (2, 1, 1), (3, 2, 3), (9, 9, 2), (12, 11, 0)])
        );
    }

    #[test]
    fn empty_sequences() {
        let e: Vec<char> = vec![];
        let a = chars("abc");
        assert_eq!(
            MyersSequenceMatcher::new(&e, &a).opcodes(),
            vec![Chunk::new(Tag::Insert, 0, 0, 0, 3)]
        );
        assert_eq!(
            MyersSequenceMatcher::new(&a, &e).opcodes(),
            vec![Chunk::new(Tag::Delete, 0, 3, 0, 0)]
        );
        assert!(MyersSequenceMatcher::new(&e, &e).opcodes().is_empty());
        assert_eq!(
            MyersSequenceMatcher::new(&a, &a).opcodes(),
            vec![Chunk::new(Tag::Equal, 0, 3, 0, 3)]
        );
    }

    #[test]
    fn cancellation() {
        let a: Vec<usize> = (0..500).collect();
        let b: Vec<usize> = (0..500).rev().collect();
        let mut m = MyersSequenceMatcher::new(&a, &b);
        assert_eq!(m.run(&|| true), Err(DiffError::Cancelled));
        assert!(m.run(&|| false).is_ok());
    }

    #[test]
    fn prefix_suffix() {
        assert_eq!(find_common_prefix(&[1, 2, 3], &[1, 2, 4]), 2);
        assert_eq!(find_common_suffix(&[1, 2, 3], &[0, 2, 3]), 2);
        assert_eq!(find_common_prefix::<i32>(&[], &[1]), 0);
    }
}
