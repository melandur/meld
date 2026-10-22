//! The strip between two panes that links corresponding chunks with
//! curved, filled polygons (replaces `meld/linkmap.py`).
//!
//! ```ignore
//! link_map(&chunks, &style)
//!     .scroll(scroll_left, scroll_right)
//!     .line_height(metrics.line_height)
//!     .current(current_chunk)
//! ```
//!
//! `chunks` are [`DiffChunk`]s with `a` = left pane lines and `b` = right
//! pane lines, sorted. The widget is a [`canvas`]. Its only message is the
//! optional [`LinkMap::on_scroll`]: Meld moves to the previous or next
//! change when the wheel is turned over the link map.

use std::f32::consts::FRAC_PI_2;
use std::marker::PhantomData;
use std::ops::Range;
use std::time::Duration;

use iced::time::Instant;

use iced::mouse;
use iced::widget::canvas::{self, Action, Event, Frame, Geometry, Path, Stroke};
use iced::{Element, Length, Point, Rectangle, Renderer};

use crate::chunk::{DiffChunk, visible_chunk_range};
use crate::style::PaneStyle;
use crate::wrap::{WrapLayout, or_identity};

/// Pixels of smooth (touchpad) scrolling that count as one wheel step.
const SCROLL_STEP: f32 = 40.0;

/// Rounded corner radius for culled chunk ends (Meld's `RADIUS`).
const RADIUS: f32 = 3.0;

/// Creates a [`LinkMap`].
pub fn link_map<'a, Message>(
    chunks: &'a [DiffChunk],
    style: &'a PaneStyle,
) -> LinkMap<'a, Message> {
    LinkMap::new(chunks, style)
}

/// Link map builder. Convert into an [`Element`] with `.into()`.
pub struct LinkMap<'a, Message> {
    chunks: &'a [DiffChunk],
    style: &'a PaneStyle,
    scroll: [f32; 2],
    offset: [f32; 2],
    heights: [f32; 2],
    wrap: [Option<&'a WrapLayout>; 2],
    line_height: f32,
    line_count: [usize; 2],
    current: Option<usize>,
    on_scroll: Option<Box<dyn Fn(bool) -> Message + 'a>>,
    width: Length,
    _message: PhantomData<Message>,
}

impl<'a, Message> LinkMap<'a, Message> {
    /// Creates a link map for `chunks` (left = `a`, right = `b`).
    pub fn new(chunks: &'a [DiffChunk], style: &'a PaneStyle) -> Self {
        Self {
            chunks,
            style,
            scroll: [0.0; 2],
            offset: [0.0; 2],
            heights: [0.0; 2],
            wrap: [None; 2],
            line_height: 18.0,
            line_count: [usize::MAX; 2],
            current: None,
            on_scroll: None,
            width: Length::Fixed(50.0),
            _message: PhantomData,
        }
    }

    /// Vertical scroll offsets of the left and right panes, in pixels.
    pub fn scroll(mut self, left: f32, right: f32) -> Self {
        self.scroll = [left, right];
        self
    }

    /// Vertical offset of each pane's first line relative to the top of
    /// the link map (e.g. when a pane has an info bar above it).
    pub fn pane_offsets(mut self, left: f32, right: f32) -> Self {
        self.offset = [left, right];
        self
    }

    /// Heights of the panes' text areas. A pane shorter than the link map
    /// sits at its bottom (a message area above it), so its lines are
    /// offset by the difference.
    pub fn pane_heights(mut self, left: f32, right: f32) -> Self {
        self.heights = [left, right];
        self
    }

    /// Wrap layouts of the panes (`None`: one row per line).
    pub fn wrap(mut self, left: Option<&'a WrapLayout>, right: Option<&'a WrapLayout>) -> Self {
        self.wrap = [left, right];
        self
    }

    /// Message for the mouse wheel over the map: `true` when scrolling
    /// down (Meld goes to the next change), `false` up.
    pub fn on_scroll(mut self, f: impl Fn(bool) -> Message + 'a) -> Self {
        self.on_scroll = Some(Box::new(f));
        self
    }

    /// Line height shared by both panes.
    pub fn line_height(mut self, line_height: f32) -> Self {
        self.line_height = line_height;
        self
    }

    /// Line counts of the panes (optional; used to clamp visibility).
    pub fn line_counts(mut self, left: usize, right: usize) -> Self {
        self.line_count = [left, right];
        self
    }

    /// Index of the current chunk in `chunks`, drawn emphasised.
    pub fn current(mut self, current: Option<usize>) -> Self {
        self.current = current;
        self
    }

    /// Sets the width (default 50px, like Meld's linkmap).
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Offset of a pane's first line below the top of a map `height` tall.
    fn offset(&self, side: usize, height: f32) -> f32 {
        let h = self.heights[side];
        let below = if h > 0.0 && h < height {
            height - h
        } else {
            0.0
        };
        self.offset[side] + below
    }

    fn visible(&self, side: usize, height: f32) -> Range<usize> {
        let top = self.scroll[side] - self.offset(side, height);
        or_identity(self.wrap[side]).visible_lines(
            top,
            height,
            self.line_height,
            self.line_count[side],
        )
    }

    fn draw_into(&self, frame: &mut Frame, bounds: Rectangle) {
        let style = self.style;
        let (width, height) = (bounds.width, bounds.height);
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), style.widget_background);

        let vis_a = self.visible(0, height);
        let vis_b = self.visible(1, height);
        let range = visible_chunk_range(self.chunks, &vis_a, &vis_b);
        let lh = self.line_height;
        let offsets = [self.offset(0, height), self.offset(1, height)];
        let y = |side: usize, line: usize| {
            or_identity(self.wrap[side]).y_for_line(line, lh) - self.scroll[side]
                + offsets[side]
                + 1.0
        };
        let x_steps = [-0.5, width / 2.0, width + 0.5];
        let stroke = |color| Stroke::default().with_width(1.0).with_color(color);

        for (i, c) in self.chunks[range.clone()].iter().enumerate() {
            let index = range.start + i;
            let (f0, mut f1) = (y(0, c.a.start), y(0, c.a.end));
            let (t0, mut t1) = (y(1, c.b.start), y(1, c.b.end));
            // We want the last pixel of the previous line.
            if f1 != f0 {
                f1 -= 1.0;
            }
            if t1 != t0 {
                t1 -= 1.0;
            }

            let off = |a: f32, b: f32| (a < 0.0 && b < 0.0) || (a > height && b > height);
            let path = if off(t0, t1) {
                // Right end off-screen: draw a rounded tab on the left.
                if f0 == f1 {
                    continue;
                }
                Path::new(|p| {
                    arc(
                        p,
                        Point::new(x_steps[0], f0 - 0.5 + RADIUS),
                        3.0 * FRAC_PI_2,
                        4.0 * FRAC_PI_2,
                        true,
                    );
                    arc(
                        p,
                        Point::new(x_steps[0], f1 + 0.5 - RADIUS),
                        0.0,
                        FRAC_PI_2,
                        false,
                    );
                    p.close();
                })
            } else if off(f0, f1) {
                if t0 == t1 {
                    continue;
                }
                Path::new(|p| {
                    arc(
                        p,
                        Point::new(x_steps[2], t0 - 0.5 + RADIUS),
                        3.0 * FRAC_PI_2,
                        2.0 * FRAC_PI_2,
                        true,
                    );
                    arc(
                        p,
                        Point::new(x_steps[2], t1 + 0.5 - RADIUS),
                        2.0 * FRAC_PI_2,
                        FRAC_PI_2,
                        false,
                    );
                    p.close();
                })
            } else {
                Path::new(|p| {
                    p.move_to(Point::new(x_steps[0], f0 - 0.5));
                    p.bezier_curve_to(
                        Point::new(x_steps[1], f0 - 0.5),
                        Point::new(x_steps[1], t0 - 0.5),
                        Point::new(x_steps[2], t0 - 0.5),
                    );
                    p.line_to(Point::new(x_steps[2], t1 + 0.5));
                    p.bezier_curve_to(
                        Point::new(x_steps[1], t1 + 0.5),
                        Point::new(x_steps[1], f1 + 0.5),
                        Point::new(x_steps[0], f1 + 0.5),
                    );
                    p.close();
                })
            };

            let colors = style.chunks.get(c.kind);
            frame.fill(&path, colors.fill);
            if self.current == Some(index) {
                frame.fill(&path, style.chunks.current_chunk);
            }
            frame.stroke(&path, stroke(colors.line));
        }
    }
}

/// Appends a circular arc (clockwise for increasing angles) as line
/// segments, continuing the current sub-path unless `first`.
fn arc(p: &mut canvas::path::Builder, center: Point, start: f32, end: f32, first: bool) {
    const STEPS: usize = 6;
    for i in 0..=STEPS {
        let t = start + (end - start) * i as f32 / STEPS as f32;
        let pt = Point::new(center.x + RADIUS * t.cos(), center.y + RADIUS * t.sin());
        if i == 0 && first {
            p.move_to(pt);
        } else {
            p.line_to(pt);
        }
    }
}

/// Canvas state of a [`LinkMap`]: smooth scrolling not yet turned into
/// a step, and when the last step was sent.
#[derive(Debug, Default)]
pub struct LinkState {
    pending: f32,
    last_step: Option<(Instant, bool)>,
}

/// Steps in the same direction closer than this are one wheel notch
/// reported twice (X11 sends both a legacy and a smooth scroll event).
const STEP_DEBOUNCE: Duration = Duration::from_millis(40);

impl<Message, Theme> canvas::Program<Message, Theme, Renderer> for LinkMap<'_, Message> {
    type State = LinkState;

    fn update(
        &self,
        state: &mut LinkState,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        let on_scroll = self.on_scroll.as_ref()?;
        let Event::Mouse(mouse::Event::WheelScrolled { delta }) = event else {
            return None;
        };
        if !cursor.is_over(bounds) {
            return None;
        }
        let down = match *delta {
            mouse::ScrollDelta::Lines { y, .. } if y != 0.0 => y < 0.0,
            mouse::ScrollDelta::Pixels { y, .. } => {
                state.pending -= y;
                if state.pending.abs() < SCROLL_STEP {
                    return Some(Action::capture());
                }
                let down = state.pending > 0.0;
                state.pending = 0.0;
                down
            }
            _ => return None,
        };
        let now = Instant::now();
        if state
            .last_step
            .is_some_and(|(at, d)| d == down && now.duration_since(at) < STEP_DEBOUNCE)
        {
            return Some(Action::capture());
        }
        state.last_step = Some((now, down));
        Some(Action::publish(on_scroll(down)).and_capture())
    }

    fn draw(
        &self,
        _state: &LinkState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        self.draw_into(&mut frame, bounds);
        vec![frame.into_geometry()]
    }
}

impl<'a, Message: 'a, Theme: 'a> From<LinkMap<'a, Message>>
    for Element<'a, Message, Theme, Renderer>
{
    fn from(map: LinkMap<'a, Message>) -> Self {
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
    use crate::chunk::ChunkKind;

    #[test]
    fn visible_chunk_selection() {
        let chunks = vec![
            DiffChunk::new(ChunkKind::Replace, 0..2, 0..3),
            DiffChunk::new(ChunkKind::Insert, 10..10, 11..15),
            DiffChunk::new(ChunkKind::Delete, 50..60, 55..55),
            DiffChunk::new(ChunkKind::Replace, 100..101, 95..96),
        ];
        assert_eq!(visible_chunk_range(&chunks, &(0..5), &(0..5)), 0..1);
        assert_eq!(visible_chunk_range(&chunks, &(8..20), &(8..20)), 1..2);
        assert_eq!(visible_chunk_range(&chunks, &(40..70), &(45..75)), 2..3);
        assert_eq!(visible_chunk_range(&chunks, &(200..220), &(200..220)), 4..4);
        // Chunk visible on one side only still counts.
        assert_eq!(visible_chunk_range(&chunks, &(98..120), &(90..110)), 3..4);
    }

    #[test]
    fn panes_below_a_message_are_offset() {
        let style = PaneStyle::light();
        let map: LinkMap<'_, ()> = link_map(&[], &style)
            .line_height(10.0)
            .pane_heights(400.0, 340.0)
            .line_counts(1000, 1000);
        assert_eq!(map.offset(0, 400.0), 0.0);
        assert_eq!(map.offset(1, 400.0), 60.0);
        assert_eq!(map.visible(0, 400.0), 0..40);
        assert_eq!(map.visible(1, 400.0), 0..34);
    }

    #[test]
    fn wrapped_panes_use_their_rows() {
        use crate::buffer::Buffer;
        use crate::wrap::WrapMode;
        let mut lines: Vec<String> = (0..50).map(|i| i.to_string()).collect();
        lines[0] = "aaaa ".repeat(10);
        let buffer = Buffer::new(lines);
        let mut wrap = WrapLayout::new(WrapMode::Word);
        wrap.set_columns(5);
        wrap.update(&buffer);
        let style = PaneStyle::light();
        let map: LinkMap<'_, ()> = link_map(&[], &style)
            .line_height(10.0)
            .wrap(Some(&wrap), None)
            .line_counts(50, 50);
        // Line 0 takes 10 rows on the left: 100px shows only it.
        assert_eq!(map.visible(0, 100.0), 0..1);
        assert_eq!(map.visible(1, 100.0), 0..10);
    }
}
