//! The Why column (port of `meld/ui/whycolumn.py`): a card per change,
//! level with the change's lines in the comparison next to it.
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
use iced::widget::{button, column, container, mouse_area, row, space, text, tooltip};
use iced::{Background, Border, Color, Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector};
use meld_ailens::cards::{CARD_GAP, layout_cards, reflow};
use meld_ailens::picker::{parse_iso_date, relative_date};
use meld_ailens::{Card, CardKind, ExplainAction, ExplanationSource, Side};

use super::diffhost::PaneGeometry;
use super::ellipsis::{Mode, ellipsized};
use crate::ui;

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
            let top = panes[pane].map(|g| {
                let line = line.saturating_sub(1).min(g.line_count);
                line as f32 * g.line_height - g.scroll_y
            });
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

/// A Unix time in local time, e.g. `Tue 21 Oct 2026 17:08:48` (the
/// date's tooltip, `strftime("%c")` in Python).
pub fn local_date(timestamp: f64) -> Option<String> {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
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
        DAYS.get(tm.tm_wday as usize)?,
        tm.tm_mday,
        MONTHS.get(tm.tm_mon as usize)?,
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

/// Per-card state the tab keeps across reloads of the cards.
#[derive(Default)]
pub struct CardStates {
    pub busy: HashSet<String>,
    pub errors: HashMap<String, String>,
    pub expanded: HashSet<String>,
    pub hovered: Option<String>,
}

/// What the cards' buttons send.
pub struct Callbacks<M> {
    pub explain: fn(String) -> M,
    pub commit: fn(String) -> M,
    pub build: fn(String) -> M,
    pub hover: fn(Option<String>) -> M,
    pub expand: fn(String) -> M,
}

impl<M> Clone for Callbacks<M> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<M> Copy for Callbacks<M> {}

fn badge<'a, M: 'a>(label: String, background: Color, color: Option<Color>, italic: bool) -> Element<'a, M> {
    let mut t = text(label).size(11).wrapping(text::Wrapping::None);
    if italic {
        t = t.font(iced::Font {
            style: iced::font::Style::Italic,
            ..iced::Font::DEFAULT
        });
    }
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

fn long_text<'a, M: Clone + 'a>(
    body: String,
    key: &str,
    states: &CardStates,
    on_expand: fn(String) -> M,
    bold: bool,
) -> Element<'a, M> {
    let font = if bold { ui::bold() } else { iced::Font::DEFAULT };
    if body.chars().count() <= LONG_TEXT {
        return text(body).size(13).font(font).wrapping(text::Wrapping::WordOrGlyph).into();
    }
    let expanded = states.expanded.contains(key);
    let shown = if expanded { body } else { shorten(&body, SHORT_TEXT) };
    column![
        text(shown).size(13).font(font).wrapping(text::Wrapping::WordOrGlyph),
        button(text(if expanded { "Show less" } else { "Show more" }).size(12))
            .padding([0, 0])
            .style(link_button)
            .on_press(on_expand(key.to_string())),
    ]
    .spacing(2)
    .into()
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

/// One card: its commit, author or agent, What and Why, the change's own
/// explanation, the explain button and the builds that cover it.
pub fn card_view<'a, M: Clone + 'a>(
    card: &'a Card,
    states: &'a CardStates,
    callbacks: Callbacks<M>,
    now: f64,
) -> Element<'a, M> {
    let key = card.key.as_str();
    let dim = ui::dim_text;

    let mut header = row![].spacing(6).align_y(iced::Center);
    match (card.kind, &card.sha) {
        (CardKind::Commit, Some(sha)) => {
            let sha_button = button(
                text(card.short_sha.as_str())
                    .size(12)
                    .font(iced::Font::MONOSPACE),
            )
            .padding([0, 4])
            .style(ui::flat_button)
            .on_press((callbacks.commit)(sha.clone()));
            header = header.push(tooltip(
                sha_button,
                container(text("Show the full commit message").size(12))
                    .padding([4, 8])
                    .style(ui::tooltip_box),
                tooltip::Position::Bottom,
            ));
        }
        (kind, _) => {
            let label = match kind {
                CardKind::Staged => "Staged",
                CardKind::Unstaged => "Unstaged",
                _ => "Commit unknown",
            };
            header = header.push(text(label).size(12).font(ui::bold()).wrapping(text::Wrapping::None));
        }
    }
    if card.agent() {
        let claude = hex(0xd97757);
        header = header.push(tooltip(
            badge("Claude".into(), claude.scale_alpha(0.2), Some(claude), false),
            container(text(card.agent_name.as_str()).size(12))
                .padding([4, 8])
                .style(ui::tooltip_box),
            tooltip::Position::Bottom,
        ));
    }
    // The model or author gives way to the date
    let who = if card.agent() { &card.agent_model } else { &card.author };
    let filled = !who.is_empty();
    if filled {
        header = header.push(ellipsized(who.as_str(), Mode::End).size(12).style(dim).width(Length::Fill));
    }
    if let Some(date) = parse_iso_date(&card.date).filter(|_| !card.date.is_empty()) {
        if !filled {
            header = header.push(space::horizontal());
        }
        header = header.push(tooltip(
            text(relative_date(date, now)).size(12).style(dim).wrapping(text::Wrapping::None),
            container(text(local_date(date).unwrap_or_else(|| card.date.clone())).size(12))
                .padding([4, 8])
                .style(ui::tooltip_box),
            tooltip::Position::Bottom,
        ));
    }

    let mut body = column![header].spacing(3).width(Length::Fill);
    if !card.what.is_empty() {
        body = body.push(long_text(reflow(&card.what), &format!("{key}:what"), states, callbacks.expand, true));
    }
    if !card.why.is_empty() {
        match card.source {
            Some(ExplanationSource::Inferred) => {
                let by = if card.inferred_by.is_empty() { "a model" } else { card.inferred_by.as_str() };
                body = body.push(badge_line(format!("Inferred by {by}, not the author’s words")));
            }
            Some(source) => {
                let label = match source {
                    ExplanationSource::Commit => "from the commit",
                    ExplanationSource::Note => "from a note",
                    ExplanationSource::Inferred => "inferred",
                };
                body = body.push(text(label).size(11).style(dim));
            }
            None => {}
        }
        body = body.push(long_text(reflow(&card.why), &format!("{key}:why"), states, callbacks.expand, false));
    }

    if !card.change_what.is_empty() || !card.change_why.is_empty() {
        let model = if card.change_model.is_empty() { "a model" } else { card.change_model.as_str() };
        let mut change = column![
            ui::separator(false),
            badge_line(format!("This change · inferred by {model}")),
        ]
        .spacing(3);
        if !card.change_what.is_empty() {
            change = change.push(text(card.change_what.as_str()).size(13).font(ui::bold()));
        }
        if !card.change_why.is_empty() {
            change = change.push(long_text(
                reflow(&card.change_why),
                &format!("{key}:change"),
                states,
                callbacks.expand,
                false,
            ));
        }
        body = body.push(change);
    }

    let busy = states.busy.contains(key);
    if let Some(action) = card.explain {
        let label = match action {
            ExplainAction::Commit | ExplainAction::Uncommitted => "Explain with Claude",
            ExplainAction::Changes => "Explain changes",
        };
        let mut explain = row![
            button(text(label).size(12))
                .padding([3, 10])
                .style(button::secondary)
                .on_press_maybe((!busy).then(|| (callbacks.explain)(card.key.clone())))
        ]
        .spacing(6)
        .align_y(iced::Center);
        if busy {
            explain = explain.push(text("Explaining…").size(12).style(dim));
        }
        body = body.push(explain);
    }
    if let Some(error) = states.errors.get(key).filter(|_| !busy) {
        let (first, rest) = super::condense(error);
        let error = match rest {
            Some(rest) => shorten(&format!("{first} {rest}"), 160),
            None => first,
        };
        body = body.push(
            text(error)
                .size(12)
                .style(|theme: &Theme| text::Style {
                    color: Some(theme.extended_palette().danger.base.color),
                }),
        );
    }
    for build in &card.builds {
        body = body.push(
            button(text(format!("Build {}", build.strip_suffix(".md").unwrap_or(build))).size(12))
                .padding(0)
                .style(link_button)
                .on_press((callbacks.build)(build.clone())),
        );
    }

    let color = card.clone();
    let hovered = states.hovered.as_deref() == Some(key);
    let edge = container(space::vertical())
        .width(4)
        .height(Length::Fill)
        .style(move |theme: &Theme| container::Style {
            background: Some(Background::Color(card_color(&color, theme.extended_palette().background.base.text))),
            border: Border::default().rounded(iced::border::Radius::default().left(6)),
            ..container::Style::default()
        });
    let boxed = container(row![edge, container(body).padding([6, 8]).width(Length::Fill)])
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
        .on_enter((callbacks.hover)(Some(card.key.clone())))
        .on_exit((callbacks.hover)(None))
        .into()
}

fn badge_line<'a, M: 'a>(label: String) -> Element<'a, M> {
    let element: Element<'a, M> = container(text(label).size(11).font(iced::Font {
        style: iced::font::Style::Italic,
        ..iced::Font::DEFAULT
    }))
    .padding([0, 6])
    .style(|theme: &Theme| container::Style {
        background: Some(Background::Color(
            theme.extended_palette().background.base.text.scale_alpha(0.08),
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

/// Lays out cards at the tops they want, pushed down to clear each other,
/// clipped to its bounds. Takes all the space it gets and never asks for
/// height: the cards scroll with the panes.
pub struct CardColumn<'a, M> {
    children: Vec<Element<'a, M>>,
    wanted: Vec<f32>,
    gap: f32,
}

/// Cards wanting to be at `wanted` (one per card), plus an optional
/// placeholder shown at the top.
pub fn card_column<'a, M: 'a>(
    cards: Vec<Element<'a, M>>,
    wanted: Vec<f32>,
    placeholder: Option<Element<'a, M>>,
) -> CardColumn<'a, M> {
    let mut children = cards;
    let mut wanted = wanted;
    wanted.resize(children.len(), 0.0);
    if let Some(p) = placeholder {
        children.push(p);
        wanted.push(24.0);
    }
    CardColumn {
        children,
        wanted,
        gap: CARD_GAP,
    }
}

impl<'a, M: 'a> CardColumn<'a, M> {
    /// Visible part of `bounds` only gets the cursor, so cards scrolled
    /// out of the column can't be clicked through the widgets around it.
    fn cursor_in(bounds: Rectangle, cursor: mouse::Cursor) -> mouse::Cursor {
        if cursor.is_over(bounds) {
            cursor
        } else {
            mouse::Cursor::Unavailable
        }
    }
}

impl<'a, M: 'a> Widget<M, Theme, Renderer> for CardColumn<'a, M> {
    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
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
        let heights: Vec<f32> = nodes.iter().map(|n| n.size().height).collect();
        let tops = place_cards(&self.wanted, &heights, self.gap);
        for (node, top) in nodes.iter_mut().zip(tops) {
            node.move_to_mut((0.0, top));
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
            for ((child, state), layout) in self.children.iter_mut().zip(&mut tree.children).zip(layout.children()) {
                child.as_widget_mut().operate(state, layout, renderer, operation);
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
        let bounds = layout.bounds();
        let cursor = Self::cursor_in(bounds, cursor);
        let viewport = bounds.intersection(viewport).unwrap_or(bounds);
        for ((child, state), layout) in self.children.iter_mut().zip(&mut tree.children).zip(layout.children()) {
            child
                .as_widget_mut()
                .update(state, event, layout, cursor, renderer, clipboard, shell, &viewport);
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
        let bounds = layout.bounds();
        let cursor = Self::cursor_in(bounds, cursor);
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, state), layout)| {
                child.as_widget().mouse_interaction(state, layout, cursor, viewport, renderer)
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
        let bounds = layout.bounds();
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };
        let cursor = Self::cursor_in(bounds, cursor);
        use iced::advanced::Renderer as _;
        renderer.with_layer(clip, |renderer| {
            for ((child, state), layout) in self
                .children
                .iter()
                .zip(&tree.children)
                .zip(layout.children())
                .filter(|(_, l)| l.bounds().intersects(&clip))
            {
                child.as_widget().draw(state, renderer, theme, style, layout, cursor, &clip);
            }
        });
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        overlay::from_children(&mut self.children, tree, layout, renderer, viewport, translation)
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
            added: added.map(|(s, e)| LineRange::new(s, e)).into_iter().collect(),
            deleted: deleted.map(|(s, e)| LineRange::new(s, e)).into_iter().collect(),
            ..Default::default()
        }
    }

    fn geometry(scroll_y: f32) -> Option<PaneGeometry> {
        Some(PaneGeometry {
            scroll_y,
            line_height: 20.0,
            viewport_height: 400.0,
            line_count: 100,
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
        assert_eq!(tops, [2.0 * 20.0 - 20.0, 9.0 * 20.0 - 100.0, 100.0 * 20.0 - 20.0]);
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
    fn long_texts_are_shortened_at_words() {
        assert_eq!(shorten("short", 10), "short");
        assert_eq!(shorten("one two three four", 12), "one two…");
        assert_eq!(shorten("abcdefghijklmnop", 5), "abcde…");
    }
}
