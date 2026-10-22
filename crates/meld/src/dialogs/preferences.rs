//! The preferences dialog (`preferences.py`).

use iced::widget::{button, text};
use iced::{Element, Task};

use crate::shared::Shared;
use crate::ui::modal::dialog;

pub struct Preferences {}

#[derive(Debug, Clone)]
pub enum Message {
    Close,
}

/// What a preferences update did.
pub struct Outcome {
    pub task: Task<Message>,
    /// Settings changed: save them and tell the tabs.
    pub changed: bool,
    pub close: bool,
}

impl Preferences {
    pub fn new(_shared: &Shared) -> Self {
        Preferences {}
    }

    pub fn update(&mut self, message: Message, _shared: &mut Shared) -> Outcome {
        match message {
            Message::Close => Outcome {
                task: Task::none(),
                changed: false,
                close: true,
            },
        }
    }

    pub fn view<'a>(&'a self, _shared: &'a Shared) -> Element<'a, Message> {
        dialog("Preferences", text("…"), vec![button(text("Close")).on_press(Message::Close).into()])
    }
}
