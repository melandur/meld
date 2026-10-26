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

use crate::forge::relative_date;
use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{self, Tree, Widget};
use iced::advanced::{Clipboard, Shell, mouse};
use iced::keyboard;
use iced::widget::{
    button, column, container, mouse_area, rich_text, row, space, span, text, tooltip,
};
use iced::{
    Background, Border, Color, Element, Event, Font, Length, Rectangle, Renderer, Size, Theme,
    Vector,
};
pub use meld_ailens::cards::CARD_GAP;
use meld_ailens::cards::{layout_cards, reflow};
use meld_ailens::picker::parse_iso_date;
use meld_ailens::{
    Card, CardKind, CardStep, ChangeGuide, ChangeKind, ExplainAction, ExplanationSource, Side,
};

use super::diffhost::PaneGeometry;
use crate::ui;
use crate::ui::ellipsis::{Mode, ellipsized};

/// Number of commit colors (`.ailens-color-N` in style.css).
pub const COLOR_COUNT: usize = 8;
pub const DEFAULT_WIDTH: f32 = 300.0;
pub const MIN_WIDTH: f32 = 160.0;
pub const MAX_WIDTH: f32 = 900.0;

/// Texts longer than this are cut short until the card is expanded.
const LONG_TEXT: usize = 360;
const SHORT_TEXT: usize = 280;

fn hex(rgb: u32) -> Color {
    Color::from_rgb8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// The colors of `.ailens-color-0` … `-7`.
const COMMIT_COLORS: [u32; COLOR_COUNT] = [
    0x3584e4, 0x9141ac, 0xe66100, 0x26a269, 0xc01c28, 0x1c71d8, 0x865e3c, 0x613583,
];

/// The color of a card's left edge: one per commit, by its place in the
/// comparison; fixed colors for staged, unstaged and unknown changes.
pub fn card_color(card: &Card, text: Color) -> Color {
    match card.kind {
        CardKind::Commit if card.color_index >= 0 => {
            hex(COMMIT_COLORS[card.color_index as usize % COLOR_COUNT])
        }
        CardKind::Staged => hex(0x2ec27e),
        CardKind::Unstaged => hex(0xe5a50a),
        CardKind::Commit => text.scale_alpha(0.3),
        CardKind::Unknown => text.scale_alpha(0.2),
    }
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

/// A Unix time in local time, e.g. `Tue 21 Oct 2026 17:08:48` (the
/// date's tooltip, `strftime("%c")` in Python).
pub fn local_date(timestamp: f64) -> Option<String> {
    let days = [
        trc!("abbreviated weekday", "Sun"),
        trc!("abbreviated weekday", "Mon"),
        trc!("abbreviated weekday", "Tue"),
        trc!("abbreviated weekday", "Wed"),
        trc!("abbreviated weekday", "Thu"),
        trc!("abbreviated weekday", "Fri"),
        trc!("abbreviated weekday", "Sat"),
    ];
    let months = [
        trc!("abbreviated month", "Jan"),
        trc!("abbreviated month", "Feb"),
        trc!("abbreviated month", "Mar"),
        trc!("abbreviated month", "Apr"),
        trc!("abbreviated month", "May"),
        trc!("abbreviated month", "Jun"),
        trc!("abbreviated month", "Jul"),
        trc!("abbreviated month", "Aug"),
        trc!("abbreviated month", "Sep"),
        trc!("abbreviated month", "Oct"),
        trc!("abbreviated month", "Nov"),
        trc!("abbreviated month", "Dec"),
    ];
    if !timestamp.is_finite() {
        return None;
    }
    let t = timestamp.floor() as libc::time_t;
    // SAFETY: localtime_r only writes into the provided struct.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return None;
    }
    Some(format!(
        "{} {:02} {} {} {:02}:{:02}:{:02}",
        days.get(tm.tm_wday as usize)?,
        tm.tm_mday,
        months.get(tm.tm_mon as usize)?,
        tm.tm_year + 1900,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    ))
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
    /// Open cards ([`open_key`]), open commits of the summary
    /// ([`commit_key`]) and long texts shown in full, by key
    pub expanded: HashSet<String>,
    pub hovered: Option<String>,
    /// The step of the hovered card under the pointer, if any
    pub hovered_step: Option<usize>,
}

/// The key in [`CardStates::expanded`] of an open card.
pub fn open_key(key: &str) -> String {
    format!("{key}:open")
}

/// The key in [`CardStates::expanded`] of a commit opened in the summary.
pub fn commit_key(sha: &str) -> String {
    format!("commit:{sha}")
}

/// The key in [`CardStates::expanded`] of a summary showing every commit.
const ALL_COMMITS_KEY: &str = "commits:all";

/// Commits the summary shows until it is told to show them all.
const SUMMARY_COMMITS: usize = 3;

/// What the cards' buttons send.
pub struct Callbacks<M> {
    pub explain: fn(String) -> M,
    pub commit: fn(String) -> M,
    pub build: fn(String) -> M,
    pub hover: fn(Option<String>) -> M,
    /// The pointer left a card (its key)
    pub leave: fn(String) -> M,
    pub expand: fn(String) -> M,
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

/// Where to read each card: its number and how many cards are numbered.
///
/// Cards are numbered by their commit, oldest first ([`commit_sort_key`]), then
/// staged and
/// unstaged changes, and within those by the reading order the model gave
/// their changes. None for cards without an order, and for all of them
/// when fewer than two have one.
pub fn reading_order(cards: &[Card]) -> Vec<Option<(usize, usize)>> {
    let mut numbers = vec![None; cards.len()];
    let mut ordered: Vec<usize> = (0..cards.len())
        .filter(|&i| cards[i].change_order.is_some())
        .collect();
    if ordered.len() < 2 {
        return numbers;
    }
    let rank = |card: &Card| match card.kind {
        CardKind::Commit => 0,
        CardKind::Staged => 1,
        CardKind::Unstaged => 2,
        CardKind::Unknown => 3,
    };
    ordered.sort_by_cached_key(|&i| {
        let card = &cards[i];
        (rank(card), commit_sort_key(card), card.change_order, i)
    });
    let total = ordered.len();
    for (number, index) in ordered.into_iter().enumerate() {
        numbers[index] = Some((number + 1, total));
    }
    numbers
}

/// Where a commit sorts, oldest first, in the summary and the reading
/// order alike: by place in the range; commits outside it (all color -1)
/// by when they were committed (a rebase or cherry-pick updates that, not
/// the author date), then by SHA, which keeps each one's cards together.
/// Parsed once per card: sort with `sort_by_cached_key`.
fn commit_sort_key(card: &Card) -> (i32, i64, Option<&str>) {
    // Milliseconds; undated last
    let committed = parse_iso_date(&card.committed).map_or(i64::MAX, |date| (date * 1000.0) as i64);
    (card.color_index, committed, card.sha.as_deref())
}

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

/// The chip of a change's kind: colored for the kinds to read closely, grey
/// for those to skim.
fn kind_chip<'a, M: 'a>(kind: ChangeKind, font: CardFont) -> Element<'a, M> {
    let (label, rgb) = match kind {
        ChangeKind::Behavior => (trc!("change kind", "behavior"), 0x3584e4),
        ChangeKind::Fix => (trc!("change kind", "fix"), 0xc01c28),
        ChangeKind::Refactor => (trc!("change kind", "refactor"), 0x9141ac),
        ChangeKind::Rename => (trc!("change kind", "rename"), 0x77767b),
        ChangeKind::Mechanical => (trc!("change kind", "mechanical"), 0x77767b),
        ChangeKind::Test => (trc!("change kind", "test"), 0x77767b),
    };
    let color = hex(rgb);
    badge(
        label.to_string(),
        color.scale_alpha(0.15),
        Some(color),
        false,
        font,
    )
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

fn long_text<'a, M: Clone + 'a>(
    body: String,
    key: &str,
    states: &CardStates,
    callbacks: Callbacks<M>,
    font: CardFont,
) -> Element<'a, M> {
    if body.chars().count() <= LONG_TEXT {
        return rich(&body, font.size, font.font, false, callbacks.jump);
    }
    let expanded = states.expanded.contains(key);
    let shown = if expanded {
        body
    } else {
        shorten(&body, SHORT_TEXT)
    };
    column![
        rich(&shown, font.size, font.font, false, callbacks.jump),
        button(
            text(if expanded {
                tr!("Show less")
            } else {
                tr!("Show more")
            })
            .size(font.smaller(1.0))
            .font(font.font)
        )
        .padding([0, 0])
        .style(link_button)
        .on_press((callbacks.expand)(key.to_string())),
    ]
    .spacing(2)
    .into()
}

/// A small label over its text, e.g. Check or Why.
fn labeled<'a, M: Clone + 'a>(
    label: &'a str,
    body: Element<'a, M>,
    font: CardFont,
) -> Element<'a, M> {
    column![small_label(label, font), body].spacing(1).into()
}

/// What the code did before and does now, `label` beside the text.
fn before_after<'a, M: Clone + 'a>(
    label: &'a str,
    body: &str,
    callbacks: Callbacks<M>,
    font: CardFont,
) -> Element<'a, M> {
    row![
        small_label(label, font)
            .width(font.size * 2.4)
            .wrapping(text::Wrapping::None),
        rich(&reflow(body), font.size, font.font, false, callbacks.jump),
    ]
    .spacing(4)
    .align_y(iced::Alignment::Start)
    .into()
}

/// One step of a change, level with the lines it explains: the code's
/// font, size and line height, so each of its lines is beside a line of
/// code. Steps of changes to skim are dimmed; hovering one highlights its
/// lines.
fn step_view<'a, M: Clone + 'a>(
    card: &'a Card,
    index: usize,
    step: &'a CardStep,
    states: &CardStates,
    callbacks: Callbacks<M>,
    font: CardFont,
    line_height: f32,
) -> Element<'a, M> {
    let skim = card.change_kind.is_some_and(ChangeKind::skimmable);
    let body = rich_with(
        &step.text,
        font.size,
        font.font,
        skim,
        callbacks.jump,
        Some(line_height),
    );
    // The step itself, or all of its card's
    let tint = match (
        states.hovered.as_deref() == Some(card.key.as_str()),
        states.hovered_step,
    ) {
        (true, Some(hovered)) if hovered == index => Some(0.14),
        (true, None) => Some(0.06),
        _ => None,
    };
    let line = container(row![
        // Fainter than the card's own: a step, not a card
        edge(card, 2.0, 0.6, Border::default()),
        container(body).padding([0, 6]).width(Length::Fill)
    ])
    .width(Length::Fill)
    .style(move |theme: &Theme| {
        let palette = theme.extended_palette();
        container::Style {
            background: tint
                .map(|alpha| Background::Color(palette.primary.base.color.scale_alpha(alpha))),
            ..container::Style::default()
        }
    });
    mouse_area(container(line).padding([0, 6]))
        .on_enter((callbacks.hover_step)(card.key.clone(), index, true))
        .on_exit((callbacks.hover_step)(card.key.clone(), index, false))
        .into()
}

/// A card's colored edge, `width` wide: the color of its commit.
fn edge<'a, M: 'a>(card: &Card, width: f32, alpha: f32, border: Border) -> Element<'a, M> {
    // Only what the color depends on, not the whole card
    let color = Card {
        kind: card.kind,
        color_index: card.color_index,
        ..Card::default()
    };
    container(space::vertical())
        .width(width)
        .height(Length::Fill)
        .style(move |theme: &Theme| container::Style {
            background: Some(Background::Color(
                card_color(&color, theme.extended_palette().background.base.text)
                    .scale_alpha(alpha),
            )),
            border,
            ..container::Style::default()
        })
        .into()
}

/// A small dim label, e.g. Check or Was.
fn small_label<'a>(label: &'a str, font: CardFont) -> iced::widget::Text<'a> {
    text(label)
        .size(font.smaller(2.0))
        .font(font.font)
        .style(ui::dim_text)
}

/// The line that opens and closes a card: quiet until hovered.
fn toggle_button(theme: &Theme, status: button::Status) -> button::Style {
    let text = theme.extended_palette().background.base.text;
    let color = match status {
        button::Status::Hovered | button::Status::Pressed => text,
        _ => text.scale_alpha(0.6),
    };
    button::Style {
        background: None,
        text_color: color,
        ..button::Style::default()
    }
}

fn link_button(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let color = match status {
        button::Status::Hovered | button::Status::Pressed => palette.primary.strong.color,
        _ => palette.primary.base.color,
    };
    button::Style {
        background: None,
        text_color: color,
        ..button::Style::default()
    }
}

/// The commit's short SHA, which shows its whole message; or what the
/// change is when it has no commit.
fn origin<'a, M: Clone + 'a>(
    card: &'a Card,
    callbacks: Callbacks<M>,
    font: CardFont,
) -> Element<'a, M> {
    match (card.kind, &card.sha) {
        (CardKind::Commit, Some(sha)) => {
            let sha_button = button(
                text(card.short_sha.as_str())
                    .size(font.smaller(1.0))
                    .font(font.font)
                    .style(ui::dim_text),
            )
            .padding([0, 4])
            .style(ui::flat_button)
            .on_press((callbacks.commit)(sha.clone()));
            tooltip(
                sha_button,
                container(text(tr!("Show the full commit message")).size(12))
                    .padding([4, 8])
                    .style(ui::tooltip_box),
                tooltip::Position::Bottom,
            )
            .into()
        }
        (kind, _) => {
            let label = match kind {
                CardKind::Staged => tr!("Staged"),
                CardKind::Unstaged => tr!("Unstaged"),
                _ => tr!("Commit unknown"),
            };
            text(label)
                .size(font.smaller(1.0))
                .font(font.bold())
                .wrapping(text::Wrapping::None)
                .into()
        }
    }
}

/// Who made a commit and when: the agent's badge, the model or author and
/// the date.
fn people_line<'a, M: 'a>(card: &'a Card, now: f64, font: CardFont) -> Option<Element<'a, M>> {
    let mut line = row![].spacing(6).align_y(iced::Center);
    let mut any = false;
    if card.agent() {
        let claude = hex(0xd97757);
        line = line.push(tooltip(
            badge(
                "Claude".into(),
                claude.scale_alpha(0.2),
                Some(claude),
                false,
                font,
            ),
            container(text(card.agent_name.as_str()).size(12))
                .padding([4, 8])
                .style(ui::tooltip_box),
            tooltip::Position::Bottom,
        ));
        any = true;
    }
    // The model or author gives way to the date
    let who = if card.agent() {
        &card.agent_model
    } else {
        &card.author
    };
    let filled = !who.is_empty();
    if filled {
        line = line.push(
            ellipsized(who.as_str(), Mode::End)
                .size(font.smaller(1.0))
                .font(font.font)
                .style(ui::dim_text)
                .width(Length::Fill),
        );
        any = true;
    }
    if let Some(date) = parse_iso_date(&card.date).filter(|_| !card.date.is_empty()) {
        if !filled {
            line = line.push(space::horizontal());
        }
        line = line.push(tooltip(
            text(relative_date(date, now))
                .size(font.smaller(1.0))
                .font(font.font)
                .style(ui::dim_text)
                .wrapping(text::Wrapping::None),
            container(text(local_date(date).unwrap_or_else(|| card.date.clone())).size(12))
                .padding([4, 8])
                .style(ui::tooltip_box),
            tooltip::Position::Bottom,
        ));
        any = true;
    }
    any.then(|| line.into())
}

/// Where a commit's Why comes from: its message, a note, or a model.
fn source_line<'a, M: 'a>(card: &Card, font: CardFont) -> Option<Element<'a, M>> {
    match card.source? {
        ExplanationSource::Inferred => {
            let line = if card.inferred_by.is_empty() {
                tr!("Inferred by a model, not the author’s words").to_string()
            } else {
                tr!(
                    "Inferred by {model}, not the author’s words",
                    model = card.inferred_by
                )
            };
            Some(badge_line(line, font))
        }
        source => {
            let label = match source {
                ExplanationSource::Commit => tr!("from the commit"),
                ExplanationSource::Note => tr!("from a note"),
                ExplanationSource::Inferred => tr!("inferred"),
            };
            Some(
                text(label)
                    .size(font.smaller(2.0))
                    .font(font.font)
                    .style(ui::dim_text)
                    .into(),
            )
        }
    }
}

fn build_links<'a, M: Clone + 'a>(
    card: &'a Card,
    callbacks: Callbacks<M>,
    font: CardFont,
) -> Vec<Element<'a, M>> {
    card.builds
        .iter()
        .map(|build| {
            button(
                text(tr!(
                    "Build {name}",
                    name = build.strip_suffix(".md").unwrap_or(build)
                ))
                .size(font.smaller(1.0))
                .font(font.font),
            )
            .padding(0)
            .style(link_button)
            .on_press((callbacks.build)(build.clone()))
            .into()
        })
        .collect()
}

/// What the column shows, and where: each element wants to start at `y`;
/// a block's header wants to end there, or when there is no room, to start
/// at `below` ([`wanted_starts`]).
pub struct Item<'a, M> {
    pub element: Element<'a, M>,
    pub y: f32,
    pub below: Option<f32>,
}

/// The column laid out like the panes beside it: per change block its card
/// as a header, ending where the block's first line starts (or under the
/// block, when there is no room above it), then each step level with the
/// first line it explains. Steps without lines start with the block; steps
/// on deleted lines of a block that also adds lines come right after the
/// block, so they don't push the steps of its added lines off their lines.
/// For a [`card_column`] without a gap: the headers keep their own space
/// above them.
#[allow(clippy::too_many_arguments)]
pub fn explain_items<'a, M: Clone + 'a>(
    cards: &'a [Card],
    states: &'a CardStates,
    callbacks: Callbacks<M>,
    now: f64,
    font: CardFont,
    reading: Vec<Option<(usize, usize)>>,
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
    for ((card, reading), top) in cards.iter().zip(reading).zip(tops) {
        // Under the block: after its added lines, else its deleted ones
        let end = line_end(Side::New, &card.added)
            .or_else(|| line_end(Side::Old, &card.deleted))
            .unwrap_or(top);
        // The space above the header is its own: steps sit on their lines
        // with no gap between them
        items.push(Item {
            element: container(card_view(card, states, callbacks, now, font, reading))
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

/// The card's headline: what its change does, else what its commit did.
fn headline(card: &Card) -> &str {
    if !card.change_what.is_empty() {
        &card.change_what
    } else {
        &card.what
    }
}

/// Characters of a closed card's headline.
const HEADLINE_TEXT: usize = 160;

/// A headline for a closed card: its first paragraph (a card covering
/// several changes joins theirs), cut near [`HEADLINE_TEXT`] characters,
/// with an ellipsis when anything is left out.
pub fn short_headline(title: &str) -> String {
    let title = title.trim();
    let (first, more) = match title.split_once("\n\n") {
        Some((first, _)) => (first.trim(), true),
        None => (title, false),
    };
    let mut short = shorten(first, HEADLINE_TEXT);
    // Cut inside a name from the code: cut before it instead, so its
    // backtick isn't left without its pair; with nothing before it, show
    // it as plain text
    if short != first
        && short.matches('`').count() % 2 == 1
        && let Some(open) = short.rfind('`')
    {
        let before = short[..open].trim_end();
        short = if before.is_empty() {
            short.replace('`', "")
        } else {
            format!("{before}…")
        };
    }
    if more && !short.ends_with('…') {
        format!("{short} …")
    } else {
        short
    }
}

/// Whether the card has its own explanation of the change.
fn has_change(card: &Card) -> bool {
    !card.change_what.is_empty() || !card.change_why.is_empty()
}

/// One card, the header of its change block, read at a glance: the
/// change's kind, its place in the reading order and a statement of what
/// the changed code does (in notes from before steps, what it did before
/// and does now). The steps are beside their lines ([`explain_items`]).
/// Open, it adds what to check, the Was and Now, the Why and
/// Justification, who made the change and the builds that cover it.
/// Changes to skim (renames, mechanical changes, tests) show their headline
/// only until opened. What the commit as a
/// whole did is in the summary above the cards ([`commit_summary`]).
pub fn card_view<'a, M: Clone + 'a>(
    card: &'a Card,
    states: &'a CardStates,
    callbacks: Callbacks<M>,
    now: f64,
    font: CardFont,
    reading: Option<(usize, usize)>,
) -> Element<'a, M> {
    let key = card.key.as_str();
    let dim = ui::dim_text;
    let open = states.expanded.contains(&open_key(key));
    let skim = !open && card.change_kind.is_some_and(ChangeKind::skimmable);

    let mut top = row![].spacing(6).align_y(iced::Center);
    if let Some(kind) = card.change_kind {
        top = top.push(kind_chip(kind, font));
    }
    if let Some(chip) = card.change_risk.and_then(|risk| risk_chip(risk, font)) {
        top = top.push(chip);
    }
    if let Some((number, total)) = reading {
        let label = if number == 1 {
            tr!(
                "Start here · {number}/{total}",
                number = number,
                total = total
            )
        } else {
            format!("{number}/{total}")
        };
        top = top.push(
            text(label)
                .size(font.smaller(2.0))
                .font(if number == 1 { font.bold() } else { font.font })
                .style(dim)
                .wrapping(text::Wrapping::None),
        );
    }
    top = top.push(space::horizontal());
    top = top.push(origin(card, callbacks, font));

    let mut body = column![top].spacing(3).width(Length::Fill);
    let title = headline(card);
    if !title.is_empty() {
        let title_font = if skim { font.font } else { font.bold() };
        // At a glance, its first paragraph cut short; all of it when open
        let title = if open {
            title.to_string()
        } else {
            short_headline(title)
        };
        body = body.push(rich(&title, font.size, title_font, skim, callbacks.jump));
    }
    let steps = !card.change_steps.is_empty();
    // With steps, the before and after wait until the card is opened
    let was_now = if steps { open } else { !skim };
    if was_now {
        if !card.change_was.is_empty() {
            body = body.push(before_after(tr!("Was"), &card.change_was, callbacks, font));
        }
        if !card.change_now.is_empty() {
            body = body.push(before_after(tr!("Now"), &card.change_now, callbacks, font));
        }
    }
    if !skim {
        // Notes from before changes had a before and after: the change's
        // Why stands in, cut short
        if !open
            && !steps
            && card.change_was.is_empty()
            && card.change_now.is_empty()
            && !card.change_why.is_empty()
        {
            body = body.push(rich(
                &shorten(&reflow(&card.change_why), SHORT_TEXT),
                font.size,
                font.font,
                false,
                callbacks.jump,
            ));
        }
    }

    if open {
        if !card.change_check.is_empty() {
            body = body.push(labeled(
                tr!("Check"),
                rich(
                    &reflow(&card.change_check),
                    font.size,
                    font.font,
                    false,
                    callbacks.jump,
                ),
                font,
            ));
        }
        if has_change(card) {
            if !card.change_why.is_empty() {
                body = body.push(labeled(
                    tr!("Why"),
                    long_text(
                        reflow(&card.change_why),
                        &format!("{key}:change"),
                        states,
                        callbacks,
                        font,
                    ),
                    font,
                ));
            }
            if !card.change_justification.is_empty() {
                body = body.push(labeled(
                    tr!("Justification"),
                    long_text(
                        reflow(&card.change_justification),
                        &format!("{key}:change-justification"),
                        states,
                        callbacks,
                        font,
                    ),
                    font,
                ));
            }
            let line = if card.change_model.is_empty() {
                tr!("This change · inferred by a model").to_string()
            } else {
                tr!(
                    "This change · inferred by {model}",
                    model = card.change_model
                )
            };
            body = body.push(badge_line(line, font));
        } else if !card.why.is_empty() {
            // No explanation of its own: the commit's stands in
            if let Some(source) = source_line(card, font) {
                body = body.push(source);
            }
            body = body.push(labeled(
                tr!("Why"),
                long_text(
                    reflow(&card.why),
                    &format!("{key}:why"),
                    states,
                    callbacks,
                    font,
                ),
                font,
            ));
            if !card.justification.is_empty() {
                body = body.push(labeled(
                    tr!("Justification"),
                    long_text(
                        reflow(&card.justification),
                        &format!("{key}:justification"),
                        states,
                        callbacks,
                        font,
                    ),
                    font,
                ));
            }
        }
        if let Some(people) = people_line(card, now, font) {
            body = body.push(people);
        }
        for link in build_links(card, callbacks, font) {
            body = body.push(link);
        }
    }

    let busy = states.busy.contains(key);
    if let Some(action) = card.explain {
        let label = match action {
            ExplainAction::Commit | ExplainAction::Uncommitted => tr!("Explain with Claude"),
            ExplainAction::Changes => tr!("Explain changes"),
        };
        let mut explain = row![
            button(text(label).size(font.smaller(1.0)).font(font.font))
                .padding([3, 10])
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
                    .style(dim),
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

    // What opening the card shows
    let mut more: Vec<String> = Vec::new();
    if !was_now && (!card.change_was.is_empty() || !card.change_now.is_empty()) {
        more.push(tr!("Was/Now").to_string());
    }
    if !card.change_check.is_empty() {
        more.push(tr!("Check").to_string());
    }
    if !card.change_why.is_empty() || !card.why.is_empty() {
        more.push(tr!("Why").to_string());
    }
    if card.kind == CardKind::Commit {
        more.push(tr!("Details").to_string());
    }
    if open || !more.is_empty() {
        let label = if open {
            format!("▾ {}", tr!("Less"))
        } else {
            format!("▸ {}", more.join(" · "))
        };
        body = body.push(
            button(text(label).size(font.smaller(1.0)).font(font.font))
                .padding(0)
                .style(toggle_button)
                .on_press((callbacks.expand)(open_key(key))),
        );
    }

    framed(card, body, states.hovered.as_deref() == Some(key))
        .on_enter((callbacks.hover)(Some(card.key.clone())))
        .on_exit((callbacks.leave)(card.key.clone()))
        .on_press((callbacks.expand)(open_key(key)))
        .into()
}

/// `body` in a card with the commit's color at its left edge.
fn framed<'a, M: 'a>(
    card: &Card,
    body: iced::widget::Column<'a, M>,
    hovered: bool,
) -> iced::widget::MouseArea<'a, M> {
    let edge = edge(
        card,
        4.0,
        1.0,
        Border::default().rounded(iced::border::Radius::default().left(6)),
    );
    let boxed = container(row![
        edge,
        container(body).padding([6, 8]).width(Length::Fill)
    ])
    .width(Length::Fill)
    .style(move |theme: &Theme| {
        let palette = theme.extended_palette();
        let base = palette.background.base.color;
        let background = if hovered {
            mix(base, palette.primary.base.color, 0.08)
        } else {
            base
        };
        container::Style {
            background: Some(Background::Color(background)),
            text_color: Some(palette.background.base.text),
            border: Border::default().rounded(6),
            shadow: iced::Shadow {
                color: Color::BLACK.scale_alpha(0.12),
                offset: Vector::new(0.0, 1.0),
                blur_radius: 2.0,
            },
            ..container::Style::default()
        }
    });
    mouse_area(container(boxed).padding([0, 6]))
}

/// The commits of the file's cards, once each and oldest first, above the
/// cards: what each did, and opened, why, by whom and the builds that cover
/// it. None when no card has a commit.
pub fn commit_summary<'a, M: Clone + 'a>(
    cards: &'a [Card],
    states: &'a CardStates,
    callbacks: Callbacks<M>,
    now: f64,
    font: CardFont,
) -> Option<Element<'a, M>> {
    let mut commits: Vec<&Card> = Vec::new();
    for card in cards {
        if card.kind == CardKind::Commit
            && card.sha.is_some()
            && !commits.iter().any(|c| c.sha == card.sha)
        {
            commits.push(card);
        }
    }
    if commits.is_empty() {
        return None;
    }
    // In the order the cards are numbered in
    commits.sort_by_cached_key(|card| commit_sort_key(card));
    let all = states.expanded.contains(ALL_COMMITS_KEY);
    let shown = if all { commits.len() } else { SUMMARY_COMMITS };
    let mut list = column![].spacing(4).width(Length::Fill);
    for card in commits.iter().take(shown) {
        list = list.push(commit_row(card, states, callbacks, now, font));
    }
    if commits.len() > SUMMARY_COMMITS {
        let label = if all {
            tr!("Fewer commits").to_string()
        } else {
            let count = commits.len() - SUMMARY_COMMITS;
            trn!(
                "{count} more commit",
                "{count} more commits",
                count,
                count = count
            )
        };
        list = list.push(
            button(text(label).size(font.smaller(1.0)).font(font.font))
                .padding(0)
                .style(link_button)
                .on_press((callbacks.expand)(ALL_COMMITS_KEY.to_string())),
        );
    }
    Some(
        column![
            container(list).padding([4, 8]).width(Length::Fill),
            ui::separator(false),
        ]
        .into(),
    )
}

/// One commit of the summary: its SHA and What; opened, its Why,
/// Justification, people and builds.
fn commit_row<'a, M: Clone + 'a>(
    card: &'a Card,
    states: &'a CardStates,
    callbacks: Callbacks<M>,
    now: f64,
    font: CardFont,
) -> Element<'a, M> {
    let sha = card.sha.clone().unwrap_or_default();
    let key = commit_key(&sha);
    let opened = states.expanded.contains(&key);
    let what = button(
        row![
            // Names from the code without their backticks: one line has no
            // room to set them apart
            ellipsized(card.what.replace('`', ""), Mode::End)
                .size(font.smaller(1.0))
                .font(font.bold())
                .width(Length::Fill),
            text(if opened { "▾" } else { "▸" })
                .size(font.smaller(2.0))
                .style(ui::dim_text),
        ]
        .spacing(4)
        .align_y(iced::Center),
    )
    .padding(0)
    .width(Length::Fill)
    .style(ui::flat_button)
    .on_press((callbacks.expand)(key.clone()));
    let mut details = column![
        row![origin(card, callbacks, font), what]
            .spacing(4)
            .align_y(iced::Center)
    ]
    .spacing(3)
    .width(Length::Fill);
    if opened {
        if let Some(source) = source_line(card, font) {
            details = details.push(source);
        }
        if card.what.chars().count() > 40 {
            details = details.push(rich(
                &card.what,
                font.size,
                font.font,
                false,
                callbacks.jump,
            ));
        }
        if !card.why.is_empty() {
            details = details.push(labeled(
                tr!("Why"),
                long_text(
                    reflow(&card.why),
                    &format!("{key}:why"),
                    states,
                    callbacks,
                    font,
                ),
                font,
            ));
        }
        if !card.justification.is_empty() {
            details = details.push(labeled(
                tr!("Justification"),
                long_text(
                    reflow(&card.justification),
                    &format!("{key}:justification"),
                    states,
                    callbacks,
                    font,
                ),
                font,
            ));
        }
        if let Some(people) = people_line(card, now, font) {
            details = details.push(people);
        }
        for link in build_links(card, callbacks, font) {
            details = details.push(link);
        }
    }
    row![edge(card, 3.0, 1.0, Border::default().rounded(2)), details]
        .spacing(6)
        .into()
}

fn badge_line<'a, M: 'a>(label: String, font: CardFont) -> Element<'a, M> {
    let element: Element<'a, M> =
        container(text(label).size(font.smaller(2.0)).font(font.italic()))
            .padding([0, 6])
            .style(|theme: &Theme| container::Style {
                background: Some(Background::Color(
                    theme
                        .extended_palette()
                        .background
                        .base
                        .text
                        .scale_alpha(0.08),
                )),
                border: Border::default().rounded(9),
                ..container::Style::default()
            })
            .into();
    row![element].into()
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    Color::from_rgba(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
        a.a,
    )
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
    /// Whether Shift is held, for [`Wheel::WithShift`]
    shift: bool,
}

/// Where the wheel scrolls cards that don't fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wheel {
    /// Anywhere over the column; once the panes scroll, the cards follow
    /// their lines again
    Anywhere,
    /// Only with Shift held, the panes being under the column: the plain
    /// wheel always scrolls the code, so cards and code never take turns
    WithShift,
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
    /// Where the wheel scrolls cards that don't fit
    wheel: Wheel,
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
        wheel: Wheel::Anywhere,
    }
}

impl<'a, M: 'a> CardColumn<'a, M> {
    /// The space between cards that clear each other ([`CARD_GAP`] unless
    /// set)
    pub fn gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }

    /// Where the wheel scrolls cards that don't fit ([`Wheel::Anywhere`]
    /// unless set)
    pub fn wheel(mut self, wheel: Wheel) -> Self {
        self.wheel = wheel;
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
        if let Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) = event {
            tree.state.downcast_mut::<ColumnState>().shift = modifiers.shift();
        }
        if shell.is_event_captured() {
            return;
        }
        if let Event::Mouse(mouse::Event::WheelScrolled { delta }) = event {
            if !cursor.is_over(self.card_area(layout)) {
                return;
            }
            let state = tree.state.downcast_mut::<ColumnState>();
            if self.wheel == Wheel::WithShift && !state.shift {
                return;
            }
            // Some systems turn Shift+wheel into a sideways scroll
            let (dx, dy) = match *delta {
                mouse::ScrollDelta::Lines { x, y } => (x * WHEEL_LINE, y * WHEEL_LINE),
                mouse::ScrollDelta::Pixels { x, y } => (x, y),
            };
            let dy = if dy == 0.0 { dx } else { dy };
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
            viewport_height: 400.0,
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
            commit: Msg::Key,
            build: Msg::Key,
            hover: Msg::Hover,
            leave: Msg::Key,
            expand: Msg::Key,
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
            0.0,
            font,
            vec![None, None],
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
            0.0,
            font,
            vec![None],
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
        let items = explain_items(
            &cards,
            &states,
            callbacks(),
            0.0,
            font,
            vec![None],
            [None, None],
        );
        let placed: Vec<(f32, Option<f32>)> = items.iter().map(|i| (i.y, i.below)).collect();
        assert_eq!(placed, [(0.0, Some(0.0)), (0.0, None)]);
    }

    #[test]
    fn commit_colors_cycle() {
        let text = Color::BLACK;
        let mut c = Card {
            kind: CardKind::Commit,
            color_index: 9,
            ..Default::default()
        };
        assert_eq!(card_color(&c, text), hex(COMMIT_COLORS[1]));
        c.color_index = -1;
        assert_eq!(card_color(&c, text), text.scale_alpha(0.3));
        c.kind = CardKind::Staged;
        assert_eq!(card_color(&c, text), hex(0x2ec27e));
    }

    #[test]
    fn dates_are_shown_in_local_time() {
        // Mid-month and mid-year, so that no time zone changes the month
        let date = local_date(1_784_548_800.0).unwrap();
        assert!(date.contains(" Jul 2026 "), "{date}");
        assert_eq!(date.len(), "Wed 15 Jul 2026 12:00:00".len());
        assert_eq!(local_date(f64::NAN), None);
    }

    #[test]
    fn cards_are_numbered_in_reading_order_by_commit() {
        let ordered = |kind, color_index, order| Card {
            kind,
            color_index,
            change_order: order,
            ..Default::default()
        };
        let cards = [
            ordered(CardKind::Commit, 1, Some(1)),
            ordered(CardKind::Commit, 0, Some(2)),
            ordered(CardKind::Staged, -1, Some(1)),
            ordered(CardKind::Commit, 0, None),
            ordered(CardKind::Commit, 0, Some(1)),
        ];
        assert_eq!(
            reading_order(&cards),
            [Some((3, 4)), Some((2, 4)), Some((4, 4)), None, Some((1, 4))]
        );
        // One ordered card is no order to read in
        assert_eq!(reading_order(&cards[..1]), [None]);
    }

    #[test]
    fn commits_outside_the_range_keep_their_changes_together() {
        let ordered = |sha: &str, order| Card {
            kind: CardKind::Commit,
            color_index: -1,
            sha: Some(sha.into()),
            committed: if sha == "aaa" {
                "2026-03-01T10:00:00+02:00".into()
            } else {
                "2026-01-01T10:00:00+00:00".into()
            },
            // Authored the other way round: a rebase keeps author dates
            date: if sha == "aaa" {
                "2025-01-01T10:00:00+00:00".into()
            } else {
                "2025-06-01T10:00:00+00:00".into()
            },
            change_order: Some(order),
            ..Default::default()
        };
        let cards = [
            ordered("bbb", 1),
            ordered("aaa", 2),
            ordered("bbb", 2),
            ordered("aaa", 1),
        ];
        // bbb is older, though its SHA comes later
        assert_eq!(
            reading_order(&cards),
            [Some((1, 4)), Some((4, 4)), Some((2, 4)), Some((3, 4))]
        );
        // Times are compared as times, not as text
        let at = |date: &str| Card {
            committed: date.into(),
            ..Default::default()
        };
        assert!(
            commit_sort_key(&at("2026-01-01T10:00:00+02:00"))
                < commit_sort_key(&at("2026-01-01T09:30:00+00:00"))
        );
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
    fn closed_cards_show_the_start_of_their_headline() {
        assert_eq!(short_headline("  Adds a cache. "), "Adds a cache.");
        // A card covering several changes: the first one's, marked
        assert_eq!(
            short_headline("Adds a cache.\n\nRetries."),
            "Adds a cache. …"
        );
        let long = "word ".repeat(60);
        let short = short_headline(&long);
        assert!(short.ends_with('…') && short.chars().count() <= HEADLINE_TEXT + 1);
        assert!(!short_headline(&format!("{long}\n\nmore")).ends_with("… …"));
        // Not cut inside a name from the code
        let code = format!("{} `fn parse(a: &str, b: usize)` here", "word ".repeat(30));
        let short = short_headline(&code);
        assert_eq!(short.matches('`').count(), 0, "{short}");
        assert!(short.ends_with("word…"), "{short}");
        let whole = format!("{} `parse` here", "word ".repeat(10));
        assert_eq!(short_headline(&whole), whole.trim());
        // A lone backtick in a headline not cut short stays
        assert_eq!(
            short_headline("Quotes the ` character"),
            "Quotes the ` character"
        );
        // Cut inside a name it starts with: the name, as plain text
        let leading = format!("`fn refresh({})` now caches", "a: u8, ".repeat(40));
        let short = short_headline(&leading);
        assert!(
            short.starts_with("fn refresh(a: u8") && !short.contains('`'),
            "{short}"
        );
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
