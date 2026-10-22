//! A wrapper widget that turns key presses no inner widget consumed into
//! messages. Unlike a keyboard subscription it only sees events while
//! its tab is on screen, so arrow keys move the selection of the visible
//! version control view only.

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree};
use iced::advanced::{Clipboard, Shell, Widget, mouse, overlay, renderer};
use iced::keyboard::{self, Key, Modifiers};
use iced::{Element, Event, Length, Rectangle, Size, Vector};

pub struct KeyHandler<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    on_key: Box<dyn Fn(&Key, Modifiers) -> Option<Message> + 'a>,
    on_modifiers: Box<dyn Fn(Modifiers) -> Message + 'a>,
}

/// Wraps `content`; `on_key` maps unconsumed key presses to a message
/// (the key press is then consumed), `on_modifiers` reports modifier
/// changes.
pub fn key_handler<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    on_key: impl Fn(&Key, Modifiers) -> Option<Message> + 'a,
    on_modifiers: impl Fn(Modifiers) -> Message + 'a,
) -> KeyHandler<'a, Message, Theme, Renderer> {
    KeyHandler {
        content: content.into(),
        on_key: Box::new(on_key),
        on_modifiers: Box::new(on_modifiers),
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for KeyHandler<'_, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        self.content.as_widget_mut().layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(&mut self, tree: &mut Tree, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                shell.publish((self.on_modifiers)(*modifiers));
            }
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) if !shell.is_event_captured() => {
                if let Some(message) = (self.on_key)(key, *modifiers) {
                    shell.publish(message);
                    shell.capture_event();
                }
            }
            _ => {}
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
        self.content
            .as_widget()
            .mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
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
        self.content
            .as_widget()
            .draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}

impl<'a, Message, Theme, Renderer> From<KeyHandler<'a, Message, Theme, Renderer>> for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: renderer::Renderer + 'a,
{
    fn from(handler: KeyHandler<'a, Message, Theme, Renderer>) -> Self {
        Element::new(handler)
    }
}
