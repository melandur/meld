//! The Base and Target buttons (port of `meld/ui/revisionpicker.py`): each
//! shows the picked revision and opens a searchable list of every known
//! revision: HEAD, the staged/unstaged/working-tree versions of each
//! worktree, branches, remote branches, tags and recent commits. Typing
//! filters the list, offers the typed text as a ref or SHA, and after a
//! pause searches the whole history ([`RepositoryInfo::search_commits`]).
//!
//! When the repository has a remote on GitHub or GitLab, a Pull requests
//! section ([`PrSection`]) lists its pull or merge requests, with chips to
//! filter them by state and to pick the remote; picking one reviews it.
//!
//! [`RepositoryInfo::search_commits`]: meld_ailens::RepositoryInfo::search_commits

use std::collections::HashSet;
use std::time::Duration;

use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Element, Length, Theme};
use meld_ailens::picker::{EntryKind, filter_entries};
use meld_ailens::{PickerEntry, PickerSide, Revision, RevisionKind};
use meld_forge::StateFilter;

use super::popover::popover;
use crate::ui;
use crate::ui::ellipsis::{Mode, ellipsized};

/// Most rows the list shows at once; typing narrows it down.
pub const MAX_ROWS: usize = 400;

/// How long typing must pause before searching the whole history.
pub const SEARCH_DELAY: Duration = Duration::from_millis(300);

/// The widest a picker's label gets (`set_max_width_chars(28)`).
const LABEL_WIDTH: f32 = 220.0;

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
    /// Pull requests, when the repository has a forge remote.
    prs: Option<PrSection>,
}

/// The pull requests a picker lists.
#[derive(Debug, Clone, Default)]
pub struct PrSection {
    /// Labels of the forge remotes to choose from.
    pub remotes: Vec<String>,
    pub selected_remote: Option<usize>,
    pub filter: StateFilter,
    pub entries: Vec<PickerEntry>,
    /// Found by searching the forge for the query.
    pub search_results: Vec<PickerEntry>,
    /// e.g. "Loading…" or why they couldn't be listed.
    pub status: String,
}

/// What the owner should do after a picker message.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    None,
    /// Search history for this text once the delay passed (generation).
    ScheduleSearch(u64),
    /// The user picked an entry.
    Picked(PickerEntry),
    /// List the pull requests in this state.
    PrFilter(StateFilter),
    /// List the pull requests of this remote.
    PrRemote(usize),
}

#[derive(Debug, Clone)]
pub enum Message {
    Query(String),
    Submit,
    Pick(usize),
    Move(isize),
    PrFilter(StateFilter),
    PrRemote(usize),
}

impl Picker {
    pub fn new(side: PickerSide) -> Self {
        Picker {
            side,
            label: tr!("Choose…").into(),
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
            prs: None,
        }
    }

    pub fn title(&self) -> &'static str {
        match self.side {
            PickerSide::Base => tr!("Base"),
            PickerSide::Target => tr!("Target"),
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

    /// Shows (or hides) the Pull requests section.
    pub fn set_prs(&mut self, prs: Option<PrSection>) {
        self.prs = prs;
        self.refilter_keeping_selection();
    }

    #[cfg(test)]
    pub fn prs(&self) -> Option<&PrSection> {
        self.prs.as_ref()
    }

    /// Pull requests found by searching the forge for `query`; ignored
    /// once stale.
    pub fn set_pr_search_results(&mut self, query: &str, entries: Vec<PickerEntry>) {
        if query != self.query.trim() {
            return;
        }
        if let Some(prs) = &mut self.prs {
            let known: HashSet<&Revision> = prs.entries.iter().map(|e| &e.revision).collect();
            prs.search_results = entries
                .into_iter()
                .filter(|e| !known.contains(&e.revision))
                .collect();
        }
        self.refilter_keeping_selection();
    }

    fn refilter_keeping_selection(&mut self) {
        let selected = self.selected;
        self.refilter();
        self.selected = selected.min(self.rows.len().saturating_sub(1));
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
        if let Some(prs) = &mut self.prs {
            prs.search_results.clear();
        }
        self.refilter();
    }

    fn refilter(&mut self) {
        // Pull requests come before the commits
        let split = self
            .entries
            .iter()
            .position(|e| e.kind == EntryKind::Commit)
            .unwrap_or(self.entries.len());
        let prs = self
            .prs
            .iter()
            .flat_map(|p| p.entries.iter().chain(&p.search_results));
        let all: Vec<PickerEntry> = self.entries[..split]
            .iter()
            .chain(prs)
            .chain(&self.entries[split..])
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
            tr!(
                "Showing {shown} of {total}; type to narrow down",
                shown = MAX_ROWS,
                total = self.total
            )
        } else {
            self.status.clone()
        }
    }

    pub fn update(&mut self, message: Message) -> Outcome {
        match message {
            Message::Query(query) => {
                self.query = query;
                self.search_results.clear();
                if let Some(prs) = &mut self.prs {
                    prs.search_results.clear();
                }
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
            Message::PrFilter(filter) => Outcome::PrFilter(filter),
            Message::PrRemote(index) => Outcome::PrRemote(index),
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
            scrollable::AbsoluteOffset {
                x: None,
                y: Some(y.max(0.0)),
            },
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
            // Long refs and commit subjects keep both ends
            ellipsized(self.label.as_str(), Mode::Middle)
                .size(14)
                .max_width(LABEL_WIDTH),
            text("▾").size(12),
        ]
        .spacing(6)
        .align_y(iced::Center);
        let anchor = button(face)
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

    fn popup<'a, M: Clone + 'a>(
        &'a self,
        map: impl Fn(Message) -> M + Copy + 'a,
    ) -> Element<'a, M> {
        let placeholder = if self.prs.is_some() {
            tr!("Branch, tag, commit message, author, SHA or pull request")
        } else {
            tr!("Branch, tag, commit message, author or SHA")
        };
        let input = text_input(placeholder, &self.query)
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
                    container(
                        text(section_label(entry.section.as_str()))
                            .size(13)
                            .font(ui::bold()),
                    )
                    .padding(iced::Padding {
                        top: if section.is_some() { 8.0 } else { 2.0 },
                        bottom: 2.0,
                        left: 6.0,
                        right: 6.0,
                    }),
                );
                section = Some(entry.section.as_str());
            }
            list = list.push(entry_row(
                entry,
                index == self.selected,
                map(Message::Pick(index)),
            ));
        }
        if self.rows.is_empty() {
            list = list.push(
                container(
                    text(tr!("No matching revisions"))
                        .size(13)
                        .style(ui::dim_text),
                )
                .padding(20)
                .center_x(Length::Fill),
            );
        }

        let mut header = column![input].spacing(6);
        if let Some(prs) = &self.prs {
            let filters = crate::forge::chips(
                StateFilter::ALL
                    .iter()
                    .map(|f| {
                        (
                            crate::i18n::pgettext("pull request state", f.label()).to_string(),
                            map(Message::PrFilter(*f)),
                        )
                    })
                    .collect(),
                StateFilter::ALL.iter().position(|f| *f == prs.filter),
            );
            let mut line = row![
                text(tr!("Pull requests")).size(12).style(ui::dim_text),
                filters
            ]
            .spacing(8)
            .align_y(iced::Center);
            if prs.remotes.len() > 1 {
                let remotes = prs
                    .remotes
                    .iter()
                    .enumerate()
                    .map(|(i, label)| (label.clone(), map(Message::PrRemote(i))))
                    .collect();
                line = line.push(crate::forge::chips(remotes, prs.selected_remote));
            }
            header = header.push(line);
            if !prs.status.is_empty() {
                header = header.push(
                    ellipsized(prs.status.as_str(), Mode::End)
                        .size(11)
                        .style(ui::dim_text)
                        .width(Length::Fill),
                );
            }
        }
        let mut body = column![
            header,
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
            .width(if self.prs.as_ref().is_some_and(|p| p.remotes.len() > 1) {
                640
            } else {
                480
            })
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
        "pull-request" => "↳",
        _ => match entry.kind {
            EntryKind::Commit => "•",
            _ => "·",
        },
    }
}

/// A row's tooltip: its label and description in full.
pub fn entry_tooltip(entry: &PickerEntry) -> String {
    if entry.description.is_empty() {
        entry.label.clone()
    } else {
        format!("{}\n{}", entry.label, entry.description)
    }
}

fn entry_row<'a, M: Clone + 'a>(
    entry: &'a PickerEntry,
    selected: bool,
    on_press: M,
) -> Element<'a, M> {
    let mut texts = column![
        ellipsized(entry.label.as_str(), Mode::End)
            .size(14)
            .width(Length::Fill)
    ];
    if !entry.description.is_empty() {
        texts = texts.push(
            ellipsized(entry.description.as_str(), Mode::End)
                .size(11)
                .style(ui::dim_text)
                .width(Length::Fill),
        );
    }
    let row = button(
        row![
            text(icon(entry)).size(14).width(18).center(),
            texts.width(Length::Fill)
        ]
        .spacing(8)
        .align_y(iced::Center),
    )
    .width(Length::Fill)
    .padding([3, 6])
    .style(move |theme: &Theme, status| {
        let mut style = ui::flat_button(theme, status);
        if selected {
            style.background = Some(
                theme
                    .extended_palette()
                    .primary
                    .weak
                    .color
                    .scale_alpha(0.5)
                    .into(),
            );
        }
        style
    })
    .on_press(on_press);
    iced::widget::tooltip(
        row,
        container(text(entry_tooltip(entry)).size(12))
            .max_width(480)
            .padding([4, 8])
            .style(ui::tooltip_box),
        iced::widget::tooltip::Position::FollowCursor,
    )
    .delay(Duration::from_millis(700))
    .into()
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
    let worktree = revision
        .worktree
        .as_ref()
        .and_then(|w| w.file_name())
        .map(|name| name.to_string_lossy());
    match (revision.kind, worktree) {
        (RevisionKind::Index, Some(name)) => tr!("Staged ({worktree})", worktree = name),
        (RevisionKind::Index, None) => tr!("Staged").into(),
        (RevisionKind::WorkingTree, Some(name)) => {
            tr!("Working Tree ({worktree})", worktree = name)
        }
        (RevisionKind::WorkingTree, None) => tr!("Working Tree").into(),
        (RevisionKind::Commit, _) => revision.ref_name.clone().unwrap_or_else(|| "HEAD".into()),
    }
}

/// A section heading of the list: the ones `meld_ailens::picker` and the
/// pull request sections name, translated.
pub fn section_label(section: &str) -> std::borrow::Cow<'_, str> {
    let known = match section {
        "Uncommitted" => tr!("Uncommitted"),
        "Branches" => tr!("Branches"),
        "Remote branches" => tr!("Remote branches"),
        "Tags" => tr!("Tags"),
        "Commits" => tr!("Commits"),
        "Typed" => tr!("Typed"),
        "Pull requests" => tr!("Pull requests"),
        "Merge requests" => tr!("Merge requests"),
        _ => {
            return match section.strip_prefix("Worktree ") {
                Some(name) => tr!("Worktree {name}", name = name).into(),
                None => section.into(),
            };
        }
    };
    known.into()
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
        assert!(
            p.rows()
                .iter()
                .all(|e| e.search_text.contains("number") && e.search_text.contains("44"))
        );
        assert!(p.rows().iter().any(|e| e.label == "Change number 449"));
        assert_eq!(p.status_text(), "");
        // Something that could be a SHA is offered first
        assert_eq!(
            p.update(Message::Query("c0ffee".into())),
            Outcome::ScheduleSearch(2)
        );
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
                commits: &[
                    commit(&"f".repeat(40), "An old change"),
                    commit(&format!("{:040x}", 3), "Change number 3"),
                ],
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

    fn pr_entry(number: u64, title: &str) -> PickerEntry {
        let remote = meld_forge::remote::parse_remote("github", "git@github.com:o/r.git").unwrap();
        let pr = meld_forge::PullRequest {
            number,
            title: title.into(),
            ..Default::default()
        };
        super::super::review::picker_entry(&remote, &pr, 0.0)
    }

    #[test]
    fn pull_requests_are_listed_before_commits_and_filtered() {
        let mut p = picker();
        p.set_prs(Some(PrSection {
            remotes: vec!["github · o/r".into()],
            selected_remote: Some(0),
            entries: vec![pr_entry(12, "Add reviews"), pr_entry(3, "Fix typo")],
            ..Default::default()
        }));
        let kinds: Vec<EntryKind> = p.rows().iter().map(|e| e.kind).take(4).collect();
        assert_eq!(
            kinds,
            [
                EntryKind::Head,
                EntryKind::Branch,
                EntryKind::PullRequest,
                EntryKind::PullRequest
            ]
        );
        assert_eq!(p.rows()[4].kind, EntryKind::Commit);
        p.update(Message::Query("reviews".into()));
        let labels: Vec<&str> = p.rows().iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["Use “reviews”", "#12 Add reviews"]);
        // Forge search results join, without repeating known ones
        p.set_pr_search_results(
            "reviews",
            vec![pr_entry(12, "Add reviews"), pr_entry(40, "More reviews")],
        );
        let labels: Vec<&str> = p.rows().iter().map(|e| e.label.as_str()).collect();
        assert_eq!(
            labels,
            ["Use “reviews”", "#12 Add reviews", "#40 More reviews"]
        );
        assert_eq!(
            p.update(Message::PrFilter(StateFilter::Merged)),
            Outcome::PrFilter(StateFilter::Merged)
        );
        assert_eq!(p.update(Message::PrRemote(1)), Outcome::PrRemote(1));
        assert!(p.prs().is_some());
        p.set_prs(None);
        assert!(p.rows().iter().all(|e| e.kind != EntryKind::PullRequest));
    }

    #[test]
    fn rows_tooltips_show_label_and_description() {
        let p = picker();
        let head = &p.rows()[0];
        assert_eq!(head.label, "HEAD");
        assert_eq!(entry_tooltip(head), format!("HEAD\n{}", head.description));
        let mut bare = head.clone();
        bare.description.clear();
        assert_eq!(entry_tooltip(&bare), "HEAD");
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
