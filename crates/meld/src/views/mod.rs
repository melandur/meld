//! Comparison tabs (`melddoc.py`).
//!
//! Every tab type implements the same informal interface; [`Tab`] and
//! [`Message`] dispatch to it:
//!
//! - `title()` / `tooltip()`: the tab label and its tooltip
//! - `update(message, shared, events) -> Task<Message>`
//! - `view(shared)`: the tab body; `toolbar(shared)`: its header-bar buttons
//! - `menu_items(shared)`: its section of the main menu
//! - `command(command, shared, events)`: window-level shortcuts
//! - `request_close(shared, events)`: emit [`Event::Close`] now or after
//!   asking about unsaved changes
//! - `unsaved()`: names of documents with unsaved changes
//! - `subscription()`
//!
//! Tabs talk to the window through [`Event`]s.

pub mod dirdiff;
pub mod filediff;
pub mod gitcompare;
pub mod imagediff;
pub mod newtab;
pub mod vcview;

use iced::{Element, Subscription, Task};

use crate::command::Command;
use crate::comparison::Comparison;
use crate::shared::Shared;
use crate::ui::menu;

/// What a tab asks of the window.
#[derive(Debug, Clone)]
pub enum Event {
    /// Open a comparison in a new tab.
    Open(Comparison),
    /// Replace this tab with a comparison (the new comparison page does this).
    Replace(Comparison),
    /// The tab is done; `status` is the exit status reported to the command
    /// line that opened it (0 for success).
    Close { status: i32 },
    /// A short notice for the window's toast area.
    Notify(String),
    /// The tab changed settings; save them and refresh other tabs.
    SettingsChanged,
    /// Record a comparison in the recent comparisons list.
    Recent(Comparison),
    /// A file was saved; other tabs showing it refresh.
    FileChanged(std::path::PathBuf),
}

pub enum Tab {
    New(newtab::NewTab),
    File(Box<filediff::FileDiff>),
    Folder(Box<dirdiff::DirDiff>),
    Vc(Box<vcview::VcView>),
    Git(Box<gitcompare::GitCompare>),
    Image(Box<imagediff::ImageDiff>),
}

#[derive(Debug, Clone)]
pub enum Message {
    New(newtab::Message),
    File(filediff::Message),
    Folder(dirdiff::Message),
    Vc(vcview::Message),
    Git(gitcompare::Message),
    Image(imagediff::Message),
}

macro_rules! dispatch {
    ($self:expr, $tab:ident => $body:expr) => {
        match $self {
            Tab::New($tab) => $body,
            Tab::File($tab) => $body,
            Tab::Folder($tab) => $body,
            Tab::Vc($tab) => $body,
            Tab::Git($tab) => $body,
            Tab::Image($tab) => $body,
        }
    };
    ($self:expr, $tab:ident => $body:expr, map) => {
        match $self {
            Tab::New($tab) => $body.map(Message::New),
            Tab::File($tab) => $body.map(Message::File),
            Tab::Folder($tab) => $body.map(Message::Folder),
            Tab::Vc($tab) => $body.map(Message::Vc),
            Tab::Git($tab) => $body.map(Message::Git),
            Tab::Image($tab) => $body.map(Message::Image),
        }
    };
}

impl Tab {
    /// Opens a comparison as a tab.
    pub fn open(comparison: Comparison, shared: &mut Shared) -> Result<(Tab, Task<Message>), String> {
        Ok(match comparison {
            Comparison::Files {
                paths,
                labels,
                merge_output,
                auto_merge,
                extras,
            } => {
                let (tab, task) = filediff::FileDiff::new(
                    filediff::Options {
                        paths,
                        labels,
                        merge_output,
                        auto_merge,
                        extras,
                        ..filediff::Options::default()
                    },
                    shared,
                );
                (Tab::File(Box::new(tab)), task.map(Message::File))
            }
            Comparison::Folders {
                paths,
                labels,
                auto_compare,
            } => {
                let (tab, task) = dirdiff::DirDiff::new(paths, labels, auto_compare, shared);
                (Tab::Folder(Box::new(tab)), task.map(Message::Folder))
            }
            Comparison::VersionControl {
                location,
                auto_compare,
            } => {
                let (tab, task) = vcview::VcView::new(location, auto_compare, shared);
                (Tab::Vc(Box::new(tab)), task.map(Message::Vc))
            }
            Comparison::VersionControlFile(path) => {
                let (tab, task) = filediff::FileDiff::new_vc_file(path, shared)?;
                (Tab::File(Box::new(tab)), task.map(Message::File))
            }
            Comparison::Git {
                location,
                base,
                target,
            } => {
                let (tab, task) = gitcompare::GitCompare::new(location, base, target, shared);
                (Tab::Git(Box::new(tab)), task.map(Message::Git))
            }
            Comparison::Images { paths } => {
                let (tab, task) = imagediff::ImageDiff::new(paths, shared);
                (Tab::Image(Box::new(tab)), task.map(Message::Image))
            }
            Comparison::SavedFile(path) => {
                let comparison = crate::shared::read_comparison_file(&path)?;
                if matches!(comparison, Comparison::SavedFile(_)) {
                    return Err("invalid saved comparison".into());
                }
                return Tab::open(comparison, shared);
            }
        })
    }

    pub fn new_comparison(shared: &Shared) -> Tab {
        Tab::New(newtab::NewTab::new(shared))
    }

    pub fn title(&self) -> String {
        dispatch!(self, t => t.title())
    }

    pub fn tooltip(&self) -> String {
        dispatch!(self, t => t.tooltip())
    }

    pub fn unsaved(&self) -> Vec<String> {
        dispatch!(self, t => t.unsaved())
    }

    pub fn update(&mut self, message: Message, shared: &mut Shared, events: &mut Vec<Event>) -> Task<Message> {
        match (self, message) {
            (Tab::New(t), Message::New(m)) => t.update(m, shared, events).map(Message::New),
            (Tab::File(t), Message::File(m)) => t.update(m, shared, events).map(Message::File),
            (Tab::Folder(t), Message::Folder(m)) => t.update(m, shared, events).map(Message::Folder),
            (Tab::Vc(t), Message::Vc(m)) => t.update(m, shared, events).map(Message::Vc),
            (Tab::Git(t), Message::Git(m)) => t.update(m, shared, events).map(Message::Git),
            (Tab::Image(t), Message::Image(m)) => t.update(m, shared, events).map(Message::Image),
            // A message for a tab that was replaced in the meantime.
            _ => Task::none(),
        }
    }

    pub fn command(&mut self, command: Command, shared: &mut Shared, events: &mut Vec<Event>) -> Task<Message> {
        dispatch!(self, t => t.command(command, shared, events), map)
    }

    pub fn request_close(&mut self, shared: &mut Shared, events: &mut Vec<Event>) -> Task<Message> {
        dispatch!(self, t => t.request_close(shared, events), map)
    }

    /// Settings changed elsewhere (preferences, another tab).
    pub fn settings_changed(&mut self, shared: &Shared) -> Task<Message> {
        dispatch!(self, t => t.settings_changed(shared), map)
    }

    pub fn view<'a>(&'a self, shared: &'a Shared) -> Element<'a, Message> {
        dispatch!(self, t => t.view(shared), map)
    }

    pub fn toolbar<'a>(&'a self, shared: &'a Shared) -> Element<'a, Message> {
        dispatch!(self, t => t.toolbar(shared), map)
    }

    pub fn menu_items(&self, shared: &Shared) -> Vec<menu::Item<Message>> {
        match self {
            Tab::New(t) => menu::map_items(t.menu_items(shared), Message::New),
            Tab::File(t) => menu::map_items(t.menu_items(shared), Message::File),
            Tab::Folder(t) => menu::map_items(t.menu_items(shared), Message::Folder),
            Tab::Vc(t) => menu::map_items(t.menu_items(shared), Message::Vc),
            Tab::Git(t) => menu::map_items(t.menu_items(shared), Message::Git),
            Tab::Image(t) => menu::map_items(t.menu_items(shared), Message::Image),
        }
    }

    /// Another tab changed `path` on disk.
    pub fn file_changed(&mut self, path: &std::path::Path) -> Task<Message> {
        match self {
            Tab::Folder(t) => t.file_changed(path).map(Message::Folder),
            Tab::Vc(t) => t.file_changed(path).map(Message::Vc),
            _ => Task::none(),
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        dispatch!(self, t => t.subscription(), map)
    }
}
