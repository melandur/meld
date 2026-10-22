//! A code view/editor for one side of a comparison (replaces
//! `MeldSourceView` and `GutterRendererChunkLines`).
//!
//! The application owns a [`Buffer`] and the scroll offsets. The widget
//! draws only the visible lines, caches shaped paragraphs per line, and
//! emits [`Action`]s:
//!
//! - text/cursor actions go to [`Buffer::perform`], which returns the
//!   [`Edit`](crate::buffer::Edit) for the diff engine, undo stack and
//!   [`Highlighter::edit`];
//! - [`Action::Scrolled`] carries the new (clamped) scroll offsets; the
//!   application stores them and syncs the other panes (see
//!   [`crate::scroll_sync`]). The pane also emits `Scrolled` by itself to
//!   reveal the cursor after it moved;
//! - [`Action::Resized`] reports the text area size so the application
//!   knows the page height for syncing and the chunk map.
//!
//! The pane draws no vertical scrollbar: Meld's [`chunk_map`](crate::chunk_map)
//! overview next to it is the vertical scrolling control. A thin horizontal
//! scrollbar appears at the bottom when lines are wider than the view.
//!
//! Key presses the pane does not act on (Ctrl+S, Ctrl+Z, Alt+arrows,
//! F-keys, Escape, ...) are left uncaptured so that
//! `iced::keyboard::listen()` sees them.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use iced::advanced::clipboard::{self, Clipboard};
use iced::advanced::input_method::{self, InputMethod};
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Quad};
use iced::advanced::text::{self, Paragraph as _, Span};
use iced::advanced::widget::{self, Tree, Widget, operation, tree};
use iced::advanced::{Shell, mouse};
use iced::keyboard::{self, key};
use iced::time::Instant;
use iced::{
    Border, Color, Element, Event, Font, Length, Pixels, Point, Rectangle, Size, Vector, window,
};
use unicode_segmentation::UnicodeSegmentation;

pub use crate::buffer::{Action, Buffer, EditAction, Motion, Position};
use crate::buffer::{char_len, column_for_display};
use crate::chunk::{InlineHighlight, LineHighlight};
use crate::highlight::Highlighter;
use crate::style::{PaneStyle, TextStyle};

/// Text size and line height in logical pixels. Use the same metrics for
/// the panes, link maps and gutters so that they line up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// Font size.
    pub text_size: f32,
    /// Height of one line.
    pub line_height: f32,
}

impl Metrics {
    /// Metrics for a text size, with a line height of 1.35× rounded to
    /// whole pixels.
    pub fn new(text_size: f32) -> Self {
        Self {
            text_size,
            line_height: (text_size * 1.35).round().max(1.0),
        }
    }

    /// Overrides the line height.
    pub fn with_line_height(mut self, line_height: f32) -> Self {
        self.line_height = line_height.max(1.0);
        self
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new(14.0)
    }
}

/// Default number of lines that can be scrolled past the end
/// (Meld's `overscroll-num-lines`).
pub const DEFAULT_OVERSCROLL_LINES: usize = 5;

/// Height of the scrollable content: all lines plus the overscroll margin.
pub fn content_height(line_count: usize, line_height: f32, overscroll_lines: usize) -> f32 {
    (line_count.max(1) + overscroll_lines) as f32 * line_height
}

/// Largest valid vertical scroll offset.
pub fn max_scroll_y(
    line_count: usize,
    line_height: f32,
    overscroll_lines: usize,
    viewport_height: f32,
) -> f32 {
    (content_height(line_count, line_height, overscroll_lines) - viewport_height).max(0.0)
}

/// Lines (partially) visible at a scroll offset, clamped to `line_count`
/// (at least line 0).
pub fn visible_lines(
    scroll_y: f32,
    viewport_height: f32,
    line_height: f32,
    line_count: usize,
) -> Range<usize> {
    let count = line_count.max(1);
    let first = ((scroll_y / line_height).floor().max(0.0) as usize).min(count);
    let last = (((scroll_y + viewport_height) / line_height).ceil().max(0.0) as usize).min(count);
    first..last.max(first)
}

const TEXT_MARGIN: f32 = 4.0;
const GUTTER_PAD: f32 = 4.0;
const SCROLLBAR_HEIGHT: f32 = 7.0;
const WHEEL_LINES: f32 = 3.0;
const HIGHLIGHT_BUDGET: Duration = Duration::from_millis(8);
const CURSOR_BLINK_MS: u128 = 500;

/// Creates a [`TextPane`].
pub fn text_pane<'a, Message>(buffer: &'a Buffer, style: &'a PaneStyle) -> TextPane<'a, Message> {
    TextPane::new(buffer, style)
}

/// The text pane widget. See the module docs.
pub struct TextPane<'a, Message> {
    buffer: &'a Buffer,
    style: &'a PaneStyle,
    on_action: Option<Box<dyn Fn(Action) -> Message + 'a>>,
    id: Option<widget::Id>,
    font: Font,
    metrics: Metrics,
    scroll: Vector,
    highlights: &'a [LineHighlight],
    inline: &'a [InlineHighlight],
    sync_points: &'a [usize],
    syntax: Option<&'a Highlighter>,
    show_line_numbers: bool,
    show_whitespace: bool,
    highlight_current_line: bool,
    overscroll_lines: usize,
    width: Length,
    height: Length,
}

impl<'a, Message> TextPane<'a, Message> {
    /// Creates a pane showing `buffer`.
    pub fn new(buffer: &'a Buffer, style: &'a PaneStyle) -> Self {
        Self {
            buffer,
            style,
            on_action: None,
            id: None,
            font: Font::MONOSPACE,
            metrics: Metrics::default(),
            scroll: Vector::ZERO,
            highlights: &[],
            inline: &[],
            sync_points: &[],
            syntax: None,
            show_line_numbers: true,
            show_whitespace: false,
            highlight_current_line: false,
            overscroll_lines: DEFAULT_OVERSCROLL_LINES,
            width: Length::Fill,
            height: Length::Fill,
        }
    }

    /// Sets the message produced for each [`Action`]. Without it the pane
    /// is display-only and ignores input.
    pub fn on_action(mut self, f: impl Fn(Action) -> Message + 'a) -> Self {
        self.on_action = Some(Box::new(f));
        self
    }

    /// Sets the widget id (for focus operations).
    pub fn id(mut self, id: impl Into<widget::Id>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Sets the font (should be monospace).
    pub fn font(mut self, font: Font) -> Self {
        self.font = font;
        self
    }

    /// Sets text size and line height.
    pub fn metrics(mut self, metrics: Metrics) -> Self {
        self.metrics = metrics;
        self
    }

    /// Sets both scroll offsets in pixels.
    pub fn scroll(mut self, scroll: Vector) -> Self {
        self.scroll = scroll;
        self
    }

    /// Sets the vertical scroll offset in pixels.
    pub fn scroll_y(mut self, y: f32) -> Self {
        self.scroll.y = y;
        self
    }

    /// Sets the horizontal scroll offset in pixels.
    pub fn scroll_x(mut self, x: f32) -> Self {
        self.scroll.x = x;
        self
    }

    /// Chunk backgrounds (empty ranges draw zero-height markers).
    pub fn highlights(mut self, highlights: &'a [LineHighlight]) -> Self {
        self.highlights = highlights;
        self
    }

    /// Intraline change highlights.
    pub fn inline_highlights(mut self, inline: &'a [InlineHighlight]) -> Self {
        self.inline = inline;
        self
    }

    /// Lines with a sync point (outline drawn at the top of the line).
    pub fn sync_points(mut self, lines: &'a [usize]) -> Self {
        self.sync_points = lines;
        self
    }

    /// Syntax highlighter for the buffer.
    pub fn syntax(mut self, highlighter: &'a Highlighter) -> Self {
        self.syntax = Some(highlighter);
        self
    }

    /// Shows or hides the line number gutter.
    pub fn show_line_numbers(mut self, show: bool) -> Self {
        self.show_line_numbers = show;
        self
    }

    /// Draws markers for spaces and tabs.
    pub fn show_whitespace(mut self, show: bool) -> Self {
        self.show_whitespace = show;
        self
    }

    /// Highlights the cursor line.
    pub fn highlight_current_line(mut self, highlight: bool) -> Self {
        self.highlight_current_line = highlight;
        self
    }

    /// Number of lines that can be scrolled past the end.
    pub fn overscroll_lines(mut self, lines: usize) -> Self {
        self.overscroll_lines = lines;
        self
    }

    /// Sets the width.
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Sets the height.
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }
}

#[derive(Debug, Clone, Copy)]
struct Focus {
    updated_at: Instant,
    now: Instant,
    is_window_focused: bool,
}

impl Focus {
    fn now() -> Self {
        let now = Instant::now();
        Self {
            updated_at: now,
            now,
            is_window_focused: true,
        }
    }

    fn is_cursor_visible(&self) -> bool {
        self.is_window_focused
            && ((self.now - self.updated_at).as_millis() / CURSOR_BLINK_MS).is_multiple_of(2)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Drag {
    Text,
    Gutter,
    HScrollbar { grab_x: f32, start: f32 },
}

/// A shaped line.
struct LineLayout<P> {
    paragraph: P,
    /// Expanded display text (tabs as spaces).
    display: String,
    /// Display byte offset of each source char boundary (`chars + 1`).
    offsets: Vec<u32>,
    /// Display column of each source char boundary (`chars + 1`).
    columns: Vec<u32>,
    /// Source line is ASCII: positions follow the monospace grid.
    ascii: bool,
    /// Background quads from syntax styles: char ranges and colours.
    backgrounds: Vec<(Range<usize>, Color)>,
}

struct CacheEntry<P> {
    layout: LineLayout<P>,
    last_used: u64,
}

struct LineCache<P> {
    entries: HashMap<u64, CacheEntry<P>>,
    frame: u64,
    char_width: f32,
    measured: Option<(Font, u32)>,
}

impl<P> Default for LineCache<P> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            frame: 0,
            char_width: 8.0,
            measured: None,
        }
    }
}

/// Widget state (tree state of a [`TextPane`]).
pub struct State<P> {
    focus: Option<Focus>,
    last_click: Option<mouse::Click>,
    drag: Option<Drag>,
    last_cursor_rev: Option<u64>,
    last_size: Option<Size>,
    preedit: Option<input_method::Preedit>,
    modifiers: keyboard::Modifiers,
    cache: RefCell<LineCache<P>>,
}

impl<P> State<P> {
    /// Whether the pane has keyboard focus.
    pub fn is_focused(&self) -> bool {
        self.focus.is_some()
    }
}

impl<P> operation::Focusable for State<P> {
    fn is_focused(&self) -> bool {
        self.focus.is_some()
    }

    fn focus(&mut self) {
        self.focus = Some(Focus::now());
    }

    fn unfocus(&mut self) {
        self.focus = None;
    }
}

/// Pixel geometry of one frame.
#[derive(Debug, Clone, Copy)]
struct Geometry {
    bounds: Rectangle,
    gutter: Rectangle,
    text: Rectangle,
    origin_x: f32,
    lh: f32,
    cw: f32,
    scroll: Vector,
}

impl Geometry {
    fn line_y(&self, line: usize) -> f32 {
        self.bounds.y + line as f32 * self.lh - self.scroll.y
    }
}

impl<'a, Message> TextPane<'a, Message> {
    fn gutter_width(&self, cw: f32) -> f32 {
        if !self.show_line_numbers {
            return 0.0;
        }
        let digits = digits(self.buffer.line_count().max(99));
        digits as f32 * cw + 2.0 * GUTTER_PAD + 2.0
    }

    fn geometry(&self, bounds: Rectangle, cw: f32) -> Geometry {
        let gw = self.gutter_width(cw);
        let gutter = Rectangle {
            width: gw,
            ..bounds
        };
        let text = Rectangle {
            x: bounds.x + gw,
            width: (bounds.width - gw).max(0.0),
            ..bounds
        };
        Geometry {
            bounds,
            gutter,
            text,
            origin_x: text.x + TEXT_MARGIN - self.scroll.x,
            lh: self.metrics.line_height,
            cw,
            scroll: self.scroll,
        }
    }

    fn content_width(&self, cw: f32) -> f32 {
        self.buffer.max_display_columns() as f32 * cw + 2.0 * TEXT_MARGIN + cw
    }

    fn max_scroll(&self, g: &Geometry) -> Vector {
        Vector::new(
            (self.content_width(g.cw) - g.text.width).max(0.0),
            max_scroll_y(
                self.buffer.line_count(),
                g.lh,
                self.overscroll_lines,
                g.bounds.height,
            ),
        )
    }

    fn clamp_scroll(&self, g: &Geometry, s: Vector) -> Vector {
        let max = self.max_scroll(g);
        Vector::new(s.x.clamp(0.0, max.x), s.y.clamp(0.0, max.y))
    }

    fn hscrollbar(&self, g: &Geometry) -> Option<(Rectangle, Rectangle)> {
        let content = self.content_width(g.cw);
        if content <= g.text.width || g.text.width <= 0.0 {
            return None;
        }
        let track = Rectangle {
            x: g.text.x,
            y: g.text.y + g.text.height - SCROLLBAR_HEIGHT,
            width: g.text.width,
            height: SCROLLBAR_HEIGHT,
        };
        let thumb_w = (g.text.width / content * track.width)
            .max(20.0)
            .min(track.width);
        let max = content - g.text.width;
        let frac = (g.scroll.x / max).clamp(0.0, 1.0);
        let thumb = Rectangle {
            x: track.x + frac * (track.width - thumb_w),
            width: thumb_w,
            ..track
        };
        Some((track, thumb))
    }

    fn line_key(&self, line: usize, text: &str) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        self.buffer.tab_width().hash(&mut h);
        self.metrics.text_size.to_bits().hash(&mut h);
        self.metrics.line_height.to_bits().hash(&mut h);
        hash_font(&self.font, &mut h);
        self.style.text.r.to_bits().hash(&mut h);
        if let Some(hl) = self.syntax {
            hl.with_line_spans(line, |spans| {
                for s in spans {
                    s.hash(&mut h);
                    if let Some(st) = self.style.syntax_style(s.style.name()) {
                        hash_text_style(&st, &mut h);
                    }
                }
            });
        }
        h.finish()
    }

    /// Ensures the layout for `line` is cached; returns its key.
    fn ensure_line<P: text::Paragraph<Font = Font>>(
        &self,
        cache: &mut LineCache<P>,
        line: usize,
    ) -> u64 {
        let text = self.buffer.line(line);
        let key = self.line_key(line, text);
        let frame = cache.frame;
        if let Some(e) = cache.entries.get_mut(&key) {
            e.last_used = frame;
            return key;
        }
        let layout = self.build_line::<P>(line, text);
        cache.entries.insert(
            key,
            CacheEntry {
                layout,
                last_used: frame,
            },
        );
        key
    }

    fn build_line<P: text::Paragraph<Font = Font>>(
        &self,
        line: usize,
        text: &str,
    ) -> LineLayout<P> {
        let tab = self.buffer.tab_width();
        let ascii = text.is_ascii();
        let mut display = String::with_capacity(text.len());
        let mut offsets = Vec::with_capacity(text.len() + 1);
        let mut columns = Vec::with_capacity(text.len() + 1);
        let mut col = 0usize;
        for ch in text.chars() {
            offsets.push(display.len() as u32);
            columns.push(col as u32);
            if ch == '\t' {
                let n = tab - col % tab;
                display.extend(std::iter::repeat_n(' ', n));
                col += n;
            } else {
                display.push(ch);
                col += 1;
            }
        }
        offsets.push(display.len() as u32);
        columns.push(col as u32);

        // Syntax spans → display byte ranges.
        let mut styled: Vec<(Range<usize>, TextStyle)> = Vec::new();
        if let Some(hl) = self.syntax {
            hl.with_line_spans(line, |spans| {
                let n = offsets.len() - 1;
                for s in spans {
                    let (a, b) = ((s.start as usize).min(n), (s.end as usize).min(n));
                    if a < b
                        && let Some(st) = self.style.syntax_style(s.style.name())
                    {
                        styled.push((a..b, st));
                    }
                }
            });
        }
        let backgrounds = styled
            .iter()
            .filter_map(|(r, st)| st.bg.map(|bg| (r.clone(), bg)))
            .collect();

        let mut spans: Vec<Span<'_, (), Font>> = Vec::new();
        let mut pos = 0usize;
        let byte = |c: usize| offsets[c] as usize;
        for (r, st) in &styled {
            let (a, b) = (byte(r.start), byte(r.end));
            if a < pos {
                continue;
            }
            if a > pos {
                spans.push(Span::new(&display[pos..a]));
            }
            let mut font = self.font;
            if st.bold {
                font.weight = iced::font::Weight::Bold;
            }
            if st.italic {
                font.style = iced::font::Style::Italic;
            }
            let mut span = Span::new(&display[a..b])
                .underline(st.underline)
                .strikethrough(st.strikethrough);
            if let Some(fg) = st.fg {
                span = span.color(fg);
            }
            if font != self.font {
                span = span.font(font);
            }
            spans.push(span);
            pos = b;
        }
        if pos < display.len() || spans.is_empty() {
            spans.push(Span::new(&display[pos..]));
        }

        let paragraph = P::with_spans(text::Text {
            content: spans.as_slice(),
            bounds: Size::new(f32::INFINITY, self.metrics.line_height),
            size: Pixels(self.metrics.text_size),
            line_height: text::LineHeight::Absolute(Pixels(self.metrics.line_height)),
            font: self.font,
            align_x: text::Alignment::Left,
            align_y: iced::alignment::Vertical::Top,
            shaping: if display.is_ascii() {
                text::Shaping::Basic
            } else {
                text::Shaping::Advanced
            },
            wrapping: text::Wrapping::None,
        });
        drop(spans);

        LineLayout {
            paragraph,
            display,
            offsets,
            columns,
            ascii,
            backgrounds,
        }
    }

    fn measure<P: text::Paragraph<Font = Font>>(&self, cache: &mut LineCache<P>) {
        let id = (self.font, self.metrics.text_size.to_bits());
        if cache.measured == Some(id) {
            return;
        }
        let sample = "0123456789".repeat(4);
        let p = P::with_text(text::Text {
            content: sample.as_str(),
            bounds: Size::new(f32::INFINITY, f32::INFINITY),
            size: Pixels(self.metrics.text_size),
            line_height: text::LineHeight::Absolute(Pixels(self.metrics.line_height)),
            font: self.font,
            align_x: text::Alignment::Left,
            align_y: iced::alignment::Vertical::Top,
            shaping: text::Shaping::Basic,
            wrapping: text::Wrapping::None,
        });
        let w = p.min_width() / sample.len() as f32;
        cache.char_width = if w > 0.0 {
            w
        } else {
            self.metrics.text_size * 0.6
        };
        cache.measured = Some(id);
        cache.entries.clear();
    }

    /// Character column under `x` (window coordinates) on `line`.
    fn column_at<P: text::Paragraph<Font = Font>>(
        &self,
        cache: &mut LineCache<P>,
        g: &Geometry,
        line: usize,
        x: f32,
    ) -> usize {
        let rel = x - g.origin_x;
        if rel <= 0.0 {
            return 0;
        }
        let text = self.buffer.line(line);
        if text.is_ascii() {
            let dcol = (rel / g.cw).round() as usize;
            return column_for_display(text, dcol, self.buffer.tab_width());
        }
        let key = self.ensure_line(cache, line);
        let layout = &cache.entries[&key].layout;
        let Some(hit) = layout.paragraph.hit_test(Point::new(rel, g.lh / 2.0)) else {
            return char_len(text);
        };
        let byte = hit.cursor() as u32;
        let i = layout
            .offsets
            .partition_point(|o| *o <= byte)
            .saturating_sub(1);
        if i + 1 < layout.offsets.len() {
            let (a, b) = (layout.offsets[i], layout.offsets[i + 1]);
            if byte - a > (b - a) / 2 {
                return i + 1;
            }
        }
        i
    }

    fn position_at<P: text::Paragraph<Font = Font>>(
        &self,
        cache: &mut LineCache<P>,
        g: &Geometry,
        point: Point,
    ) -> Position {
        let line = ((point.y - g.bounds.y + g.scroll.y) / g.lh).floor();
        let max_line = self.buffer.line_count().saturating_sub(1);
        if line < 0.0 {
            return Position::new(0, 0);
        }
        let line = line as usize;
        if line > max_line {
            return self.buffer.end_position();
        }
        Position::new(line, self.column_at(cache, g, line, point.x))
    }

    fn publish(&self, shell: &mut Shell<'_, Message>, action: Action) {
        if let Some(f) = &self.on_action {
            shell.publish(f(action));
        }
    }

    fn scroll_to(&self, shell: &mut Shell<'_, Message>, g: &Geometry, target: Vector) {
        let target = self.clamp_scroll(g, target);
        if (target.x - self.scroll.x).abs() > 0.01 || (target.y - self.scroll.y).abs() > 0.01 {
            self.publish(
                shell,
                Action::Scrolled {
                    x: target.x,
                    y: target.y,
                },
            );
        }
    }

    /// Scroll offsets that bring the cursor into view.
    fn reveal_cursor(&self, g: &Geometry) -> Vector {
        let c = self.buffer.cursor();
        let mut s = self.scroll;
        let top = c.line as f32 * g.lh;
        let view_h = g.bounds.height;
        if top < s.y {
            s.y = top;
        } else if top + g.lh > s.y + view_h {
            s.y = top + g.lh - view_h;
        }
        let x = self.buffer.display_column(c) as f32 * g.cw + TEXT_MARGIN;
        let view_w = g.text.width;
        if x - TEXT_MARGIN < s.x {
            s.x = (x - TEXT_MARGIN - 4.0 * g.cw).max(0.0);
        } else if x + 2.0 * g.cw > s.x + view_w {
            s.x = x + 4.0 * g.cw - view_w;
        }
        s
    }

    fn page_lines(&self, g: &Geometry) -> usize {
        ((g.bounds.height / g.lh).floor() as usize)
            .saturating_sub(1)
            .max(1)
    }

    fn handle_key(
        &self,
        (key, physical_key, modifiers, text): KeyInput<'_>,
        g: &Geometry,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        let ctrl = modifiers.command();
        let shift = modifiers.shift();
        if modifiers.alt() || modifiers.logo() {
            return;
        }
        let editable = !self.buffer.is_read_only();
        let mut actions: Vec<Action> = Vec::new();

        if ctrl {
            match key.to_latin(physical_key) {
                Some('c') => {
                    if let Some(sel) = self.buffer.selected_text() {
                        clipboard.write(clipboard::Kind::Standard, sel);
                        shell.capture_event();
                    }
                    return;
                }
                Some('x') if editable => {
                    if let Some(sel) = self.buffer.selected_text() {
                        clipboard.write(clipboard::Kind::Standard, sel);
                        actions.push(Action::Edit(EditAction::Delete));
                    }
                }
                Some('v') if editable && !shift => {
                    if let Some(text) = clipboard.read(clipboard::Kind::Standard) {
                        actions.push(Action::Edit(EditAction::Paste(Arc::new(text))));
                    }
                    shell.capture_event();
                }
                Some('a') if !shift => actions.push(Action::SelectAll),
                _ => {}
            }
            if !actions.is_empty() {
                shell.capture_event();
                for a in actions {
                    self.publish(shell, a);
                }
                return;
            }
        }

        use key::Named;
        let motion = |m: Motion| {
            if shift {
                Action::Select(m)
            } else {
                Action::Move(m)
            }
        };
        let action = match key.as_ref() {
            keyboard::Key::Named(named) => match named {
                Named::ArrowLeft => {
                    Some(motion(if ctrl { Motion::WordLeft } else { Motion::Left }))
                }
                Named::ArrowRight => Some(motion(if ctrl {
                    Motion::WordRight
                } else {
                    Motion::Right
                })),
                Named::ArrowUp if !ctrl => Some(motion(Motion::Up)),
                Named::ArrowDown if !ctrl => Some(motion(Motion::Down)),
                Named::Home => Some(motion(if ctrl {
                    Motion::DocumentStart
                } else {
                    Motion::Home
                })),
                Named::End => Some(motion(if ctrl {
                    Motion::DocumentEnd
                } else {
                    Motion::End
                })),
                Named::PageUp if !ctrl => Some(motion(Motion::PageUp(self.page_lines(g)))),
                Named::PageDown if !ctrl => Some(motion(Motion::PageDown(self.page_lines(g)))),
                Named::Insert if ctrl && !shift => {
                    if let Some(sel) = self.buffer.selected_text() {
                        clipboard.write(clipboard::Kind::Standard, sel);
                    }
                    shell.capture_event();
                    None
                }
                Named::Insert if shift && !ctrl && editable => clipboard
                    .read(clipboard::Kind::Standard)
                    .map(|t| Action::Edit(EditAction::Paste(Arc::new(t)))),
                Named::Enter if !ctrl && editable => Some(Action::Edit(EditAction::Enter)),
                Named::Backspace if editable => Some(Action::Edit(if ctrl {
                    EditAction::DeleteWordBackward
                } else {
                    EditAction::Backspace
                })),
                Named::Delete if shift && !ctrl && editable => {
                    if let Some(sel) = self.buffer.selected_text() {
                        clipboard.write(clipboard::Kind::Standard, sel);
                        Some(Action::Edit(EditAction::Delete))
                    } else {
                        None
                    }
                }
                Named::Delete if editable => Some(Action::Edit(if ctrl {
                    EditAction::DeleteWordForward
                } else {
                    EditAction::Delete
                })),
                Named::Tab if !ctrl && editable => Some(Action::Edit(if shift {
                    EditAction::Unindent
                } else {
                    EditAction::Tab
                })),
                Named::Space if !ctrl && editable => Some(Action::Edit(EditAction::Insert(' '))),
                _ => None,
            },
            _ => None,
        };
        let action = action.or_else(|| {
            if ctrl || !editable {
                return None;
            }
            let text = text?;
            let text: String = text.chars().filter(|c| !c.is_control()).collect();
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (None, _) => None,
                (Some(c), None) => Some(Action::Edit(EditAction::Insert(c))),
                _ => Some(Action::Edit(EditAction::Paste(Arc::new(text)))),
            }
        });
        if let Some(action) = action {
            shell.capture_event();
            self.publish(shell, action);
        }
    }
}

/// Key, physical key, modifiers and produced text of a key press.
type KeyInput<'a> = (
    &'a keyboard::Key,
    key::Physical,
    keyboard::Modifiers,
    Option<&'a str>,
);

fn digits(n: usize) -> usize {
    n.max(1).ilog10() as usize + 1
}

fn hash_font(font: &Font, h: &mut impl Hasher) {
    // Font implements Hash.
    font.hash(h);
}

fn hash_text_style(st: &TextStyle, h: &mut impl Hasher) {
    for c in [st.fg, st.bg].into_iter().flatten() {
        c.r.to_bits().hash(h);
        c.g.to_bits().hash(h);
        c.b.to_bits().hash(h);
        c.a.to_bits().hash(h);
    }
    (st.bold, st.italic, st.underline, st.strikethrough).hash(h);
}

fn quad(bounds: Rectangle) -> Quad {
    Quad {
        bounds,
        border: Border::default(),
        ..Quad::default()
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for TextPane<'_, Message>
where
    Renderer: text::Renderer<Font = Font>,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<Renderer::Paragraph>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::<Renderer::Paragraph> {
            focus: None,
            last_click: None,
            drag: None,
            last_cursor_rev: None,
            last_size: None,
            preedit: None,
            modifiers: keyboard::Modifiers::empty(),
            cache: RefCell::new(LineCache::default()),
        })
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
        self.measure(state.cache.get_mut());
        layout::Node::new(limits.resolve(self.width, self.height, Size::ZERO))
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        _renderer: &Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
        operation.focusable(self.id.as_ref(), layout.bounds(), state);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        if self.on_action.is_none() {
            return;
        }
        let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
        let bounds = layout.bounds();
        let cw = state.cache.get_mut().char_width;
        let g = self.geometry(bounds, cw);

        match event {
            Event::Window(window::Event::Unfocused) => {
                if let Some(f) = &mut state.focus {
                    f.is_window_focused = false;
                }
            }
            Event::Window(window::Event::Focused) => {
                if let Some(f) = &mut state.focus {
                    f.is_window_focused = true;
                    f.updated_at = Instant::now();
                    shell.request_redraw();
                }
            }
            Event::Window(window::Event::RedrawRequested(now)) => {
                if let Some(f) = &mut state.focus
                    && f.is_window_focused
                {
                    f.now = *now;
                    let ms = CURSOR_BLINK_MS - (f.now - f.updated_at).as_millis() % CURSOR_BLINK_MS;
                    shell.request_redraw_at(f.now + Duration::from_millis(ms as u64));
                }
                let size = g.text.size();
                if state.last_size != Some(size) {
                    state.last_size = Some(size);
                    self.publish(shell, Action::Resized(size));
                }
                let rev = self.buffer.cursor_revision();
                if state.last_cursor_rev != Some(rev) {
                    let first = state.last_cursor_rev.is_none();
                    state.last_cursor_rev = Some(rev);
                    if !first {
                        self.scroll_to(shell, &g, self.reveal_cursor(&g));
                    }
                }
                if let Some(hl) = self.syntax {
                    let vis =
                        visible_lines(self.scroll.y, bounds.height, g.lh, self.buffer.line_count());
                    if !hl.is_valid_through(vis.end) {
                        shell.request_redraw();
                    }
                }
                if let Some(f) = &state.focus
                    && f.is_window_focused
                {
                    let c = self.buffer.cursor();
                    let x = g.origin_x + self.buffer.display_column(c) as f32 * g.cw;
                    let y = g.line_y(c.line);
                    shell.request_input_method(&InputMethod::Enabled {
                        cursor: Rectangle::new(Point::new(x, y), Size::new(1.0, g.lh)),
                        purpose: input_method::Purpose::Normal,
                        preedit: state.preedit.as_ref().map(input_method::Preedit::as_ref),
                    });
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(button)) => {
                let Some(pos) = cursor.position_over(bounds) else {
                    if *button == mouse::Button::Left && state.focus.is_some() {
                        state.focus = None;
                        state.drag = None;
                        self.publish(shell, Action::Focused(false));
                    }
                    return;
                };
                let was_focused = state.focus.is_some();
                state.focus = Some(Focus::now());
                if !was_focused {
                    self.publish(shell, Action::Focused(true));
                }
                let cache = state.cache.get_mut();
                match button {
                    mouse::Button::Left => {
                        if let Some((track, thumb)) = self.hscrollbar(&g)
                            && track.contains(pos)
                        {
                            if !thumb.contains(pos) {
                                // Page towards the click.
                                let page = g.text.width * 0.9;
                                let dx = if pos.x < thumb.x { -page } else { page };
                                self.scroll_to(
                                    shell,
                                    &g,
                                    Vector::new(self.scroll.x + dx, self.scroll.y),
                                );
                            }
                            state.drag = Some(Drag::HScrollbar {
                                grab_x: pos.x,
                                start: self.scroll.x,
                            });
                            shell.capture_event();
                            return;
                        }
                        if g.gutter.contains(pos) && g.gutter.width > 0.0 {
                            let p = self.position_at(cache, &g, pos);
                            self.publish(shell, Action::SelectLine(p));
                            state.drag = Some(Drag::Gutter);
                            state.last_click = None;
                            shell.capture_event();
                            return;
                        }
                        let click = mouse::Click::new(pos, mouse::Button::Left, state.last_click);
                        state.last_click = Some(click);
                        let p = self.position_at(cache, &g, pos);
                        let action = match click.kind() {
                            mouse::click::Kind::Single => {
                                state.drag = Some(Drag::Text);
                                let shift = state.modifiers.shift();
                                if shift {
                                    Action::ExtendTo(p)
                                } else {
                                    Action::Click(p)
                                }
                            }
                            mouse::click::Kind::Double => Action::SelectWord(p),
                            mouse::click::Kind::Triple => Action::SelectLine(p),
                        };
                        self.publish(shell, action);
                        shell.capture_event();
                    }
                    mouse::Button::Middle => {
                        if !self.buffer.is_read_only()
                            && g.text.contains(pos)
                            && let Some(text) = clipboard.read(clipboard::Kind::Primary)
                        {
                            let p = self.position_at(cache, &g, pos);
                            self.publish(shell, Action::Click(p));
                            self.publish(shell, Action::Edit(EditAction::Paste(Arc::new(text))));
                        }
                        shell.capture_event();
                    }
                    mouse::Button::Right => {
                        let p = self.position_at(cache, &g, pos);
                        self.publish(shell, Action::ContextMenu(p, pos));
                        shell.capture_event();
                    }
                    _ => {}
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => match state.drag {
                Some(Drag::Text) => {
                    let p = self.position_at(state.cache.get_mut(), &g, *position);
                    self.publish(shell, Action::ExtendTo(p));
                }
                Some(Drag::Gutter) => {
                    let mut p = self.position_at(state.cache.get_mut(), &g, *position);
                    p.column = 0;
                    if p.line + 1 < self.buffer.line_count()
                        && self.buffer.anchor().is_some_and(|a| a.line <= p.line)
                    {
                        p.line += 1;
                    }
                    self.publish(shell, Action::ExtendTo(p));
                }
                Some(Drag::HScrollbar { grab_x, start }) => {
                    if let Some((track, thumb)) = self.hscrollbar(&g) {
                        let max = self.content_width(g.cw) - g.text.width;
                        let span = (track.width - thumb.width).max(1.0);
                        let x = start + (position.x - grab_x) / span * max;
                        self.scroll_to(shell, &g, Vector::new(x, self.scroll.y));
                    }
                }
                None => {}
            },
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if state.drag.take() == Some(Drag::Text)
                    && let Some(sel) = self.buffer.selected_text()
                {
                    clipboard.write(clipboard::Kind::Primary, sel);
                }
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if cursor.is_over(bounds) => {
                let (mut dx, mut dy) = match delta {
                    mouse::ScrollDelta::Lines { x, y } => {
                        (-x * WHEEL_LINES * g.cw * 2.0, -y * WHEEL_LINES * g.lh)
                    }
                    mouse::ScrollDelta::Pixels { x, y } => (-x, -y),
                };
                if state.modifiers.shift() && dx == 0.0 {
                    std::mem::swap(&mut dx, &mut dy);
                }
                self.scroll_to(
                    shell,
                    &g,
                    Vector::new(self.scroll.x + dx, self.scroll.y + dy),
                );
                shell.capture_event();
            }
            Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => {
                state.modifiers = *m;
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                physical_key,
                modifiers,
                text,
                ..
            }) if state.focus.is_some() => {
                state.modifiers = *modifiers;
                if let Some(f) = &mut state.focus {
                    f.updated_at = Instant::now();
                }
                self.handle_key(
                    (key, *physical_key, *modifiers, text.as_deref()),
                    &g,
                    clipboard,
                    shell,
                );
            }
            Event::InputMethod(ime) if state.focus.is_some() => match ime {
                input_method::Event::Opened => {
                    state.preedit = Some(input_method::Preedit::new());
                }
                input_method::Event::Closed => {
                    state.preedit = None;
                }
                input_method::Event::Preedit(content, selection) => {
                    state.preedit = Some(input_method::Preedit {
                        content: content.clone(),
                        selection: selection.clone(),
                        text_size: Some(Pixels(self.metrics.text_size)),
                    });
                    shell.request_redraw();
                }
                input_method::Event::Commit(text) => {
                    if !self.buffer.is_read_only() && !text.is_empty() {
                        self.publish(
                            shell,
                            Action::Edit(EditAction::Paste(Arc::new(text.clone()))),
                        );
                        shell.capture_event();
                    }
                }
            },
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State<Renderer::Paragraph>>();
        let cw = state.cache.borrow().char_width;
        let g = self.geometry(layout.bounds(), cw);
        match cursor.position_over(layout.bounds()) {
            Some(p) if self.on_action.is_some() => {
                if self.hscrollbar(&g).is_some_and(|(t, _)| t.contains(p)) || g.gutter.contains(p) {
                    mouse::Interaction::Idle
                } else {
                    mouse::Interaction::Text
                }
            }
            _ => mouse::Interaction::default(),
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State<Renderer::Paragraph>>();
        let bounds = layout.bounds();
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };
        let style = self.style;
        let mut guard = state.cache.borrow_mut();
        guard.frame += 1;
        let g = self.geometry(bounds, guard.char_width);
        let lh = g.lh;
        let line_count = self.buffer.line_count();
        let vis = visible_lines(self.scroll.y, bounds.height, lh, line_count);

        if let Some(hl) = self.syntax {
            hl.highlight_visible(self.buffer.lines(), vis.clone(), Some(HIGHLIGHT_BUDGET));
        }

        let cursor_pos = self.buffer.cursor();
        renderer.with_layer(clip, |renderer| {
            renderer.fill_quad(quad(bounds), style.background);
            if g.gutter.width > 0.0 {
                renderer.fill_quad(quad(g.gutter), style.gutter_background);
            }

            // Chunk backgrounds and outlines (Meld's do_snapshot_layer).
            let full_x = bounds.x;
            let full_w = bounds.width;
            for h in self.highlights {
                let (start, end) = (h.lines.start, h.lines.end);
                if end < vis.start || start > vis.end {
                    continue;
                }
                let colors = style.chunks.get(h.kind);
                let y0 = g.line_y(start);
                let y1 = g.line_y(end);
                if start != end {
                    let r =
                        Rectangle::new(Point::new(full_x, y0), Size::new(full_w, y1 - y0 + 1.0));
                    renderer.fill_quad(quad(r), colors.fill);
                    if h.current {
                        renderer.fill_quad(quad(r), style.chunks.current_chunk);
                    }
                }
                let y1 = if start == end { y0 + 1.0 } else { y1 };
                for y in [y0, y1] {
                    renderer.fill_quad(
                        quad(Rectangle::new(
                            Point::new(full_x, y),
                            Size::new(full_w, 1.0),
                        )),
                        colors.line,
                    );
                }
            }

            // Overscroll area below the last line.
            let end_y = g.line_y(line_count.max(1)) + 2.0;
            let bottom = bounds.y + bounds.height;
            if end_y < bottom {
                renderer.fill_quad(
                    quad(Rectangle::new(
                        Point::new(g.text.x, end_y),
                        Size::new(g.text.width, bottom - end_y),
                    )),
                    style.overscroll,
                );
            }

            if self.highlight_current_line && vis.contains(&cursor_pos.line) {
                renderer.fill_quad(
                    quad(Rectangle::new(
                        Point::new(g.text.x, g.line_y(cursor_pos.line)),
                        Size::new(g.text.width, lh),
                    )),
                    style.current_line,
                );
            }

            for &line in self.sync_points {
                if line >= vis.start && line <= vis.end {
                    renderer.fill_quad(
                        quad(Rectangle::new(
                            Point::new(g.text.x, g.line_y(line) - 0.5),
                            Size::new(g.text.width, 1.0),
                        )),
                        style.sync_point,
                    );
                }
            }
        });

        // Make sure all visible lines are shaped.
        let keys: Vec<(usize, u64)> = vis
            .clone()
            .filter(|l| *l < line_count)
            .map(|l| (l, self.ensure_line(&mut guard, l)))
            .collect();
        let cache = &*guard;
        let x_of = |layout: &LineLayout<Renderer::Paragraph>, col: usize| -> f32 {
            let col = col.min(layout.offsets.len() - 1);
            if layout.ascii || col == 0 {
                return layout.columns[col] as f32 * g.cw;
            }
            let byte = layout.offsets[col] as usize;
            let graphemes = layout.display[..byte].graphemes(true).count();
            layout
                .paragraph
                .grapheme_position(0, graphemes)
                .map_or(layout.columns[col] as f32 * g.cw, |p| p.x)
        };

        let focused = state.focus.is_some();
        let selection = self.buffer.selection();
        let inline: Vec<&InlineHighlight> = self
            .inline
            .iter()
            .filter(|h| h.line >= vis.start && h.line < vis.end)
            .collect();

        if let Some(text_clip) = g.text.intersection(&clip) {
            renderer.with_layer(text_clip, |renderer| {
                for &(line, key) in &keys {
                    let layout = &cache.entries[&key].layout;
                    let y = g.line_y(line);
                    let span_rect = |a: usize, b: usize| {
                        let (xa, xb) = (x_of(layout, a), x_of(layout, b));
                        Rectangle::new(Point::new(g.origin_x + xa, y), Size::new(xb - xa, lh))
                    };
                    for h in inline.iter().filter(|h| h.line == line) {
                        renderer.fill_quad(
                            quad(span_rect(h.columns.start, h.columns.end)),
                            style.inline,
                        );
                    }
                    for (r, bg) in &layout.backgrounds {
                        renderer.fill_quad(quad(span_rect(r.start, r.end)), *bg);
                    }
                    if let Some((s, e)) = selection
                        && line >= s.line
                        && line <= e.line
                    {
                        let n = layout.offsets.len() - 1;
                        let a = if line == s.line { s.column } else { 0 };
                        let (b, extra) = if line == e.line {
                            (e.column, 0.0)
                        } else {
                            (n, g.cw)
                        };
                        let mut r = span_rect(a, b);
                        r.width += extra;
                        let color = if focused {
                            style.selection
                        } else {
                            style.selection_unfocused
                        };
                        if r.width > 0.0 {
                            renderer.fill_quad(quad(r), color);
                        }
                    }
                    if self.show_whitespace {
                        for (i, ch) in self.buffer.line(line).chars().enumerate() {
                            let xa = g.origin_x + x_of(layout, i);
                            let xb = g.origin_x + x_of(layout, i + 1);
                            if ch == ' ' {
                                let d = (g.cw * 0.18).clamp(1.5, 3.0);
                                renderer.fill_quad(
                                    quad(Rectangle::new(
                                        Point::new((xa + xb - d) / 2.0, y + (lh - d) / 2.0),
                                        Size::new(d, d),
                                    )),
                                    style.whitespace,
                                );
                            } else if ch == '\t' {
                                renderer.fill_quad(
                                    quad(Rectangle::new(
                                        Point::new(xa + 2.0, (y + lh / 2.0).floor()),
                                        Size::new((xb - xa - 4.0).max(1.0), 1.0),
                                    )),
                                    style.whitespace,
                                );
                                renderer.fill_quad(
                                    quad(Rectangle::new(
                                        Point::new(xb - 3.0, (y + lh / 2.0).floor() - 2.0),
                                        Size::new(1.0, 5.0),
                                    )),
                                    style.whitespace,
                                );
                            }
                        }
                    }
                    renderer.fill_paragraph(
                        &layout.paragraph,
                        Point::new(g.origin_x, y),
                        style.text,
                        text_clip,
                    );
                }

                if let Some(f) = &state.focus
                    && f.is_cursor_visible()
                    && state.preedit.as_ref().is_none_or(|p| p.content.is_empty())
                    && let Some(&(_, key)) = keys.iter().find(|(l, _)| *l == cursor_pos.line)
                {
                    let layout = &cache.entries[&key].layout;
                    let x = g.origin_x + x_of(layout, cursor_pos.column);
                    renderer.fill_quad(
                        quad(Rectangle::new(
                            Point::new(x.floor(), g.line_y(cursor_pos.line)),
                            Size::new(1.5, lh),
                        )),
                        style.cursor,
                    );
                } else if line_count == 0
                    && state.focus.as_ref().is_some_and(Focus::is_cursor_visible)
                {
                    renderer.fill_quad(
                        quad(Rectangle::new(
                            Point::new(g.origin_x.floor(), g.line_y(0)),
                            Size::new(1.5, lh),
                        )),
                        style.cursor,
                    );
                }
            });
        }

        if g.gutter.width > 0.0
            && let Some(gutter_clip) = g.gutter.intersection(&clip)
        {
            renderer.with_layer(gutter_clip, |renderer| {
                let bold = Font {
                    weight: iced::font::Weight::Bold,
                    ..self.font
                };
                let right = g.gutter.x + g.gutter.width - GUTTER_PAD - 2.0;
                for line in vis.clone() {
                    let label = (line + 1).to_string();
                    let w = label.len() as f32 * g.cw;
                    let color = if line == cursor_pos.line {
                        style.line_number_current
                    } else {
                        style.line_number
                    };
                    renderer.fill_text(
                        text::Text {
                            content: label,
                            bounds: Size::new(w + g.cw, lh),
                            size: Pixels(self.metrics.text_size),
                            line_height: text::LineHeight::Absolute(Pixels(lh)),
                            font: bold,
                            align_x: text::Alignment::Left,
                            align_y: iced::alignment::Vertical::Top,
                            shaping: text::Shaping::Basic,
                            wrapping: text::Wrapping::None,
                        },
                        Point::new(right - w, g.line_y(line)),
                        color,
                        gutter_clip,
                    );
                }
            });
        }

        if let Some((_track, thumb)) = self.hscrollbar(&g) {
            renderer.with_layer(clip, |renderer| {
                renderer.fill_quad(
                    Quad {
                        bounds: Rectangle {
                            y: thumb.y + 1.0,
                            height: thumb.height - 2.0,
                            ..thumb
                        },
                        border: Border {
                            radius: 3.0.into(),
                            ..Border::default()
                        },
                        ..Quad::default()
                    },
                    style.scrollbar,
                );
            });
        }

        // Evict stale shaped lines.
        let frame = guard.frame;
        let limit = 4 * (vis.len() + 16);
        if guard.entries.len() > limit {
            guard.entries.retain(|_, e| e.last_used + 2 >= frame);
        }
    }
}

impl<'a, Message, Theme, Renderer> From<TextPane<'a, Message>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Renderer: text::Renderer<Font = Font> + 'a,
    Theme: 'a,
{
    fn from(pane: TextPane<'a, Message>) -> Self {
        Element::new(pane)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_line_math() {
        assert_eq!(visible_lines(0.0, 100.0, 20.0, 100), 0..5);
        assert_eq!(visible_lines(10.0, 100.0, 20.0, 100), 0..6);
        assert_eq!(visible_lines(1990.0, 100.0, 20.0, 100), 99..100);
        assert_eq!(visible_lines(0.0, 100.0, 20.0, 0), 0..1);
        assert_eq!(content_height(10, 20.0, 5), 300.0);
        assert_eq!(max_scroll_y(10, 20.0, 5, 100.0), 200.0);
        assert_eq!(max_scroll_y(1, 20.0, 0, 100.0), 0.0);
    }

    #[test]
    fn digit_count() {
        assert_eq!(digits(0), 1);
        assert_eq!(digits(9), 1);
        assert_eq!(digits(99), 2);
        assert_eq!(digits(100), 3);
    }
}
