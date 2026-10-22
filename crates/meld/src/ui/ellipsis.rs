//! A one-line label that shortens itself with “…” to fit its width, like a
//! `Gtk.Label` with `set_ellipsize()`: file names keep both ends
//! ([`Mode::Middle`]), repository paths their end ([`Mode::Start`]), other
//! texts their start ([`Mode::End`]).
//!
//! The text is shaped once to find where each character ends; fitting it
//! to a width is then arithmetic ([`fit`]), checked by shaping the result.

use std::borrow::Cow;

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::text::{self as core_text, Paragraph as _, paragraph};
use iced::advanced::widget::{self, Tree, tree};
use iced::advanced::{Widget, mouse};
use iced::widget::text;
use iced::{Color, Element, Font, Length, Pixels, Point, Rectangle, Renderer, Size, Theme};

const ELLIPSIS: &str = "…";

/// Which part of the text gives way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Start,
    Middle,
    End,
}

/// How many characters to keep from the start and from the end of a text
/// whose character boundaries are at `edges` (`edges[i]` is where character
/// `i` starts, the last entry where the text ends) so that it fits `max`
/// with an ellipsis `ellipsis` wide between them; `None` if it fits whole.
pub fn fit(edges: &[f32], ellipsis: f32, max: f32, mode: Mode) -> Option<(usize, usize)> {
    let count = edges.len().saturating_sub(1);
    let total = edges.last().copied().unwrap_or(0.0);
    if total <= max {
        return None;
    }
    let head = |n: usize| edges[n] - edges[0];
    let tail = |n: usize| total - edges[count - n];
    let room = max - ellipsis;
    let (mut h, mut t) = (0, 0);
    match mode {
        Mode::End => {
            while h < count && head(h + 1) <= room {
                h += 1;
            }
        }
        Mode::Start => {
            while t < count && tail(t + 1) <= room {
                t += 1;
            }
        }
        Mode::Middle => loop {
            // Grow the narrower side first, so that both ends show about
            // as much
            let grow_head = h + t < count && head(h + 1) + tail(t) <= room;
            let grow_tail = h + t < count && head(h) + tail(t + 1) <= room;
            match (grow_head, grow_tail) {
                (true, true) if head(h) <= tail(t) => h += 1,
                (true, true) => t += 1,
                (true, false) => h += 1,
                (false, true) => t += 1,
                (false, false) => break,
            }
        },
    }
    Some((h, t))
}

/// `content` with the middle cut out: `head` characters, “…”, and the last
/// `tail` characters; spaces next to the ellipsis are dropped.
pub fn shortened(content: &str, head: usize, tail: usize) -> String {
    let chars: Vec<char> = content.chars().collect();
    let tail = tail.min(chars.len().saturating_sub(head));
    let start: String = chars[..head].iter().collect();
    let end: String = chars[chars.len() - tail..].iter().collect();
    format!("{}{ELLIPSIS}{}", start.trim_end(), end.trim_start())
}

pub struct Ellipsized<'a> {
    content: Cow<'a, str>,
    mode: Mode,
    size: Option<Pixels>,
    font: Option<Font>,
    width: Length,
    max_width: f32,
    color: Option<Color>,
    style: Option<fn(&Theme) -> text::Style>,
}

/// `content` on one line, shortened per `mode` when it doesn't fit.
pub fn ellipsized<'a>(content: impl Into<Cow<'a, str>>, mode: Mode) -> Ellipsized<'a> {
    Ellipsized {
        content: content.into(),
        mode,
        size: None,
        font: None,
        width: Length::Shrink,
        max_width: f32::INFINITY,
        color: None,
        style: None,
    }
}

impl<'a> Ellipsized<'a> {
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = Some(size.into());
        self
    }

    pub fn font(mut self, font: Font) -> Self {
        self.font = Some(font);
        self
    }

    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// The widest the label gets, also when given more room.
    pub fn max_width(mut self, max_width: f32) -> Self {
        self.max_width = max_width;
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }

    pub fn style(mut self, style: fn(&Theme) -> text::Style) -> Self {
        self.style = Some(style);
        self
    }
}

type Para = <Renderer as core_text::Renderer>::Paragraph;

/// What the text was shaped with.
#[derive(PartialEq)]
struct Key {
    content: String,
    size: Pixels,
    font: Font,
}

#[derive(Default)]
struct State {
    key: Option<Key>,
    /// Character boundaries of the whole text, and the ellipsis' width.
    edges: Vec<f32>,
    ellipsis: f32,
    /// The width the text was last fitted to.
    fitted_to: Option<f32>,
    shown: paragraph::Plain<Para>,
}

fn one_line(content: &str, size: Pixels, font: Font) -> core_text::Text<&str, Font> {
    core_text::Text {
        content,
        bounds: Size::INFINITE,
        size,
        line_height: core_text::LineHeight::default(),
        font,
        align_x: core_text::Alignment::Default,
        align_y: iced::alignment::Vertical::Top,
        shaping: core_text::Shaping::Advanced,
        wrapping: core_text::Wrapping::None,
    }
}

/// Where each character of `content` starts, and where the text ends.
fn edges(content: &str, size: Pixels, font: Font) -> Vec<f32> {
    let shaped = Para::with_text(one_line(content, size, font));
    let count = content.chars().count();
    let mut edges: Vec<f32> = (0..=count)
        .map(|i| shaped.grapheme_position(0, i).map_or(0.0, |p| p.x))
        .collect();
    // Positions are per grapheme; keep them rising for combined characters
    for i in 1..edges.len() {
        edges[i] = edges[i].max(edges[i - 1]);
    }
    if let Some(last) = edges.last_mut() {
        *last = last.max(shaped.min_width());
    }
    edges
}

impl<'a, M> Widget<M, Theme, Renderer> for Ellipsized<'a> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        use iced::advanced::text::Renderer as _;
        let state = tree.state.downcast_mut::<State>();
        let size = self.size.unwrap_or_else(|| renderer.default_size());
        let font = self.font.unwrap_or_else(|| renderer.default_font());
        let key = Key {
            content: self.content.to_string(),
            size,
            font,
        };
        if state.key.as_ref() != Some(&key) {
            state.edges = edges(&self.content, size, font);
            state.ellipsis = Para::with_text(one_line(ELLIPSIS, size, font)).min_width();
            state.key = Some(key);
            state.fitted_to = None;
        }
        let max = limits.max().width.min(self.max_width);
        if state.fitted_to != Some(max) {
            state.fitted_to = Some(max);
            let shown = match fit(&state.edges, state.ellipsis, max, self.mode) {
                None => self.content.to_string(),
                Some((mut head, mut tail)) => loop {
                    // Shaping the shortened text can come out a little
                    // wider than the sum of its parts
                    let shown = shortened(&self.content, head, tail);
                    let width = Para::with_text(one_line(&shown, size, font)).min_width();
                    if width <= max || head + tail == 0 {
                        break shown;
                    }
                    if head >= tail {
                        head -= 1;
                    } else {
                        tail -= 1;
                    }
                },
            };
            state.shown =
                paragraph::Plain::new(one_line(&shown, size, font).with_content(shown.clone()));
        }
        let text_size = state.shown.min_bounds();
        let height = text_size
            .height
            .max(core_text::LineHeight::default().to_absolute(size).0);
        let width = if self.width == Length::Shrink || !max.is_finite() {
            text_size.width.min(max)
        } else {
            max
        };
        layout::Node::new(limits.resolve(self.width, Length::Shrink, Size::new(width, height)))
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        use iced::advanced::text::Renderer as _;
        let state = tree.state.downcast_ref::<State>();
        let color = self
            .color
            .or_else(|| self.style.and_then(|s| s(theme).color))
            .unwrap_or(style.text_color);
        let bounds = layout.bounds();
        let top = bounds.y + (bounds.height - state.shown.min_bounds().height) / 2.0;
        renderer.fill_paragraph(
            state.shown.raw(),
            Point::new(bounds.x, top),
            color,
            *viewport,
        );
    }

    fn operate(
        &mut self,
        _tree: &mut Tree,
        layout: Layout<'_>,
        _renderer: &Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        operation.text(None, layout.bounds(), &self.content);
    }
}

impl<'a, M: 'a> From<Ellipsized<'a>> for Element<'a, M> {
    fn from(label: Ellipsized<'a>) -> Self {
        Element::new(label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Edges of `n` characters 10 px wide each.
    fn even(n: usize) -> Vec<f32> {
        (0..=n).map(|i| i as f32 * 10.0).collect()
    }

    #[test]
    fn texts_that_fit_stay_whole() {
        assert_eq!(fit(&even(5), 10.0, 50.0, Mode::Middle), None);
        assert_eq!(fit(&even(0), 10.0, 0.0, Mode::End), None);
    }

    #[test]
    fn each_mode_keeps_its_end() {
        // 10 characters, 100 px, into 65 px: 55 px for text besides "…"
        assert_eq!(fit(&even(10), 10.0, 65.0, Mode::End), Some((5, 0)));
        assert_eq!(fit(&even(10), 10.0, 65.0, Mode::Start), Some((0, 5)));
        assert_eq!(fit(&even(10), 10.0, 65.0, Mode::Middle), Some((3, 2)));
        // No room at all: only the ellipsis
        assert_eq!(fit(&even(10), 10.0, 5.0, Mode::Middle), Some((0, 0)));
    }

    #[test]
    fn middle_balances_by_width_not_count() {
        // A wide first character: the end side gets the extra characters
        let edges = [0.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0];
        assert_eq!(fit(&edges, 10.0, 80.0, Mode::Middle), Some((1, 3)));
    }

    #[test]
    fn shortened_texts_join_both_ends() {
        assert_eq!(shortened("generate_build.py", 6, 5), "genera…ld.py");
        assert_eq!(shortened("one two three", 4, 0), "one…");
        assert_eq!(shortened("one two three", 0, 6), "…three");
        assert_eq!(shortened("abc", 2, 5), "ab…c");
        assert_eq!(shortened("", 0, 0), "…");
    }
}
