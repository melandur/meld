//! The overview strip beside a pane: whole-file chunk positions scaled to
//! the strip height, with a viewport indicator; click or drag to scroll
//! (replaces `TextViewChunkMap` in `meld/chunkmap.py`).
//!
//! ```ignore
//! chunk_map(&map_chunks, &style)
//!     .lines(buffer.line_count(), max_line_count_of_all_panes)
//!     .viewport(scroll_y, viewport_height, metrics.line_height)
//!     .on_scroll(|s: MapScroll| Message::Scroll(pane, s.scroll_y))
//! ```
//!
//! The strip is the pane's vertical scrolling control: there is no other
//! vertical scrollbar.
//!
//! With [`MapStyle::CompactSource`] or [`MapStyle::FullSource`] and
//! [`ChunkMap::source`], the strip is a miniature of the text instead
//! (Meld's `MeldSourceMap`, a `GtkSourceMap`): a few pixels per line with
//! the chunk backgrounds, one bar per line from its indentation to its end
//! (compact) or one bar per run of characters (full). A long file scrolls
//! inside the miniature, which keeps the viewport handle in view.

use std::cell::Cell;
use std::hash::{Hash, Hasher};

use iced::mouse;
use iced::widget::canvas::{self, Action, Cache, Event, Frame, Geometry, Path, Stroke};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Size};

use crate::buffer::Buffer;
use crate::chunk::MapChunk;
use crate::style::PaneStyle;
use crate::text_pane::DEFAULT_OVERSCROLL_LINES;
use crate::wrap::{WrapLayout, or_identity};

/// How the overview strip draws (`overview-map-style`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MapStyle {
    /// Chunk positions scaled to the strip (`chunkmap`).
    #[default]
    Chunks,
    /// A narrow miniature with one bar per line (`compact-sourcemap`).
    CompactSource,
    /// A miniature with bars per run of characters (`full-sourcemap`).
    FullSource,
}

impl MapStyle {
    /// The strip width Meld uses for the style.
    pub fn width(self) -> f32 {
        match self {
            MapStyle::Chunks => 20.0,
            MapStyle::CompactSource => 40.0,
            MapStyle::FullSource => 100.0,
        }
    }
}

/// Height of a line in the source miniature.
const SOURCE_LINE: f32 = 3.0;
/// Width of a character cell in the full miniature.
const SOURCE_CHAR: f32 = 1.2;
/// Columns the compact miniature squeezes into its width.
const COMPACT_COLUMNS: f32 = 80.0;

/// Scroll request from a [`ChunkMap`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapScroll {
    /// Position clicked, as a fraction of the pane's content height.
    pub fraction: f32,
    /// Vertical scroll offset (pixels, clamped) that centres the view on
    /// that position, as Meld does.
    pub scroll_y: f32,
}

/// Padding by which the viewport handle overdraws the map
/// (Meld's `overdraw_padding`).
const OVERDRAW: f32 = 2.0;

/// Creates a [`ChunkMap`].
pub fn chunk_map<'a, Message>(
    chunks: &'a [MapChunk],
    style: &'a PaneStyle,
) -> ChunkMap<'a, Message> {
    ChunkMap::new(chunks, style)
}

/// Chunk map builder. Convert into an [`Element`] with `.into()`.
pub struct ChunkMap<'a, Message> {
    chunks: &'a [MapChunk],
    style: &'a PaneStyle,
    line_count: usize,
    max_line_count: usize,
    scroll_y: f32,
    viewport_height: f32,
    line_height: f32,
    overscroll_lines: usize,
    wrap: Option<&'a WrapLayout>,
    map_style: MapStyle,
    source: Option<&'a Buffer>,
    on_scroll: Option<Box<dyn Fn(MapScroll) -> Message + 'a>>,
    width: Length,
}

impl<'a, Message> ChunkMap<'a, Message> {
    /// Creates a chunk map for one pane's chunks.
    pub fn new(chunks: &'a [MapChunk], style: &'a PaneStyle) -> Self {
        Self {
            chunks,
            style,
            line_count: 1,
            max_line_count: 1,
            scroll_y: 0.0,
            viewport_height: 0.0,
            line_height: 18.0,
            overscroll_lines: DEFAULT_OVERSCROLL_LINES,
            wrap: None,
            map_style: MapStyle::Chunks,
            source: None,
            on_scroll: None,
            width: Length::Fixed(20.0),
        }
    }

    /// Line count of the mapped pane, and the largest line count among the
    /// panes of the comparison (shorter files get a shorter map, like
    /// Meld's paired adjustments). With [`ChunkMap::wrap`], pass the
    /// largest number of visual rows instead.
    pub fn lines(mut self, line_count: usize, max_line_count: usize) -> Self {
        self.line_count = line_count.max(1);
        self.max_line_count = max_line_count.max(self.line_count);
        self
    }

    /// Current scroll offset, viewport height and line height of the pane.
    pub fn viewport(mut self, scroll_y: f32, viewport_height: f32, line_height: f32) -> Self {
        self.scroll_y = scroll_y;
        self.viewport_height = viewport_height;
        self.line_height = line_height;
        self
    }

    /// Overscroll lines of the pane (default 5).
    pub fn overscroll_lines(mut self, lines: usize) -> Self {
        self.overscroll_lines = lines;
        self
    }

    /// Wrap layout of the mapped pane (`None`: one row per line).
    pub fn wrap(mut self, layout: Option<&'a WrapLayout>) -> Self {
        self.wrap = layout;
        self
    }

    /// How the strip draws; the source styles need [`ChunkMap::source`].
    pub fn map_style(mut self, style: MapStyle) -> Self {
        self.map_style = style;
        self
    }

    /// The mapped pane's text, for the source miniature.
    pub fn source(mut self, buffer: &'a Buffer) -> Self {
        self.source = Some(buffer);
        self
    }

    /// Message for click/drag scrolling.
    pub fn on_scroll(mut self, f: impl Fn(MapScroll) -> Message + 'a) -> Self {
        self.on_scroll = Some(Box::new(f));
        self
    }

    /// Sets the width (default 20px).
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    fn rows(&self) -> usize {
        or_identity(self.wrap).total_rows(self.line_count)
    }

    fn upper(&self) -> f32 {
        (self.rows() + self.overscroll_lines) as f32 * self.line_height
    }

    fn height_scale(&self) -> f32 {
        let max_rows = self.max_line_count.max(self.rows());
        let max = (max_rows + self.overscroll_lines) as f32 * self.line_height;
        if max > 0.0 { self.upper() / max } else { 1.0 }
    }

    fn source_buffer(&self) -> Option<&'a Buffer> {
        self.source.filter(|_| self.map_style != MapStyle::Chunks)
    }

    /// Vertical offset of the miniature (whole lines) in a strip `height`
    /// tall: zero when the text fits, else following the pane's scroll
    /// position proportionally, like `GtkSourceMap`.
    fn source_offset(&self, lines: usize, height: f32) -> f32 {
        let total = lines as f32 * SOURCE_LINE;
        if total <= height {
            return 0.0;
        }
        let max = (self.upper() - self.viewport_height).max(1.0);
        let frac = (self.scroll_y / max).clamp(0.0, 1.0);
        ((total - height) * frac / SOURCE_LINE).round() * SOURCE_LINE
    }

    /// The scroll request for a click at `y` on the source miniature:
    /// centre the clicked line.
    fn source_scroll_for(&self, lines: usize, y: f32, height: f32) -> MapScroll {
        let offset = self.source_offset(lines, height);
        let pos = ((y + offset) / SOURCE_LINE).clamp(0.0, lines as f32);
        let line = (pos.floor() as usize).min(lines.saturating_sub(1));
        let wrap = or_identity(self.wrap);
        let lh = self.line_height;
        let y = wrap.y_for_line(line, lh) + pos.fract() * wrap.line_height(line, lh);
        let max = (self.upper() - self.viewport_height).max(0.0);
        MapScroll {
            fraction: pos / lines.max(1) as f32,
            scroll_y: (y - self.viewport_height / 2.0).clamp(0.0, max).floor(),
        }
    }

    fn scroll_at(&self, y: f32, height: f32) -> MapScroll {
        match self.source_buffer() {
            Some(b) => self.source_scroll_for(b.display_line_count(), y, height),
            None => self.scroll_for(y, height),
        }
    }

    /// The scroll request for a click at `y` (relative to the map).
    pub fn scroll_for(&self, y: f32, height: f32) -> MapScroll {
        let h = (self.height_scale() * height).max(1.0);
        let fraction = (y / h).clamp(0.0, 1.0);
        let upper = self.upper();
        let max = (upper - self.viewport_height).max(0.0);
        let scroll_y = (fraction * upper - self.viewport_height / 2.0)
            .clamp(0.0, max)
            .floor();
        MapScroll { fraction, scroll_y }
    }

    fn cache_key(&self, size: Size) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.chunks.hash(&mut h);
        self.line_count.hash(&mut h);
        self.max_line_count.hash(&mut h);
        self.line_height.to_bits().hash(&mut h);
        size.width.to_bits().hash(&mut h);
        size.height.to_bits().hash(&mut h);
        self.rows().hash(&mut h);
        self.map_style.hash(&mut h);
        if let Some(b) = self.source_buffer() {
            (b.revision(), b.line_count(), b.lines().as_ptr() as usize).hash(&mut h);
            self.source_offset(b.display_line_count(), size.height)
                .to_bits()
                .hash(&mut h);
            let t = self.style.text;
            for c in [t.r, t.g, t.b, t.a] {
                c.to_bits().hash(&mut h);
            }
        }
        let s = self.style;
        for c in [
            s.map_base,
            s.border,
            s.chunks.insert.fill,
            s.chunks.replace.fill,
            s.chunks.delete.fill,
        ] {
            c.r.to_bits().hash(&mut h);
            c.g.to_bits().hash(&mut h);
            c.b.to_bits().hash(&mut h);
        }
        h.finish()
    }
}

impl<Message> ChunkMap<'_, Message> {
    fn draw_source(
        &self,
        state: &MapState,
        renderer: &Renderer,
        buffer: &Buffer,
        bounds: Rectangle,
    ) -> Vec<Geometry> {
        let (width, height) = (bounds.width, bounds.height);
        let style = self.style;
        let lines = buffer.display_line_count();
        let offset = self.source_offset(lines, height);
        let first = (offset / SOURCE_LINE).floor() as usize;
        let last = (((offset + height) / SOURCE_LINE).ceil() as usize).min(lines);
        let key = self.cache_key(bounds.size());
        if state.key.get() != key {
            state.key.set(key);
            state.cache.clear();
        }
        let compact = self.map_style == MapStyle::CompactSource;
        let map = state.cache.draw(renderer, bounds.size(), |frame| {
            frame.fill_rectangle(Point::ORIGIN, bounds.size(), style.widget_background);
            let text_h = (lines as f32 * SOURCE_LINE - offset).min(height);
            frame.fill_rectangle(Point::ORIGIN, Size::new(width, text_h), style.map_base);
            let y_of = |line: usize| line as f32 * SOURCE_LINE - offset;
            // Chunk backgrounds (no room for zero-height inserts).
            let start = self.chunks.partition_point(|c| c.lines.end <= first);
            for c in &self.chunks[start..] {
                if c.lines.start >= last {
                    break;
                }
                if c.lines.is_empty() {
                    continue;
                }
                let y0 = y_of(c.lines.start);
                let h = (c.lines.end - c.lines.start) as f32 * SOURCE_LINE;
                frame.fill_rectangle(
                    Point::new(0.0, y0),
                    Size::new(width, h),
                    style.chunks.get(c.kind).fill,
                );
            }
            let ink = Color {
                a: if compact { 0.45 } else { 0.55 },
                ..style.text
            };
            let tab = buffer.tab_width().max(1);
            let cell = if compact {
                (width - 4.0) / COMPACT_COLUMNS
            } else {
                SOURCE_CHAR
            };
            let max_cols = ((width - 2.0) / cell).floor().max(0.0) as usize;
            let bar_h = SOURCE_LINE - 1.0;
            for line in first..last {
                let y = y_of(line) + 0.5;
                let runs = source_runs(buffer.line(line), tab, max_cols, compact);
                for (s, e) in runs {
                    frame.fill_rectangle(
                        Point::new(1.0 + s as f32 * cell, y),
                        Size::new((e - s) as f32 * cell, bar_h),
                        ink,
                    );
                }
            }
        });

        // Viewport handle over the lines shown in the pane.
        let mut frame = Frame::new(renderer, bounds.size());
        let wrap = or_identity(self.wrap);
        let lh = self.line_height;
        let at = |y: f32| {
            let (line, frac) = wrap.line_at_y(y, lh, lines);
            (line as f32 + frac) * SOURCE_LINE - offset
        };
        let y0 = at(self.scroll_y);
        let y1 = at(self.scroll_y + self.viewport_height).max(y0 + 2.0);
        let handle = Path::rectangle(
            Point::new(0.5, y0.round() + 0.5),
            Size::new(width - 1.0, (y1 - y0).round()),
        );
        let text = style.map_text;
        frame.fill(&handle, Color { a: 0.12, ..text });
        frame.stroke(
            &handle,
            Stroke::default()
                .with_width(1.0)
                .with_color(Color { a: 0.35, ..text }),
        );
        vec![map, frame.into_geometry()]
    }
}

/// Display-column runs of non-blank text in `line`, cut at `max_cols`;
/// `compact` merges them into one run from the indentation to the end.
fn source_runs(line: &str, tab: usize, max_cols: usize, compact: bool) -> Vec<(usize, usize)> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut col = 0usize;
    let mut run: Option<usize> = None;
    for ch in line.chars() {
        if col >= max_cols {
            break;
        }
        let w = if ch == '\t' { tab - col % tab } else { 1 };
        if ch.is_whitespace() {
            if let Some(s) = run.take() {
                runs.push((s, col));
            }
        } else if run.is_none() {
            run = Some(col);
        }
        col += w;
    }
    if let Some(s) = run {
        runs.push((s, col.min(max_cols)));
    }
    if compact && let (Some(&(s, _)), Some(&(_, e))) = (runs.first(), runs.last()) {
        runs = vec![(s, e)];
    }
    runs.retain(|(s, e)| e > s);
    runs
}

/// Canvas state of a [`ChunkMap`].
#[derive(Default)]
pub struct MapState {
    cache: Cache,
    key: Cell<u64>,
    dragging: bool,
}

impl<Message, Theme> canvas::Program<Message, Theme, Renderer> for ChunkMap<'_, Message> {
    type State = MapState;

    fn update(
        &self,
        state: &mut MapState,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        let on_scroll = self.on_scroll.as_ref()?;
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_in(bounds)?;
                state.dragging = true;
                Some(Action::publish(on_scroll(self.scroll_at(p.y, bounds.height))).and_capture())
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) if state.dragging => {
                let y = position.y - bounds.y;
                Some(Action::publish(on_scroll(self.scroll_at(y, bounds.height))).and_capture())
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if state.dragging => {
                state.dragging = false;
                Some(Action::capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &MapState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let (width, height) = (bounds.width, bounds.height);
        if width <= 0.0 || height <= 0.0 {
            return Vec::new();
        }
        if let Some(buffer) = self.source_buffer() {
            return self.draw_source(state, renderer, buffer, bounds);
        }
        let style = self.style;
        let x0 = OVERDRAW + 0.5;
        let x1 = width - 2.0 * x0;
        let hs = height * self.height_scale();

        let key = self.cache_key(bounds.size());
        if state.key.get() != key {
            state.key.set(key);
            state.cache.clear();
        }
        let map = state.cache.draw(renderer, bounds.size(), |frame| {
            frame.fill_rectangle(Point::new(0.0, 0.0), bounds.size(), style.widget_background);
            frame.fill_rectangle(
                Point::new(x0, -0.5),
                Size::new(x1, hs + 0.5),
                style.map_base,
            );
            let wrap = or_identity(self.wrap);
            let max_y = self.rows() as f32;
            for c in self.chunks {
                let colors = style.chunks.get(c.kind);
                let f0 = wrap.rows_before(c.lines.start) as f32 / max_y;
                let f1 = wrap.rows_before(c.lines.end) as f32 / max_y;
                let y0 = (f0 * hs).round() + 0.5;
                let y1 = (f1 * hs).round() - 0.5;
                let rect = Path::rectangle(Point::new(x0, y0), Size::new(x1, (y1 - y0).max(0.0)));
                frame.fill(&rect, colors.fill);
                frame.stroke(
                    &rect,
                    Stroke::default().with_width(1.0).with_color(colors.line),
                );
            }
            let outline = Path::rectangle(Point::new(x0, 0.5), Size::new(x1, hs - 1.0));
            frame.stroke(
                &outline,
                Stroke::default().with_width(1.0).with_color(style.border),
            );
        });

        // Viewport handle.
        let mut frame = Frame::new(renderer, bounds.size());
        let upper = self.upper();
        if upper > 0.0 {
            let adj_y = self.scroll_y / upper;
            let adj_h = (self.viewport_height / upper).min(1.0);
            let handle = Path::rectangle(
                Point::new(x0 - OVERDRAW, (hs * adj_y).round() + 0.5),
                Size::new(x1 + 2.0 * OVERDRAW, ((hs * adj_h).round() - 1.0).max(1.0)),
            );
            let text = style.map_text;
            frame.fill(&handle, Color { a: 0.2, ..text });
            frame.stroke(
                &handle,
                Stroke::default()
                    .with_width(1.0)
                    .with_color(Color { a: 0.4, ..text }),
            );
        }
        vec![map, frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &MapState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if self.on_scroll.is_some() && cursor.is_over(bounds) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

impl<'a, Message: 'a, Theme: 'a> From<ChunkMap<'a, Message>>
    for Element<'a, Message, Theme, Renderer>
{
    fn from(map: ChunkMap<'a, Message>) -> Self {
        let width = map.width;
        canvas::Canvas::new(map)
            .width(width)
            .height(Length::Fill)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_fraction_centres_view() {
        let style = PaneStyle::light();
        let map: ChunkMap<'_, ()> = chunk_map(&[], &style)
            .lines(95, 95)
            .viewport(0.0, 200.0, 10.0)
            .overscroll_lines(5);
        // upper = 1000, map height 500 → y=250 is the middle.
        let s = map.scroll_for(250.0, 500.0);
        assert_eq!(s.fraction, 0.5);
        assert_eq!(s.scroll_y, 400.0);
        assert_eq!(map.scroll_for(0.0, 500.0).scroll_y, 0.0);
        assert_eq!(map.scroll_for(500.0, 500.0).scroll_y, 800.0);
    }

    #[test]
    fn wrapped_rows_scale_the_map() {
        use crate::wrap::WrapMode;
        let mut lines: Vec<String> = (0..90).map(|i| i.to_string()).collect();
        lines[0] = "aaaa ".repeat(6);
        let buffer = Buffer::new(lines);
        let mut wrap = WrapLayout::new(WrapMode::Word);
        wrap.set_columns(5);
        wrap.update(&buffer);
        let style = PaneStyle::light();
        let map: ChunkMap<'_, ()> = chunk_map(&[], &style)
            .lines(90, 95)
            .wrap(Some(&wrap))
            .viewport(0.0, 200.0, 10.0);
        // 95 rows: as tall as the longest pane.
        assert_eq!(map.rows(), 95);
        assert_eq!(map.height_scale(), 1.0);
        assert_eq!(map.upper(), 1000.0);
    }

    #[test]
    fn source_map_scrolls_with_the_pane() {
        let buffer = Buffer::new((0..1000).map(|i| i.to_string()).collect());
        let style = PaneStyle::light();
        let map = |scroll_y: f32| -> ChunkMap<'_, ()> {
            chunk_map(&[], &style)
                .lines(1000, 1000)
                .viewport(scroll_y, 200.0, 10.0)
                .map_style(MapStyle::FullSource)
                .source(&buffer)
        };
        // 3000px of miniature in a 600px strip.
        assert_eq!(map(0.0).source_offset(1000, 600.0), 0.0);
        let max = 10050.0 - 200.0;
        assert_eq!(map(max).source_offset(1000, 600.0), 2400.0);
        // Clicking line 100 of the miniature centres it.
        let s = map(0.0).source_scroll_for(1000, 300.0, 600.0);
        assert_eq!(s.scroll_y, 1000.0 - 100.0);
        // Short files are not scrolled.
        assert_eq!(map(500.0).source_offset(100, 600.0), 0.0);
    }

    #[test]
    fn source_runs_follow_the_text() {
        assert_eq!(source_runs("  ab cd", 4, 80, false), [(2, 4), (5, 7)]);
        assert_eq!(source_runs("  ab cd", 4, 80, true), [(2, 7)]);
        assert_eq!(source_runs("\tx", 4, 80, false), [(4, 5)]);
        assert_eq!(source_runs("abcdef", 4, 3, false), [(0, 3)]);
        assert!(source_runs("   ", 4, 80, true).is_empty());
    }

    #[test]
    fn shorter_file_gets_shorter_map() {
        let style = PaneStyle::light();
        let map: ChunkMap<'_, ()> = chunk_map(&[], &style)
            .lines(45, 95)
            .viewport(0.0, 200.0, 10.0);
        assert_eq!(map.height_scale(), 0.5);
        assert_eq!(map.scroll_for(250.0, 500.0).fraction, 1.0);
    }
}
