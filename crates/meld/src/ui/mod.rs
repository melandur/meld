//! Shared look and small building blocks: header-bar buttons, menus,
//! modal dialogs and the palette used by every view.

pub mod menu;
pub mod modal;

use iced::widget::{button, container, text, tooltip};
use iced::{Background, Border, Color, Element, Length, Theme};

/// Glyphs used as icons. They come from the system fonts, so stick to
/// widely covered symbols.
pub mod icon {
    pub const CLOSE: &str = "✕";
    pub const ADD: &str = "+";
    pub const MENU: &str = "☰";
    pub const UP: &str = "↑";
    pub const DOWN: &str = "↓";
    pub const LEFT: &str = "←";
    pub const RIGHT: &str = "→";
    pub const REFRESH: &str = "⟳";
    pub const SAVE: &str = "💾";
    pub const UNDO: &str = "↶";
    pub const REDO: &str = "↷";
    pub const FILTER: &str = "⚲";
    pub const FOLDER: &str = "📁";
    pub const FILE: &str = "📄";
    pub const SWAP: &str = "⇄";
    pub const MODIFIED: &str = "●";
    pub const CONFLICT_PREV: &str = "⇡";
    pub const CONFLICT_NEXT: &str = "⇣";
    pub const STOP: &str = "■";
}

pub const HEADER_HEIGHT: f32 = 40.0;

/// A flat icon button for header bars and toolbars, with a tooltip.
pub fn tool_button<'a, M: Clone + 'a>(
    glyph: &'a str,
    tip: &'a str,
    on_press: Option<M>,
) -> Element<'a, M> {
    let b = button(text(glyph).size(16).center())
        .padding([4, 8])
        .style(flat_button)
        .on_press_maybe(on_press);
    tooltip(b, container(text(tip).size(13)).padding([4, 8]).style(tooltip_box), tooltip::Position::Bottom)
        .into()
}

/// A flat text button.
pub fn text_button<'a, M: Clone + 'a>(label: impl text::IntoFragment<'a>, on_press: Option<M>) -> button::Button<'a, M> {
    button(text(label).size(14))
        .padding([5, 10])
        .style(flat_button)
        .on_press_maybe(on_press)
}

pub fn flat_button(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let base = button::Style {
        background: None,
        text_color: palette.background.base.text,
        border: Border::default().rounded(6),
        ..button::Style::default()
    };
    match status {
        button::Status::Active => base,
        button::Status::Hovered => button::Style {
            background: Some(Background::Color(palette.background.strong.color.scale_alpha(0.5))),
            ..base
        },
        button::Status::Pressed => button::Style {
            background: Some(Background::Color(palette.background.strong.color)),
            ..base
        },
        button::Status::Disabled => button::Style {
            text_color: palette.background.base.text.scale_alpha(0.35),
            ..base
        },
    }
}

/// Style for the selected tab in the tab strip.
pub fn tab_button(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let palette = theme.extended_palette();
        let mut style = flat_button(theme, status);
        if selected {
            style.background = Some(Background::Color(palette.background.base.color));
            style.border = Border::default()
                .rounded(6)
                .width(1)
                .color(palette.background.strong.color);
        }
        style
    }
}

pub fn header_bar(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(palette.background.weak.color)),
        border: Border::default().width(0),
        ..container::Style::default()
    }
}

pub fn tooltip_box(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(palette.background.strong.color)),
        text_color: Some(palette.background.strong.text),
        border: Border::default().rounded(4),
        ..container::Style::default()
    }
}

/// A card-like box for popovers and dialogs.
pub fn popover(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(palette.background.base.color)),
        text_color: Some(palette.background.base.text),
        border: Border::default()
            .rounded(10)
            .width(1)
            .color(palette.background.strong.color),
        shadow: iced::Shadow {
            color: Color::BLACK.scale_alpha(0.3),
            offset: iced::Vector::new(0.0, 4.0),
            blur_radius: 16.0,
        },
        ..container::Style::default()
    }
}

/// A one-pixel separator line.
pub fn separator<'a, M: 'a>(vertical: bool) -> Element<'a, M> {
    let c = container(iced::widget::Space::new())
        .style(|theme: &Theme| container::Style {
            background: Some(Background::Color(theme.extended_palette().background.strong.color)),
            ..container::Style::default()
        });
    if vertical {
        c.width(1).height(Length::Fill).into()
    } else {
        c.height(1).width(Length::Fill).into()
    }
}

/// A banner for errors and notices at the top of a view (`msgarea.py`).
pub fn banner<'a, M: Clone + 'a>(
    kind: BannerKind,
    primary: impl text::IntoFragment<'a>,
    secondary: Option<String>,
    actions: Vec<(String, M)>,
) -> Element<'a, M> {
    use iced::widget::{column, row};

    let mut body = column![text(primary).size(14).font(bold())].spacing(2);
    if let Some(s) = secondary {
        body = body.push(text(s).size(13));
    }
    let mut r = row![body.width(Length::Fill)].spacing(8).align_y(iced::Center);
    for (label, msg) in actions {
        r = r.push(button(text(label).size(13)).padding([4, 10]).on_press(msg));
    }
    container(r)
        .padding([8, 12])
        .width(Length::Fill)
        .style(move |theme: &Theme| {
            let palette = theme.extended_palette();
            let color = match kind {
                BannerKind::Info => palette.primary.weak.color,
                BannerKind::Warning => palette.warning.weak.color,
                BannerKind::Error => palette.danger.weak.color,
            };
            container::Style {
                background: Some(Background::Color(color.scale_alpha(0.35))),
                text_color: Some(palette.background.base.text),
                ..container::Style::default()
            }
        })
        .into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerKind {
    Info,
    Warning,
    Error,
}

pub fn bold() -> iced::Font {
    iced::Font {
        weight: iced::font::Weight::Bold,
        ..iced::Font::DEFAULT
    }
}

pub fn dim_text(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(theme.extended_palette().background.base.text.scale_alpha(0.6)),
    }
}
