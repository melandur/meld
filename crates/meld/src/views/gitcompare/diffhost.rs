//! The two-pane file comparison inside a Git Compare tab.
//!
//! This module is the only place of Git Compare that knows what compares
//! the files. Python embeds a `FileDiff(2)` and attaches the Why column to
//! its right; [`DiffHost`] wraps the embedded
//! [`FileDiff`](crate::views::filediff::FileDiff) the same way:
//!
//! - `DiffHost::embedded(shared)`, `set_files(Vec<EmbeddedFile>, shared)`
//! - `pane_geometry(pane) -> Option<PaneGeometry>` (0-based line `i` is at
//!   `PaneGeometry::line_y(i)` below `header_height`, which accounts for
//!   wrapped lines and a message shown above that pane)
//! - `scroll_to_line(pane, line)`, `first_change()`, `is_loading()`,
//!   `cursor_line()` (where a line comment of a review goes)
//! - [`DiffHost::header_height`]: how far below the top of the view the
//!   file header row ends, so that the Why column lines up
//! - [`DiffHost::set_hover`]: lines of the hovered card, drawn highlighted
//! - the tab methods, forwarded unchanged

use std::ops::Range;

use iced::{Element, Subscription, Task};

use crate::command::Command;
use crate::shared::Shared;
use crate::ui::menu;
use crate::views::Event;
use crate::views::filediff::FileDiff;

pub use crate::views::filediff::{EmbeddedFile, Message, PaneGeometry};

pub struct DiffHost(FileDiff);

impl DiffHost {
    pub fn embedded(shared: &mut Shared) -> Self {
        DiffHost(FileDiff::embedded(shared))
    }

    pub fn set_files(&mut self, files: Vec<EmbeddedFile>, shared: &mut Shared) -> Task<Message> {
        self.0.set_files(files, shared)
    }

    pub fn is_loading(&self) -> bool {
        self.0.is_loading()
    }

    pub fn pane_geometry(&self, pane: usize) -> Option<PaneGeometry<'_>> {
        self.0.pane_geometry(pane)
    }

    /// The pane (0 base, 1 target) and 0-based line of the cursor.
    pub fn cursor_line(&self) -> Option<(usize, usize)> {
        self.0.cursor_line()
    }

    pub fn header_height(&self) -> f32 {
        self.0.header_height()
    }

    pub fn first_change(&self) -> Option<usize> {
        self.0.first_change()
    }

    pub fn scroll_to_line(&mut self, pane: usize, line: usize) -> Task<Message> {
        self.0.scroll_to_line(pane, line)
    }

    pub fn set_hover(&mut self, lines: [Vec<Range<usize>>; 2]) {
        self.0.set_hover(lines);
    }

    pub fn update(
        &mut self,
        message: Message,
        shared: &mut Shared,
        events: &mut Vec<Event>,
    ) -> Task<Message> {
        self.0.update(message, shared, events)
    }

    pub fn command(
        &mut self,
        command: Command,
        shared: &mut Shared,
        events: &mut Vec<Event>,
    ) -> Task<Message> {
        self.0.command(command, shared, events)
    }

    pub fn settings_changed(&mut self, shared: &Shared) -> Task<Message> {
        self.0.settings_changed(shared)
    }

    pub fn unsaved(&self) -> Vec<String> {
        self.0.unsaved()
    }

    pub fn request_close(&mut self, shared: &mut Shared, events: &mut Vec<Event>) -> Task<Message> {
        self.0.request_close(shared, events)
    }

    pub fn subscription(&self) -> Subscription<Message> {
        self.0.subscription()
    }

    pub fn toolbar<'a>(&'a self, shared: &'a Shared) -> Element<'a, Message> {
        self.0.toolbar(shared)
    }

    pub fn menu_items(&self, shared: &Shared) -> Vec<menu::Item<Message>> {
        self.0.menu_items(shared)
    }

    pub fn view<'a>(&'a self, shared: &'a Shared) -> Element<'a, Message> {
        self.0.view(shared)
    }
}
