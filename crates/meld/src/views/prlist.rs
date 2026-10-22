//! The list of pull requests on the new comparison page, under Git
//! Compare: the repository's forge remotes, a state filter, a search, and
//! a row per pull request (number, title, author, state, last update).
//! Picking one opens it in Git Compare.

use std::path::PathBuf;
use std::time::Duration;

use iced::widget::{button, column, container, row, scrollable, space, text, text_input};
use iced::{Element, Length, Task, Theme};
use meld_ailens::CancelToken;
use meld_forge::{PullRequest, StateFilter};

use crate::comparison::PrSpec;
use crate::forge::{self, Remotes};
use crate::ui;

/// How many pull requests a page lists.
pub const PAGE: usize = 50;
const SEARCH_DELAY: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, PartialEq)]
enum Status {
    Idle,
    Loading(String),
    Error(String),
}

pub struct PrList {
    location: Option<PathBuf>,
    remotes: Option<Remotes>,
    filter: StateFilter,
    search: String,
    limit: usize,
    prs: Vec<PullRequest>,
    status: Status,
    /// Newest request; older answers are ignored.
    generation: u64,
    search_generation: u64,
    cancel: Option<CancelToken>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Remotes(u64, Result<Remotes, String>),
    Listed(u64, Result<Vec<PullRequest>, String>),
    Remote(usize),
    Filter(StateFilter),
    Search(String),
    SearchDue(u64),
    More,
    Retry,
    Open(u64),
}

impl Default for PrList {
    fn default() -> Self {
        PrList {
            location: None,
            remotes: None,
            filter: StateFilter::Open,
            search: String::new(),
            limit: PAGE,
            prs: Vec::new(),
            status: Status::Idle,
            generation: 0,
            search_generation: 0,
            cancel: None,
        }
    }
}

impl Drop for PrList {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
    }
}

impl PrList {
    fn restart(&mut self) -> (u64, CancelToken) {
        if let Some(old) = self.cancel.take() {
            old.cancel();
        }
        self.generation += 1;
        let cancel = CancelToken::new();
        self.cancel = Some(cancel.clone());
        (self.generation, cancel)
    }

    /// Lists the pull requests of the repository at `location`.
    pub fn set_location(&mut self, location: PathBuf) -> Task<Message> {
        if self.location.as_ref() == Some(&location) && self.remotes.is_some() {
            return Task::none();
        }
        self.location = Some(location.clone());
        self.remotes = None;
        self.prs.clear();
        self.status = Status::Loading(tr!("Looking for pull requests…").into());
        let (generation, cancel) = self.restart();
        Task::perform(
            forge::blocking(move || {
                forge::load_remotes(&location, None, &cancel).map_err(|e| e.to_string())
            }),
            move |result| {
                Message::Remotes(
                    generation,
                    result.unwrap_or_else(|| Err(tr!("Internal error").into())),
                )
            },
        )
    }

    fn list(&mut self) -> Task<Message> {
        let Some(forge) = self.remotes.as_ref().and_then(|r| r.current()).cloned() else {
            return Task::none();
        };
        let (generation, cancel) = self.restart();
        self.status = Status::Loading(forge::loading_prs(Some(forge.kind())).into());
        let (filter, search, limit) = (self.filter, self.search.trim().to_string(), self.limit);
        Task::perform(
            forge::blocking(move || {
                forge
                    .list(filter, &search, limit, &cancel)
                    .map_err(|e| e.to_string())
            }),
            move |result| {
                Message::Listed(
                    generation,
                    result.unwrap_or_else(|| Err(tr!("Internal error").into())),
                )
            },
        )
    }

    /// Handles a message; `Some` when a pull request was picked.
    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<PrSpec>) {
        let task = match message {
            Message::Remotes(generation, result) => {
                if generation != self.generation {
                    return (Task::none(), None);
                }
                match result {
                    Ok(remotes) if remotes.forges.is_empty() => {
                        self.status = Status::Error(
                            tr!("No remote of this repository is on GitHub or GitLab").into(),
                        );
                        self.remotes = Some(remotes);
                        Task::none()
                    }
                    Ok(remotes) => {
                        self.remotes = Some(remotes);
                        self.list()
                    }
                    Err(err) => {
                        self.status = Status::Error(err);
                        Task::none()
                    }
                }
            }
            Message::Listed(generation, result) => {
                if generation != self.generation {
                    return (Task::none(), None);
                }
                match result {
                    Ok(prs) => {
                        self.prs = prs;
                        self.status = Status::Idle;
                    }
                    Err(err) if err == "Cancelled" => {}
                    Err(err) => {
                        self.prs.clear();
                        self.status = Status::Error(err);
                    }
                }
                Task::none()
            }
            Message::Remote(index) => {
                if let Some(remotes) = &mut self.remotes {
                    remotes.select(index);
                }
                self.limit = PAGE;
                self.prs.clear();
                self.list()
            }
            Message::Filter(filter) => {
                self.filter = filter;
                self.limit = PAGE;
                self.list()
            }
            Message::Search(search) => {
                self.search = search;
                self.search_generation += 1;
                let generation = self.search_generation;
                Task::perform(tokio::time::sleep(SEARCH_DELAY), move |_| {
                    Message::SearchDue(generation)
                })
            }
            Message::SearchDue(generation) => {
                if generation != self.search_generation {
                    return (Task::none(), None);
                }
                self.limit = PAGE;
                self.list()
            }
            Message::More => {
                self.limit += PAGE;
                self.list()
            }
            Message::Retry => {
                if self.remotes.as_ref().is_some_and(|r| r.current().is_some()) {
                    self.list()
                } else if let Some(location) = self.location.take() {
                    self.set_location(location)
                } else {
                    Task::none()
                }
            }
            Message::Open(number) => {
                let remote = self
                    .remotes
                    .as_ref()
                    .and_then(|r| r.current())
                    .map(|f| f.remote.name.clone());
                return (Task::none(), Some(PrSpec { number, remote }));
            }
        };
        (task, None)
    }

    pub fn view(&self) -> Element<'_, Message> {
        let forge = self.remotes.as_ref().and_then(|r| r.current());
        let kind = forge.map(|f| f.kind());
        let mut title = row![
            text(forge::nouns_heading(kind)).size(14).font(ui::bold()),
            space::horizontal()
        ]
        .spacing(8)
        .align_y(iced::Center);
        if let Some(remotes) = self.remotes.as_ref().filter(|r| r.forges.len() > 1) {
            let options = remotes
                .forges
                .iter()
                .enumerate()
                .map(|(i, f)| (f.remote.label(), Message::Remote(i)))
                .collect();
            title = title.push(forge::chips(options, remotes.selected));
        } else if let Some(forge) = forge {
            title = title.push(text(forge.remote.label()).size(12).style(ui::dim_text));
        }

        let filters = forge::chips(
            StateFilter::ALL
                .iter()
                .map(|f| {
                    (
                        crate::i18n::pgettext("pull request state", f.label()).to_string(),
                        Message::Filter(*f),
                    )
                })
                .collect(),
            StateFilter::ALL.iter().position(|f| *f == self.filter),
        );
        let search = text_input(tr!("Search title, author, label or #number"), &self.search)
            .on_input(Message::Search)
            .on_submit(Message::SearchDue(self.search_generation))
            .padding([4, 8])
            .size(13);
        let controls = row![filters, search].spacing(10).align_y(iced::Center);

        let now = meld_ailens::time::now_timestamp();
        let mut list = column![].spacing(2).width(Length::Fill);
        for pr in &self.prs {
            list = list.push(pr_row(pr, now, forge.map(|f| f.kind())));
        }
        let mut footer = row![].spacing(8).align_y(iced::Center);
        match &self.status {
            Status::Idle if self.prs.is_empty() => {
                footer = footer.push(
                    text(forge::no_prs(kind, self.filter))
                        .size(12)
                        .style(ui::dim_text),
                );
            }
            Status::Idle => {}
            Status::Loading(what) => {
                footer = footer.push(text(format!("⏳ {what}")).size(12).style(ui::dim_text))
            }
            Status::Error(err) => {
                let (first, _) = crate::views::gitcompare::condense(err);
                footer = footer
                    .push(
                        text(first)
                            .size(12)
                            .style(|theme: &Theme| text::Style {
                                color: Some(theme.extended_palette().danger.base.color),
                            })
                            .wrapping(text::Wrapping::WordOrGlyph)
                            .width(Length::Fill),
                    )
                    .push(ui::text_button(tr!("Retry"), Some(Message::Retry)));
            }
        }
        footer = footer.push(space::horizontal());
        if self.status == Status::Idle && self.prs.len() >= self.limit {
            footer = footer.push(ui::text_button(tr!("Load more"), Some(Message::More)));
        }
        column![
            title,
            controls,
            container(scrollable(list).spacing(4).height(Length::Fill))
                .height(Length::Fill)
                .style(|theme: &Theme| container::Style {
                    border: iced::Border::default()
                        .rounded(6)
                        .width(1)
                        .color(theme.extended_palette().background.strong.color),
                    ..container::Style::default()
                })
                .padding(2),
            footer,
        ]
        .spacing(8)
        .height(Length::Fill)
        .into()
    }
}

fn pr_row<'a>(
    pr: &'a PullRequest,
    now: f64,
    kind: Option<meld_forge::ForgeKind>,
) -> Element<'a, Message> {
    let reference = kind
        .unwrap_or(meld_forge::ForgeKind::GitHub)
        .reference(pr.number);
    let line = row![
        text(reference)
            .size(13)
            .style(ui::dim_text)
            .wrapping(text::Wrapping::None),
        crate::views::gitcompare::ellipsized_label(&pr.title, 13.0, true),
        forge::state_badge(pr),
    ]
    .spacing(8)
    .align_y(iced::Center);
    let description = forge::pr_description(pr, now);
    button(
        column![
            line,
            crate::views::gitcompare::ellipsized_label(description, 11.0, false),
        ]
        .spacing(2),
    )
    .width(Length::Fill)
    .padding([4, 8])
    .style(ui::flat_button)
    .on_press(Message::Open(pr.number))
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_answers_are_ignored_and_picks_name_the_remote() {
        let mut list = PrList::default();
        let _ = list.set_location(PathBuf::from("/nonexistent"));
        let (_, picked) = list.update(Message::Listed(0, Ok(vec![PullRequest::default()])));
        assert!(picked.is_none());
        assert!(list.prs.is_empty(), "an answer of an older request");
        let (_, picked) = list.update(Message::Open(4));
        assert_eq!(
            picked,
            Some(PrSpec {
                number: 4,
                remote: None
            })
        );
    }
}
