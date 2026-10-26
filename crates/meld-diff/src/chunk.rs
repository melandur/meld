//! Chunk and matching-block types shared by the matchers and the differ.

use std::fmt;

/// The kind of a diff chunk (Python's opcode tag strings).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tag {
    /// Both ranges hold the same lines.
    Equal,
    /// Range `a` was replaced by range `b`.
    Replace,
    /// Range `a` is not present in `b` (`b` is empty).
    Delete,
    /// Range `b` is not present in `a` (`a` is empty).
    Insert,
    /// Three-way only: both sides changed the same base lines differently.
    Conflict,
}

impl Tag {
    /// The Python/difflib name of the tag (`"replace"`, `"insert"`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            Tag::Equal => "equal",
            Tag::Replace => "replace",
            Tag::Delete => "delete",
            Tag::Insert => "insert",
            Tag::Conflict => "conflict",
        }
    }

    /// Parse a Python/difflib tag name.
    pub fn from_name(name: &str) -> Option<Tag> {
        Some(match name {
            "equal" => Tag::Equal,
            "replace" => Tag::Replace,
            "delete" => Tag::Delete,
            "insert" => Tag::Insert,
            "conflict" => Tag::Conflict,
            _ => return None,
        })
    }

    /// The tag seen from the other side (`insert` <-> `delete`).
    pub fn reversed(self) -> Tag {
        match self {
            Tag::Insert => Tag::Delete,
            Tag::Delete => Tag::Insert,
            other => other,
        }
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A diff chunk: `a[start_a..end_a]` corresponds to `b[start_b..end_b]`.
///
/// Ranges are 0-based and half-open. Line ranges for line diffs, character
/// (Unicode scalar value) ranges for inline diffs. Port of `DiffChunk`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chunk {
    pub tag: Tag,
    pub start_a: usize,
    pub end_a: usize,
    pub start_b: usize,
    pub end_b: usize,
}

impl Chunk {
    pub const fn new(tag: Tag, start_a: usize, end_a: usize, start_b: usize, end_b: usize) -> Self {
        Chunk {
            tag,
            start_a,
            end_a,
            start_b,
            end_b,
        }
    }

    /// Swap the `a` and `b` sides (port of `diffutil.reverse_chunk`).
    pub fn reversed(&self) -> Chunk {
        Chunk::new(
            self.tag.reversed(),
            self.start_b,
            self.end_b,
            self.start_a,
            self.end_a,
        )
    }

    /// Shift the `a` range by `oa` and the `b` range by `ob`.
    pub fn offset(&self, oa: usize, ob: usize) -> Chunk {
        Chunk::new(
            self.tag,
            self.start_a + oa,
            self.end_a + oa,
            self.start_b + ob,
            self.end_b + ob,
        )
    }
}

/// A matching block: `a[a..a + size] == b[b..b + size]` (difflib's `Match`).
///
/// Lists of matching blocks always end with the sentinel `(len(a), len(b), 0)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MatchingBlock {
    pub a: usize,
    pub b: usize,
    pub size: usize,
}

impl MatchingBlock {
    pub const fn new(a: usize, b: usize, size: usize) -> Self {
        MatchingBlock { a, b, size }
    }
}

impl From<(usize, usize, usize)> for MatchingBlock {
    fn from((a, b, size): (usize, usize, usize)) -> Self {
        MatchingBlock { a, b, size }
    }
}

/// Turn matching blocks into opcodes, exactly like
/// `difflib.SequenceMatcher.get_opcodes`.
pub fn opcodes_from_blocks(blocks: &[MatchingBlock]) -> Vec<Chunk> {
    let mut opcodes = Vec::new();
    let (mut i, mut j) = (0, 0);
    push_opcodes(blocks, &mut i, &mut j, &mut opcodes);
    opcodes
}

/// Shared loop of `get_opcodes`, continuing from the running `i`/`j`.
pub(crate) fn push_opcodes(
    blocks: &[MatchingBlock],
    i: &mut usize,
    j: &mut usize,
    opcodes: &mut Vec<Chunk>,
) {
    for block in blocks {
        let (ai, bj, size) = (block.a, block.b, block.size);
        let tag = if *i < ai && *j < bj {
            Some(Tag::Replace)
        } else if *i < ai {
            Some(Tag::Delete)
        } else if *j < bj {
            Some(Tag::Insert)
        } else {
            None
        };
        if let Some(tag) = tag {
            opcodes.push(Chunk::new(tag, *i, ai, *j, bj));
        }
        *i = ai + size;
        *j = bj + size;
        if size > 0 {
            opcodes.push(Chunk::new(Tag::Equal, ai, *i, bj, *j));
        }
    }
}

/// Python slice semantics for non-negative bounds: `s[lo:hi]`, clipped.
pub(crate) fn py_slice<T>(s: &[T], lo: usize, hi: usize) -> &[T] {
    let hi = hi.min(s.len());
    let lo = lo.min(hi);
    &s[lo..hi]
}

/// Python slice assignment for non-negative bounds: `v[lo:hi] = items`.
pub(crate) fn py_splice<T>(
    v: &mut Vec<T>,
    lo: usize,
    hi: usize,
    items: impl IntoIterator<Item = T>,
) {
    let lo = lo.min(v.len());
    let hi = hi.clamp(lo, v.len());
    v.splice(lo..hi, items);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverse_chunk_swaps_sides_and_tags() {
        let c = Chunk::new(Tag::Insert, 1, 1, 2, 5);
        assert_eq!(c.reversed(), Chunk::new(Tag::Delete, 2, 5, 1, 1));
        let c = Chunk::new(Tag::Conflict, 1, 2, 3, 4);
        assert_eq!(c.reversed().tag, Tag::Conflict);
    }

    #[test]
    fn opcodes_like_difflib() {
        // difflib doc example: "qabxcd" vs "abycdf"
        let blocks = [
            MatchingBlock::new(1, 0, 2),
            MatchingBlock::new(4, 3, 2),
            MatchingBlock::new(6, 6, 0),
        ];
        let ops = opcodes_from_blocks(&blocks);
        assert_eq!(
            ops,
            vec![
                Chunk::new(Tag::Delete, 0, 1, 0, 0),
                Chunk::new(Tag::Equal, 1, 3, 0, 2),
                Chunk::new(Tag::Replace, 3, 4, 2, 3),
                Chunk::new(Tag::Equal, 4, 6, 3, 5),
                Chunk::new(Tag::Insert, 6, 6, 5, 6),
            ]
        );
    }

    #[test]
    fn python_slicing() {
        let v = [1, 2, 3];
        assert_eq!(py_slice(&v, 1, 9), &[2, 3]);
        assert_eq!(py_slice(&v, 5, 9), &[] as &[i32]);
        assert_eq!(py_slice(&v, 2, 1), &[] as &[i32]);
        let mut w = vec![1, 2, 3];
        py_splice(&mut w, 1, 2, [7, 8]);
        assert_eq!(w, vec![1, 7, 8, 3]);
        py_splice(&mut w, 9, 12, [9]);
        assert_eq!(w, vec![1, 7, 8, 3, 9]);
    }
}
