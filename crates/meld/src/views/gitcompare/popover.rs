//! A popup anchored below a widget (`Gtk.Popover` of the revision pickers
//! and the "more" menu): drawn over everything, closed by clicking outside
//! it or pressing Escape.

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{self, Tree, Widget};
use iced::advanced::{Clipboard, Shell, mouse};
use iced::keyboard::{self, key};
use iced::{Element, Event, Length, Point, Rectangle, Renderer, Size, Theme, Vector};

const GAP: f32 = 4.0;
const MARGIN: f32 = 6.0;

pub struct Popover<'a, M> {
    anchor: Element<'a, M>,
    popup: Option<Element<'a, M>>,
    on_dismiss: M,
    on_key: Option<Box<dyn Fn(key::Named) -> Option<M> + 'a>>,
    align_right: bool,
}

/// `anchor`, with `popup` shown below it when given.
pub fn popover<'a, M: Clone + 'a>(
    anchor: impl Into<Element<'a, M>>,
    popup: Option<Element<'a, M>>,
    on_dismiss: M,
) -> Popover<'a, M> {
    Popover {
        anchor: anchor.into(),
        popup,
        on_dismiss,
        on_key: None,
        align_right: false,
    }
}

impl<'a, M: Clone + 'a> Popover<'a, M> {
    /// Keys handled before the popup's widgets see them (arrow keys for
    /// moving through a list while a text field has focus).
    pub fn on_key(mut self, f: impl Fn(key::Named) -> Option<M> + 'a) -> Self {
        self.on_key = Some(Box::new(f));
        self
    }

    /// Aligns the popup's right edge with the anchor's.
    pub fn align_right(mut self, right: bool) -> Self {
        self.align_right = right;
        self
    }
}

impl<'a, M: Clone + 'a> Widget<M, Theme, Renderer> for Popover<'a, M> {
    fn children(&self) -> Vec<Tree> {
        let mut children = vec![Tree::new(&self.anchor)];
        if let Some(popup) = &self.popup {
            children.push(Tree::new(popup));
        }
        children
    }

    fn diff(&self, tree: &mut Tree) {
        match &self.popup {
            Some(popup) => tree.diff_children(&[&self.anchor, popup]),
            None => tree.diff_children(&[&self.anchor]),
        }
    }

    fn size(&self) -> Size<Length> {
        self.anchor.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.anchor.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.anchor
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
        self.anchor.as_widget_mut().update(
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
        self.anchor.as_widget().mouse_interaction(
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
        self.anchor.as_widget().draw(
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
        self.anchor
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
        let mut children = tree.children.iter_mut();
        let anchor_tree = children.next()?;
        let anchor_overlay = self.anchor.as_widget_mut().overlay(
            anchor_tree,
            layout,
            renderer,
            viewport,
            translation,
        );
        let popup = match (&mut self.popup, children.next()) {
            (Some(popup), Some(popup_tree)) => Some(overlay::Element::new(Box::new(Popup {
                popup,
                tree: popup_tree,
                anchor: layout.bounds() + translation,
                on_dismiss: self.on_dismiss.clone(),
                on_key: self.on_key.as_deref(),
                align_right: self.align_right,
            }))),
            _ => None,
        };
        if anchor_overlay.is_none() && popup.is_none() {
            return None;
        }
        Some(
            overlay::Group::with_children(anchor_overlay.into_iter().chain(popup).collect())
                .overlay(),
        )
    }
}

impl<'a, M: Clone + 'a> From<Popover<'a, M>> for Element<'a, M> {
    fn from(popover: Popover<'a, M>) -> Self {
        Element::new(popover)
    }
}

struct Popup<'a, 'b, M> {
    popup: &'b mut Element<'a, M>,
    tree: &'b mut Tree,
    anchor: Rectangle,
    on_dismiss: M,
    on_key: Option<&'b (dyn Fn(key::Named) -> Option<M> + 'a)>,
    align_right: bool,
}

/// Where a popup of `size` goes below `anchor` within a window of `bounds`.
pub fn place(anchor: Rectangle, size: Size, bounds: Size, align_right: bool) -> Point {
    let mut x = if align_right {
        anchor.x + anchor.width - size.width
    } else {
        anchor.x
    };
    x = x.min(bounds.width - size.width - MARGIN).max(MARGIN);
    let mut y = anchor.y + anchor.height + GAP;
    if y + size.height > bounds.height - MARGIN {
        y = (bounds.height - MARGIN - size.height).max(MARGIN);
    }
    Point::new(x, y)
}

impl<'a, M: Clone + 'a> overlay::Overlay<M, Theme, Renderer> for Popup<'a, '_, M> {
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        let max = Size::new(
            (bounds.width - 2.0 * MARGIN).max(0.0),
            (bounds.height - self.anchor.y - self.anchor.height - GAP - MARGIN).max(200.0),
        );
        let node = self.popup.as_widget_mut().layout(
            self.tree,
            renderer,
            &layout::Limits::new(Size::ZERO, max),
        );
        let position = place(self.anchor, node.size(), bounds, self.align_right);
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
            key: keyboard::Key::Named(named),
            ..
        }) = event
        {
            if *named == key::Named::Escape {
                shell.publish(self.on_dismiss.clone());
                shell.capture_event();
                return;
            }
            if let Some(message) = self.on_key.and_then(|f| f(*named)) {
                shell.publish(message);
                shell.capture_event();
                return;
            }
        }
        let bounds = layout.bounds();
        self.popup.as_widget_mut().update(
            self.tree, event, layout, cursor, renderer, clipboard, shell, &bounds,
        );
        if let Event::Mouse(mouse::Event::ButtonPressed(_)) = event
            && !cursor.is_over(bounds)
        {
            shell.publish(self.on_dismiss.clone());
            shell.capture_event();
        }
    }

    /// Overlays of the popup's own widgets: tooltips of the picker's rows.
    fn overlay<'c>(
        &'c mut self,
        layout: Layout<'c>,
        renderer: &Renderer,
    ) -> Option<overlay::Element<'c, M, Theme, Renderer>> {
        let bounds = layout.bounds();
        self.popup
            .as_widget_mut()
            .overlay(self.tree, layout, renderer, &bounds, Vector::ZERO)
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let bounds = layout.bounds();
        let interaction = self
            .popup
            .as_widget()
            .mouse_interaction(self.tree, layout, cursor, &bounds, renderer);
        // Anything but `None` hides the pointer from the widgets below, so
        // that they don't light up or show their tooltips through the popup
        if interaction == mouse::Interaction::None && cursor.is_over(bounds) {
            mouse::Interaction::Idle
        } else {
            interaction
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popups_open_below_and_stay_in_the_window() {
        let anchor = Rectangle::new(Point::new(100.0, 10.0), Size::new(80.0, 30.0));
        let window = Size::new(800.0, 600.0);
        assert_eq!(
            place(anchor, Size::new(300.0, 200.0), window, false),
            Point::new(100.0, 44.0)
        );
        // Too wide to the right: pushed left
        assert_eq!(
            place(anchor, Size::new(750.0, 200.0), window, false).x,
            44.0
        );
        // Right-aligned with the anchor, but not off the left edge
        assert_eq!(place(anchor, Size::new(120.0, 50.0), window, true).x, 60.0);
        assert_eq!(
            place(anchor, Size::new(300.0, 50.0), window, true).x,
            MARGIN
        );
        // Too tall: moved up
        assert_eq!(
            place(anchor, Size::new(100.0, 580.0), window, false).y,
            14.0
        );
    }
}
