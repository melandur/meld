//! The new comparison page (`newdifftab.py`).

use std::path::PathBuf;

use iced::widget::{button, checkbox, column, container, row, space, text};
use iced::{Element, Length, Subscription, Task};

use crate::command::Command;
use crate::comparison::Comparison;
use crate::shared::Shared;
use crate::ui::{self, menu};
use crate::views::Event;
use crate::views::prlist::{self, PrList};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Folder,
    VersionControl,
    Git,
}

impl Kind {
    const ALL: [Kind; 4] = [Kind::File, Kind::Folder, Kind::VersionControl, Kind::Git];

    fn label(self) -> &'static str {
        match self {
            Kind::File => "File comparison",
            Kind::Folder => "Folder comparison",
            Kind::VersionControl => "Version control view",
            Kind::Git => "Git Compare",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Kind::File => "Compare two or three files, and merge changes between them",
            Kind::Folder => "Compare two or three folders, and copy files between them",
            Kind::VersionControl => "Browse a version controlled folder's changes and commit them",
            Kind::Git => "Compare two revisions of a Git repository, with why each change was made",
        }
    }

    fn picks_folders(self) -> bool {
        self != Kind::File
    }

    fn supports_blank(self) -> bool {
        matches!(self, Kind::File | Kind::Folder)
    }

    fn chooser_labels(self) -> [&'static str; 3] {
        match self {
            Kind::File => ["First file", "Second file", "Third file"],
            Kind::Folder => ["First folder", "Second folder", "Third folder"],
            Kind::VersionControl => ["Version control folder", "", ""],
            Kind::Git => ["Git repository", "", ""],
        }
    }
}

pub struct NewTab {
    kind: Option<Kind>,
    three_way: bool,
    /// Chosen paths per kind and pane.
    files: [Option<PathBuf>; 3],
    folders: [Option<PathBuf>; 3],
    vc: Option<PathBuf>,
    git: Option<PathBuf>,
    /// The folder the last file chooser was in.
    last_folder: Option<PathBuf>,
    /// Pull requests of the chosen Git repository.
    prs: PrList,
}

#[derive(Debug, Clone)]
pub enum Message {
    SelectKind(Kind),
    ThreeWay(bool),
    Choose(usize),
    Chosen(usize, Option<PathBuf>),
    Compare,
    Blank,
    Prs(prlist::Message),
}

impl NewTab {
    pub fn new(_shared: &Shared) -> Self {
        NewTab {
            kind: None,
            three_way: false,
            files: Default::default(),
            folders: Default::default(),
            vc: None,
            // The repository Meld was started in, if any: its pull requests
            // show as soon as Git Compare is picked
            git: std::env::current_dir().ok().filter(|d| in_repository(d)),
            last_folder: None,
            prs: PrList::default(),
        }
    }

    pub fn title(&self) -> String {
        "New comparison".into()
    }

    pub fn tooltip(&self) -> String {
        self.title()
    }

    pub fn unsaved(&self) -> Vec<String> {
        Vec::new()
    }

    fn pane_count(&self) -> usize {
        match self.kind {
            Some(Kind::File | Kind::Folder) if self.three_way => 3,
            Some(Kind::File | Kind::Folder) => 2,
            _ => 1,
        }
    }

    fn chosen(&self, pane: usize) -> Option<&PathBuf> {
        match self.kind? {
            Kind::File => self.files[pane].as_ref(),
            Kind::Folder => self.folders[pane].as_ref(),
            Kind::VersionControl => self.vc.as_ref().filter(|_| pane == 0),
            Kind::Git => self.git.as_ref().filter(|_| pane == 0),
        }
    }

    fn ready(&self) -> bool {
        self.kind.is_some() && (0..self.pane_count()).all(|i| self.chosen(i).is_some())
    }

    pub fn update(&mut self, message: Message, _shared: &mut Shared, events: &mut Vec<Event>) -> Task<Message> {
        match message {
            Message::SelectKind(kind) => {
                self.kind = Some(kind);
                if let (Kind::Git, Some(git)) = (kind, &self.git) {
                    return self.prs.set_location(git.clone()).map(Message::Prs);
                }
            }
            Message::ThreeWay(on) => self.three_way = on,
            Message::Choose(pane) => {
                let Some(kind) = self.kind else {
                    return Task::none();
                };
                let title = kind.chooser_labels()[pane];
                let mut dialog = rfd::AsyncFileDialog::new().set_title(title);
                let start = self
                    .chosen(pane)
                    .and_then(|p| p.parent().map(PathBuf::from))
                    .or_else(|| self.last_folder.clone())
                    .or_else(dirs_home);
                if let Some(start) = start {
                    dialog = dialog.set_directory(start);
                }
                return if kind.picks_folders() {
                    Task::perform(dialog.pick_folder(), move |h| {
                        Message::Chosen(pane, h.map(|h| h.path().to_path_buf()))
                    })
                } else {
                    Task::perform(dialog.pick_file(), move |h| {
                        Message::Chosen(pane, h.map(|h| h.path().to_path_buf()))
                    })
                };
            }
            Message::Chosen(pane, path) => {
                let Some(path) = path else {
                    return Task::none();
                };
                self.last_folder = path.parent().map(PathBuf::from);
                match self.kind {
                    Some(Kind::File) => self.files[pane] = Some(path),
                    Some(Kind::Folder) => self.folders[pane] = Some(path),
                    Some(Kind::VersionControl) => self.vc = Some(path),
                    Some(Kind::Git) => {
                        self.git = Some(path.clone());
                        return self.prs.set_location(path).map(Message::Prs);
                    }
                    None => {}
                }
            }
            Message::Compare => {
                if !self.ready() {
                    return Task::none();
                }
                let n = self.pane_count();
                let comparison = match self.kind {
                    Some(Kind::File) => {
                        Comparison::files(self.files[..n].to_vec())
                    }
                    Some(Kind::Folder) => Comparison::Folders {
                        paths: self.folders[..n].iter().flatten().cloned().collect(),
                        labels: Vec::new(),
                        auto_compare: false,
                    },
                    Some(Kind::VersionControl) => Comparison::VersionControl {
                        location: self.vc.clone().unwrap_or_default(),
                        auto_compare: false,
                    },
                    Some(Kind::Git) => Comparison::Git {
                        location: self.git.clone().unwrap_or_default(),
                        base: None,
                        target: None,
                        pr: None,
                    },
                    None => return Task::none(),
                };
                events.push(Event::Replace(comparison));
            }
            Message::Prs(message) => {
                let (task, picked) = self.prs.update(message);
                if let (Some(pr), Some(location)) = (picked, &self.git) {
                    events.push(Event::Replace(Comparison::Git {
                        location: location.clone(),
                        base: None,
                        target: None,
                        pr: Some(pr),
                    }));
                    return Task::none();
                }
                return task.map(Message::Prs);
            }
            Message::Blank => {
                let n = self.pane_count();
                let comparison = match self.kind {
                    Some(Kind::File) => Comparison::files(vec![None; n]),
                    Some(Kind::Folder) => Comparison::Folders {
                        paths: vec![PathBuf::new(); n],
                        labels: Vec::new(),
                        auto_compare: false,
                    },
                    _ => return Task::none(),
                };
                events.push(Event::Replace(comparison));
            }
        }
        Task::none()
    }

    pub fn command(&mut self, command: Command, shared: &mut Shared, events: &mut Vec<Event>) -> Task<Message> {
        match command {
            Command::Activate => self.update(Message::Compare, shared, events),
            _ => Task::none(),
        }
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
        let mut kinds = column![].spacing(8).width(300);
        for kind in Kind::ALL {
            let selected = self.kind == Some(kind);
            kinds = kinds.push(
                button(
                    column![
                        text(kind.label()).size(16).font(ui::bold()),
                        text(kind.description()).size(13).style(ui::dim_text),
                    ]
                    .spacing(4),
                )
                .width(Length::Fill)
                .padding([10, 14])
                .style(ui::tab_button(selected))
                .on_press(Message::SelectKind(kind)),
            );
        }

        let choosers: Element<'_, Message> = match self.kind {
            None => container(text("Choose a comparison type").style(ui::dim_text))
                .center(Length::Fill)
                .into(),
            Some(kind) => {
                let labels = kind.chooser_labels();
                let mut col = column![].spacing(10).width(Length::Fill);
                for (pane, label) in labels.iter().enumerate().take(self.pane_count()) {
                    let current = self
                        .chosen(pane)
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "(None)".into());
                    col = col.push(
                        row![
                            text(*label).size(14).width(170),
                            button(text(current).size(14).wrapping(text::Wrapping::None))
                                .width(Length::Fill)
                                .style(button::secondary)
                                .on_press(Message::Choose(pane)),
                        ]
                        .spacing(10)
                        .align_y(iced::Center),
                    );
                }
                if matches!(kind, Kind::File | Kind::Folder) {
                    col = col.push(checkbox(self.three_way).label("3-way comparison").on_toggle(Message::ThreeWay));
                }
                if kind == Kind::Git {
                    if self.git.is_some() {
                        col = col.push(container(self.prs.view().map(Message::Prs)).height(Length::Fill));
                    } else {
                        col = col.push(
                            text("Choose a repository to compare two of its revisions, or to review one of its pull requests")
                                .size(13)
                                .style(ui::dim_text),
                        );
                        col = col.push(space::vertical());
                    }
                } else {
                    col = col.push(space::vertical());
                }
                let mut buttons = row![space::horizontal()].spacing(10);
                if kind.supports_blank() {
                    buttons = buttons.push(
                        button(text("Blank comparison"))
                            .style(button::secondary)
                            .on_press(Message::Blank),
                    );
                }
                buttons = buttons.push(button(text("Compare")).on_press_maybe(self.ready().then_some(Message::Compare)));
                col.push(buttons).into()
            }
        };

        container(
            container(row![kinds, ui::separator(true), choosers].spacing(20).height(480)).max_width(1000),
        )
        .padding(30)
        .center(Length::Fill)
        .into()
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Whether `dir` is in a Git working tree (has a `.git` folder or file, or
/// a folder above it has).
fn in_repository(dir: &std::path::Path) -> bool {
    dir.ancestors().any(|d| d.join(".git").exists())
}
