//! Reviewing a pull or merge request in Git Compare ([`meld_forge`]).
//!
//! When a tab compares a pull request (its merge base → its head, fetched
//! into `refs/meld/pr/…`), [`PrReview`] holds what the forge said about
//! it and what the user is writing:
//!
//! - the user's pending line comments, posted with the review
//! - the composer for a line comment or the review, and the confirmation
//!   that always comes before anything is posted ([`PrReview::dialog`]).
//!   Nothing reaches the forge without the user pressing Post in that
//!   confirmation.

use iced::widget::{button, column, container, scrollable, text, text_editor};
use iced::{Background, Border, Element, Length, Theme};
use meld_ailens::picker::EntryKind;
use meld_ailens::{PickerEntry, Revision, Side};
use meld_forge::fetch::pr_ref;
use meld_forge::{
    DraftComment, FetchedPullRequest, Forge, ForgeKind, ForgeRemote, PullRequest,
    PullRequestDetails, ReviewEvent, ReviewSubmission,
};

use super::whycolumn::shorten;
use crate::ui;

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
    Review,
}

pub struct PrReview {
    pub forge: Forge,
    pub details: PullRequestDetails,
    pub fetched: FetchedPullRequest,
    /// Line comments written here, posted with the review.
    pub drafts: Vec<DraftComment>,
    /// The review's own comment, kept while the composer is closed.
    pub review_body: String,
    pub review_event: ReviewEvent,
    pub composer: Option<Composer>,
    /// The confirmation shown before posting.
    pub confirm: Option<ReviewSubmission>,
    pub posting: bool,
    /// The composer's editor, to focus it when it opens.
    pub editor_id: iced::widget::Id,
}

#[derive(Debug, Clone)]
pub enum Message {
    OpenInBrowser,
    /// Comment on the line with the cursor (the tab knows where it is).
    AddComment,
    StartReview,
    Editor(text_editor::Action),
    SetEvent(ReviewEvent),
    /// Save the line comment, or ask to confirm the review.
    Save,
    CancelComposer,
    /// Post what the confirmation shows.
    ConfirmPost,
    CancelConfirm,
    /// Clicking beside a dialog does nothing (the text stays).
    Ignore,
}

/// What the tab has to do after a message.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    None,
    /// Post this; the user confirmed it.
    Post(ReviewSubmission),
    OpenUrl(String),
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
            drafts: Vec::new(),
            review_body: String::new(),
            review_event: ReviewEvent::Comment,
            composer: None,
            confirm: None,
            posting: false,
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
    pub fn comment_on(
        &mut self,
        path: String,
        original_path: Option<String>,
        side: Side,
        line: u32,
    ) {
        // Editing the pending comment on that line, if there is one
        if let Some(index) = self
            .drafts
            .iter()
            .position(|d| d.path == path && d.side == side && d.line == line)
        {
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
            Message::OpenInBrowser => return Action::OpenUrl(self.url()),
            // Handled by the tab, which knows the cursor
            Message::AddComment => {}
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
                self.confirm = Some(submission);
            }
        }
        Action::None
    }

    /// The forge answered the review: on success the composer and the
    /// pending comments are done; on failure they stay, with the error.
    pub fn posted(&mut self, result: Result<(), String>) {
        self.posting = false;
        self.confirm = None;
        match result {
            Ok(()) => {
                self.drafts.clear();
                self.review_body.clear();
                self.review_event = ReviewEvent::Comment;
                self.composer = None;
            }
            Err(err) => {
                if let Some(composer) = &mut self.composer {
                    composer.problem = Some(err);
                }
            }
        }
    }

    // -- views --

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
                path,
                side,
                line,
                edit,
                ..
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
                    if edit.is_some() {
                        tr!("Save Comment")
                    } else {
                        tr!("Add to Review")
                    },
                )
            }
            ComposerKind::Review => {
                let options = ReviewEvent::ALL
                    .iter()
                    .filter(|e| {
                        **e != ReviewEvent::RequestChanges || self.kind().can_request_changes()
                    })
                    .map(|e| {
                        (
                            crate::i18n::gettext(e.label()).to_string(),
                            Message::SetEvent(*e),
                        )
                    })
                    .collect::<Vec<_>>();
                let selected = ReviewEvent::ALL
                    .iter()
                    .filter(|e| {
                        **e != ReviewEvent::RequestChanges || self.kind().can_request_changes()
                    })
                    .position(|e| *e == composer.event);
                body = body.push(crate::forge::chips(options, selected, 12.0));
                if self.drafts.is_empty() {
                    body = body.push(
                        text(tr!(
                            "No line comments. Click a line and press Comment on Line to add some."
                        ))
                        .size(12)
                        .style(ui::dim_text),
                    );
                } else {
                    let count = self.drafts.len();
                    let mut list = column![
                        text(trn!("{} line comment", "{} line comments", count, count))
                            .size(12)
                            .font(ui::bold())
                    ]
                    .spacing(2);
                    for draft in &self.drafts {
                        list = list.push(
                            text(format!(
                                "{} — {}",
                                draft.location(),
                                shorten(first_line(&draft.body), 80)
                            ))
                            .size(12)
                            .style(ui::dim_text),
                        );
                    }
                    body = body.push(list);
                }
                (
                    tr!("Review {reference}", reference = self.reference()),
                    tr!("Post Review…"),
                )
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
                button(text(tr!("Cancel")))
                    .style(button::secondary)
                    .on_press(Message::CancelComposer)
                    .into(),
                button(text(save)).on_press(Message::Save).into(),
            ],
        )
    }

    fn confirm_dialog<'a>(&'a self, review: &'a ReviewSubmission) -> Element<'a, Message> {
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
        body = body.push(container(scrollable(items)).max_height(360));
        if self.posting {
            body = body.push(
                text(format!("⏳ {}", tr!("Posting…")))
                    .size(13)
                    .style(ui::dim_text),
            );
        }
        let post_label = match review.event {
            ReviewEvent::Comment => tr!("Post Comment"),
            ReviewEvent::Approve => tr!("Post Approval"),
            ReviewEvent::RequestChanges => tr!("Post Request for Changes"),
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
    let description = [
        crate::i18n::pgettext("pull request state", pr.state_label()).to_string(),
        crate::forge::pr_description(pr, now),
    ]
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
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim()
}

fn quote<'a, M: 'a>(body: String) -> Element<'a, M> {
    container(text(body).size(13).wrapping(text::Wrapping::WordOrGlyph))
        .width(Length::Fill)
        .padding([4, 10])
        .style(|theme: &Theme| container::Style {
            background: Some(Background::Color(
                theme.extended_palette().background.weak.color,
            )),
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

#[cfg(test)]
mod tests {
    use super::*;
    use meld_forge::{PrState, ScriptedRunner};
    use std::sync::Arc;

    fn review() -> PrReview {
        let remote =
            meld_forge::remote::parse_remote("github", "git@github.com:melandur/meld.git").unwrap();
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
            review.update(Message::Editor(text_editor::Action::Edit(
                text_editor::Edit::Insert(c),
            )));
        }
    }

    #[test]
    fn line_comments_are_kept_as_drafts_without_posting() {
        let mut r = review();
        r.comment_on("a.rs".into(), None, Side::New, 4);
        assert_eq!(r.update(Message::Save), Action::None);
        assert!(
            r.composer.as_ref().unwrap().problem.is_some(),
            "empty comments are refused"
        );
        type_text(&mut r, "Why?");
        assert_eq!(r.update(Message::Save), Action::None);
        assert!(r.composer.is_none() && r.confirm.is_none());
        assert_eq!(r.drafts.len(), 1);
        assert_eq!(r.drafts[0].body, "Why?");
        // The same line edits the draft
        r.comment_on("a.rs".into(), None, Side::New, 4);
        assert_eq!(editor_text(&r.composer.as_ref().unwrap().editor), "Why?");
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
        let Some(submission) = r.confirm.clone() else {
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
        assert_eq!(action, Action::Post(submission));
        // Pressing Post twice posts once
        assert_eq!(r.update(Message::ConfirmPost), Action::None);
        r.posted(Ok(()));
        assert!(r.drafts.is_empty() && r.composer.is_none() && r.confirm.is_none());
    }

    #[test]
    fn failed_posts_keep_the_text_and_say_why() {
        let mut r = review();
        r.update(Message::StartReview);
        type_text(&mut r, "Hmm");
        r.update(Message::Save);
        let Action::Post(_) = r.update(Message::ConfirmPost) else {
            panic!("not posted");
        };
        r.posted(Err("HTTP 422".into()));
        let composer = r.composer.as_ref().unwrap();
        assert_eq!(composer.problem.as_deref(), Some("HTTP 422"));
        assert_eq!(editor_text(&composer.editor), "Hmm");
        // Closing the composer keeps the review's comment for later
        r.update(Message::CancelComposer);
        assert_eq!(r.review_body, "Hmm");
    }

    #[test]
    fn pull_requests_become_picker_entries() {
        let remote =
            meld_forge::remote::parse_remote("origin", "https://gitlab.gnome.org/GNOME/meld.git")
                .unwrap();
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
        assert_eq!(
            entry.revision,
            Revision::commit("refs/meld/pr/origin/5/head")
        );
        assert!(entry.search_text.contains("zed"));
        let r = review();
        assert!(r.matches(
            &Revision::commit("b".repeat(40)),
            &Revision::commit("refs/meld/pr/github/12/head")
        ));
        assert_eq!(r.place(), "pull request #12 of github.com/melandur/meld");
    }
}
