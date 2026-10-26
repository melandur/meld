//! Inline (character-level) highlighting of replace chunks.
//!
//! Ports the matching part of `FileDiff.on_diffs_changed` /
//! `apply_highlight` and the caching idea of
//! `meld/matchers/helpers.py:CachedSequenceMatcher` (without the worker
//! process: callers run [`InlineCache::get_or_compute`] wherever they like).

use std::collections::HashMap;

use crate::chunk::{Chunk, Tag};
use crate::myers::inline_opcodes;

/// Above this many characters (both sides together) Meld does not compute
/// inline highlights unless forced, and highlights the whole chunk instead.
pub const INLINE_LIMIT: usize = 20000;

/// Whether the inline comparison of `a` and `b` is too long to do by default.
pub fn exceeds_inline_limit(a: &str, b: &str) -> bool {
    a.chars().count() + b.chars().count() > INLINE_LIMIT
}

/// Select the ranges to highlight from the inline opcodes of `a` vs `b`.
///
/// Keeps every non-equal opcode, and also short (< 3 character) equal runs
/// in the middle, so that highlights don't get chopped into confetti. Equal
/// runs at the very start or end are never highlighted. Offsets are in
/// `char`s relative to the start of each text. (Meld additionally widens
/// ranges to grapheme boundaries for combining marks; that is left to the
/// renderer.)
pub fn highlight_ranges(a: &str, b: &str, opcodes: &[Chunk]) -> Vec<Chunk> {
    let offsets = [a.chars().count(), b.chars().count()];
    opcodes
        .iter()
        .filter(|m| {
            if m.tag != Tag::Equal {
                return true;
            }
            let is_start = m.start_a == 0 && m.start_b == 0;
            let is_end = m.end_a == offsets[0] && m.end_b == offsets[1];
            if is_start || is_end {
                return false;
            }
            m.end_a - m.start_a < 3 || m.end_b - m.start_b < 3
        })
        .copied()
        .collect()
}

/// Inline opcodes plus [`highlight_ranges`] in one go.
pub fn inline_highlights(a: &str, b: &str) -> Vec<Chunk> {
    highlight_ranges(a, b, &inline_opcodes(a, b))
}

/// An LRU-ish cache of inline opcodes keyed by the compared text pair
/// (`CachedSequenceMatcher`). Stores the raw opcodes (including `equal`).
#[derive(Clone, Debug, Default)]
pub struct InlineCache {
    cache: HashMap<(String, String), (Vec<Chunk>, u64)>,
    clock: u64,
}

impl InlineCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// Cached opcodes for the pair, marking them as recently used.
    pub fn get(&mut self, a: &str, b: &str) -> Option<Vec<Chunk>> {
        let now = self.tick();
        let entry = self.cache.get_mut(&(a.to_owned(), b.to_owned()))?;
        entry.1 = now;
        Some(entry.0.clone())
    }

    /// Store opcodes for the pair.
    pub fn insert(&mut self, a: &str, b: &str, opcodes: Vec<Chunk>) {
        let now = self.tick();
        self.cache
            .insert((a.to_owned(), b.to_owned()), (opcodes, now));
    }

    /// Cached opcodes, or compute them with the inline Myers matcher.
    pub fn get_or_compute(&mut self, a: &str, b: &str) -> Vec<Chunk> {
        if let Some(opcodes) = self.get(a, b) {
            return opcodes;
        }
        let opcodes = inline_opcodes(a, b);
        self.insert(a, b, opcodes.clone());
        opcodes
    }

    /// Evict old entries if there are more than `3 * size_hint`, keeping the
    /// `2 * size_hint` most recently used (`clean`). Meld passes the number
    /// of chunks as `size_hint`.
    pub fn clean(&mut self, size_hint: usize) {
        if self.cache.len() <= size_hint * 3 {
            return;
        }
        let mut stamps: Vec<u64> = self.cache.values().map(|(_, t)| *t).collect();
        stamps.sort_unstable();
        // Python's `items[:-size_hint * 2]` deletes nothing for size_hint 0.
        let keep = size_hint * 2;
        if keep == 0 {
            return;
        }
        let cutoff = stamps[stamps.len() - keep];
        self.cache.retain(|_, (_, t)| *t >= cutoff);
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.cache.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_drop_edge_equals_and_keep_short_middle_equals() {
        let a = "red, blue, yellow, white";
        let b = "black green, hue, white";
        let ops = inline_opcodes(a, b);
        let hl = highlight_ranges(a, b, &ops);
        assert!(
            hl.iter()
                .all(|c| c.tag != Tag::Equal || c.end_a - c.start_a < 3)
        );
        assert!(
            !hl.iter()
                .any(|c| c.end_a == 24 && c.end_b == 23 && c.tag == Tag::Equal)
        );

        let ops = vec![
            Chunk::new(Tag::Equal, 0, 2, 0, 2),
            Chunk::new(Tag::Replace, 2, 3, 2, 3),
            Chunk::new(Tag::Equal, 3, 4, 3, 4),
            Chunk::new(Tag::Replace, 4, 5, 4, 5),
            Chunk::new(Tag::Equal, 5, 9, 5, 9),
            Chunk::new(Tag::Insert, 9, 9, 9, 10),
            Chunk::new(Tag::Equal, 9, 10, 10, 11),
        ];
        let a = "0123456789";
        let b = "01234567890";
        assert_eq!(
            highlight_ranges(a, b, &ops),
            vec![ops[1], ops[2], ops[3], ops[5]]
        );
    }

    #[test]
    fn cache_eviction() {
        let mut cache = InlineCache::new();
        for i in 0..10 {
            cache.get_or_compute(&format!("a{i}"), &format!("b{i}"));
        }
        // touch the oldest
        assert!(cache.get("a0", "b0").is_some());
        cache.clean(4);
        assert_eq!(cache.len(), 10);
        cache.clean(3);
        assert_eq!(cache.len(), 6);
        assert!(cache.get("a0", "b0").is_some());
        assert!(cache.get("a1", "b1").is_none());
        cache.clean(0);
        assert_eq!(cache.len(), 6);
    }

    #[test]
    fn limit() {
        assert!(!exceeds_inline_limit("abc", "def"));
        let big = "x".repeat(INLINE_LIMIT);
        assert!(exceeds_inline_limit(&big, "y"));
    }
}
