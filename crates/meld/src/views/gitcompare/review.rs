//! Reviewing a pull or merge request in Git Compare ([`meld_forge`]).
//!
//! When a tab compares a pull request (its merge base → its head, fetched
//! into `refs/meld/pr/…`), [`PrReview`] holds what the forge said about
//! it and what the user is writing:
//!
//! - the strip under the header: state, number, title, author, labels,
//!   checks, and the review actions; the description and the conversation
//!   fold out under it ([`PrReview::strip`])
//! - line comments: existing threads and the user's pending comments as
//!   cards in the Why column, level with their line ([`PrReview::cards`])
//! - the composer for a line comment, a reply or the review, and the
//!   confirmation that always comes before anything is posted
//!   ([`PrReview::dialog`]). Nothing reaches the forge without the user
//!   pressing Post in that confirmation.

use std::collections::HashSet;
use std::time::Duration;

use iced::widget::{button, column, container, mouse_area, row, scrollable, space, text, text_editor, tooltip};
use iced::{Background, Border, Color, Element, Length, Theme, Vector};
use crate::forge::relative_date;
use meld_ailens::picker::{EntryKind, parse_iso_date};
use meld_ailens::{PickerEntry, Revision, Side};
use meld_forge::fetch::pr_ref;
use meld_forge::{
    Comment, ConversationItem, Discussion, DraftComment, FetchedPullRequest, Forge, ForgeKind, ForgeRemote,
    PullRequest, PullRequestDetails, ReviewEvent, ReviewSubmission, ReviewThread,
};

use super::diffhost::PaneGeometry;
use crate::ui::ellipsis::{Mode, ellipsized};
use super::whycolumn::{local_date, shorten};
use crate::ui;

/// Comments longer than this are cut short until expanded.
const LONG_COMMENT: usize = 300;
/// Replies shown under a thread's first comment until expanded.
const SHOWN_REPLIES: usize = 2;

fn hex(rgb: u32) -> Color {
    Color::from_rgb8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

const THREAD_COLOR: u32 = 0x0969da;
const DRAFT_COLOR: u32 = 0xbf8700;

/// What the user is writing.
pub struct Composer {
    pub kind: ComposerKind,
    pub editor: text_editor::Content,
    pub event: ReviewEvent,
    /// Why it can't be posted yet, or why posting failed.
    pub problem: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ComposerKind {
    /// A pending line comment; `edit` is the draft it replaces.
    Line {
        path: String,
        original_path: Option<String>,
        side: Side,
        line: u32,
        edit: Option<usize>,
    },
    Reply { thread: String },
    Review,
}

/// What the confirmation would post.
#[derive(Debug, Clone, PartialEq)]
pub enum Post {
    Review(ReviewSubmission),
    Reply { thread: ReviewThread, body: String },
}

pub struct PrReview {
    pub forge: Forge,
    pub details: PullRequestDetails,
    pub fetched: FetchedPullRequest,
    pub discussion: Discussion,
    /// "Loading comments…", or why they couldn't be loaded.
    pub discussion_status: Option<String>,
    /// Line comments written here, posted with the review.
    pub drafts: Vec<DraftComment>,
    /// The review's own comment, kept while the composer is closed.
    pub review_body: String,
    pub review_event: ReviewEvent,
    pub show_details: bool,
    pub composer: Option<Composer>,
    /// The confirmation shown before posting.
    pub confirm: Option<Post>,
    pub posting: bool,
    /// Threads (and comments) shown in full.
    pub expanded: HashSet<String>,
    /// The composer's editor, to focus it when it opens.
    pub editor_id: iced::widget::Id,
}

#[derive(Debug, Clone)]
pub enum Message {
    ToggleDetails,
    OpenInBrowser,
    /// Comment on the line with the cursor (the tab knows where it is).
    AddComment,
    EditDraft(usize),
    DeleteDraft(usize),
    Reply(String),
    StartReview,
    Editor(text_editor::Action),
    SetEvent(ReviewEvent),
    /// Save the line comment, or ask to confirm the reply or review.
    Save,
    CancelComposer,
    /// Post what the confirmation shows.
    ConfirmPost,
    CancelConfirm,
    Expand(String),
    RefreshComments,
    Hover(Option<(Side, u32)>),
    /// Clicking beside a dialog does nothing (the text stays).
    Ignore,
}

/// What the tab has to do after a message.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    None,
    /// Post this; the user confirmed it.
    Post(Post),
    RefreshComments,
    OpenUrl(String),
    Hover(Option<(Side, u32)>),
}

impl PrReview {
    pub fn new(forge: Forge, mut details: PullRequestDetails, fetched: FetchedPullRequest) -> Self {
        // A review is about the version that was fetched and shown, even
        // if the pull request moved on since
        details.pr.head_sha = fetched.head.clone();
        PrReview {
            forge,
            details,
            fetched,
            discussion: Discussion::default(),
            discussion_status: Some(tr!("Loading comments…").into()),
            drafts: Vec::new(),
            review_body: String::new(),
            review_event: ReviewEvent::Comment,
            show_details: false,
            composer: None,
            confirm: None,
            posting: false,
            expanded: HashSet::new(),
            editor_id: iced::widget::Id::unique(),
        }
    }

    pub fn kind(&self) -> ForgeKind {
        self.forge.kind()
    }

    pub fn number(&self) -> u64 {
        self.details.pr.number
    }

    /// `#12` or `!12`
    pub fn reference(&self) -> String {
        self.kind().reference(self.number())
    }

    /// e.g. `pull request #12 of github.com/melandur/meld`
    pub fn place(&self) -> String {
        let repository = format!("{}/{}", self.forge.remote.host, self.forge.remote.path);
        match self.kind() {
            ForgeKind::GitHub => tr!(
                "pull request {reference} of {repository}",
                reference = self.reference(),
                repository = repository
            ),
            ForgeKind::GitLab => tr!(
                "merge request {reference} of {repository}",
                reference = self.reference(),
                repository = repository
            ),
        }
    }

    pub fn url(&self) -> String {
        if self.details.pr.url.is_empty() {
            self.forge.remote.pr_url(self.number())
        } else {
            self.details.pr.url.clone()
        }
    }

    /// Whether the comparison shows this pull request's changes.
    pub fn matches(&self, base: &Revision, target: &Revision) -> bool {
        base.ref_name.as_deref() == Some(self.fetched.base.as_str())
            && target.ref_name.as_deref() == Some(self.fetched.head_ref.as_str())
    }

    /// Opens the composer for a line comment.
    pub fn comment_on(&mut self, path: String, original_path: Option<String>, side: Side, line: u32) {
        // Editing the pending comment on that line, if there is one
        if let Some(index) = self.drafts.iter().position(|d| d.path == path && d.side == side && d.line == line) {
            self.edit_draft(index);
            return;
        }
        self.composer = Some(Composer {
            kind: ComposerKind::Line {
                path,
                original_path,
                side,
                line,
                edit: None,
            },
            editor: text_editor::Content::new(),
            event: self.review_event,
            problem: None,
        });
    }

    fn edit_draft(&mut self, index: usize) {
        let Some(draft) = self.drafts.get(index) else {
            return;
        };
        self.composer = Some(Composer {
            kind: ComposerKind::Line {
                path: draft.path.clone(),
                original_path: draft.original_path.clone(),
                side: draft.side,
                line: draft.line,
                edit: Some(index),
            },
            editor: text_editor::Content::with_text(&draft.body),
            event: self.review_event,
            problem: None,
        });
    }

    fn close_composer(&mut self) {
        if let Some(composer) = self.composer.take()
            && composer.kind == ComposerKind::Review
        {
            self.review_body = editor_text(&composer.editor);
            self.review_event = composer.event;
        }
    }

    pub fn update(&mut self, message: Message) -> Action {
        match message {
            Message::ToggleDetails => self.show_details = !self.show_details,
            Message::OpenInBrowser => return Action::OpenUrl(self.url()),
            // Handled by the tab, which knows the cursor
            Message::AddComment => {}
            Message::EditDraft(index) => self.edit_draft(index),
            Message::DeleteDraft(index) => {
                if index < self.drafts.len() {
                    self.drafts.remove(index);
                }
            }
            Message::Reply(thread) => {
                self.composer = Some(Composer {
                    kind: ComposerKind::Reply { thread },
                    editor: text_editor::Content::new(),
                    event: self.review_event,
                    problem: None,
                });
            }
            Message::StartReview => {
                self.composer = Some(Composer {
                    kind: ComposerKind::Review,
                    editor: text_editor::Content::with_text(&self.review_body),
                    event: self.review_event,
                    problem: None,
                });
            }
            Message::Editor(action) => {
                if let Some(composer) = &mut self.composer {
                    composer.editor.perform(action);
                    composer.problem = None;
                }
            }
            Message::SetEvent(event) => {
                if let Some(composer) = &mut self.composer {
                    composer.event = event;
                    composer.problem = None;
                }
            }
            Message::Save => return self.save(),
            Message::CancelComposer => self.close_composer(),
            Message::ConfirmPost => {
                if self.posting {
                    return Action::None;
                }
                if let Some(post) = self.confirm.clone() {
                    self.posting = true;
                    return Action::Post(post);
                }
            }
            Message::CancelConfirm => {
                if !self.posting {
                    self.confirm = None;
                }
            }
            Message::Expand(key) => {
                if !self.expanded.remove(&key) {
                    self.expanded.insert(key);
                }
            }
            Message::RefreshComments => return Action::RefreshComments,
            Message::Hover(at) => return Action::Hover(at),
            Message::Ignore => {}
        }
        Action::None
    }

    fn save(&mut self) -> Action {
        let kind = self.kind();
        let Some(composer) = &mut self.composer else {
            return Action::None;
        };
        let body = editor_text(&composer.editor);
        match composer.kind.clone() {
            ComposerKind::Line {
                path,
                original_path,
                side,
                line,
                edit,
            } => {
                if body.trim().is_empty() {
                    composer.problem = Some(tr!("Write the comment first.").into());
                    return Action::None;
                }
                let draft = DraftComment {
                    path,
                    original_path,
                    side,
                    line,
                    body,
                };
                match edit.filter(|i| *i < self.drafts.len()) {
                    Some(index) => self.drafts[index] = draft,
                    None => self.drafts.push(draft),
                }
                self.composer = None;
            }
            ComposerKind::Reply { thread } => {
                if body.trim().is_empty() {
                    composer.problem = Some(tr!("Write the reply first.").into());
                    return Action::None;
                }
                let Some(thread) = self.discussion.threads.iter().find(|t| t.id == thread).cloned() else {
                    composer.problem = Some(tr!("That thread is gone; reload the comments.").into());
                    return Action::None;
                };
                self.confirm = Some(Post::Reply { thread, body });
            }
            ComposerKind::Review => {
                let event = composer.event;
                let submission = ReviewSubmission {
                    event,
                    body,
                    comments: self.drafts.clone(),
                };
                if let Some(problem) = submission.problem(kind) {
                    composer.problem = Some(problem);
                    return Action::None;
                }
                self.confirm = Some(Post::Review(submission));
            }
        }
        Action::None
    }

    /// The forge answered a post: on success the composer and (for a
    /// review) the pending comments are done; on failure they stay, with
    /// the error.
    pub fn posted(&mut self, post: &Post, result: Result<(), String>) {
        self.posting = false;
        self.confirm = None;
        match result {
            Ok(()) => {
                if let Post::Review(_) = post {
                    self.drafts.clear();
                    self.review_body.clear();
                    self.review_event = ReviewEvent::Comment;
                }
                self.composer = None;
            }
            Err(err) => {
                if let Some(composer) = &mut self.composer {
                    composer.problem = Some(err);
                }
            }
        }
    }

    pub fn set_discussion(&mut self, result: Result<Discussion, String>) {
        match result {
            Ok(discussion) => {
                self.discussion = discussion;
                self.discussion_status = None;
            }
            Err(err) => self.discussion_status = Some(tr!("Couldn’t load the comments: {err}", err = err)),
        }
    }

    /// Threads and pending comments on `path`, in the order they appear.
    fn on_file<'a>(&'a self, path: &'a str, original: Option<&'a str>) -> impl Iterator<Item = &'a ReviewThread> + 'a {
        self.discussion
            .threads
            .iter()
            .filter(move |t| t.is_on(path) || original.is_some_and(|o| t.is_on(o)))
    }

    /// Number of threads on `path`.
    #[cfg(test)]
    pub fn thread_count(&self, path: &str) -> usize {
        self.on_file(path, None).count() + self.drafts.iter().filter(|d| d.path == path).count()
    }

    // -- views --

    /// The strip under the header bar.
    pub fn strip(&self) -> Element<'_, Message> {
        let pr = &self.details.pr;
        let mut line = row![
            crate::forge::state_badge(pr),
            text(self.reference()).size(14).style(ui::dim_text).wrapping(text::Wrapping::None),
            ellipsized(pr.title.as_str(), Mode::End).size(14).font(ui::bold()).max_width(520.0),
        ]
        .spacing(8)
        .align_y(iced::Center);
        let mut facts = Vec::new();
        if !pr.author.is_empty() {
            facts.push(tr!("by {author}", author = pr.author));
        }
        if !pr.head_branch.is_empty() && !pr.base_branch.is_empty() {
            facts.push(format!("{} → {}", pr.head_branch, pr.base_branch));
        }
        if let Some(decision) = &self.details.review_decision {
            facts.push(decision.clone());
        }
        if let Some(checks) = &self.details.checks {
            facts.push(checks.clone());
        }
        let facts = facts.join(" · ");
        line = line.push(
            container(ellipsized(facts, Mode::End).size(12).style(ui::dim_text).width(Length::Fill))
                .width(Length::Fill)
                .clip(true),
        );
        for label in pr.labels.iter().take(4) {
            line = line.push(pill(label.clone()));
        }
        let comments = self.discussion.threads.len() + self.discussion.conversation.len();
        let details_label = format!(
            "{} {}",
            if self.show_details { "▴" } else { "▾" },
            if comments == 0 {
                tr!("Description").to_string()
            } else {
                trn!("Description · {} comment", "Description · {} comments", comments, comments)
            }
        );
        line = line
            .push(
                button(text(details_label).size(13))
                    .padding([3, 8])
                    .style(ui::flat_button)
                    .on_press(Message::ToggleDetails),
            )
            .push(tip(
                button(text(tr!("Comment on Line")).size(13))
                    .padding([3, 10])
                    .style(button::secondary)
                    .on_press(Message::AddComment),
                tr!("Write a comment on the line with the cursor (click a line first); it is posted with the review"),
            ));
        let review_label = match self.drafts.len() {
            0 => tr!("Review…").to_string(),
            n => trn!("Review ({} pending)…", "Review ({} pending)…", n, n),
        };
        line = line
            .push(tip(
                button(text(review_label).size(13))
                    .padding([3, 10])
                    .style(button::primary)
                    .on_press(Message::StartReview),
                tr!("Comment, approve or request changes"),
            ))
            .push(ui::tool_button("⟳", tr!("Reload the comments"), Some(Message::RefreshComments)))
            .push(ui::tool_button("↗", tr!("Open in the browser"), Some(Message::OpenInBrowser)));
        let mut strip = column![container(line).padding([4, 8])];
        if self.show_details {
            strip = strip.push(ui::separator(false)).push(self.details_panel());
        }
        container(strip)
            .width(Length::Fill)
            .style(|theme: &Theme| container::Style {
                background: Some(Background::Color(theme.extended_palette().background.weak.color)),
                ..container::Style::default()
            })
            .into()
    }

    fn details_panel(&self) -> Element<'_, Message> {
        let now = meld_ailens::time::now_timestamp();
        let body = self.details.body.trim();
        let mut panel = column![].spacing(10).width(Length::Fill);
        let mut created = Vec::new();
        if let Some(date) = parse_iso_date(&self.details.pr.created_at).filter(|_| !self.details.pr.created_at.is_empty()) {
            created.push(tr!("opened {when}", when = relative_date(date, now)));
        }
        if let (Some(add), Some(del)) = (self.details.additions, self.details.deletions) {
            created.push(format!("+{add} −{del}"));
        }
        if let Some(files) = self.details.changed_files {
            created.push(trn!("{} file", "{} files", files, files));
        }
        if !created.is_empty() {
            panel = panel.push(text(created.join(" · ")).size(12).style(ui::dim_text));
        }
        panel = panel.push(if body.is_empty() {
            Element::from(text(tr!("No description.")).size(13).style(ui::dim_text))
        } else {
            text(body).size(13).wrapping(text::Wrapping::WordOrGlyph).into()
        });
        panel = panel.push(ui::separator(false));
        panel = panel.push(text(tr!("Conversation")).size(13).font(ui::bold()));
        if let Some(status) = &self.discussion_status {
            panel = panel.push(text(status.as_str()).size(12).style(ui::dim_text));
        } else if self.discussion.conversation.is_empty() {
            panel = panel.push(text(tr!("No comments outside the code.")).size(12).style(ui::dim_text));
        }
        for item in &self.discussion.conversation {
            panel = panel.push(conversation_item(item, &self.expanded, now));
        }
        if !self.discussion.threads.is_empty() {
            let files: HashSet<&str> = self.discussion.threads.iter().map(|t| t.path.as_str()).collect();
            panel = panel.push(
                text(trn!(
                    "{threads} line comment threads on {} file, shown beside the code",
                    "{threads} line comment threads on {} files, shown beside the code",
                    files.len(),
                    files.len(),
                    threads = self.discussion.threads.len()
                ))
                .size(12)
                .style(ui::dim_text),
            );
        }
        container(scrollable(container(panel).padding([8, 12])).height(Length::Shrink))
            .max_height(280)
            .into()
    }

    /// Cards for the Why column: the threads and pending comments on the
    /// file at `path` (`original`: its path in the base), with where each
    /// wants to be.
    pub fn cards<'a>(
        &'a self,
        path: &'a str,
        original: Option<&'a str>,
        panes: [Option<PaneGeometry<'_>>; 2],
    ) -> Vec<(f32, Element<'a, Message>)> {
        let now = meld_ailens::time::now_timestamp();
        let top = |side: Side, line: u32| -> f32 {
            let pane = match side {
                Side::Old => 0,
                Side::New => 1,
            };
            panes[pane].map_or(0.0, |g| g.line_y(line.saturating_sub(1) as usize))
        };
        let mut cards = Vec::new();
        for thread in self.on_file(path, original) {
            let line = thread.anchor_line().unwrap_or(1);
            cards.push((top(thread.side, line), self.thread_card(thread, now)));
        }
        for (index, draft) in self.drafts.iter().enumerate() {
            if draft.path == path {
                cards.push((top(draft.side, draft.line), draft_card(draft, index)));
            }
        }
        cards
    }

    fn thread_card<'a>(&'a self, thread: &'a ReviewThread, now: f64) -> Element<'a, Message> {
        let expanded = self.expanded.contains(&thread.id);
        let mut body = column![].spacing(4).width(Length::Fill);
        let mut header = row![text("💬").size(12)].spacing(6).align_y(iced::Center);
        if let Some(first) = thread.comments.first() {
            header = header.push(
                ellipsized(first.author.as_str(), Mode::End)
                    .size(12)
                    .font(ui::bold())
                    .width(Length::Fill),
            );
        } else {
            header = header.push(space::horizontal());
        }
        if thread.outdated {
            header = header.push(tip(pill(tr!("Outdated").into()), tr!("Made on an older version of this change")));
        }
        if thread.resolved {
            header = header.push(pill(tr!("Resolved").into()));
        }
        if let Some(line) = thread.anchor_line() {
            header = header.push(text(format!("L{line}")).size(11).style(ui::dim_text));
        }
        body = body.push(header);
        let count = thread.comments.len();
        let hidden = if expanded { 0 } else { count.saturating_sub(1 + SHOWN_REPLIES) };
        for (index, comment) in thread.comments.iter().enumerate() {
            if index > 0 && index <= hidden {
                continue;
            }
            if index > 0 {
                body = body.push(ui::separator(false));
                body = body.push(comment_header(comment, now));
            } else if let Some(date) = date_label(&comment.created_at, now) {
                body = body.push(date);
            }
            body = body.push(comment_body(comment, &format!("{}:{}", thread.id, comment.id), &self.expanded));
            if index == 0 && hidden > 0 {
                body = body.push(
                    button(text(trn!("{} more reply", "{} more replies", hidden, hidden)).size(12))
                        .padding(0)
                        .style(link_button)
                        .on_press(Message::Expand(thread.id.clone())),
                );
            }
        }
        body = body.push(
            row![
                button(text(tr!("Reply…")).size(12))
                    .padding([2, 10])
                    .style(button::secondary)
                    .on_press(Message::Reply(thread.id.clone())),
            ]
            .spacing(6),
        );
        let at = thread.anchor_line().map(|line| (thread.side, line));
        card(body.into(), hex(THREAD_COLOR), thread.outdated, at)
    }

    /// The composer or the confirmation, when one is open.
    pub fn dialog(&self) -> Option<(Element<'_, Message>, Message)> {
        if let Some(post) = &self.confirm {
            return Some((self.confirm_dialog(post), Message::CancelConfirm));
        }
        let composer = self.composer.as_ref()?;
        Some((self.composer_dialog(composer), Message::Ignore))
    }

    fn composer_dialog<'a>(&'a self, composer: &'a Composer) -> Element<'a, Message> {
        let editor = text_editor(&composer.editor)
            .id(self.editor_id.clone())
            .on_action(Message::Editor)
            .size(14)
            .padding(8)
            .height(160)
            .wrapping(text::Wrapping::WordOrGlyph);
        let mut body = column![].spacing(10).width(600);
        let (title, save) = match &composer.kind {
            ComposerKind::Line {
                path, side, line, edit, ..
            } => {
                body = body.push(
                    text(match side {
                        Side::Old => tr!(
                            "On line {line} of the base version. Line comments are posted together with the review.",
                            line = line
                        ),
                        Side::New => tr!(
                            "On line {line} of the changed version. Line comments are posted together with the review.",
                            line = line
                        ),
                    })
                    .size(13)
                    .style(ui::dim_text),
                );
                (
                    tr!("Comment on {path}", path = path),
                    if edit.is_some() { tr!("Save Comment") } else { tr!("Add to Review") },
                )
            }
            ComposerKind::Reply { thread } => {
                if let Some(thread) = self.discussion.threads.iter().find(|t| t.id == *thread) {
                    if let Some(last) = thread.comments.last() {
                        body = body.push(quote(format!("{}: {}", last.author, shorten(last.body.trim(), 400))));
                    }
                    (
                        tr!(
                            "Reply on {path} line {line}",
                            path = thread.path,
                            line = thread.anchor_line().unwrap_or(0)
                        ),
                        tr!("Post Reply…"),
                    )
                } else {
                    (tr!("Reply").to_string(), tr!("Post Reply…"))
                }
            }
            ComposerKind::Review => {
                let options = ReviewEvent::ALL
                    .iter()
                    .filter(|e| **e != ReviewEvent::RequestChanges || self.kind().can_request_changes())
                    .map(|e| (crate::i18n::gettext(e.label()).to_string(), Message::SetEvent(*e)))
                    .collect::<Vec<_>>();
                let selected = ReviewEvent::ALL
                    .iter()
                    .filter(|e| **e != ReviewEvent::RequestChanges || self.kind().can_request_changes())
                    .position(|e| *e == composer.event);
                body = body.push(crate::forge::chips(options, selected));
                if self.drafts.is_empty() {
                    body = body.push(
                        text(tr!("No line comments. Click a line and press Comment on Line to add some."))
                            .size(12)
                            .style(ui::dim_text),
                    );
                } else {
                    let count = self.drafts.len();
                    let mut list = column![text(trn!("{} line comment", "{} line comments", count, count)).size(12).font(ui::bold())]
                        .spacing(2);
                    for draft in &self.drafts {
                        list = list.push(
                            text(format!("{} — {}", draft.location(), shorten(first_line(&draft.body), 80)))
                                .size(12)
                                .style(ui::dim_text),
                        );
                    }
                    body = body.push(list);
                }
                (tr!("Review {reference}", reference = self.reference()), tr!("Post Review…"))
            }
        };
        body = body.push(editor);
        if let Some(problem) = &composer.problem {
            body = body.push(error_text(problem.clone()));
        }
        ui::modal::dialog(
            title,
            body,
            vec![
                button(text(tr!("Cancel"))).style(button::secondary).on_press(Message::CancelComposer).into(),
                button(text(save)).on_press(Message::Save).into(),
            ],
        )
    }

    fn confirm_dialog<'a>(&'a self, post: &'a Post) -> Element<'a, Message> {
        let kind = self.kind();
        let mut body = column![
            text(tr!(
                "This posts to {place} (“{title}”), as the account {tool} is logged in with:",
                place = self.place(),
                title = shorten(&self.details.pr.title, 60),
                tool = kind.tool()
            ))
            .size(13)
        ]
        .spacing(10)
        .width(600);
        let mut items = column![].spacing(8);
        match post {
            Post::Review(review) => {
                items = items.push(text(review.summary()).size(14).font(ui::bold()));
                if !review.body.trim().is_empty() {
                    items = items.push(quote(review.body.trim().to_string()));
                }
                for comment in &review.comments {
                    items = items.push(
                        column![
                            text(comment.location()).size(12).font(ui::bold()),
                            quote(comment.body.trim().to_string()),
                        ]
                        .spacing(2),
                    );
                }
            }
            Post::Reply { thread, body: reply } => {
                let path = thread.path.as_str();
                let line = thread.anchor_line().unwrap_or(0);
                items = items.push(
                    text(match thread.comments.first() {
                        Some(first) => tr!(
                            "A reply to {author}’s thread on {path} line {line}",
                            author = first.author,
                            path = path,
                            line = line
                        ),
                        None => tr!("A reply to the thread on {path} line {line}", path = path, line = line),
                    })
                    .size(14)
                    .font(ui::bold()),
                );
                items = items.push(quote(reply.trim().to_string()));
            }
        }
        body = body.push(container(scrollable(items)).max_height(360));
        if self.posting {
            body = body.push(text(format!("⏳ {}", tr!("Posting…"))).size(13).style(ui::dim_text));
        }
        let post_label = match post {
            Post::Review(review) => match review.event {
                ReviewEvent::Comment => tr!("Post Comment"),
                ReviewEvent::Approve => tr!("Post Approval"),
                ReviewEvent::RequestChanges => tr!("Post Request for Changes"),
            },
            Post::Reply { .. } => tr!("Post Reply"),
        };
        ui::modal::dialog(
            tr!("Post to {forge}?", forge = kind.name()),
            body,
            vec![
                button(text(tr!("Cancel")))
                    .style(button::secondary)
                    .on_press_maybe((!self.posting).then_some(Message::CancelConfirm))
                    .into(),
                button(text(post_label))
                    .on_press_maybe((!self.posting).then_some(Message::ConfirmPost))
                    .into(),
            ],
        )
    }
}

/// A pull request as a row of the Base and Target pickers.
pub fn picker_entry(remote: &ForgeRemote, pr: &PullRequest, now: f64) -> PickerEntry {
    let reference = remote.kind.reference(pr.number);
    let label = format!("{reference} {}", pr.title);
    let description = [crate::i18n::pgettext("pull request state", pr.state_label()).to_string(), crate::forge::pr_description(pr, now)]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let search_text = format!("{} {label} {description}", pr.search_text()).to_lowercase();
    PickerEntry {
        kind: EntryKind::PullRequest,
        section: capitalized(remote.kind.nouns()),
        label,
        description,
        icon_name: "pull-request".into(),
        revision: Revision::commit(pr_ref(&remote.name, pr.number, "head")),
        short_label: reference,
        paired_base: None,
        search_text,
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn editor_text(editor: &text_editor::Content) -> String {
    let text = editor.text();
    // The editor always ends with a newline
    text.strip_suffix('\n').unwrap_or(&text).to_string()
}

fn first_line(text: &str) -> &str {
    text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim()
}

fn tip<'a>(content: impl Into<Element<'a, Message>>, tip: &'a str) -> Element<'a, Message> {
    tooltip(
        content,
        container(text(tip).size(12)).max_width(360).padding([4, 8]).style(ui::tooltip_box),
        tooltip::Position::Bottom,
    )
    .delay(Duration::from_millis(500))
    .into()
}

fn pill<'a, M: 'a>(label: String) -> Element<'a, M> {
    container(text(label).size(11).wrapping(text::Wrapping::None))
        .padding([0, 6])
        .style(|theme: &Theme| container::Style {
            background: Some(Background::Color(theme.extended_palette().background.base.text.scale_alpha(0.1))),
            border: Border::default().rounded(9),
            ..container::Style::default()
        })
        .into()
}

fn quote<'a, M: 'a>(body: String) -> Element<'a, M> {
    container(text(body).size(13).wrapping(text::Wrapping::WordOrGlyph))
        .width(Length::Fill)
        .padding([4, 10])
        .style(|theme: &Theme| container::Style {
            background: Some(Background::Color(theme.extended_palette().background.weak.color)),
            border: Border::default().rounded(4),
            ..container::Style::default()
        })
        .into()
}

fn error_text<'a, M: 'a>(message: String) -> Element<'a, M> {
    text(message)
        .size(12)
        .wrapping(text::Wrapping::WordOrGlyph)
        .style(|theme: &Theme| text::Style {
            color: Some(theme.extended_palette().danger.base.color),
        })
        .into()
}

fn link_button(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    button::Style {
        background: None,
        text_color: match status {
            button::Status::Hovered | button::Status::Pressed => palette.primary.strong.color,
            _ => palette.primary.base.color,
        },
        ..button::Style::default()
    }
}

fn date_label<'a>(date: &str, now: f64) -> Option<Element<'a, Message>> {
    let parsed = parse_iso_date(date).filter(|_| !date.is_empty())?;
    Some(
        tooltip(
            text(relative_date(parsed, now)).size(11).style(ui::dim_text),
            container(text(local_date(parsed).unwrap_or_else(|| date.to_string())).size(12))
                .padding([4, 8])
                .style(ui::tooltip_box),
            tooltip::Position::Bottom,
        )
        .into(),
    )
}

fn comment_header<'a>(comment: &'a Comment, now: f64) -> Element<'a, Message> {
    let mut header = row![text(comment.author.as_str()).size(12).font(ui::bold())]
        .spacing(6)
        .align_y(iced::Center);
    if let Some(date) = date_label(&comment.created_at, now) {
        header = header.push(date);
    }
    header.into()
}

fn comment_body<'a>(comment: &'a Comment, key: &str, expanded: &HashSet<String>) -> Element<'a, Message> {
    let body = comment.body.trim();
    if body.chars().count() <= LONG_COMMENT {
        return text(body).size(13).wrapping(text::Wrapping::WordOrGlyph).into();
    }
    let open = expanded.contains(key);
    column![
        text(if open { body.to_string() } else { shorten(body, LONG_COMMENT) })
            .size(13)
            .wrapping(text::Wrapping::WordOrGlyph),
        button(text(if open { tr!("Show less") } else { tr!("Show more") }).size(12))
            .padding(0)
            .style(link_button)
            .on_press(Message::Expand(key.to_string())),
    ]
    .spacing(2)
    .into()
}

fn conversation_item<'a>(item: &'a ConversationItem, expanded: &HashSet<String>, now: f64) -> Element<'a, Message> {
    let mut header = row![text(item.comment.author.as_str()).size(13).font(ui::bold())]
        .spacing(6)
        .align_y(iced::Center);
    if let Some(review) = item.review {
        header = header.push(pill(crate::i18n::gettext(review.label()).to_string()));
    }
    if let Some(date) = date_label(&item.comment.created_at, now) {
        header = header.push(date);
    }
    let mut column = column![header].spacing(2);
    if !item.comment.body.trim().is_empty() {
        column = column.push(comment_body(&item.comment, &format!("conversation:{}", item.comment.id), expanded));
    }
    column.into()
}

fn draft_card(draft: &DraftComment, index: usize) -> Element<'_, Message> {
    let body = column![
        row![
            pill(tr!("Pending").into()),
            text(format!("L{}", draft.line)).size(11).style(ui::dim_text),
            space::horizontal(),
        ]
        .spacing(6)
        .align_y(iced::Center),
        text(draft.body.trim()).size(13).wrapping(text::Wrapping::WordOrGlyph),
        row![
            button(text(tr!("Edit")).size(12))
                .padding([2, 10])
                .style(button::secondary)
                .on_press(Message::EditDraft(index)),
            button(text(tr!("Delete")).size(12))
                .padding([2, 10])
                .style(button::secondary)
                .on_press(Message::DeleteDraft(index)),
        ]
        .spacing(6),
    ]
    .spacing(4)
    .width(Length::Fill);
    card(body.into(), hex(DRAFT_COLOR), false, Some((draft.side, draft.line)))
}

/// A card like the Why column's, with a colored edge, that highlights its
/// line while hovered.
fn card<'a>(body: Element<'a, Message>, color: Color, faded: bool, at: Option<(Side, u32)>) -> Element<'a, Message> {
    let edge_color = if faded { color.scale_alpha(0.4) } else { color };
    let edge = container(space::vertical())
        .width(4)
        .height(Length::Fill)
        .style(move |_theme: &Theme| container::Style {
            background: Some(Background::Color(edge_color)),
            border: Border::default().rounded(iced::border::Radius::default().left(6)),
            ..container::Style::default()
        });
    let boxed = container(row![edge, container(body).padding([6, 8]).width(Length::Fill)])
        .width(Length::Fill)
        .style(move |theme: &Theme| {
            let palette = theme.extended_palette();
            container::Style {
                background: Some(Background::Color(palette.background.base.color)),
                text_color: Some(palette.background.base.text),
                border: Border::default().rounded(6).width(1).color(color.scale_alpha(0.35)),
                shadow: iced::Shadow {
                    color: Color::BLACK.scale_alpha(0.12),
                    offset: Vector::new(0.0, 1.0),
                    blur_radius: 2.0,
                },
                ..container::Style::default()
            }
        });
    mouse_area(container(boxed).padding([0, 6]))
        .on_enter(Message::Hover(at))
        .on_exit(Message::Hover(None))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use meld_forge::{PrState, ScriptedRunner};
    use std::sync::Arc;

    fn review() -> PrReview {
        let remote = meld_forge::remote::parse_remote("github", "git@github.com:melandur/meld.git").unwrap();
        let forge = Forge::new(remote, Arc::new(ScriptedRunner::new()));
        let details = PullRequestDetails {
            pr: PullRequest {
                number: 12,
                title: "Add things".into(),
                state: PrState::Open,
                ..Default::default()
            },
            ..Default::default()
        };
        let fetched = FetchedPullRequest {
            base: "b".repeat(40),
            head: "h".repeat(40),
            head_ref: pr_ref("github", 12, "head"),
        };
        PrReview::new(forge, details, fetched)
    }

    fn type_text(review: &mut PrReview, text: &str) {
        for c in text.chars() {
            review.update(Message::Editor(text_editor::Action::Edit(text_editor::Edit::Insert(c))));
        }
    }

    #[test]
    fn line_comments_are_kept_as_drafts_without_posting() {
        let mut r = review();
        r.comment_on("a.rs".into(), None, Side::New, 4);
        assert_eq!(r.update(Message::Save), Action::None);
        assert!(r.composer.as_ref().unwrap().problem.is_some(), "empty comments are refused");
        type_text(&mut r, "Why?");
        assert_eq!(r.update(Message::Save), Action::None);
        assert!(r.composer.is_none() && r.confirm.is_none());
        assert_eq!(r.drafts.len(), 1);
        assert_eq!(r.drafts[0].body, "Why?");
        // The same line edits the draft
        r.comment_on("a.rs".into(), None, Side::New, 4);
        assert_eq!(editor_text(&r.composer.as_ref().unwrap().editor), "Why?");
        r.update(Message::CancelComposer);
        r.update(Message::DeleteDraft(0));
        assert!(r.drafts.is_empty());
    }

    #[test]
    fn a_review_is_only_posted_after_confirming() {
        let mut r = review();
        r.comment_on("a.rs".into(), None, Side::Old, 2);
        type_text(&mut r, "Nit");
        r.update(Message::Save);
        r.update(Message::StartReview);
        r.update(Message::SetEvent(ReviewEvent::Approve));
        type_text(&mut r, "LGTM");
        // Saving the review asks for confirmation; nothing is posted yet
        assert_eq!(r.update(Message::Save), Action::None);
        let Some(Post::Review(submission)) = r.confirm.clone() else {
            panic!("no confirmation");
        };
        assert_eq!(submission.event, ReviewEvent::Approve);
        assert_eq!(submission.body, "LGTM");
        assert_eq!(submission.comments.len(), 1);
        // Cancelling posts nothing and keeps everything
        r.update(Message::CancelConfirm);
        assert!(r.confirm.is_none() && r.composer.is_some());
        r.update(Message::Save);
        let action = r.update(Message::ConfirmPost);
        assert_eq!(action, Action::Post(Post::Review(submission.clone())));
        // Pressing Post twice posts once
        assert_eq!(r.update(Message::ConfirmPost), Action::None);
        r.posted(&Post::Review(submission), Ok(()));
        assert!(r.drafts.is_empty() && r.composer.is_none() && r.confirm.is_none());
    }

    #[test]
    fn failed_posts_keep_the_text_and_say_why() {
        let mut r = review();
        r.update(Message::StartReview);
        type_text(&mut r, "Hmm");
        r.update(Message::Save);
        let Action::Post(post) = r.update(Message::ConfirmPost) else {
            panic!("not posted");
        };
        r.posted(&post, Err("HTTP 422".into()));
        let composer = r.composer.as_ref().unwrap();
        assert_eq!(composer.problem.as_deref(), Some("HTTP 422"));
        assert_eq!(editor_text(&composer.editor), "Hmm");
        // Closing the composer keeps the review's comment for later
        r.update(Message::CancelComposer);
        assert_eq!(r.review_body, "Hmm");
    }

    #[test]
    fn replies_need_text_and_a_thread() {
        let mut r = review();
        r.discussion.threads.push(ReviewThread {
            id: "100".into(),
            path: "a.rs".into(),
            original_path: None,
            side: Side::New,
            line: Some(3),
            original_line: Some(3),
            outdated: false,
            resolved: false,
            comments: vec![Comment {
                author: "dave".into(),
                body: "Why?".into(),
                ..Default::default()
            }],
        });
        r.update(Message::Reply("100".into()));
        r.update(Message::Save);
        assert!(r.confirm.is_none());
        type_text(&mut r, "Because");
        r.update(Message::Save);
        assert!(matches!(&r.confirm, Some(Post::Reply { thread, body }) if thread.id == "100" && body == "Because"));
        assert_eq!(r.thread_count("a.rs"), 1);
    }

    #[test]
    fn pull_requests_become_picker_entries() {
        let remote = meld_forge::remote::parse_remote("origin", "https://gitlab.gnome.org/GNOME/meld.git").unwrap();
        let pr = PullRequest {
            number: 5,
            title: "Port".into(),
            author: "zed".into(),
            ..Default::default()
        };
        let entry = picker_entry(&remote, &pr, 0.0);
        assert_eq!(entry.kind, EntryKind::PullRequest);
        assert_eq!(entry.section, "Merge requests");
        assert_eq!(entry.label, "!5 Port");
        assert_eq!(entry.revision, Revision::commit("refs/meld/pr/origin/5/head"));
        assert!(entry.search_text.contains("zed"));
        let r = review();
        assert!(r.matches(
            &Revision::commit("b".repeat(40)),
            &Revision::commit("refs/meld/pr/github/12/head")
        ));
        assert_eq!(r.place(), "pull request #12 of github.com/melandur/meld");
    }
}
