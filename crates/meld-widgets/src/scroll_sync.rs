//! Meld-style linked vertical scrolling (port of `FileDiff._sync_vscroll`
//! and `misc.calc_syncpoint`).
//!
//! When pane `source` scrolls, call [`sync_scroll`] for each pane it
//! influences (Meld: left → middle → right; the middle moves both) with
//! the chunks between them, and store the returned offset for that pane.

use crate::chunk::DiffChunk;
use crate::wrap::{WrapLayout, or_identity};

/// Scroll geometry of one pane.
#[derive(Debug, Clone, Copy)]
pub struct ScrollGeometry<'a> {
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
    /// Visual rows of the pane when it wraps lines (`None`: one row per
    /// line).
    pub wrap: Option<&'a WrapLayout>,
}

impl ScrollGeometry<'_> {
    /// Scrollable content height (the adjustment's `upper`).
    pub fn upper(&self) -> f32 {
        or_identity(self.wrap).content_height(
            self.line_count,
            self.line_height,
            self.overscroll_lines,
        )
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
    // The line at the sync point, with how far into it the point is (a
    // wrapped line may be half way through).
    let sw = or_identity(source.wrap);
    let row = (sync_y / lh).floor().max(0.0) as usize;
    let line = sw.line_at_row(row).0.min(source.line_count.max(1) - 1);
    let height = sw.line_height(line, lh).max(1.0);
    let target_line = (line as f32 + (sync_y - sw.y_for_line(line, lh)) / height).max(0.0);

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
    let tw = or_identity(target.wrap);
    let other = other_line.floor() as usize;
    let mut val = if other >= target.line_count {
        // Special case for EOF.
        tw.y_for_line(target.line_count, tlh)
    } else {
        tw.y_for_line(other, tlh) + other_line.fract() * tw.line_height(other, tlh)
    };
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

    fn geo<'a>(scroll_y: f32, lines: usize) -> ScrollGeometry<'a> {
        ScrollGeometry {
            scroll_y,
            viewport_height: 200.0,
            line_height: 10.0,
            line_count: lines,
            overscroll_lines: 5,
            wrap: None,
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
    fn wrapped_source_lines_up_by_line() {
        use crate::buffer::Buffer;
        use crate::wrap::{WrapLayout, WrapMode};
        // Line 2 wraps over 11 rows: line 20 starts at row 30 on the
        // wrapped side and at row 20 on the other.
        let mut lines: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
        lines[2] = "word ".repeat(22);
        let buffer = Buffer::new(lines);
        let mut wrap = WrapLayout::new(WrapMode::Word);
        wrap.set_columns(10);
        wrap.update(&buffer);
        assert_eq!(wrap.rows_before(20), 30);
        // The middle of the page (100px down) half way through line 20.
        let mut src = geo(305.0 - 100.0, 100);
        src.wrap = Some(&wrap);
        assert_eq!(sync_scroll(&src, &geo(0.0, 100), &[]), 205.0 - 100.0);
        // And back.
        let mut tgt = geo(0.0, 100);
        tgt.wrap = Some(&wrap);
        assert_eq!(
            sync_scroll(&geo(205.0 - 100.0, 100), &tgt, &[]),
            305.0 - 100.0
        );
    }

    #[test]
    fn scrolled_to_end_aligns_ends() {
        let chunks = [DiffChunk::new(ChunkKind::Delete, 10..60, 10..10)];
        let src = geo(geo(0.0, 300).max_scroll(), 300);
        let tgt = geo(0.0, 250);
        assert_eq!(sync_scroll(&src, &tgt, &chunks), tgt.max_scroll());
    }
}
