//! Small, crate-local input types describing diff chunks.
//!
//! These deliberately do not depend on `meld-diff`; the application
//! converts its own chunk representation into these when building views.

use std::ops::Range;

/// The kind of a diff chunk, which selects its colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ChunkKind {
    /// Lines only present on the other side (`meld:insert`).
    #[default]
    Insert,
    /// Lines only present on this side (`meld:delete`; Meld draws these
    /// with the insert colours unless a diff palette says otherwise).
    Delete,
    /// Lines that differ on both sides (`meld:replace`).
    Replace,
    /// Conflicting three-way change (`meld:conflict`).
    Conflict,
    /// Error-state lines (`meld:error`).
    Error,
}

/// A chunk pairing lines of side `a` with lines of side `b`.
///
/// For the link map, `a` is the left pane and `b` the right pane. For an
/// action gutter, `a` is the gutter's source pane and `b` its target.
/// Ranges are 0-based, half-open line ranges; an empty range marks the
/// position where lines are missing on that side.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct DiffChunk {
    /// Kind of change, from the perspective of side `a`.
    pub kind: ChunkKind,
    /// Line range on side `a`.
    pub a: Range<usize>,
    /// Line range on side `b`.
    pub b: Range<usize>,
}

impl DiffChunk {
    /// Creates a chunk.
    pub fn new(kind: ChunkKind, a: Range<usize>, b: Range<usize>) -> Self {
        Self { kind, a, b }
    }

    /// Returns the chunk seen from side `b`; insert and delete swap.
    pub fn reversed(&self) -> Self {
        let kind = match self.kind {
            ChunkKind::Insert => ChunkKind::Delete,
            ChunkKind::Delete => ChunkKind::Insert,
            other => other,
        };
        Self {
            kind,
            a: self.b.clone(),
            b: self.a.clone(),
        }
    }
}

/// A coloured line range in a single pane (chunk background).
///
/// An empty `lines` range draws Meld's zero-height chunk marker: a 2px line
/// at the top of `lines.start`, showing where the other side has lines that
/// this side lacks.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LineHighlight {
    /// Lines covered by the chunk.
    pub lines: Range<usize>,
    /// Colour selection.
    pub kind: ChunkKind,
    /// Whether this is the current chunk (drawn with the
    /// `current_chunk` overlay).
    pub current: bool,
}

/// An intraline (changed characters) highlight.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InlineHighlight {
    /// Line index.
    pub line: usize,
    /// Character (not byte) column range within the line.
    pub columns: Range<usize>,
}

/// A fading highlight over lines (Meld's `add_fading_highlight`, shown
/// after jumping to a chunk or copying one). The application animates the
/// colour's alpha and drops the flash when it reaches zero.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineFlash {
    /// First line covered.
    pub start: usize,
    /// End of the covered lines (exclusive; equal to `start` for a
    /// zero-height marker).
    pub end: usize,
    /// Current colour, including the animated alpha.
    pub color: iced::Color,
    /// Draw an outline instead of filling the lines.
    pub stroke: bool,
}

/// A chunk in the overview strip of one pane.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MapChunk {
    /// Lines of the mapped pane covered by the chunk (may be empty).
    pub lines: Range<usize>,
    /// Colour selection.
    pub kind: ChunkKind,
}

/// Converts chunks to per-pane [`LineHighlight`]s.
///
/// `side_a` selects whether the pane shows side `a` (true) or side `b`
/// (false); `current` is the index of the current chunk, if any.
pub fn line_highlights(
    chunks: &[DiffChunk],
    side_a: bool,
    current: Option<usize>,
) -> Vec<LineHighlight> {
    chunks
        .iter()
        .enumerate()
        .map(|(i, c)| LineHighlight {
            lines: if side_a { c.a.clone() } else { c.b.clone() },
            kind: if side_a { c.kind } else { c.reversed().kind },
            current: current == Some(i),
        })
        .collect()
}

/// Converts chunks to per-pane [`MapChunk`]s.
pub fn map_chunks(chunks: &[DiffChunk], side_a: bool) -> Vec<MapChunk> {
    chunks
        .iter()
        .map(|c| MapChunk {
            lines: if side_a { c.a.clone() } else { c.b.clone() },
            kind: if side_a { c.kind } else { c.reversed().kind },
        })
        .collect()
}

/// Index range of `chunks` (sorted, non-overlapping on both sides) that
/// may intersect the visible line ranges `vis_a` / `vis_b`.
pub(crate) fn visible_chunk_range(
    chunks: &[DiffChunk],
    vis_a: &Range<usize>,
    vis_b: &Range<usize>,
) -> Range<usize> {
    let start = chunks.partition_point(|c| c.a.end < vis_a.start && c.b.end < vis_b.start);
    let end = chunks.partition_point(|c| c.a.start <= vis_a.end || c.b.start <= vis_b.end);
    start..end.max(start)
}
