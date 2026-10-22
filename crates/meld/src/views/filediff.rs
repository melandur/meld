//! File comparison (filediff.py).

use iced::widget::{space, text, container};
use iced::{Element, Length, Subscription, Task};

use crate::command::Command;
use crate::shared::Shared;
use crate::ui::menu;
use crate::views::Event;

pub struct FileDiff {
    title: String,
}

#[derive(Debug, Clone)]
pub enum Message {}

impl FileDiff {
    pub fn new(options: Options, _shared: &mut Shared) -> (Self, Task<Message>) {
        (
            FileDiff {
                title: format!("{} files", options.paths.len()),
            },
            Task::none(),
        )
    }

    pub fn title(&self) -> String {
        self.title.clone()
    }

    pub fn tooltip(&self) -> String {
        self.title.clone()
    }

    pub fn unsaved(&self) -> Vec<String> {
        Vec::new()
    }

    pub fn update(&mut self, message: Message, _shared: &mut Shared, _events: &mut Vec<Event>) -> Task<Message> {
        match message {}
    }

    pub fn command(&mut self, _command: Command, _shared: &mut Shared, _events: &mut Vec<Event>) -> Task<Message> {
        Task::none()
    }

    pub fn request_close(&mut self, _shared: &mut Shared, events: &mut Vec<Event>) -> Task<Message> {
        events.push(Event::Close { status: 0 });
        Task::none()
    }

    pub fn settings_changed(&mut self, _shared: &Shared) -> Task<Message> {
        Task::none()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }

    pub fn toolbar<'a>(&'a self, _shared: &'a Shared) -> Element<'a, Message> {
        space::horizontal().into()
    }

    pub fn menu_items(&self, _shared: &Shared) -> Vec<menu::Item<Message>> {
        Vec::new()
    }

    pub fn view<'a>(&'a self, _shared: &'a Shared) -> Element<'a, Message> {
        container(text("File comparison")).center(Length::Fill).into()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub paths: Vec<Option<std::path::PathBuf>>,
    pub labels: Vec<String>,
    pub merge_output: Option<std::path::PathBuf>,
    pub auto_merge: bool,
}

impl FileDiff {
    pub fn new_vc_file(path: std::path::PathBuf, shared: &mut Shared) -> Result<(Self, Task<Message>), String> {
        Ok(Self::new(
            Options {
                paths: vec![Some(path)],
                ..Options::default()
            },
            shared,
        ))
    }
}
