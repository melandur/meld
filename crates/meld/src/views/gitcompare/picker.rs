//! The Base and Target buttons (port of `meld/ui/revisionpicker.py`): each
//! shows the picked revision and opens a searchable list of every known
//! revision: HEAD, the staged/unstaged/working-tree versions of each
//! worktree, branches, remote branches, tags and recent commits. Typing
//! filters the list, offers the typed text as a ref or SHA, and after a
//! pause searches the whole history ([`RepositoryInfo::search_commits`]).
//!
//! [`RepositoryInfo::search_commits`]: meld_ailens::RepositoryInfo::search_commits

use std::collections::HashSet;
use std::time::Duration;

use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Element, Length, Theme};
use meld_ailens::picker::{EntryKind, filter_entries};
use meld_ailens::{PickerEntry, PickerSide, Revision, RevisionKind};

use super::popover::popover;
use crate::ui;

/// Most rows the list shows at once; typing narrows it down.
pub const MAX_ROWS: usize = 400;

/// How long typing must pause before searching the whole history.
pub const SEARCH_DELAY: Duration = Duration::from_millis(300);

pub struct Picker {
    pub side: PickerSide,
    label: String,
    tooltip: String,
    entries: Vec<PickerEntry>,
    search_results: Vec<PickerEntry>,
    query: String,
    /// What the list shows for the query, at most [`MAX_ROWS`].
    rows: Vec<PickerEntry>,
    total: usize,
    selected: usize,
    status: String,
    /// Bumped on every keystroke; a pending search only runs if it is
    /// still the newest.
    search_generation: u64,
    pub input_id: iced::widget::Id,
    list_id: iced::widget::Id,
}

/// What the owner should do after a picker message.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    None,
    /// Search history for this text once the delay passed (generation).
    ScheduleSearch(u64),
    /// The user picked an entry.
    Picked(PickerEntry),
}

#[derive(Debug, Clone)]
pub enum Message {
    Query(String),
    Submit,
    Pick(usize),
    Move(isize),
}

impl Picker {
    pub fn new(side: PickerSide) -> Self {
        Picker {
            side,
            label: "Choose…".into(),
            tooltip: String::new(),
            entries: Vec::new(),
            search_results: Vec::new(),
            query: String::new(),
            rows: Vec::new(),
            total: 0,
            selected: 0,
            status: String::new(),
            search_generation: 0,
            input_id: iced::widget::Id::unique(),
            list_id: iced::widget::Id::unique(),
        }
    }

    pub fn title(&self) -> &'static str {
        match self.side {
            PickerSide::Base => "Base",
            PickerSide::Target => "Target",
        }
    }

    pub fn set_label(&mut self, label: impl Into<String>, tooltip: Option<String>) {
        self.label = label.into();
        self.tooltip = tooltip.unwrap_or_else(|| self.label.clone());
    }

    pub fn set_entries(&mut self, entries: Vec<PickerEntry>) {
        self.entries = entries;
        self.search_results.clear();
        self.refilter();
    }

    /// Commits found by searching for `query`; ignored once stale.
    pub fn set_search_results(&mut self, query: &str, entries: Vec<PickerEntry>) {
        if query != self.query.trim() {
            return;
        }
        let known: HashSet<&Revision> = self.entries.iter().map(|e| &e.revision).collect();
        self.search_results = entries
            .into_iter()
            .filter(|e| !known.contains(&e.revision))
            .collect();
        self.refilter();
    }

    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn search_generation(&self) -> u64 {
        self.search_generation
    }

    #[cfg(test)]
    pub fn rows(&self) -> &[PickerEntry] {
        &self.rows
    }

    /// Called when the popup opens: start from the full list.
    pub fn reset(&mut self) {
        self.query.clear();
        self.search_results.clear();
        self.refilter();
    }

    fn refilter(&mut self) {
        let all: Vec<PickerEntry> = self
            .entries
            .iter()
            .chain(&self.search_results)
            .cloned()
            .collect();
        let mut rows = filter_entries(&all, &self.query);
        self.total = rows.len();
        rows.truncate(MAX_ROWS);
        self.rows = rows;
        self.selected = 0;
    }

    /// The status line under the list.
    pub fn status_text(&self) -> String {
        if self.total > MAX_ROWS {
            format!("Showing {MAX_ROWS} of {}; type to narrow down", self.total)
        } else {
            self.status.clone()
        }
    }

    pub fn update(&mut self, message: Message) -> Outcome {
        match message {
            Message::Query(query) => {
                self.query = query;
                self.search_results.clear();
                self.refilter();
                self.search_generation += 1;
                if self.query.trim().is_empty() {
                    Outcome::None
                } else {
                    Outcome::ScheduleSearch(self.search_generation)
                }
            }
            Message::Submit => match self.rows.get(self.selected).or(self.rows.first()) {
                Some(entry) => Outcome::Picked(entry.clone()),
                None => Outcome::None,
            },
            Message::Pick(index) => match self.rows.get(index) {
                Some(entry) => Outcome::Picked(entry.clone()),
                None => Outcome::None,
            },
            Message::Move(step) => {
                if !self.rows.is_empty() {
                    let last = self.rows.len() as isize - 1;
                    self.selected = (self.selected as isize + step).clamp(0, last) as usize;
                }
                Outcome::None
            }
        }
    }

    /// Scrolls the list so that the selected row is visible.
    pub fn scroll_to_selected<T>(&self) -> iced::Task<T> {
        // Rows are about 38 px high, section headings 24; close enough to
        // keep the selection in view.
        let headings = self.rows[..self.selected.min(self.rows.len())]
            .windows(2)
            .filter(|w| w[0].section != w[1].section)
            .count();
        let y = self.selected as f32 * 38.0 + headings as f32 * 24.0 - 120.0;
        iced::widget::operation::scroll_to(
            self.list_id.clone(),
            scrollable::AbsoluteOffset { x: None, y: Some(y.max(0.0)) },
        )
    }

    /// The button, with its popup when `open`.
    pub fn view<'a, M: Clone + 'a>(
        &'a self,
        open: bool,
        on_toggle: M,
        on_dismiss: M,
        map: impl Fn(Message) -> M + Copy + 'a,
    ) -> Element<'a, M> {
        let face = row![
            text(self.title()).size(13).style(ui::dim_text),
            text(self.label.as_str())
                .size(14)
                .wrapping(text::Wrapping::None),
            text("▾").size(12),
        ]
        .spacing(6)
        .align_y(iced::Center);
        let anchor = button(container(face).max_width(260).clip(true))
            .padding([4, 10])
            .style(picker_button(open))
            .on_press(on_toggle);
        // No tooltip while the list is open: it would cover the search field
        let anchor: Element<'a, M> = if open {
            anchor.into()
        } else {
            iced::widget::tooltip(
                anchor,
                container(text(self.tooltip.as_str()).size(12))
                    .padding([4, 8])
                    .style(ui::tooltip_box),
                iced::widget::tooltip::Position::Bottom,
            )
            .delay(Duration::from_millis(600))
            .into()
        };
        let popup = open.then(|| self.popup(map));
        popover(anchor, popup, on_dismiss)
            .on_key(move |named| match named {
                iced::keyboard::key::Named::ArrowDown => Some(map(Message::Move(1))),
                iced::keyboard::key::Named::ArrowUp => Some(map(Message::Move(-1))),
                iced::keyboard::key::Named::PageDown => Some(map(Message::Move(10))),
                iced::keyboard::key::Named::PageUp => Some(map(Message::Move(-10))),
                _ => None,
            })
            .into()
    }

    fn popup<'a, M: Clone + 'a>(&'a self, map: impl Fn(Message) -> M + Copy + 'a) -> Element<'a, M> {
        let input = text_input("Branch, tag, commit message, author or SHA", &self.query)
            .id(self.input_id.clone())
            .on_input(move |q| map(Message::Query(q)))
            .on_submit(map(Message::Submit))
            .padding([6, 8])
            .size(14);

        let mut list = column![].spacing(0).width(Length::Fill);
        let mut section: Option<&str> = None;
        for (index, entry) in self.rows.iter().enumerate() {
            if section != Some(entry.section.as_str()) {
                list = list.push(
                    container(text(entry.section.as_str()).size(13).font(ui::bold()))
                        .padding(iced::Padding {
                            top: if section.is_some() { 8.0 } else { 2.0 },
                            bottom: 2.0,
                            left: 6.0,
                            right: 6.0,
                        }),
                );
                section = Some(entry.section.as_str());
            }
            list = list.push(entry_row(entry, index == self.selected).on_press(map(Message::Pick(index))));
        }
        if self.rows.is_empty() {
            list = list.push(
                container(text("No matching revisions").size(13).style(ui::dim_text))
                    .padding(20)
                    .center_x(Length::Fill),
            );
        }

        let mut body = column![
            input,
            scrollable(list)
                .spacing(2)
                .id(self.list_id.clone())
                .height(Length::Shrink)
                .width(Length::Fill),
        ]
        .spacing(6);
        let status = self.status_text();
        if !status.is_empty() {
            body = body.push(text(status).size(11).style(ui::dim_text));
        }
        container(body)
            .width(480)
            .max_height(560)
            .padding(8)
            .style(ui::popover)
            .into()
    }
}

/// A glyph for an entry's freedesktop icon.
pub fn icon(entry: &PickerEntry) -> &'static str {
    match entry.icon_name.as_str() {
        "go-home-symbolic" => "⌂",
        "document-edit-symbolic" => "✎",
        "list-add-symbolic" => "+",
        "object-select-symbolic" => "✓",
        "media-playlist-consecutive-symbolic" => "⑂",
        "network-server-symbolic" => "☁",
        "bookmark-new-symbolic" => "◆",
        "system-search-symbolic" => "⌕",
        _ => match entry.kind {
            EntryKind::Commit => "•",
            _ => "·",
        },
    }
}

fn entry_row<'a, M: Clone + 'a>(entry: &'a PickerEntry, selected: bool) -> button::Button<'a, M> {
    let mut texts = column![text(entry.label.as_str()).size(14).wrapping(text::Wrapping::None)];
    if !entry.description.is_empty() {
        texts = texts.push(
            text(entry.description.as_str())
                .size(11)
                .style(ui::dim_text)
                .wrapping(text::Wrapping::None),
        );
    }
    button(
        row![
            text(icon(entry)).size(14).width(18).center(),
            container(texts).clip(true).width(Length::Fill)
        ]
        .spacing(8)
        .align_y(iced::Center),
    )
    .width(Length::Fill)
    .padding([3, 6])
    .style(move |theme: &Theme, status| {
        let mut style = ui::flat_button(theme, status);
        if selected {
            style.background = Some(theme.extended_palette().primary.weak.color.scale_alpha(0.5).into());
        }
        style
    })
}

fn picker_button(open: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let palette = theme.extended_palette();
        let mut style = ui::flat_button(theme, status);
        style.border = iced::Border::default()
            .rounded(6)
            .width(1)
            .color(palette.background.strong.color);
        if open {
            style.background = Some(palette.background.strong.color.scale_alpha(0.6).into());
        }
        style
    }
}

/// The picker's label before the comparison resolved it (`_describe_revision`).
pub fn describe_revision(revision: &Revision) -> String {
    let suffix = revision
        .worktree
        .as_ref()
        .and_then(|w| w.file_name())
        .map(|name| format!(" ({})", name.to_string_lossy()))
        .unwrap_or_default();
    match revision.kind {
        RevisionKind::Index => format!("Staged{suffix}"),
        RevisionKind::WorkingTree => format!("Working Tree{suffix}"),
        RevisionKind::Commit => revision.ref_name.clone().unwrap_or_else(|| "HEAD".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meld_ailens::picker::{PickerSources, build_entries};
    use meld_ailens::{GitCommit, GitRef, GitRefType};

    fn commit(sha: &str, subject: &str) -> GitCommit {
        GitCommit {
            sha: sha.into(),
            subject: subject.into(),
            ..Default::default()
        }
    }

    fn picker() -> Picker {
        let refs = vec![GitRef {
            name: "main".into(),
            full_name: "refs/heads/main".into(),
            sha: "a".repeat(40),
            kind: GitRefType::Branch,
            current: true,
            subject: "Tip".into(),
            date: String::new(),
            worktree: None,
        }];
        let commits: Vec<GitCommit> = (0..450)
            .map(|i| commit(&format!("{i:040x}"), &format!("Change number {i}")))
            .collect();
        let head = commit(&"a".repeat(40), "Tip");
        let entries = build_entries(
            &PickerSources {
                refs: &refs,
                commits: &commits,
                head: Some(&head),
                head_branch: Some("main"),
                now: Some(0.0),
                ..Default::default()
            },
            PickerSide::Base,
        );
        let mut p = Picker::new(PickerSide::Base);
        p.set_entries(entries);
        p
    }

    #[test]
    fn the_list_is_capped_with_a_status() {
        let p = picker();
        assert_eq!(p.rows().len(), MAX_ROWS);
        assert_eq!(p.status_text(), "Showing 400 of 452; type to narrow down");
    }

    #[test]
    fn typing_filters_and_schedules_a_search() {
        let mut p = picker();
        let outcome = p.update(Message::Query("number 44".into()));
        assert_eq!(outcome, Outcome::ScheduleSearch(1));
        // Every word matches (labels or SHAs); text with spaces can't be a ref
        assert!(p.rows().iter().all(|e| e.kind == EntryKind::Commit));
        assert!(p.rows().iter().all(|e| e.search_text.contains("number") && e.search_text.contains("44")));
        assert!(p.rows().iter().any(|e| e.label == "Change number 449"));
        assert_eq!(p.status_text(), "");
        // Something that could be a SHA is offered first
        assert_eq!(p.update(Message::Query("c0ffee".into())), Outcome::ScheduleSearch(2));
        assert_eq!(p.rows()[0].kind, EntryKind::Typed);
        assert_eq!(p.rows()[0].revision, Revision::commit("c0ffee"));
        assert_eq!(p.update(Message::Query(" ".into())), Outcome::None);
    }

    #[test]
    fn search_results_are_merged_unless_stale_or_known() {
        let mut p = picker();
        p.update(Message::Query("old".into()));
        let old = build_entries(
            &PickerSources {
                commits: &[commit(&"f".repeat(40), "An old change"), commit(&format!("{:040x}", 3), "Change number 3")],
                now: Some(0.0),
                ..Default::default()
            },
            PickerSide::Target,
        );
        p.set_search_results("older", old.clone());
        assert!(p.rows().iter().all(|e| e.label != "An old change"));
        p.set_search_results("old", old);
        let labels: Vec<&str> = p.rows().iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["Use “old”", "An old change"]);
    }

    #[test]
    fn enter_and_arrows_pick_rows() {
        let mut p = picker();
        p.update(Message::Query("main".into()));
        // HEAD (on main) first, then the branch
        let Outcome::Picked(entry) = p.update(Message::Submit) else {
            panic!("nothing picked");
        };
        assert_eq!(entry.label, "HEAD");
        p.update(Message::Move(1));
        let Outcome::Picked(entry) = p.update(Message::Submit) else {
            panic!("nothing picked");
        };
        assert_eq!(entry.revision, Revision::commit("refs/heads/main"));
        p.update(Message::Query("Change number 1".into()));
        p.update(Message::Move(1));
        p.update(Message::Move(-5));
        p.update(Message::Move(2));
        let Outcome::Picked(entry) = p.update(Message::Submit) else {
            panic!("nothing picked");
        };
        assert_eq!(entry, p.rows()[2]);
        assert_eq!(p.update(Message::Pick(10_000)), Outcome::None);
    }

    #[test]
    fn revisions_are_described_before_they_resolve() {
        assert_eq!(describe_revision(&Revision::commit("v1.0")), "v1.0");
        assert_eq!(describe_revision(&Revision::index(None)), "Staged");
        assert_eq!(
            describe_revision(&Revision::working_tree(Some("/tmp/wt-2".into()))),
            "Working Tree (wt-2)"
        );
    }
}
