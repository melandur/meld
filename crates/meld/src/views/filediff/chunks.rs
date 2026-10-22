//! Chunk bookkeeping for display and navigation: what each pane, link
//! map, action gutter and overview map shows, derived from the differ's
//! merged chunks, plus the cursor details and action sensitivity of
//! `FileDiff.on_cursor_position_changed` / `on_current_diff_changed`.

use meld_diff::{Chunk, Differ, MergedChunk, Tag};
use meld_widgets::{ChunkKind, DiffChunk, LineHighlight, MapChunk};

pub fn kind(tag: Tag) -> ChunkKind {
    match tag {
        Tag::Insert => ChunkKind::Insert,
        Tag::Delete => ChunkKind::Delete,
        Tag::Conflict => ChunkKind::Conflict,
        Tag::Replace | Tag::Equal => ChunkKind::Replace,
    }
}

pub fn diff_chunk(c: &Chunk) -> DiffChunk {
    DiffChunk::new(kind(c.tag), c.start_a..c.end_a, c.start_b..c.end_b)
}

/// The chunk of merged entry `merged` as seen from `pane`
/// (`single_changes`): oriented with `a` on that pane.
pub fn pane_chunk(merged: &MergedChunk, pane: usize) -> Option<Chunk> {
    match pane {
        0 => merged[0].map(|c| c.reversed()),
        2 => merged[1].map(|c| c.reversed()),
        _ => merged[0].or(merged[1]),
    }
}

/// The chunk of merged entry `merged` between `from` and `to`, oriented
/// with `a` on `from` (`pair_changes`).
pub fn pair_chunk(merged: &MergedChunk, from: usize, to: usize) -> Option<Chunk> {
    if from == 1 {
        merged[(to / 2).min(1)]
    } else {
        merged[(from / 2).min(1)].map(|c| c.reversed())
    }
}

/// Chunks between two panes, with the merged index of each.
#[derive(Debug, Clone, Default)]
pub struct PairChunks {
    pub from: usize,
    pub to: usize,
    pub chunks: Vec<DiffChunk>,
    pub raw: Vec<Chunk>,
    pub merged: Vec<usize>,
}

impl PairChunks {
    fn build(all: &[MergedChunk], from: usize, to: usize) -> Self {
        let mut p = PairChunks {
            from,
            to,
            ..Default::default()
        };
        for (i, m) in all.iter().enumerate() {
            if let Some(c) = pair_chunk(m, from, to) {
                p.chunks.push(diff_chunk(&c));
                p.raw.push(c);
                p.merged.push(i);
            }
        }
        p
    }

    /// Index into `chunks` of merged chunk `merged`.
    pub fn position(&self, merged: Option<usize>) -> Option<usize> {
        let m = merged?;
        self.merged.binary_search(&m).ok()
    }
}

/// Everything the views draw from the chunk list.
#[derive(Debug, Clone, Default)]
pub struct Derived {
    /// Per pane: chunk backgrounds, overview map entries and the merged
    /// index of each.
    pub highlights: Vec<Vec<LineHighlight>>,
    pub maps: Vec<Vec<MapChunk>>,
    pub pane_merged: Vec<Vec<usize>>,
    /// Pairs between neighbouring panes: `(0,1)`, `(1,0)` and, in
    /// three-way comparisons, `(1,2)`, `(2,1)`.
    pub pairs: Vec<PairChunks>,
}

impl Derived {
    pub fn build(differ: &Differ, num_panes: usize, current: Option<usize>) -> Self {
        let all = differ.all_changes();
        let mut d = Derived::default();
        if num_panes < 2 {
            d.highlights = vec![Vec::new(); num_panes];
            d.maps = vec![Vec::new(); num_panes];
            d.pane_merged = vec![Vec::new(); num_panes];
            return d;
        }
        for pane in 0..num_panes {
            let mut hl = Vec::new();
            let mut maps = Vec::new();
            let mut merged = Vec::new();
            for (i, m) in all.iter().enumerate() {
                if let Some(c) = pane_chunk(m, pane) {
                    hl.push(LineHighlight {
                        lines: c.start_a..c.end_a,
                        kind: kind(c.tag),
                        current: current == Some(i),
                    });
                    maps.push(MapChunk {
                        lines: c.start_a..c.end_a,
                        kind: kind(c.tag),
                    });
                    merged.push(i);
                }
            }
            d.highlights.push(hl);
            d.maps.push(maps);
            d.pane_merged.push(merged);
        }
        d.pairs.push(PairChunks::build(all, 0, 1));
        d.pairs.push(PairChunks::build(all, 1, 0));
        if num_panes == 3 {
            d.pairs.push(PairChunks::build(all, 1, 2));
            d.pairs.push(PairChunks::build(all, 2, 1));
        }
        d
    }

    /// Updates the current-chunk flags.
    pub fn set_current(&mut self, current: Option<usize>) {
        for (hl, merged) in self.highlights.iter_mut().zip(&self.pane_merged) {
            for (h, m) in hl.iter_mut().zip(merged) {
                h.current = current == Some(*m);
            }
        }
    }

    pub fn pair(&self, from: usize, to: usize) -> Option<&PairChunks> {
        self.pairs.iter().find(|p| p.from == from && p.to == to)
    }
}

/// `CursorDetails`: where the cursor is relative to the chunks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CursorDetails {
    pub pane: Option<usize>,
    pub line: Option<usize>,
    pub chunk: Option<usize>,
    pub prev: Option<usize>,
    pub next: Option<usize>,
    pub prev_conflict: Option<usize>,
    pub next_conflict: Option<usize>,
}

impl CursorDetails {
    /// Cursor details for `line` of `pane`.
    pub fn locate(differ: &Differ, pane: usize, line: usize) -> Self {
        let loc = differ.locate_chunk(pane, line);
        let (mut prev_conflict, mut next_conflict) = (None, None);
        for &conflict in differ.conflicts() {
            if let Some(prev) = loc.prev
                && conflict <= prev
            {
                prev_conflict = Some(conflict);
            }
            if let Some(next) = loc.next
                && conflict >= next
            {
                next_conflict = Some(conflict);
                break;
            }
        }
        CursorDetails {
            pane: Some(pane),
            line: Some(line),
            chunk: loc.current,
            prev: loc.prev,
            next: loc.next,
            prev_conflict,
            next_conflict,
        }
    }
}

/// Which change actions apply at the cursor (`on_current_diff_changed`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sensitivity {
    pub push_left: bool,
    pub push_right: bool,
    pub pull_left: bool,
    pub pull_right: bool,
    pub delete: bool,
    pub copy_left: bool,
    pub copy_right: bool,
    pub prev_pane: bool,
    pub next_pane: bool,
}

pub fn sensitivity(
    differ: &Differ,
    num_panes: usize,
    pane: Option<usize>,
    chunk_id: Option<usize>,
    editable: &[bool],
) -> Sensitivity {
    let mut s = Sensitivity::default();
    let Some(pane) = pane else {
        return s;
    };
    s.prev_pane = pane > 0;
    s.next_pane = pane + 1 < num_panes;
    let Some(chunk_id) = chunk_id else {
        return s;
    };
    let ed = |p: usize| editable.get(p).copied().unwrap_or(false);
    let three_way = num_panes == 3;
    let editable_here = ed(pane);
    let editable_left = pane > 0 && ed(pane - 1);
    let editable_right = pane + 1 < num_panes && ed(pane + 1);
    if pane == 0 || pane == 2 {
        if let Some(chunk) = differ.get_chunk(chunk_id, pane, None) {
            let is_insert = chunk.start_a == chunk.end_a;
            let is_delete = chunk.start_b == chunk.end_b;
            s.push_left = editable_left;
            s.push_right = editable_right;
            s.pull_left = pane == 2 && editable_here && !is_delete;
            s.pull_right = pane == 0 && editable_here && !is_delete;
            s.delete = editable_here && !is_insert;
            s.copy_left = editable_left && !(is_insert || is_delete);
            s.copy_right = editable_right && !(is_insert || is_delete);
        }
    } else {
        let chunk0 = differ.get_chunk(chunk_id, 1, Some(0));
        let chunk2 = if three_way {
            differ.get_chunk(chunk_id, 1, Some(2))
        } else {
            None
        };
        let left_mid_exists = chunk0.is_some_and(|c| c.start_a != c.end_a);
        let left_exists = chunk0.is_some_and(|c| c.start_b != c.end_b);
        let right_mid_exists = chunk2.is_some_and(|c| c.start_a != c.end_a);
        let right_exists = chunk2.is_some_and(|c| c.start_b != c.end_b);
        s.push_left = editable_left && (!three_way || chunk0.is_some());
        s.push_right = editable_right && (!three_way || chunk2.is_some());
        s.pull_left = editable_here && left_exists;
        s.pull_right = editable_here && right_exists;
        s.delete = editable_here && (left_mid_exists || right_mid_exists);
        s.copy_left = editable_left && left_mid_exists && left_exists;
        s.copy_right = editable_right && right_mid_exists && right_exists;
    }
    if num_panes == 2 {
        s.push_left = ed(0);
        s.push_right = ed(1);
    }
    s
}

/// `_synth_chunk`: the equal stretch around `line` between two panes, as
/// `(start0, end0, start1, end1)`.
fn synth_chunk(
    differ: &Differ,
    line_counts: &[usize],
    pane0: usize,
    pane1: usize,
    line: usize,
) -> (usize, usize, usize, usize) {
    let (mut start0, mut end0) = (0, line_counts[pane0].saturating_sub(1));
    let (mut start1, mut end1) = (0, line_counts[pane1].saturating_sub(1));
    let loc = differ.locate_chunk(pane0, line);
    if let Some(mut prev) = loc.prev {
        loop {
            let c0 = differ.get_chunk(prev, pane0, Some(pane1));
            let c1 = differ.get_chunk(prev, pane1, Some(pane0));
            if let (Some(c0), Some(c1)) = (c0, c1) {
                start0 = c0.end_a;
                start1 = c1.end_a;
                break;
            }
            if prev == 0 {
                break;
            }
            prev -= 1;
        }
    }
    if let Some(mut next) = loc.next {
        while next < differ.diff_count() {
            let c0 = differ.get_chunk(next, pane0, Some(pane1));
            let c1 = differ.get_chunk(next, pane1, Some(pane0));
            if let (Some(c0), Some(c1)) = (c0, c1) {
                end0 = c0.start_a;
                end1 = c1.start_a;
                break;
            }
            next += 1;
        }
    }
    (start0, end0, start1, end1)
}

/// `_corresponding_chunk_line`: the line of `new_pane` matching `line` of
/// `pane`. `new_cursor_line` is the current cursor line of `new_pane`.
pub fn corresponding_line(
    differ: &Differ,
    line_counts: &[usize],
    chunk: Option<usize>,
    line: usize,
    pane: usize,
    new_pane: usize,
    new_cursor_line: usize,
) -> usize {
    if (pane == 0 && new_pane == 2) || (pane == 2 && new_pane == 0) {
        let proxy = corresponding_line(differ, line_counts, chunk, line, pane, 1, new_cursor_line);
        return corresponding_line(
            differ,
            line_counts,
            chunk,
            proxy,
            1,
            new_pane,
            new_cursor_line,
        );
    }
    let cur = chunk.and_then(|c| differ.get_chunk(c, pane, Some(new_pane)));
    let (cur_start, cur_end, new_start, new_end) = match cur {
        Some(c) => (c.start_a, c.end_a, c.start_b, c.end_b),
        None => synth_chunk(differ, line_counts, pane, new_pane, line),
    };
    let cursor_chunk = differ.locate_chunk(new_pane, new_cursor_line).current;
    let already_in_chunk = match cursor_chunk {
        Some(c) => Some(c) == chunk,
        None => {
            let (_, _, s, e) = synth_chunk(differ, line_counts, pane, new_pane, new_cursor_line);
            s == new_start && e == new_end
        }
    };
    if already_in_chunk {
        return new_cursor_line;
    }
    let offset = if cur_end == cur_start {
        0.0
    } else {
        (line as f64 - cur_start as f64) / (cur_end - cur_start) as f64
    };
    new_start + (offset * (new_end as f64 - new_start as f64)) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &[&str]) -> Vec<String> {
        s.iter().map(|l| l.to_string()).collect()
    }

    fn differ(texts: &[Vec<String>]) -> Differ {
        let mut d = Differ::new();
        d.set_sequences(texts).unwrap();
        d
    }

    #[test]
    fn navigation_and_cursor_details() {
        let a = lines(&["a", "b", "c", "d", "e", "f"]);
        let b = lines(&["a", "X", "c", "d", "E", "f", "g"]);
        let d = differ(&[a, b]);
        assert_eq!(d.all_changes().len(), 3);
        let at_start = CursorDetails::locate(&d, 1, 0);
        assert_eq!(at_start.chunk, None);
        assert_eq!(at_start.next, Some(0));
        let in_first = CursorDetails::locate(&d, 1, 1);
        assert_eq!(in_first.chunk, Some(0));
        assert_eq!(in_first.next, Some(1));
        assert_eq!(in_first.prev, None);
        let last = CursorDetails::locate(&d, 0, 5);
        assert_eq!(last.prev, Some(1));
    }

    #[test]
    fn derived_pairs_and_highlights() {
        let a = lines(&["a", "b", "c"]);
        let b = lines(&["a", "c", "d"]);
        let d = differ(&[a, b]);
        let derived = Derived::build(&d, 2, Some(0));
        assert_eq!(derived.highlights[0].len(), 2);
        assert!(derived.highlights[0][0].current);
        assert_eq!(derived.highlights[0][0].kind, ChunkKind::Delete);
        assert_eq!(derived.highlights[1][0].kind, ChunkKind::Insert);
        let p01 = derived.pair(0, 1).unwrap();
        assert_eq!(p01.chunks[0].a, 1..2);
        assert_eq!(p01.chunks[0].b, 1..1);
        let p10 = derived.pair(1, 0).unwrap();
        assert_eq!(p10.chunks[0].a, 1..1);
        assert_eq!(p10.position(Some(1)), Some(1));
    }

    #[test]
    fn three_way_sensitivity() {
        let l = lines(&["a", "L", "c"]);
        let m = lines(&["a", "b", "c"]);
        let r = lines(&["a", "b", "c", "R"]);
        let d = differ(&[l, m, r]);
        let cur = CursorDetails::locate(&d, 1, 1);
        let s = sensitivity(&d, 3, Some(1), cur.chunk, &[true, true, true]);
        assert!(s.pull_left && !s.pull_right);
        assert!(s.push_left);
        let s = sensitivity(&d, 3, Some(0), cur.chunk, &[true, true, true]);
        assert!(s.push_right && s.delete && !s.pull_left);
    }

    #[test]
    fn corresponding_lines_follow_chunks() {
        let a = lines(&["a", "b", "c", "d"]);
        let b = lines(&["x", "y", "a", "b", "c", "d"]);
        let d = differ(&[a, b]);
        let counts = [4, 6];
        // Line 2 ("c") of pane 0 corresponds to line 4 of pane 1.
        let loc = d.locate_chunk(0, 2);
        assert_eq!(corresponding_line(&d, &counts, loc.current, 2, 0, 1, 0), 4);
    }
}
