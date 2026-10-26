//! The Explain column (port of `meld/ui/whycolumn.py`, where it is the Why
//! column): a card per change, level with the change's lines in the
//! comparison next to it. It can also lie over the target pane, or be
//! hidden ([`meld_config::ExplainLayout`]).
//!
//! Added lines are found in the target pane, deleted lines in the base
//! pane. A card is pushed down only to clear the one above it
//! ([`meld_ailens::cards::layout_cards`]), and the column follows the panes
//! whenever they scroll: the tab rebuilds it from the panes' geometry on
//! every view.

use std::collections::{HashMap, HashSet};

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{self, Tree, Widget};
use iced::advanced::{Clipboard, Shell, mouse};
use iced::widget::{
    button, canvas, column, container, mouse_area, rich_text, row, span, text, tooltip,
};
use iced::{
    Background, Border, Color, Element, Event, Font, Length, Rectangle, Renderer, Size, Theme,
    Vector,
};
pub use meld_ailens::cards::CARD_GAP;
use meld_ailens::cards::layout_cards;
use meld_ailens::{Card, CardStep, ChangeGuide, ExplainAction, Side};

use super::diffhost::PaneGeometry;
use crate::ui;

pub const MIN_WIDTH: f32 = 160.0;
pub const MAX_WIDTH: f32 = 900.0;

fn hex(rgb: u32) -> Color {
    Color::from_rgb8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// Where each card wants to be: level with the first line of its change,
/// in the pane that has it; y relative to the top of the panes' text.
///
/// A card whose pane has no geometry (yet) sits where the card before it
/// wanted to be.
pub fn wanted_tops(cards: &[Card], panes: [Option<PaneGeometry>; 2]) -> Vec<f32> {
    let mut last = 0.0;
    cards
        .iter()
        .map(|card| {
            let (side, line) = card.anchor();
            let pane = match side {
                Side::Old => 0,
                Side::New => 1,
            };
            let top = panes[pane].map(|g| g.line_y(line.saturating_sub(1)));
            last = top.unwrap_or(last);
            last
        })
        .collect()
}

/// Final tops for cards of `heights` wanting `wanted`, without overlaps.
///
/// Laid out top to bottom: cards anchored in the base pane (deletions) and
/// in the target pane can interleave.
pub fn place_cards(wanted: &[f32], heights: &[f32], gap: f32) -> Vec<f32> {
    let mut order: Vec<usize> = (0..wanted.len()).collect();
    order.sort_by(|&a, &b| wanted[a].total_cmp(&wanted[b]).then(a.cmp(&b)));
    let sorted_tops: Vec<f32> = order.iter().map(|&i| wanted[i]).collect();
    let sorted_heights: Vec<f32> = order.iter().map(|&i| heights[i]).collect();
    let placed = layout_cards(&sorted_tops, &sorted_heights, gap);
    let mut tops = vec![0.0; wanted.len()];
    for (index, top) in order.into_iter().zip(placed) {
        tops[index] = top;
    }
    tops
}

/// Final tops for cards of `heights` wanting `wanted`, without overlaps, and
/// kept above `bottom` (the column's height) where that can be done.
///
/// [`place_cards`] only ever pushes cards down, so near the end of a file
/// the last cards would hang below the column, where scrolling the panes
/// can't bring them. Cards in view (`wanted` above `bottom`) are pulled
/// back up just enough to end at `bottom`, the cards above them with them.
/// A card whose line (`lines`, where it is anchored) is in view, at or
/// below the column's top, never goes above `top` (under the column's
/// pinned header, which would hide it for good at the start of a file) and
/// pushes the cards after it down instead; one whose line scrolled above
/// the column may go along. Cards
/// further down are left alone.
pub fn fit_cards(
    wanted: &[f32],
    lines: &[f32],
    heights: &[f32],
    gap: f32,
    top: f32,
    bottom: f32,
) -> Vec<f32> {
    let mut tops = place_cards(wanted, heights, gap);
    let mut order: Vec<usize> = (0..wanted.len()).collect();
    order.sort_by(|&a, &b| tops[a].total_cmp(&tops[b]).then(a.cmp(&b)));
    let in_view: Vec<usize> = order
        .iter()
        .copied()
        .filter(|&i| wanted[i] < bottom)
        .collect();
    let floors: Vec<f32> = in_view
        .iter()
        .map(|&i| {
            if lines[i] >= 0.0 {
                top
            } else {
                tops[i].min(top)
            }
        })
        .collect();
    // From the bottom up: end each card above the one below it
    let mut limit = bottom;
    for &index in in_view.iter().rev() {
        tops[index] = tops[index].min(limit - heights[index]);
        limit = tops[index] - gap;
    }
    // From the top down: not above the header, and never overlapping
    let mut floor = f32::NEG_INFINITY;
    for (&index, &lowest) in in_view.iter().zip(&floors) {
        tops[index] = tops[index].max(lowest).max(floor);
        floor = tops[index] + heights[index] + gap;
    }
    tops
}

/// Where each card wants to start, and the line it is anchored at.
///
/// A card starts at its `wanted`, except a block's header (`below` set):
/// it ends at its `wanted`, above its block, when there is room there, and
/// otherwise starts at its `below`, under its block, so the block's steps
/// stay level with their lines. There is no room above when the header
/// would cover the cards before it, or, with its block in view (at or
/// below the column's top), go above `top`, under the column's header.
pub fn wanted_starts(
    wanted: &[f32],
    below: &[Option<f32>],
    heights: &[f32],
    gap: f32,
    top: f32,
) -> (Vec<f32>, Vec<f32>) {
    let below = |index: usize| below.get(index).copied().flatten();
    let mut starts: Vec<f32> = wanted
        .iter()
        .zip(heights)
        .enumerate()
        .map(|(index, (wanted, height))| match below(index) {
            Some(_) => wanted - height,
            None => *wanted,
        })
        .collect();
    let mut lines = wanted.to_vec();
    // Cards still to place, by where they start; a header that moves under
    // its block goes back in at its new place, so the cards after it see it
    let mut pending: Vec<usize> = (0..wanted.len()).collect();
    let order =
        |starts: &[f32], a: usize, b: usize| starts[a].total_cmp(&starts[b]).then(a.cmp(&b));
    // Last to place at the end, to pop
    pending.sort_by(|&a, &b| order(&starts, b, a));
    let mut moved = vec![false; wanted.len()];
    // Where the cards placed so far end, as place_cards will place them
    let mut bottom = f32::NEG_INFINITY;
    while let Some(index) = pending.pop() {
        if let (Some(after), false) = (below(index), moved[index]) {
            let floor = if wanted[index] >= 0.0 {
                bottom.max(top)
            } else {
                bottom
            };
            if starts[index] < floor {
                starts[index] = after;
                lines[index] = after;
                moved[index] = true;
                let at = pending.partition_point(|&other| order(&starts, other, index).is_gt());
                pending.insert(at, index);
                continue;
            }
        }
        let placed = starts[index].max(bottom + gap);
        bottom = bottom.max(placed + heights[index]);
    }
    (starts, lines)
}

/// `text` cut at a word boundary near `limit` characters, with an ellipsis.
pub fn shorten(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let cut: String = text.chars().take(limit).collect();
    let cut = match cut.rfind(char::is_whitespace) {
        Some(space) if space > limit / 2 => &cut[..space],
        _ => cut.as_str(),
    };
    format!("{}…", cut.trim_end())
}

/// The cards' font: that of the compared text, so that a card reads like
/// the code beside it and follows its zoom.
#[derive(Clone, Copy, Debug)]
pub struct CardFont {
    pub font: Font,
    /// Size of the cards' text in logical pixels.
    pub size: f32,
}

impl CardFont {
    fn bold(self) -> Font {
        Font {
            weight: iced::font::Weight::Bold,
            ..self.font
        }
    }

    fn italic(self) -> Font {
        Font {
            style: iced::font::Style::Italic,
            ..self.font
        }
    }

    /// A size `by` pixels below the text's, for headers and labels.
    fn smaller(self, by: f32) -> f32 {
        (self.size - by).max(6.0)
    }
}

/// Per-card state the tab keeps across reloads of the cards.
#[derive(Default)]
pub struct CardStates {
    pub busy: HashSet<String>,
    pub errors: HashMap<String, String>,
    pub hovered: Option<String>,
    /// The step of the hovered card under the pointer, if any
    pub hovered_step: Option<usize>,
}

/// What the column's buttons and pointer send.
pub struct Callbacks<M> {
    pub explain: fn(String) -> M,
    pub hover: fn(Option<String>) -> M,
    /// The pointer left a block's header (its card's key)
    pub leave: fn(String) -> M,
    /// A name from the code was clicked: find it in the panes
    pub jump: fn(String) -> M,
    /// The pointer went over a step of a card (its index), or left it
    /// (false)
    pub hover_step: fn(String, usize, bool) -> M,
}

impl<M> Clone for Callbacks<M> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<M> Copy for Callbacks<M> {}

/// `body` cut into plain text and names from the code (in backticks, which
/// are dropped); a backtick without its pair stays text.
pub fn split_code(body: &str) -> Vec<(String, bool)> {
    let parts: Vec<&str> = body.split('`').collect();
    // An even number of parts means an odd number of backticks
    let unpaired = parts.len().is_multiple_of(2);
    let mut out: Vec<(String, bool)> = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        let last = index == parts.len() - 1;
        let (part, code) = if index % 2 == 1 && unpaired && last {
            (format!("`{part}"), false)
        } else {
            (part.to_string(), index % 2 == 1)
        };
        if part.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some((previous, false)) if !code => previous.push_str(&part),
            _ => out.push((part, code)),
        }
    }
    out
}

/// Behind names from the code, in light and dark themes alike.
const CODE_BACKGROUND: Color = Color::from_rgba(0.5, 0.5, 0.5, 0.2);

/// `body` with names from the code set apart; clicking one finds it in the
/// panes.
fn rich<'a, M: Clone + 'a>(
    body: &str,
    size: f32,
    font: Font,
    dim: bool,
    on_jump: fn(String) -> M,
) -> Element<'a, M> {
    rich_with(body, size, font, dim, on_jump, None)
}

/// [`rich`] with lines `line_height` pixels apart, when given.
fn rich_with<'a, M: Clone + 'a>(
    body: &str,
    size: f32,
    font: Font,
    dim: bool,
    on_jump: fn(String) -> M,
    line_height: Option<f32>,
) -> Element<'a, M> {
    let spans: Vec<text::Span<'a, String, Font>> = split_code(body)
        .into_iter()
        .map(|(part, code)| {
            if code {
                let name = part.clone();
                span(part)
                    .font(font)
                    .background(CODE_BACKGROUND)
                    .border(Border::default().rounded(3))
                    .padding([0, 2])
                    .link(name)
            } else {
                span(part).font(font)
            }
        })
        .collect();
    let mut t = rich_text(spans)
        .size(size)
        .font(font)
        .wrapping(text::Wrapping::WordOrGlyph)
        .width(Length::Fill)
        .on_link_click(on_jump);
    if let Some(height) = line_height {
        t = t.line_height(text::LineHeight::Absolute(height.into()));
    }
    if dim {
        t = t.style(ui::dim_text);
    }
    t.into()
}

fn badge<'a, M: 'a>(
    label: String,
    background: Color,
    color: Option<Color>,
    italic: bool,
    font: CardFont,
) -> Element<'a, M> {
    let mut t = text(label)
        .size(font.smaller(2.0))
        .font(if italic { font.italic() } else { font.font })
        .wrapping(text::Wrapping::None);
    if let Some(c) = color {
        t = t.color(c);
    }
    container(t)
        .padding([0, 6])
        .style(move |_theme: &Theme| container::Style {
            background: Some(Background::Color(background)),
            border: Border::default().rounded(9),
            ..container::Style::default()
        })
        .into()
}

/// The chip of a change's risk: how much it needs checking, from grey (1,
/// not worth the time) to red (5, to check and verify).
/// None outside 1 to 5.
fn risk_chip<'a, M: 'a>(risk: u8, font: CardFont) -> Option<Element<'a, M>> {
    // The range is the model's, checked where risks are read
    if !ChangeGuide::RISKS.contains(&risk) {
        return None;
    }
    let (rgb, tip) = match risk {
        1 => (0x9a9996, tr!("Not worth your time")),
        2 => (0x77767b, tr!("Plain and safe")),
        3 => (0xc88800, tr!("Worth a careful read")),
        4 => (0xe66100, tr!("Check it against its callers and edge cases")),
        _ => (0xc01c28, tr!("Check and verify it")),
    };
    let color = hex(rgb);
    let chip = badge(
        tr!(
            "risk {risk}/{most}",
            risk = risk,
            most = ChangeGuide::RISKS.end()
        ),
        color.scale_alpha(if risk >= 4 { 0.25 } else { 0.15 }),
        Some(color),
        false,
        CardFont {
            font: if risk >= 4 { font.bold() } else { font.font },
            ..font
        },
    );
    Some(
        tooltip(
            chip,
            container(text(tip).size(12))
                .padding([4, 8])
                .style(ui::tooltip_box),
            tooltip::Position::Bottom,
        )
        .into(),
    )
}

/// Tint behind the hovered block or step.
fn hover_tint(theme: &Theme, alpha: Option<f32>) -> Option<Background> {
    alpha.map(|alpha| {
        Background::Color(
            theme
                .extended_palette()
                .primary
                .base
                .color
                .scale_alpha(alpha),
        )
    })
}

/// One step of a change, level with the lines it explains, like a line of
/// code: the code's font, size and line height, so each of its lines is
/// beside a line of code. Hovering one highlights its lines.
fn step_view<'a, M: Clone + 'a>(
    card: &'a Card,
    index: usize,
    step: &'a CardStep,
    states: &CardStates,
    callbacks: Callbacks<M>,
    font: CardFont,
    line_height: f32,
) -> Element<'a, M> {
    let body = rich_with(
        &step.text,
        font.size,
        font.font,
        false,
        callbacks.jump,
        Some(line_height),
    );
    // The step itself, or all of its block's
    let tint = match (
        states.hovered.as_deref() == Some(card.key.as_str()),
        states.hovered_step,
    ) {
        (true, Some(hovered)) if hovered == index => Some(0.16),
        (true, None) => Some(0.07),
        _ => None,
    };
    let line = container(body)
        .padding([0, 8])
        .width(Length::Fill)
        .style(move |theme: &Theme| container::Style {
            background: hover_tint(theme, tint),
            ..container::Style::default()
        });
    mouse_area(line)
        .on_enter((callbacks.hover_step)(card.key.clone(), index, true))
        .on_exit((callbacks.hover_step)(card.key.clone(), index, false))
        .into()
}

/// A change block's header: its risk and what the changed code does; the
/// explain button while it has no explanation of its own.
fn block_header<'a, M: Clone + 'a>(
    card: &'a Card,
    states: &'a CardStates,
    callbacks: Callbacks<M>,
    font: CardFont,
) -> Element<'a, M> {
    let key = card.key.as_str();
    let explained = !card.change_what.is_empty();
    let summary = if explained {
        &card.change_what
    } else {
        &card.what
    };
    let mut top = row![].spacing(8).align_y(iced::Alignment::Start);
    if let Some(chip) = card.change_risk.and_then(|risk| risk_chip(risk, font)) {
        top = top.push(chip);
    }
    if !summary.is_empty() {
        let weight = if explained { font.bold() } else { font.font };
        top = top.push(rich(summary, font.size, weight, !explained, callbacks.jump));
    }
    let mut body = column![top].spacing(4).width(Length::Fill);
    let busy = states.busy.contains(key);
    if let Some(action) = card.explain {
        let label = match action {
            ExplainAction::Commit | ExplainAction::Uncommitted => tr!("Explain with Claude"),
            ExplainAction::Changes => tr!("Explain changes"),
            ExplainAction::Block => tr!("Explain this block"),
        };
        let mut explain = row![
            button(text(label).size(font.smaller(1.0)).font(font.font))
                .padding([2, 10])
                .style(button::secondary)
                .on_press_maybe((!busy).then(|| (callbacks.explain)(card.key.clone())))
        ]
        .spacing(6)
        .align_y(iced::Center);
        if busy {
            explain = explain.push(
                text(tr!("Explaining…"))
                    .size(font.smaller(1.0))
                    .font(font.font)
                    .style(ui::dim_text),
            );
        }
        body = body.push(explain);
    }
    if let Some(error) = states.errors.get(key).filter(|_| !busy) {
        let (first, rest) = super::condense(error);
        let error = match rest {
            Some(rest) => shorten(&format!("{first} {rest}"), 160),
            None => first,
        };
        body = body.push(text(error).size(font.smaller(1.0)).font(font.font).style(
            |theme: &Theme| text::Style {
                color: Some(theme.extended_palette().danger.base.color),
            },
        ));
    }
    let hovered = states.hovered.as_deref() == Some(key);
    let header = container(body)
        .padding([3, 8])
        .width(Length::Fill)
        .style(move |theme: &Theme| container::Style {
            background: hover_tint(theme, hovered.then_some(0.07)).or_else(|| {
                Some(Background::Color(
                    theme
                        .extended_palette()
                        .background
                        .base
                        .text
                        .scale_alpha(0.06),
                ))
            }),
            ..container::Style::default()
        });
    mouse_area(header)
        .on_enter((callbacks.hover)(Some(card.key.clone())))
        .on_exit((callbacks.leave)(card.key.clone()))
        .into()
}

/// What the column shows, and where: each element wants to start at `y`;
/// a block's header wants to end there, or when there is no room, to start
/// at `below` ([`wanted_starts`]).
pub struct Item<'a, M> {
    pub element: Element<'a, M>,
    pub y: f32,
    pub below: Option<f32>,
}

/// The column laid out like the panes beside it: per change block its
/// header ([`block_header`]), ending where the block's first line starts (or under the
/// block, when there is no room above it), then each step level with the
/// first line it explains. Steps without lines start with the block; steps
/// on deleted lines of a block that also adds lines come right after the
/// block, so they don't push the steps of its added lines off their lines.
/// For a [`card_column`] without a gap: the headers keep their own space
/// above them.
pub fn explain_items<'a, M: Clone + 'a>(
    cards: &'a [Card],
    states: &'a CardStates,
    callbacks: Callbacks<M>,
    font: CardFont,
    panes: [Option<PaneGeometry>; 2],
) -> Vec<Item<'a, M>> {
    let line_height = panes
        .iter()
        .flatten()
        .map(|g| g.line_height)
        .next()
        .unwrap_or(font.size * 1.35);
    let tops = wanted_tops(cards, panes);
    let line_end = |side: Side, ranges: &[meld_ailens::LineRange]| {
        let end = ranges.iter().map(|r| r.end).max()?;
        let pane = match side {
            Side::Old => 0,
            Side::New => 1,
        };
        panes[pane].map(|g| g.line_y(end))
    };
    let mut items = Vec::new();
    for (card, top) in cards.iter().zip(tops) {
        // Under the block: after its added lines, else its deleted ones
        let end = line_end(Side::New, &card.added)
            .or_else(|| line_end(Side::Old, &card.deleted))
            .unwrap_or(top);
        // The space above the header is its own: steps sit on their lines
        // with no gap between them
        items.push(Item {
            element: container(block_header(card, states, callbacks, font))
                .padding(iced::Padding::default().top(CARD_GAP))
                .into(),
            y: top,
            below: Some(end),
        });
        for (index, step) in card.change_steps.iter().enumerate() {
            let y = step
                .lines
                .and_then(|(side, range)| {
                    if side == Side::Old && !card.added.is_empty() {
                        return Some(end);
                    }
                    let pane = match side {
                        Side::Old => 0,
                        Side::New => 1,
                    };
                    panes[pane].map(|g| g.line_y(range.start.saturating_sub(1)))
                })
                .unwrap_or(top);
            items.push(Item {
                element: step_view(card, index, step, states, callbacks, font, line_height),
                y,
                below: None,
            });
        }
    }
    items
}

/// The target pane's line numbers, level with its lines, at the left of
/// the column: so its explanations read like a pane of their own. Drawn
/// for the lines in view only.
#[derive(Clone, Copy)]
pub struct LineNumbers<'a> {
    pub geometry: Option<PaneGeometry<'a>>,
    pub font: CardFont,
    /// The scheme's colors, if it has them, else the theme's
    pub color: Option<Color>,
    pub background: Option<Color>,
}

impl LineNumbers<'_> {
    /// Wide enough for the pane's last line number
    pub fn width(&self) -> f32 {
        let lines = self.geometry.map_or(1, |g| g.line_count.max(1));
        let digits = lines.to_string().len().max(2) as f32;
        // Monospace digits are about 0.6 of the size wide
        (digits * 0.6 * self.font.size + 16.0).ceil()
    }
}

/// The 0-based lines whose tops are in `0..height`, and those tops, for
/// `geometry`; also the line partly above the top.
pub fn lines_in_view(geometry: &PaneGeometry, height: f32) -> Vec<(usize, f32)> {
    let count = geometry.line_count;
    // The first line that ends below the top
    let (mut low, mut high) = (0, count);
    while low < high {
        let middle = (low + high) / 2;
        if geometry.line_y(middle + 1) <= 0.0 {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    (low..count)
        .map(|line| (line, geometry.line_y(line)))
        .take_while(|&(_, y)| y < height)
        .collect()
}

impl<M> canvas::Program<M> for LineNumbers<'_> {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let base = &theme.extended_palette().background.base;
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        frame.fill_rectangle(
            iced::Point::ORIGIN,
            bounds.size(),
            self.background.unwrap_or(base.color),
        );
        let color = self.color.unwrap_or(base.text.scale_alpha(0.5));
        if let Some(geometry) = &self.geometry {
            for (line, y) in lines_in_view(geometry, bounds.height) {
                frame.fill_text(canvas::Text {
                    content: (line + 1).to_string(),
                    position: iced::Point::new(bounds.width - 8.0, y),
                    color,
                    size: self.font.size.into(),
                    line_height: text::LineHeight::Absolute(geometry.line_height.into()),
                    font: self.font.font,
                    align_x: iced::alignment::Horizontal::Right.into(),
                    align_y: iced::alignment::Vertical::Top,
                    ..canvas::Text::default()
                });
            }
        }
        vec![frame.into_geometry()]
    }
}

/// How far the cards are scrolled up, when those in view don't fit.
#[derive(Default)]
struct ColumnState {
    offset: f32,
    /// How far they can be: what the cards in view need beyond the column
    overflow: f32,
    /// Where the cards wanted to be when they were scrolled: once the
    /// panes move, the cards follow their lines again
    scrolled_at: Vec<f32>,
}

/// Pixels a wheel line scrolls the cards.
const WHEEL_LINE: f32 = 40.0;

/// Lays out cards at the tops they want, pushed down to clear each other,
/// clipped to its bounds. Takes all the space it gets and never asks for
/// height: the cards scroll with the panes. An optional header stays at the
/// top, over the cards scrolling past it.
pub struct CardColumn<'a, M> {
    /// The header first, if any, then the cards
    children: Vec<Element<'a, M>>,
    /// One per card
    wanted: Vec<f32>,
    /// Per card: for a block's header, where it starts when there is no
    /// room for it to end at its `wanted` ([`wanted_starts`])
    below: Vec<Option<f32>>,
    gap: f32,
    header: bool,
}

/// Cards wanting to be at `wanted` (one per card), plus an optional
/// placeholder shown at the top and an optional header pinned there.
pub fn card_column<'a, M: 'a>(
    cards: Vec<Element<'a, M>>,
    wanted: Vec<f32>,
    placeholder: Option<Element<'a, M>>,
    header: Option<Element<'a, M>>,
) -> CardColumn<'a, M> {
    let mut wanted = wanted;
    wanted.resize(cards.len(), 0.0);
    let has_header = header.is_some();
    let mut children: Vec<Element<'a, M>> = header.into_iter().collect();
    children.extend(cards);
    if let Some(p) = placeholder {
        children.push(p);
        wanted.push(24.0);
    }
    CardColumn {
        children,
        wanted,
        below: Vec::new(),
        gap: CARD_GAP,
        header: has_header,
    }
}

impl<'a, M: 'a> CardColumn<'a, M> {
    /// The space between cards that clear each other ([`CARD_GAP`] unless
    /// set)
    pub fn gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }

    /// Which cards are blocks' headers, and where each goes under its
    /// block (one per card, in order; see [`wanted_starts`])
    pub fn below(mut self, below: Vec<Option<f32>>) -> Self {
        self.below = below;
        self
    }

    /// Visible part of `bounds` only gets the cursor, so cards scrolled
    /// out of the column can't be clicked through the widgets around it.
    fn cursor_in(bounds: Rectangle, cursor: mouse::Cursor) -> mouse::Cursor {
        if cursor.is_over(bounds) {
            cursor
        } else {
            mouse::Cursor::Unavailable
        }
    }

    /// The column below its header: where cards can be seen and clicked.
    fn card_area(&self, layout: Layout<'_>) -> Rectangle {
        let bounds = layout.bounds();
        let header = match (self.header, layout.children().next()) {
            (true, Some(header)) => header.bounds().height.min(bounds.height),
            _ => 0.0,
        };
        Rectangle {
            y: bounds.y + header,
            height: bounds.height - header,
            ..bounds
        }
    }

    /// The area each child gets the cursor in.
    fn areas(&self, layout: Layout<'_>) -> impl Iterator<Item = Rectangle> + use<'a, M> {
        let bounds = layout.bounds();
        let cards = self.card_area(layout);
        let header = self.header;
        (0..self.children.len()).map(
            move |index| {
                if header && index == 0 { bounds } else { cards }
            },
        )
    }
}

impl<'a, M: 'a> Widget<M, Theme, Renderer> for CardColumn<'a, M> {
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<ColumnState>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(ColumnState::default())
    }

    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = limits.max();
        // Cards take the column's width and as much height as they need:
        // compressing the height makes fluid heights inside them (the
        // colored edge) match the card instead of growing without end
        let child_limits = layout::Limits::with_compression(
            Size::ZERO,
            Size::new(size.width, f32::INFINITY),
            Size::new(false, true),
        );
        let mut nodes: Vec<layout::Node> = self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .map(|(child, state)| child.as_widget_mut().layout(state, renderer, &child_limits))
            .collect();
        let first_card = usize::from(self.header);
        let heights: Vec<f32> = nodes[first_card..]
            .iter()
            .map(|n| n.size().height)
            .collect();
        let header = if self.header {
            nodes[0].size().height
        } else {
            0.0
        };
        let (wanted, lines) = wanted_starts(&self.wanted, &self.below, &heights, self.gap, header);
        let tops = fit_cards(&wanted, &lines, &heights, self.gap, header, size.height);
        // What the cards in view still need below the column, after fitting
        let overflow = wanted
            .iter()
            .zip(&tops)
            .zip(&heights)
            .filter(|((wanted, _), _)| **wanted < size.height)
            .map(|((_, top), height)| top + height - size.height)
            .fold(0.0_f32, f32::max);
        let state = tree.state.downcast_mut::<ColumnState>();
        if state.scrolled_at != self.wanted {
            state.offset = 0.0;
        }
        state.overflow = overflow;
        state.offset = state.offset.clamp(0.0, overflow);
        let offset = state.offset;
        for (node, top) in nodes[first_card..].iter_mut().zip(tops) {
            node.move_to_mut((0.0, top - offset));
        }
        layout::Node::with_children(size, nodes)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            for ((child, state), layout) in self
                .children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
            {
                child
                    .as_widget_mut()
                    .operate(state, layout, renderer, operation);
            }
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        let areas: Vec<Rectangle> = self.areas(layout).collect();
        for (((child, state), layout), area) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
            .zip(areas)
        {
            let cursor = Self::cursor_in(area, cursor);
            let viewport = area.intersection(viewport).unwrap_or(area);
            child.as_widget_mut().update(
                state, event, layout, cursor, renderer, clipboard, shell, &viewport,
            );
        }
        // Cards in view that don't fit scroll with the wheel; at either end
        // the wheel goes on to what is below
        if shell.is_event_captured() {
            return;
        }
        if let Event::Mouse(mouse::Event::WheelScrolled { delta }) = event {
            if !cursor.is_over(self.card_area(layout)) {
                return;
            }
            let state = tree.state.downcast_mut::<ColumnState>();
            let dy = match *delta {
                mouse::ScrollDelta::Lines { y, .. } => y * WHEEL_LINE,
                mouse::ScrollDelta::Pixels { y, .. } => y,
            };
            let offset = (state.offset - dy).clamp(0.0, state.overflow);
            if offset != state.offset {
                state.offset = offset;
                state.scrolled_at = self.wanted.clone();
                shell.capture_event();
                shell.invalidate_layout();
                shell.request_redraw();
            }
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .zip(self.areas(layout))
            .map(|(((child, state), layout), area)| {
                child.as_widget().mouse_interaction(
                    state,
                    layout,
                    Self::cursor_in(area, cursor),
                    viewport,
                    renderer,
                )
            })
            .max()
            .unwrap_or_default()
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        use iced::advanced::Renderer as _;
        let bounds = layout.bounds();
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };
        let first_card = usize::from(self.header);
        if let Some(cards_clip) = self.card_area(layout).intersection(&clip) {
            let cursor = Self::cursor_in(cards_clip, cursor);
            renderer.with_layer(cards_clip, |renderer| {
                for ((child, state), layout) in self
                    .children
                    .iter()
                    .zip(&tree.children)
                    .zip(layout.children())
                    .skip(first_card)
                    .filter(|(_, l)| l.bounds().intersects(&cards_clip))
                {
                    child.as_widget().draw(
                        state,
                        renderer,
                        theme,
                        style,
                        layout,
                        cursor,
                        &cards_clip,
                    );
                }
            });
        }
        if let (true, Some(header), Some(state), Some(header_layout)) = (
            self.header,
            self.children.first(),
            tree.children.first(),
            layout.children().next(),
        ) {
            let cursor = Self::cursor_in(bounds, cursor);
            renderer.with_layer(clip, |renderer| {
                // Cards scroll under the header: it hides them
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: header_layout.bounds(),
                        ..renderer::Quad::default()
                    },
                    theme.extended_palette().background.weak.color,
                );
                header.as_widget().draw(
                    state,
                    renderer,
                    theme,
                    style,
                    header_layout,
                    cursor,
                    &clip,
                );
            });
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        overlay::from_children(
            &mut self.children,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, M: 'a> From<CardColumn<'a, M>> for Element<'a, M> {
    fn from(column: CardColumn<'a, M>) -> Self {
        Element::new(column)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meld_ailens::LineRange;

    fn card(added: Option<(usize, usize)>, deleted: Option<(usize, usize)>) -> Card {
        Card {
            added: added
                .map(|(s, e)| LineRange::new(s, e))
                .into_iter()
                .collect(),
            deleted: deleted
                .map(|(s, e)| LineRange::new(s, e))
                .into_iter()
                .collect(),
            ..Default::default()
        }
    }

    fn geometry(scroll_y: f32) -> Option<PaneGeometry<'static>> {
        Some(PaneGeometry {
            scroll_y,
            line_height: 20.0,
            line_count: 100,
            top: 0.0,
            wrap: None,
        })
    }

    #[test]
    fn cards_sit_level_with_their_first_line() {
        let cards = [
            card(Some((3, 5)), None),
            card(None, Some((10, 12))),
            card(Some((200, 201)), None),
        ];
        // The base pane is scrolled further than the target pane
        let tops = wanted_tops(&cards, [geometry(100.0), geometry(20.0)]);
        assert_eq!(
            tops,
            [2.0 * 20.0 - 20.0, 9.0 * 20.0 - 100.0, 100.0 * 20.0 - 20.0]
        );
    }

    #[test]
    fn cards_without_pane_geometry_follow_the_previous_card() {
        let cards = [card(Some((3, 3)), None), card(None, Some((8, 8)))];
        assert_eq!(wanted_tops(&cards, [None, geometry(0.0)]), [40.0, 40.0]);
        assert_eq!(wanted_tops(&cards[1..], [None, None]), [0.0]);
    }

    #[test]
    fn placed_cards_never_overlap_and_keep_their_order_by_position() {
        // The second card (a deletion) wants to be above the first
        let tops = place_cards(&[100.0, 40.0, 120.0], &[50.0, 80.0, 10.0], 4.0);
        assert_eq!(tops, [124.0, 40.0, 178.0]);
        // Equal positions keep diff order
        assert_eq!(place_cards(&[0.0, 0.0], &[10.0, 10.0], 4.0), [0.0, 14.0]);
        assert!(place_cards(&[], &[], 4.0).is_empty());
    }

    #[test]
    fn cards_in_view_are_pulled_up_to_fit_above_the_bottom() {
        // The last card's line is in view, but the card would end at 130
        let fit = |wanted: &[f32], heights: &[f32], top| {
            fit_cards(wanted, wanted, heights, 4.0, top, 100.0)
        };
        let tops = fit(&[10.0, 60.0, 90.0], &[20.0, 20.0, 40.0], 0.0);
        assert_eq!(tops, [10.0, 36.0, 60.0]);
        // Cards whose lines are below the column stay where they are
        let tops = fit(&[10.0, 150.0], &[20.0, 40.0], 0.0);
        assert_eq!(tops, [10.0, 150.0]);
        // Never pulled above the header; what doesn't fit still hangs below
        let tops = fit(&[30.0, 50.0], &[60.0, 60.0], 24.0);
        assert_eq!(tops, [24.0, 88.0]);
        // A card already scrolled above the header may stay there
        let tops = fit(&[-50.0, 80.0], &[60.0, 40.0], 24.0);
        assert_eq!(tops, [-50.0, 60.0]);
    }

    #[test]
    fn cards_above_their_line_end_there() {
        // A header, then steps on consecutive lines 20 apart
        let lines = [100.0, 100.0, 120.0, 140.0];
        let heights = [30.0, 20.0, 20.0, 20.0];
        let header = [Some(160.0), None];
        let (starts, anchors) = wanted_starts(&lines, &header, &heights, 0.0, 0.0);
        assert_eq!(starts, [70.0, 100.0, 120.0, 140.0]);
        assert_eq!(anchors, lines);
        // The header ends at the line, and each step stays on its own line:
        // without a gap they don't drift down
        let tops = fit_cards(&starts, &anchors, &heights, 0.0, 0.0, 400.0);
        assert_eq!(tops, [70.0, 100.0, 120.0, 140.0]);
        // A step two lines tall pushes the next one down, no further
        let tall = [30.0, 40.0, 20.0, 20.0];
        let (starts, anchors) = wanted_starts(&lines, &header, &tall, 0.0, 0.0);
        let tops = fit_cards(&starts, &anchors, &tall, 0.0, 0.0, 400.0);
        assert_eq!(tops, [70.0, 100.0, 140.0, 160.0]);
    }

    #[test]
    fn headers_without_room_above_go_under_their_block() {
        let heights = [30.0, 20.0, 20.0];
        let header = [Some(50.0), None, None];
        // The block's line is at 10, in view: above it, the header would
        // go above the column's top (under the pinned summary, at 24)
        let lines = [10.0, 10.0, 30.0];
        let (starts, anchors) = wanted_starts(&lines, &header, &heights, 0.0, 24.0);
        assert_eq!(starts, [50.0, 10.0, 30.0]);
        assert_eq!(anchors, [50.0, 10.0, 30.0]);
        // A step whose line is under the pinned summary comes out from
        // under it, pushing what follows: hidden there, it couldn't be
        // scrolled into view at the start of a file
        let tops = fit_cards(&starts, &anchors, &heights, 0.0, 24.0, 400.0);
        assert_eq!(tops, [64.0, 24.0, 44.0]);

        // The previous block's steps are in the way
        let lines = [0.0, 80.0, 80.0, 100.0];
        let heights = [20.0, 30.0, 20.0, 20.0];
        let header = [None, Some(120.0), None, None];
        let (starts, _) = wanted_starts(&lines, &header, &heights, 0.0, 0.0);
        assert_eq!(starts, [0.0, 50.0, 80.0, 100.0]);
        let lines = [0.0, 40.0, 40.0, 60.0];
        let (starts, _) = wanted_starts(&lines, &header, &heights, 0.0, 0.0);
        assert_eq!(starts, [0.0, 120.0, 40.0, 60.0]);

        // Its block scrolled above the column: it goes along, above it
        let lines = [-10.0, -10.0];
        let header = [Some(10.0), None];
        let (starts, anchors) = wanted_starts(&lines, &header, &[30.0, 20.0], 0.0, 0.0);
        assert_eq!(starts, [-40.0, -10.0]);
        let tops = fit_cards(&starts, &anchors, &[30.0, 20.0], 0.0, 0.0, 400.0);
        assert_eq!(tops, [-40.0, -10.0]);
    }

    // Only built, never read: the tests look at where items go
    #[allow(dead_code)]
    #[derive(Debug, Clone)]
    enum Msg {
        Key(String),
        Hover(Option<String>),
        Step(String, usize, bool),
    }

    fn callbacks() -> Callbacks<Msg> {
        Callbacks {
            explain: Msg::Key,
            hover: Msg::Hover,
            leave: Msg::Key,
            jump: Msg::Key,
            hover_step: Msg::Step,
        }
    }

    fn step(lines: Option<(Side, usize, usize)>, text: &str) -> CardStep {
        CardStep {
            lines: lines.map(|(side, start, end)| (side, LineRange::new(start, end))),
            text: text.into(),
        }
    }

    #[test]
    fn explain_items_put_headers_above_blocks_and_steps_beside_their_lines() {
        let mut first = card(Some((3, 6)), None);
        first.change_steps = vec![
            step(Some((Side::New, 3, 3)), "three"),
            step(Some((Side::New, 5, 6)), "five"),
            step(None, "anywhere"),
        ];
        // Only deletions, in the base pane, scrolled differently
        let mut second = card(None, Some((10, 10)));
        second.change_steps = vec![step(Some((Side::Old, 10, 10)), "gone")];
        let cards = [first, second];
        let states = CardStates::default();
        let font = CardFont {
            font: Font::MONOSPACE,
            size: 14.0,
        };
        let items = explain_items(
            &cards,
            &states,
            callbacks(),
            font,
            [geometry(100.0), geometry(20.0)],
        );
        let placed: Vec<(f32, Option<f32>)> = items.iter().map(|i| (i.y, i.below)).collect();
        assert_eq!(
            placed,
            [
                // Line 3 of the target pane is at 2 * 20 - 20; the block
                // ends with line 6, at 6 * 20 - 20
                (20.0, Some(100.0)),
                (20.0, None),
                (60.0, None),
                // A step without lines starts where its block does
                (20.0, None),
                // Line 10 of the base pane is at 9 * 20 - 100
                (80.0, Some(100.0)),
                (80.0, None),
            ]
        );
    }

    #[test]
    fn deleted_line_steps_of_a_block_that_adds_lines_come_after_it() {
        let mut replaced = card(Some((3, 4)), Some((3, 3)));
        replaced.change_steps = vec![
            step(Some((Side::New, 3, 3)), "new three"),
            step(Some((Side::Old, 3, 3)), "old three"),
            step(Some((Side::New, 4, 4)), "new four"),
        ];
        let cards = [replaced];
        let states = CardStates::default();
        let font = CardFont {
            font: Font::MONOSPACE,
            size: 14.0,
        };
        let items = explain_items(
            &cards,
            &states,
            callbacks(),
            font,
            [geometry(0.0), geometry(0.0)],
        );
        let ys: Vec<f32> = items.iter().map(|i| i.y).collect();
        // Header at line 3 (40); old three after line 4 (80)
        assert_eq!(ys, [40.0, 40.0, 80.0, 60.0]);
    }

    #[test]
    fn steps_without_pane_geometry_start_with_their_block() {
        let mut only = card(Some((3, 3)), None);
        only.change_steps = vec![step(Some((Side::New, 3, 3)), "three")];
        let cards = [only];
        let states = CardStates::default();
        let font = CardFont {
            font: Font::MONOSPACE,
            size: 14.0,
        };
        let items = explain_items(&cards, &states, callbacks(), font, [None, None]);
        let placed: Vec<(f32, Option<f32>)> = items.iter().map(|i| (i.y, i.below)).collect();
        assert_eq!(placed, [(0.0, Some(0.0)), (0.0, None)]);
    }

    #[test]
    fn only_risks_from_one_to_five_get_a_chip() {
        let font = CardFont {
            font: Font::MONOSPACE,
            size: 14.0,
        };
        for risk in 1..=5 {
            assert!(risk_chip::<Msg>(risk, font).is_some(), "{risk}");
        }
        for risk in [0, 6, 9] {
            assert!(risk_chip::<Msg>(risk, font).is_none(), "{risk}");
        }
    }

    #[test]
    fn a_header_moved_under_its_block_is_in_the_way_of_the_next() {
        // Block A at line 0 (no room above), ending at 40; block B's line
        // at 70, with room for its 30 tall header only if A's header (40 to
        // 70) weren't there
        let lines = [0.0, 0.0, 20.0, 70.0, 70.0];
        let heights = [30.0, 20.0, 20.0, 30.0, 20.0];
        let below = [Some(40.0), None, None, Some(90.0), None];
        let (starts, anchors) = wanted_starts(&lines, &below, &heights, 0.0, 0.0);
        // A's header goes under A; B's has no room above B either
        assert_eq!(starts, [40.0, 0.0, 20.0, 90.0, 70.0]);
        let tops = fit_cards(&starts, &anchors, &heights, 0.0, 0.0, 400.0);
        // Every step on its line
        assert_eq!(tops, [40.0, 0.0, 20.0, 90.0, 70.0]);
    }

    #[test]
    fn line_numbers_are_drawn_for_the_lines_in_view() {
        // Lines 20 high, scrolled by 50: line 2 (0-based) is partly above
        let lines = lines_in_view(&geometry(50.0).unwrap(), 100.0);
        assert_eq!(
            lines,
            [
                (2, -10.0),
                (3, 10.0),
                (4, 30.0),
                (5, 50.0),
                (6, 70.0),
                (7, 90.0)
            ]
        );
        // Past the end: none
        assert!(lines_in_view(&geometry(5000.0).unwrap(), 100.0).is_empty());
        let numbers = LineNumbers {
            geometry: geometry(0.0),
            font: CardFont {
                font: Font::MONOSPACE,
                size: 10.0,
            },
            color: None,
            background: None,
        };
        // 100 lines: three digits
        assert_eq!(numbers.width(), (3.0 * 0.6 * 10.0 + 16.0_f32).ceil());
    }

    #[test]
    fn names_in_backticks_are_code() {
        let code = |s: &str| (s.to_string(), true);
        let plain = |s: &str| (s.to_string(), false);
        assert_eq!(
            split_code("Calls `run` from `main`."),
            [
                plain("Calls "),
                code("run"),
                plain(" from "),
                code("main"),
                plain(".")
            ]
        );
        assert_eq!(split_code("`a` b `c"), [code("a"), plain(" b `c")]);
        assert_eq!(split_code("no code"), [plain("no code")]);
        assert!(split_code("``").is_empty());
    }

    #[test]
    fn long_texts_are_shortened_at_words() {
        assert_eq!(shorten("short", 10), "short");
        assert_eq!(shorten("one two three four", 12), "one two…");
        assert_eq!(shorten("abcdefghijklmnop", 5), "abcde…");
    }
}
