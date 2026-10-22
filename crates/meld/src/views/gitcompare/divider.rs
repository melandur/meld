//! A vertical divider to drag (the `Gtk.Paned` handle beside the file list
//! and the Why column's handle). Pressing it reports where the pointer
//! pressed, so that the drag follows the pointer from its first move;
//! double-clicking may reset the width.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{self, Tree, tree};
use iced::advanced::{Clipboard, Shell, Widget, mouse};
use iced::widget::container;
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme};

use crate::ui;

pub struct Divider<'a, M> {
    content: Element<'a, M>,
    on_press: Box<dyn Fn(f32) -> M + 'a>,
    on_double_click: Option<M>,
}

/// A divider; `on_press` gets the pointer's x in window coordinates.
pub fn divider<'a, M: Clone + 'a>(
    on_press: impl Fn(f32) -> M + 'a,
    on_double_click: Option<M>,
) -> Divider<'a, M> {
    Divider {
        content: container(ui::separator(true))
            .width(5)
            .height(Length::Fill)
            .align_x(iced::alignment::Horizontal::Center)
            .into(),
        on_press: Box::new(on_press),
        on_double_click,
    }
}

#[derive(Default)]
struct State {
    last_click: Option<mouse::Click>,
}

impl<'a, M: Clone + 'a> Widget<M, Theme, Renderer> for Divider<'a, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        _viewport: &Rectangle,
    ) {
        let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event else {
            return;
        };
        let Some(position) = cursor.position_over(layout.bounds()) else {
            return;
        };
        let state = tree.state.downcast_mut::<State>();
        let click = mouse::Click::new(position, mouse::Button::Left, state.last_click);
        state.last_click = Some(click);
        match (&self.on_double_click, click.kind()) {
            (Some(reset), mouse::click::Kind::Double) => shell.publish(reset.clone()),
            _ => shell.publish((self.on_press)(position.x)),
        }
        shell.capture_event();
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            mouse::Interaction::ResizingHorizontally
        } else {
            mouse::Interaction::default()
        }
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
        self.content.as_widget().draw(
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
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }
}

impl<'a, M: Clone + 'a> From<Divider<'a, M>> for Element<'a, M> {
    fn from(divider: Divider<'a, M>) -> Self {
        Element::new(divider)
    }
}
