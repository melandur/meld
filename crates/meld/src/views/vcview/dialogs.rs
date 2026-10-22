//! The version control view's dialogs, drawn as modals over the tab
//! (`vcdialogs.py`, `commit-dialog.ui`, `push-dialog.ui`, the remove
//! confirmation of `vcview.py` and a revert confirmation modelled on
//! `revert-dialog.ui`).

use std::path::{Path, PathBuf};

use iced::widget::{button, column, container, pick_list, row, scrollable, space, stack, text, text_editor};
use iced::{Background, Border, Element, Length, Theme};
use meld_vc::MessageHistory;

use super::Message;
use crate::shared::Shared;
use crate::ui;

/// An entry of the "Previous logs" chooser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryItem {
    pub index: usize,
    pub summary: String,
}

impl std::fmt::Display for HistoryItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.summary)
    }
}

pub struct CommitDialog {
    /// The selection being committed.
    pub selected: Vec<PathBuf>,
    /// Files to be committed; `None` while loading.
    pub files: Option<Result<Vec<String>, String>>,
    pub content: text_editor::Content,
    pub history: MessageHistory,
    pub history_choice: Option<HistoryItem>,
}

impl CommitDialog {
    pub fn new(selected: Vec<PathBuf>) -> Self {
        CommitDialog {
            selected,
            files: None,
            content: text_editor::Content::new(),
            history: MessageHistory::default(),
            history_choice: None,
        }
    }

    pub fn message(&self) -> String {
        self.content.text()
    }

    fn history_items(&self) -> Vec<HistoryItem> {
        self.history
            .summaries()
            .into_iter()
            .enumerate()
            .map(|(index, summary)| HistoryItem { index, summary })
            .collect()
    }

    /// Replaces the message with a previous one.
    pub fn choose_history(&mut self, item: HistoryItem) {
        if let Some(message) = self.history.messages().get(item.index) {
            self.content = text_editor::Content::with_text(message);
        }
        self.history_choice = Some(item);
    }
}

pub enum Dialog {
    Commit(Box<CommitDialog>),
    /// The push confirmation with the summary of commits to push
    /// (`None` while loading).
    Push(Option<Result<String, String>>),
    ConfirmRemove(Vec<PathBuf>),
    ConfirmRevert(Vec<PathBuf>),
    /// Files that could not be moved to the trash.
    ConfirmDelete(Vec<PathBuf>),
}

pub const COMMIT_EDITOR: &str = "vc-commit-message";

/// Dialog bodies have fixed widths: the dialog frame shrinks to them.
const COMMIT_MIN_WIDTH: f32 = 520.0;
const ALERT_WIDTH: f32 = 460.0;

fn card(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(palette.background.weak.color.scale_alpha(0.5))),
        border: Border::default().rounded(6).width(1).color(palette.background.strong.color),
        ..container::Style::default()
    }
}

/// A dialog frame of a fixed width: a title, a body and a row of buttons
/// (like [`ui::modal::dialog`], which stretches to the window width when
/// the body fills).
fn frame<'a>(
    title: &'a str,
    body: impl Into<Element<'a, Message>>,
    buttons: Vec<Element<'a, Message>>,
    width: f32,
) -> Element<'a, Message> {
    let mut button_row = row![space::horizontal()].spacing(8);
    for b in buttons {
        button_row = button_row.push(b);
    }
    container(
        column![text(title).size(18).font(ui::bold()), body.into(), button_row]
            .spacing(16)
            .width(Length::Fill),
    )
    .padding(20)
    .width(width + 40.0)
    .style(ui::popover)
    .into()
}

fn heading<'a>(label: &'a str) -> Element<'a, Message> {
    text(label).size(14).font(ui::bold()).into()
}

fn cancel_button<'a>() -> Element<'a, Message> {
    button(text("Cancel")).style(button::secondary).on_press(Message::CloseDialog).into()
}

fn file_list<'a>(lines: Vec<String>) -> Element<'a, Message> {
    let mut list = column![].spacing(2).width(Length::Fill);
    for line in lines {
        list = list.push(text(line).size(13).wrapping(text::Wrapping::None));
    }
    container(scrollable(container(list).padding([6, 10])).height(Length::Shrink))
        .max_height(200)
        .width(Length::Fill)
        .style(card)
        .into()
}

/// Paths shown relative to the view's location.
fn relative_names(paths: &[PathBuf], location: &Path) -> Vec<String> {
    paths
        .iter()
        .map(|p| match p.strip_prefix(location) {
            Ok(rel) if !rel.as_os_str().is_empty() => rel.display().to_string(),
            _ => p.display().to_string(),
        })
        .collect()
}

pub fn view<'a>(dialog: &'a Dialog, shared: &'a Shared, location: &Path) -> Element<'a, Message> {
    match dialog {
        Dialog::Commit(commit) => commit_view(commit, shared),
        Dialog::Push(summary) => {
            let mut body = column![
                text("The commits to be pushed are determined by your version control system.").size(14)
            ]
            .spacing(8)
            .width(ALERT_WIDTH);
            body = body.push(match summary {
                None => text("Checking for commits to push…").size(13).style(ui::dim_text),
                Some(Ok(s)) if s.is_empty() => text("No unpushed commits were found.").size(13).style(ui::dim_text),
                Some(Ok(s)) => text(s.clone()).size(14).font(ui::bold()),
                Some(Err(e)) => text(e.clone()).size(13).style(text::danger),
            });
            frame(
                "Push local commits to remote?",
                body,
                vec![
                    cancel_button(),
                    button(text("Push commits")).on_press(Message::ConfirmDialog).into(),
                ],
                ALERT_WIDTH,
            )
        }
        Dialog::ConfirmRemove(_) => frame(
            "Remove folder and all its files?",
            text(
                "This will remove all selected files and folders, and all files within any selected \
                 folders, from version control.",
            )
            .size(14)
            .width(ALERT_WIDTH),
            vec![
                cancel_button(),
                button(text("Remove"))
                    .style(button::danger)
                    .on_press(Message::ConfirmDialog)
                    .into(),
            ],
            ALERT_WIDTH,
        ),
        Dialog::ConfirmRevert(paths) => frame(
            "Revert changes to the selected files?",
            column![
                text("Changes made to the following files will be permanently lost:").size(14),
                file_list(relative_names(paths, location)),
            ]
            .spacing(10)
            .width(ALERT_WIDTH),
            vec![
                cancel_button(),
                button(text("Revert"))
                    .style(button::danger)
                    .on_press(Message::ConfirmDialog)
                    .into(),
            ],
            ALERT_WIDTH,
        ),
        Dialog::ConfirmDelete(paths) => frame(
            "Delete Permanently?",
            column![
                text(if paths.len() == 1 {
                    "This file can’t be put in the trash. Do you want to delete it permanently?"
                } else {
                    "These files can’t be put in the trash. Do you want to delete them permanently?"
                })
                .size(14),
                file_list(relative_names(paths, location)),
            ]
            .spacing(10)
            .width(ALERT_WIDTH),
            vec![
                cancel_button(),
                button(text("Delete Permanently"))
                    .style(button::danger)
                    .on_press(Message::ConfirmDialog)
                    .into(),
            ],
            ALERT_WIDTH,
        ),
    }
}

fn commit_view<'a>(commit: &'a CommitDialog, shared: &'a Shared) -> Element<'a, Message> {
    let files: Element<'a, Message> = match &commit.files {
        None => text("Looking for files to commit…").size(13).style(ui::dim_text).into(),
        Some(Ok(lines)) => file_list(lines.clone()),
        Some(Err(e)) => text(e.clone()).size(13).style(text::danger).into(),
    };

    let font_size = shared.mono_size();
    let padding = 8.0;
    // Wide enough for a standard 80 character commit message; monospace
    // glyphs are about 0.6em wide.
    let char_width = font_size * 0.6;
    let width = (80.0 * char_width + 2.0 * padding + 2.0).max(COMMIT_MIN_WIDTH);
    let editor = text_editor(&commit.content)
        .id(COMMIT_EDITOR)
        .font(shared.mono_font())
        .size(font_size)
        .padding(padding)
        .height(200)
        .wrapping(text::Wrapping::Word)
        .placeholder("Log message")
        .on_action(Message::CommitEdit);

    let settings = &shared.settings;
    let editor: Element<'a, Message> = if settings.vc_show_commit_margin && settings.vc_commit_margin > 0 {
        let x = padding + settings.vc_commit_margin as f32 * char_width;
        let line = container(space::vertical())
            .width(1)
            .height(Length::Fill)
            .style(|theme: &Theme| container::Style {
                background: Some(Background::Color(
                    theme.extended_palette().background.strong.color.scale_alpha(0.8),
                )),
                ..container::Style::default()
            });
        stack![editor, row![space::horizontal().width(x), line].height(Length::Fill)].into()
    } else {
        editor.into()
    };

    let history = commit.history_items();
    let previous = row![
        text("Previous logs:").size(14),
        pick_list(history, commit.history_choice.clone(), Message::CommitHistory)
            .placeholder("")
            .text_size(13)
            .width(Length::Fill),
    ]
    .spacing(12)
    .align_y(iced::Center);

    let body = column![
        heading("Changed Files"),
        files,
        heading("Log Message"),
        container(editor).style(card).width(Length::Fill),
        previous,
    ]
    .spacing(8)
    .width(Length::Fill);

    let can_commit = commit.files.is_some() && !commit.message().trim().is_empty();
    frame(
        "Commit Files",
        body,
        vec![
            cancel_button(),
            button(text("Commit"))
                .on_press_maybe(can_commit.then_some(Message::ConfirmDialog))
                .into(),
        ],
        width,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_relative_to_location() {
        let names = relative_names(
            &[PathBuf::from("/r/a/b.txt"), PathBuf::from("/r"), PathBuf::from("/x/y")],
            Path::new("/r"),
        );
        assert_eq!(names, ["a/b.txt", "/r", "/x/y"]);
    }

    #[test]
    fn history_choice_replaces_message() {
        let mut dialog = CommitDialog::new(Vec::new());
        dialog.history.prepend("Older message\n\nwith body");
        dialog.history.prepend("Newest message");
        let items = dialog.history_items();
        assert_eq!(items[1].summary, "Older message");
        dialog.choose_history(items[1].clone());
        assert_eq!(dialog.message(), "Older message\n\nwith body");
    }
}
