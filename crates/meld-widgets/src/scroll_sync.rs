//! Meld-style linked vertical scrolling (port of `FileDiff._sync_vscroll`
//! and `misc.calc_syncpoint`).
//!
//! When pane `source` scrolls, call [`sync_scroll`] for each pane it
//! influences (Meld: left → middle → right; the middle moves both) with
//! the chunks between them, and store the returned offset for that pane.

use crate::chunk::DiffChunk;
use crate::text_pane::content_height;

/// Scroll geometry of one pane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollGeometry {
    /// Current vertical offset (ignored for the target pane).
    pub scroll_y: f32,
    /// Visible height of the pane.
    pub viewport_height: f32,
    /// Line height.
    pub line_height: f32,
    /// Number of lines.
    pub line_count: usize,
    /// Lines that can be scrolled past the end.
    pub overscroll_lines: usize,
}

impl ScrollGeometry {
    /// Scrollable content height (the adjustment's `upper`).
    pub fn upper(&self) -> f32 {
        content_height(self.line_count, self.line_height, self.overscroll_lines)
    }

    fn max_scroll(&self) -> f32 {
        (self.upper() - self.viewport_height).max(0.0)
    }
}

/// Meld's `calc_syncpoint`: the fraction of the page used as the sync
/// point. Normally 0.5 (middle of the screen), scaling to 0.0 within the
/// first half screen and to 1.0 within the last.
pub fn calc_syncpoint(value: f32, page_size: f32, upper: f32) -> f32 {
    let half = page_size / 2.0;
    if half <= 0.0 {
        return 0.0;
    }
    let mut syncpoint = 0.5 * ((value - 0.0) / half).min(1.0);
    let bottom = upper - 1.5 * page_size;
    syncpoint += 0.5 * ((value - bottom) / half).max(0.0);
    syncpoint
}

/// Computes the scroll offset of `target` that keeps it aligned with
/// `source`. `chunks` map source lines (`a`) to target lines (`b`).
pub fn sync_scroll(source: &ScrollGeometry, target: &ScrollGeometry, chunks: &[DiffChunk]) -> f32 {
    let syncpoint = calc_syncpoint(source.scroll_y, source.viewport_height, source.upper());
    let sync_y = source.scroll_y + source.viewport_height * syncpoint;
    let lh = source.line_height.max(1.0);
    let target_line = (sync_y / lh).max(0.0);

    let (mut mbegin, mut mend) = (0.0f32, source.line_count as f32);
    let (mut obegin, mut oend) = (0.0f32, target.line_count as f32);
    for c in chunks {
        let (sa, ea) = (c.a.start as f32, c.a.end as f32);
        let (sb, eb) = (c.b.start as f32, c.b.end as f32);
        if sa >= target_line {
            mend = sa;
            oend = sb;
            break;
        } else if ea >= target_line {
            (mbegin, mend, obegin, oend) = (sa, ea, sb, eb);
            break;
        } else {
            mbegin = ea;
            obegin = eb;
        }
    }

    let span = mend - mbegin;
    let fraction = (target_line - mbegin) / if span == 0.0 { 1.0 } else { span };
    let other_line = obegin + fraction * (oend - obegin);

    let tlh = target.line_height;
    let mut val = other_line.floor() * tlh;
    let at_end = other_line.floor() as usize >= target.line_count;
    let line_factor = if at_end { 1.0 } else { other_line.fract() };
    if !at_end {
        val += line_factor * tlh;
    }
    if syncpoint > 0.5 {
        let overscroll_scale = (syncpoint - 0.5) / 0.5;
        val += target.overscroll_lines as f32 * tlh * overscroll_scale;
    }
    val -= target.viewport_height * syncpoint;
    (val + 1e-3).clamp(0.0, target.max_scroll()).floor()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::ChunkKind;

    fn geo(scroll_y: f32, lines: usize) -> ScrollGeometry {
        ScrollGeometry {
            scroll_y,
            viewport_height: 200.0,
            line_height: 10.0,
            line_count: lines,
            overscroll_lines: 5,
        }
    }

    #[test]
    fn syncpoint_scales_at_ends() {
        assert_eq!(calc_syncpoint(0.0, 200.0, 1000.0), 0.0);
        assert_eq!(calc_syncpoint(50.0, 200.0, 1000.0), 0.25);
        assert_eq!(calc_syncpoint(400.0, 200.0, 1000.0), 0.5);
        assert_eq!(calc_syncpoint(800.0, 200.0, 1000.0), 1.0);
        assert_eq!(calc_syncpoint(0.0, 0.0, 1000.0), 0.0);
    }

    #[test]
    fn identical_files_scroll_together() {
        for y in [0.0, 30.0, 300.0, 555.0, 800.0] {
            assert_eq!(sync_scroll(&geo(y, 95), &geo(0.0, 95), &[]), y.floor());
        }
    }

    #[test]
    fn insert_shifts_target() {
        // 20 lines inserted on the target side at source line 10.
        let chunks = [DiffChunk::new(ChunkKind::Insert, 10..10, 10..30)];
        // Middle of a long file well after the insert: offset by 200px.
        let src = geo(1000.0, 300);
        assert_eq!(sync_scroll(&src, &geo(0.0, 320), &chunks), 1200.0);
        // Before the insert nothing changes.
        assert_eq!(sync_scroll(&geo(0.0, 300), &geo(0.0, 320), &chunks), 0.0);
    }

    #[test]
    fn scrolled_to_end_aligns_ends() {
        let chunks = [DiffChunk::new(ChunkKind::Delete, 10..60, 10..10)];
        let src = geo(geo(0.0, 300).max_scroll(), 300);
        let tgt = geo(0.0, 250);
        assert_eq!(sync_scroll(&src, &tgt, &chunks), tgt.max_scroll());
    }
}
