//! Drop-down menus for header-bar menu buttons.
//!
//! iced has no native menus, so a menu is a popover drawn over the window
//! at a fixed anchor; the app keeps track of which one is open.

use iced::widget::{button, column, container, mouse_area, opaque, row, scrollable, space, stack, text};
use iced::{Element, Length, Padding};

#[derive(Debug, Clone)]
pub enum Item<M> {
    Action {
        label: String,
        shortcut: Option<&'static str>,
        message: Option<M>,
    },
    Toggle {
        label: String,
        checked: bool,
        message: M,
    },
    Separator,
    Heading(String),
}

impl<M> Item<M> {
    pub fn action(label: impl Into<String>, shortcut: Option<&'static str>, message: M) -> Self {
        Item::Action {
            label: label.into(),
            shortcut,
            message: Some(message),
        }
    }

    pub fn disabled(label: impl Into<String>) -> Self {
        Item::Action {
            label: label.into(),
            shortcut: None,
            message: None,
        }
    }

    pub fn toggle(label: impl Into<String>, checked: bool, message: M) -> Self {
        Item::Toggle {
            label: label.into(),
            checked,
            message,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Anchor {
    TopRight(f32),
}

/// Maps the messages of menu items.
pub fn map_items<M, N>(items: Vec<Item<M>>, f: impl Fn(M) -> N) -> Vec<Item<N>> {
    items
        .into_iter()
        .map(|item| match item {
            Item::Action {
                label,
                shortcut,
                message,
            } => Item::Action {
                label,
                shortcut,
                message: message.map(&f),
            },
            Item::Toggle {
                label,
                checked,
                message,
            } => Item::Toggle {
                label,
                checked,
                message: f(message),
            },
            Item::Separator => Item::Separator,
            Item::Heading(h) => Item::Heading(h),
        })
        .collect()
}

/// Draws `items` as a menu over `base`; clicking outside sends `on_dismiss`.
pub fn overlay<'a, M: Clone + 'a>(
    base: impl Into<Element<'a, M>>,
    items: Vec<Item<M>>,
    anchor: Anchor,
    on_dismiss: M,
) -> Element<'a, M> {
    let mut list = column![].spacing(0).width(Length::Shrink);
    for item in items {
        list = list.push(item_view(item));
    }
    // Long menus scroll instead of running past the bottom of the window.
    let menu = container(scrollable(list.width(280)).height(Length::Shrink))
        .padding(6)
        .max_height(560)
        .style(super::popover);

    let positioned: Element<'a, M> = match anchor {
        Anchor::TopRight(x) => row![space::horizontal(), opaque(menu)]
            .padding(Padding {
                top: super::HEADER_HEIGHT,
                right: x,
                ..Padding::ZERO
            })
            .into(),
    };

    stack![
        base.into(),
        mouse_area(container(positioned).width(Length::Fill).height(Length::Fill))
            .on_press(on_dismiss)
    ]
    .into()
}

fn item_view<'a, M: Clone + 'a>(item: Item<M>) -> Element<'a, M> {
    match item {
        Item::Action {
            label,
            shortcut,
            message,
        } => {
            let mut r = row![text(label).size(14).width(Length::Fill)].spacing(12);
            if let Some(s) = shortcut {
                r = r.push(text(s).size(12).style(super::dim_text));
            }
            button(r)
                .width(Length::Fill)
                .padding([6, 10])
                .style(super::flat_button)
                .on_press_maybe(message)
                .into()
        }
        Item::Toggle {
            label,
            checked,
            message,
        } => button(
            row![
                text(if checked { "✓" } else { " " }).size(14).width(18),
                text(label).size(14)
            ]
            .spacing(4),
        )
        .width(Length::Fill)
        .padding([6, 10])
        .style(super::flat_button)
        .on_press(message)
        .into(),
        Item::Separator => container(super::separator(false)).padding([4, 0]).into(),
        Item::Heading(label) => container(text(label).size(12).style(super::dim_text))
            .padding([6, 10])
            .into(),
    }
}
