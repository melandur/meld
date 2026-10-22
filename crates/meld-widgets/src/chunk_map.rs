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

use std::cell::Cell;
use std::hash::{Hash, Hasher};

use iced::mouse;
use iced::widget::canvas::{self, Action, Cache, Event, Frame, Geometry, Path, Stroke};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Size};

use crate::chunk::MapChunk;
use crate::style::PaneStyle;
use crate::text_pane::{DEFAULT_OVERSCROLL_LINES, content_height};

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
            on_scroll: None,
            width: Length::Fixed(20.0),
        }
    }

    /// Line count of the mapped pane, and the largest line count among the
    /// panes of the comparison (shorter files get a shorter map, like
    /// Meld's paired adjustments).
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

    fn upper(&self) -> f32 {
        content_height(self.line_count, self.line_height, self.overscroll_lines)
    }

    fn height_scale(&self) -> f32 {
        let max = content_height(self.max_line_count, self.line_height, self.overscroll_lines);
        if max > 0.0 { self.upper() / max } else { 1.0 }
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
                Some(Action::publish(on_scroll(self.scroll_for(p.y, bounds.height))).and_capture())
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) if state.dragging => {
                let y = position.y - bounds.y;
                Some(Action::publish(on_scroll(self.scroll_for(y, bounds.height))).and_capture())
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
            let max_y = self.line_count as f32;
            for c in self.chunks {
                let colors = style.chunks.get(c.kind);
                let f0 = c.lines.start as f32 / max_y;
                let f1 = c.lines.end as f32 / max_y;
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
    fn shorter_file_gets_shorter_map() {
        let style = PaneStyle::light();
        let map: ChunkMap<'_, ()> = chunk_map(&[], &style)
            .lines(45, 95)
            .viewport(0.0, 200.0, 10.0);
        assert_eq!(map.height_scale(), 0.5);
        assert_eq!(map.scroll_for(250.0, 500.0).fraction, 1.0);
    }
}
