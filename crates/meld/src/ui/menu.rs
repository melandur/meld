//! Drop-down menus for header-bar menu buttons.
//!
//! iced has no native menus, so a menu is a popover drawn over the window
//! at a fixed anchor; the app keeps track of which one is open.

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{self, Tree, Widget};
use iced::advanced::{Clipboard, Shell, mouse};
use iced::keyboard;
use iced::widget::{
    button, column, container, mouse_area, opaque, row, scrollable, space, stack, text,
};
use iced::{Element, Event, Length, Padding, Point, Rectangle, Renderer, Size, Theme, Vector};

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
        mouse_area(
            container(positioned)
                .width(Length::Fill)
                .height(Length::Fill)
        )
        .on_press(on_dismiss)
    ]
    .into()
}

/// Draws `items` as a context menu with its top left corner at `point`
/// (relative to `base`); clicking outside sends `on_dismiss`. Menus that
/// would run past the right or bottom edge of `base` open to the left
/// or upwards instead.
pub fn context<'a, M: Clone + 'a>(
    base: impl Into<Element<'a, M>>,
    items: Vec<Item<M>>,
    point: iced::Point,
    on_dismiss: M,
) -> Element<'a, M> {
    const WIDTH: f32 = 260.0;
    let estimated_height: f32 = items
        .iter()
        .map(|item| match item {
            Item::Separator => 9.0,
            Item::Heading(_) => 28.0,
            _ => 30.0,
        })
        .sum::<f32>()
        + 12.0;
    let layer = iced::widget::responsive(move |size| {
        let mut list = column![].spacing(0).width(WIDTH);
        for item in items.iter().cloned() {
            list = list.push(item_view(item));
        }
        let menu = container(list).padding(6).style(super::popover);
        let x = if point.x + WIDTH + 12.0 > size.width {
            (point.x - WIDTH - 12.0).max(0.0)
        } else {
            point.x
        };
        let y = if point.y + estimated_height > size.height {
            (point.y - estimated_height).max(0.0)
        } else {
            point.y
        };
        mouse_area(
            iced::widget::pin(opaque(menu))
                .x(x)
                .y(y)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(on_dismiss.clone())
        .on_right_press(on_dismiss.clone())
        .into()
    });
    stack![base.into(), layer].into()
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

/// `base` with `popup` drawn over the whole window at `point`, given in
/// `base`'s coordinates (such as a pointer position reported by a widget
/// inside it): a context menu that opens at the pointer wherever `base` is
/// embedded. It moves to stay inside the window (opening upwards when there
/// is no room below); clicking outside it or Escape sends `on_dismiss`.
pub fn at_point<'a, M: Clone + 'a>(
    base: impl Into<Element<'a, M>>,
    popup: impl Into<Element<'a, M>>,
    point: Point,
    on_dismiss: M,
) -> Element<'a, M> {
    Element::new(AtPoint {
        base: base.into(),
        popup: popup.into(),
        point,
        on_dismiss,
    })
}

const POPUP_MARGIN: f32 = 6.0;

/// Where a popup of `size` opened at `at` goes in a window of `window`.
pub fn place_at(at: Point, size: Size, window: Size) -> Point {
    let x =
        at.x.min(window.width - size.width - POPUP_MARGIN)
            .max(POPUP_MARGIN);
    let y = if at.y + size.height > window.height - POPUP_MARGIN {
        (at.y - size.height).max(POPUP_MARGIN)
    } else {
        at.y
    };
    Point::new(x, y)
}

struct AtPoint<'a, M> {
    base: Element<'a, M>,
    popup: Element<'a, M>,
    point: Point,
    on_dismiss: M,
}

impl<'a, M: Clone + 'a> Widget<M, Theme, Renderer> for AtPoint<'a, M> {
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.base), Tree::new(&self.popup)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.base, &self.popup]);
    }

    fn size(&self) -> Size<Length> {
        self.base.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.base.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.base
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
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
        self.base.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.base.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
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
        self.base.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        self.base
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        let (base_tree, popup_tree) = tree.children.split_at_mut(1);
        let base = self.base.as_widget_mut().overlay(
            &mut base_tree[0],
            layout,
            renderer,
            viewport,
            translation,
        );
        let popup = overlay::Element::new(Box::new(PointPopup {
            popup: &mut self.popup,
            tree: &mut popup_tree[0],
            at: self.point + translation,
            on_dismiss: self.on_dismiss.clone(),
        }));
        Some(overlay::Group::with_children(base.into_iter().chain(Some(popup)).collect()).overlay())
    }
}

struct PointPopup<'a, 'b, M> {
    popup: &'b mut Element<'a, M>,
    tree: &'b mut Tree,
    at: Point,
    on_dismiss: M,
}

impl<'a, M: Clone + 'a> overlay::Overlay<M, Theme, Renderer> for PointPopup<'a, '_, M> {
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        let max = Size::new(
            (bounds.width - 2.0 * POPUP_MARGIN).max(0.0),
            (bounds.height - 2.0 * POPUP_MARGIN).max(0.0),
        );
        let node = self.popup.as_widget_mut().layout(
            self.tree,
            renderer,
            &layout::Limits::new(Size::ZERO, max),
        );
        let position = place_at(self.at, node.size(), bounds);
        node.move_to(position)
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        let bounds = layout.bounds();
        self.popup
            .as_widget()
            .draw(self.tree, renderer, theme, style, layout, cursor, &bounds);
    }

    fn operate(
        &mut self,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        self.popup
            .as_widget_mut()
            .operate(self.tree, layout, renderer, operation);
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
    ) {
        if let Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Escape),
            ..
        }) = event
        {
            shell.publish(self.on_dismiss.clone());
            shell.capture_event();
            return;
        }
        let bounds = layout.bounds();
        self.popup.as_widget_mut().update(
            self.tree, event, layout, cursor, renderer, clipboard, shell, &bounds,
        );
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(_)) if !cursor.is_over(bounds) => {
                shell.publish(self.on_dismiss.clone());
                shell.capture_event();
            }
            // Keep the pointer from reaching the widgets below the menu.
            Event::Mouse(_) if cursor.is_over(bounds) => shell.capture_event(),
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let bounds = layout.bounds();
        self.popup
            .as_widget()
            .mouse_interaction(self.tree, layout, cursor, &bounds, renderer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popups_open_at_the_point_inside_the_window() {
        let window = Size::new(800.0, 600.0);
        let size = Size::new(300.0, 200.0);
        assert_eq!(
            place_at(Point::new(100.0, 50.0), size, window),
            Point::new(100.0, 50.0)
        );
        // Near the right edge: pushed left.
        assert_eq!(place_at(Point::new(700.0, 50.0), size, window).x, 494.0);
        // Near the bottom: opens upwards from the point.
        assert_eq!(place_at(Point::new(100.0, 500.0), size, window).y, 300.0);
        // Taller than the space above too: kept inside.
        assert_eq!(
            place_at(Point::new(100.0, 100.0), Size::new(300.0, 550.0), window).y,
            POPUP_MARGIN
        );
    }
}
