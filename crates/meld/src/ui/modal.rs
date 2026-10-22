//! Modal dialogs drawn over the window content.

use iced::widget::{center, container, mouse_area, opaque, stack};
use iced::{Color, Element, Theme};

/// Shows `content` centred over a dimmed `base`. Clicking outside the
/// dialog sends `on_blur`.
pub fn modal<'a, M: Clone + 'a>(
    base: impl Into<Element<'a, M>>,
    content: impl Into<Element<'a, M>>,
    on_blur: M,
) -> Element<'a, M> {
    stack![
        base.into(),
        opaque(
            mouse_area(center(opaque(content)).style(|_theme: &Theme| container::Style {
                background: Some(Color::BLACK.scale_alpha(0.45).into()),
                ..container::Style::default()
            }))
            .on_press(on_blur)
        )
    ]
    .into()
}

/// The frame of a dialog: a title, a body and a row of buttons.
pub fn dialog<'a, M: Clone + 'a>(
    title: impl iced::widget::text::IntoFragment<'a>,
    body: impl Into<Element<'a, M>>,
    buttons: Vec<Element<'a, M>>,
) -> Element<'a, M> {
    use iced::widget::{column, row, space, text};

    let mut button_row = row![space::horizontal()].spacing(8);
    for b in buttons {
        button_row = button_row.push(b);
    }
    container(
        column![text(title).size(18).font(super::bold()), body.into(), button_row]
            .spacing(16)
            .max_width(640),
    )
    .padding(20)
    .max_width(680)
    .style(super::popover)
    .into()
}
