//! Per-chunk action buttons in a gutter beside a pane (replaces
//! `meld/actiongutter.py`).
//!
//! ```ignore
//! action_gutter(&chunks_from_left_to_right, &style)
//!     .scroll(scroll_y_of_source_pane)
//!     .line_height(metrics.line_height)
//!     .editable(left_editable, right_editable)
//!     .current(current_chunk)
//!     .on_action(|chunk, action| Message::ChunkAction(0, 1, chunk, action))
//! ```
//!
//! Chunks are [`DiffChunk`]s seen from the source pane (`a` = source lines,
//! `b` = target lines). The gutter draws the chunk backgrounds aligned to
//! the source pane and one button on each chunk's first line.
//!
//! The action follows Meld's modifier convention: plain click replaces
//! (push the chunk to the target), Shift deletes the chunk from the source,
//! Ctrl inserts (copy). The gutter tracks modifier keys itself unless the
//! application fixes the mode with [`ActionGutter::action_mode`]. Meld pops
//! up a menu for Ctrl+click; here the button is split instead: the upper
//! half copies above the target chunk, the lower half below.

use std::marker::PhantomData;

use iced::keyboard;
use iced::mouse;
use iced::widget::canvas::{self, Action, Event, Frame, Geometry, Path, Stroke};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Size};

use crate::chunk::{ChunkKind, DiffChunk};
use crate::style::PaneStyle;

/// What a chunk button does (Meld's `ActionMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ActionMode {
    /// Replace the target lines with the source lines.
    #[default]
    Replace,
    /// Delete the source lines.
    Delete,
    /// Copy the source lines next to the target lines.
    Insert,
}

impl ActionMode {
    /// Meld's mode for the held modifiers (Shift → delete, Ctrl → insert).
    pub fn from_modifiers(modifiers: keyboard::Modifiers) -> Self {
        if modifiers.shift() {
            ActionMode::Delete
        } else if modifiers.control() {
            ActionMode::Insert
        } else {
            ActionMode::Replace
        }
    }
}

/// An action activated on a chunk (Meld's `ChunkAction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChunkAction {
    /// Delete the chunk's source lines.
    Delete,
    /// Replace the target lines with the source lines.
    Replace,
    /// Insert the source lines above the target lines.
    CopyUp,
    /// Insert the source lines below the target lines.
    CopyDown,
}

/// Direction of the replace arrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    /// Changes flow to the right (gutter on the right of its source).
    #[default]
    Right,
    /// Changes flow to the left.
    Left,
}

/// Creates an [`ActionGutter`].
pub fn action_gutter<'a, Message>(
    chunks: &'a [DiffChunk],
    style: &'a PaneStyle,
) -> ActionGutter<'a, Message> {
    ActionGutter::new(chunks, style)
}

/// Action gutter builder. Convert into an [`Element`] with `.into()`.
pub struct ActionGutter<'a, Message> {
    chunks: &'a [DiffChunk],
    style: &'a PaneStyle,
    scroll_y: f32,
    offset: f32,
    line_height: f32,
    source_editable: bool,
    target_editable: bool,
    mode: Option<ActionMode>,
    direction: Direction,
    current: Option<usize>,
    on_action: Option<Box<dyn Fn(usize, ChunkAction) -> Message + 'a>>,
    width: Length,
    _message: PhantomData<Message>,
}

impl<'a, Message> ActionGutter<'a, Message> {
    /// Creates a gutter for chunks seen from the source pane.
    pub fn new(chunks: &'a [DiffChunk], style: &'a PaneStyle) -> Self {
        Self {
            chunks,
            style,
            scroll_y: 0.0,
            offset: 0.0,
            line_height: 18.0,
            source_editable: true,
            target_editable: true,
            mode: None,
            direction: Direction::Right,
            current: None,
            on_action: None,
            width: Length::Fixed(20.0),
            _message: PhantomData,
        }
    }

    /// Vertical scroll offset of the source pane.
    pub fn scroll(mut self, scroll_y: f32) -> Self {
        self.scroll_y = scroll_y;
        self
    }

    /// Offset of the source pane's first line relative to the gutter top.
    pub fn pane_offset(mut self, offset: f32) -> Self {
        self.offset = offset;
        self
    }

    /// Line height of the source pane.
    pub fn line_height(mut self, line_height: f32) -> Self {
        self.line_height = line_height;
        self
    }

    /// Whether the source and target panes are editable.
    pub fn editable(mut self, source: bool, target: bool) -> Self {
        self.source_editable = source;
        self.target_editable = target;
        self
    }

    /// Fixes the action mode instead of following the modifier keys.
    pub fn action_mode(mut self, mode: ActionMode) -> Self {
        self.mode = Some(mode);
        self
    }

    /// Arrow direction for replace buttons.
    pub fn direction(mut self, direction: Direction) -> Self {
        self.direction = direction;
        self
    }

    /// Index of the current chunk.
    pub fn current(mut self, current: Option<usize>) -> Self {
        self.current = current;
        self
    }

    /// Message for an activated button: `(chunk index, action)`.
    pub fn on_action(mut self, f: impl Fn(usize, ChunkAction) -> Message + 'a) -> Self {
        self.on_action = Some(Box::new(f));
        self
    }

    /// Sets the width (default 20px).
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    fn y_for_line(&self, line: usize) -> f32 {
        line as f32 * self.line_height - self.scroll_y + self.offset
    }

    /// Port of `_classify_change_actions`.
    pub fn classify(&self, chunk: &DiffChunk, mode: ActionMode) -> Option<ActionMode> {
        classify(chunk, mode, self.source_editable, self.target_editable)
    }

    fn visible(&self, height: f32) -> std::ops::Range<usize> {
        let top = self.scroll_y - self.offset;
        let first = (top / self.line_height).floor().max(0.0) as usize;
        let last = ((top + height) / self.line_height).ceil().max(0.0) as usize;
        // Chunks starting at or before `last` and ending at or after `first`.
        let start = self.chunks.partition_point(|c| c.a.end < first);
        let end = self.chunks.partition_point(|c| c.a.start <= last);
        start..end.max(start)
    }

    fn button_rect(&self, chunk: &DiffChunk, width: f32) -> Rectangle {
        let y = self.y_for_line(chunk.a.start) + 1.0;
        Rectangle::new(
            Point::new(1.0, y),
            Size::new((width - 2.0).max(1.0), (self.line_height - 2.0).max(1.0)),
        )
    }

    /// Button under a point (relative to the gutter): `(chunk, mode)`.
    fn hit(&self, p: Point, size: Size, mode: ActionMode) -> Option<(usize, ActionMode)> {
        let range = self.visible(size.height);
        self.chunks[range.clone()]
            .iter()
            .enumerate()
            .find_map(|(i, c)| {
                let action = self.classify(c, mode)?;
                self.button_rect(c, size.width)
                    .contains(p)
                    .then_some((range.start + i, action))
            })
    }

    fn mode(&self, state: &GutterState) -> ActionMode {
        self.mode
            .unwrap_or_else(|| ActionMode::from_modifiers(state.modifiers))
    }
}

/// Meld's `_classify_change_actions`: which action a chunk button offers.
pub fn classify(
    chunk: &DiffChunk,
    mode: ActionMode,
    source_editable: bool,
    target_editable: bool,
) -> Option<ActionMode> {
    if !source_editable && !target_editable {
        return None;
    }
    let mut kind = chunk.kind;
    if kind == ChunkKind::Conflict {
        kind = if chunk.a.is_empty() {
            ChunkKind::Insert
        } else if chunk.b.is_empty() {
            ChunkKind::Delete
        } else {
            ChunkKind::Replace
        };
    }
    if kind == ChunkKind::Insert {
        return None;
    }
    let mut action = Some(mode);
    if mode == ActionMode::Delete && !source_editable {
        action = None;
    } else if mode == ActionMode::Insert && kind == ChunkKind::Delete {
        action = Some(ActionMode::Replace);
    }
    if !target_editable {
        action = Some(ActionMode::Delete);
    }
    action
}

/// Canvas state of an [`ActionGutter`].
#[derive(Debug, Default)]
pub struct GutterState {
    modifiers: keyboard::Modifiers,
    hovered: Option<(usize, bool)>,
    pressed: Option<(usize, bool)>,
}

impl<Message, Theme> canvas::Program<Message, Theme, Renderer> for ActionGutter<'_, Message> {
    type State = GutterState;

    fn update(
        &self,
        state: &mut GutterState,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        let mut redraw = false;
        if let Event::Keyboard(keyboard::Event::ModifiersChanged(m)) = event {
            redraw = state.modifiers != *m;
            state.modifiers = *m;
        }
        let mode = self.mode(state);
        let hit = cursor.position_in(bounds).and_then(|p| {
            self.hit(p, bounds.size(), mode).map(|(i, m)| {
                let r = self.button_rect(&self.chunks[i], bounds.width);
                let upper = p.y < r.center_y();
                (i, m, upper)
            })
        });
        let hovered = hit.map(|(i, m, upper)| (i, m == ActionMode::Insert && upper));
        if hovered != state.hovered {
            state.hovered = hovered;
            redraw = true;
        }
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(h) = hovered {
                    state.pressed = Some(h);
                    return Some(Action::request_redraw().and_capture());
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if let Some(pressed) = state.pressed.take() {
                    if let (Some((i, m, upper)), Some(f)) = (hit, &self.on_action)
                        && i == pressed.0
                    {
                        let action = match m {
                            ActionMode::Replace => ChunkAction::Replace,
                            ActionMode::Delete => ChunkAction::Delete,
                            ActionMode::Insert if upper => ChunkAction::CopyUp,
                            ActionMode::Insert => ChunkAction::CopyDown,
                        };
                        return Some(Action::publish(f(i, action)).and_capture());
                    }
                    return Some(Action::request_redraw());
                }
            }
            _ => {}
        }
        redraw.then(Action::request_redraw)
    }

    fn draw(
        &self,
        state: &GutterState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let style = self.style;
        let width = bounds.width;
        let mut frame = Frame::new(renderer, bounds.size());
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), style.widget_background);
        let mode = self.mode(state);
        let range = self.visible(bounds.height);

        for (i, chunk) in self.chunks[range.clone()].iter().enumerate() {
            let index = range.start + i;
            let colors = style.chunks.get(chunk.kind);
            let y = self.y_for_line(chunk.a.start);
            let h = (self.y_for_line(chunk.a.end) - y + 1.0).max(2.0);
            if !chunk.a.is_empty() {
                frame.fill_rectangle(Point::new(-0.5, y), Size::new(width + 1.0, h), colors.fill);
                if self.current == Some(index) {
                    frame.fill_rectangle(
                        Point::new(-0.5, y),
                        Size::new(width + 1.0, h),
                        style.chunks.current_chunk,
                    );
                }
            }
            frame.fill_rectangle(Point::new(0.0, y), Size::new(width, 1.0), colors.line);
            frame.fill_rectangle(
                Point::new(0.0, y + h - 1.0),
                Size::new(width, 1.0),
                colors.line,
            );

            let Some(action) = self.classify(chunk, mode) else {
                continue;
            };
            let r = self.button_rect(chunk, width);
            let half = |upper: bool| {
                if action == ActionMode::Insert {
                    Rectangle {
                        y: if upper { r.y } else { r.center_y() },
                        height: r.height / 2.0,
                        ..r
                    }
                } else {
                    r
                }
            };
            for (states, factor) in [(state.pressed, 2.0), (state.hovered, 1.0)] {
                if let Some((hi, upper)) = states
                    && hi == index
                {
                    let rr = half(upper);
                    let path = Path::rounded_rectangle(rr.position(), rr.size(), 3.0.into());
                    let hover = style.button_hover;
                    frame.fill(
                        &path,
                        Color {
                            a: (hover.a * factor).min(1.0),
                            ..hover
                        },
                    );
                    break;
                }
            }
            draw_icon(&mut frame, r, action, self.direction, style.button_icon);
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &GutterState,
        _bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.hovered.is_some() {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

fn draw_icon(frame: &mut Frame, r: Rectangle, action: ActionMode, dir: Direction, color: Color) {
    let s = (r.height.min(r.width) * 0.62).max(6.0);
    let c = Point::new(r.center_x().round(), r.center_y().round());
    let stroke = Stroke::default().with_width(1.6).with_color(color);
    match action {
        ActionMode::Replace => {
            let sign = if dir == Direction::Right { 1.0 } else { -1.0 };
            let tip = Point::new(c.x + sign * s / 2.0, c.y);
            let path = Path::new(|p| {
                p.move_to(Point::new(c.x - sign * s / 2.0, c.y));
                p.line_to(tip);
                p.move_to(Point::new(tip.x - sign * s * 0.4, c.y - s * 0.4));
                p.line_to(tip);
                p.line_to(Point::new(tip.x - sign * s * 0.4, c.y + s * 0.4));
            });
            frame.stroke(&path, stroke);
        }
        ActionMode::Delete => {
            let d = s * 0.38;
            let path = Path::new(|p| {
                p.move_to(Point::new(c.x - d, c.y - d));
                p.line_to(Point::new(c.x + d, c.y + d));
                p.move_to(Point::new(c.x + d, c.y - d));
                p.line_to(Point::new(c.x - d, c.y + d));
            });
            frame.stroke(&path, stroke);
        }
        ActionMode::Insert => {
            // Up and down chevrons for copy above / copy below.
            let d = s * 0.32;
            let up = r.y + r.height * 0.25;
            let down = r.y + r.height * 0.75;
            let path = Path::new(|p| {
                p.move_to(Point::new(c.x - d, up + d / 2.0));
                p.line_to(Point::new(c.x, up - d / 2.0));
                p.line_to(Point::new(c.x + d, up + d / 2.0));
                p.move_to(Point::new(c.x - d, down - d / 2.0));
                p.line_to(Point::new(c.x, down + d / 2.0));
                p.line_to(Point::new(c.x + d, down - d / 2.0));
            });
            frame.stroke(&path, Stroke::default().with_width(1.4).with_color(color));
        }
    }
}

impl<'a, Message: 'a, Theme: 'a> From<ActionGutter<'a, Message>>
    for Element<'a, Message, Theme, Renderer>
{
    fn from(gutter: ActionGutter<'a, Message>) -> Self {
        let width = gutter.width;
        canvas::Canvas::new(gutter)
            .width(width)
            .height(Length::Fill)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(kind: ChunkKind, a: std::ops::Range<usize>, b: std::ops::Range<usize>) -> DiffChunk {
        DiffChunk::new(kind, a, b)
    }

    #[test]
    fn classify_matches_meld() {
        let replace = chunk(ChunkKind::Replace, 0..2, 0..3);
        let delete = chunk(ChunkKind::Delete, 0..2, 0..0);
        let insert = chunk(ChunkKind::Insert, 0..0, 0..2);
        use ActionMode::*;
        assert_eq!(classify(&replace, Replace, true, true), Some(Replace));
        assert_eq!(classify(&replace, Delete, true, true), Some(Delete));
        assert_eq!(classify(&replace, Insert, true, true), Some(Insert));
        assert_eq!(classify(&delete, Insert, true, true), Some(Replace));
        assert_eq!(classify(&insert, Replace, true, true), None);
        assert_eq!(classify(&replace, Replace, false, false), None);
        assert_eq!(classify(&replace, Delete, false, true), None);
        assert_eq!(classify(&replace, Replace, true, false), Some(Delete));
        // Conflicts are reclassified by their ranges.
        let c = chunk(ChunkKind::Conflict, 0..0, 0..2);
        assert_eq!(classify(&c, Replace, true, true), None);
        let c = chunk(ChunkKind::Conflict, 0..2, 0..0);
        assert_eq!(classify(&c, Insert, true, true), Some(Replace));
    }

    #[test]
    fn modifiers_select_mode() {
        assert_eq!(
            ActionMode::from_modifiers(keyboard::Modifiers::SHIFT),
            ActionMode::Delete
        );
        assert_eq!(
            ActionMode::from_modifiers(keyboard::Modifiers::CTRL),
            ActionMode::Insert
        );
        assert_eq!(
            ActionMode::from_modifiers(keyboard::Modifiers::empty()),
            ActionMode::Replace
        );
    }
}
