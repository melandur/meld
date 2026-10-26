//! Pull requests, their comments and what can be posted to them
//!
//! Line numbers are 1-based, as the forges count them. Dates are ISO 8601
//! as the forges send them.

pub use meld_ailens::Side;

/// Which forge hosts a remote
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForgeKind {
    GitHub,
    GitLab,
}

impl ForgeKind {
    /// The command-line tool that talks to it
    pub fn tool(self) -> &'static str {
        match self {
            ForgeKind::GitHub => "gh",
            ForgeKind::GitLab => "glab",
        }
    }

    pub fn cli_name(self) -> &'static str {
        match self {
            ForgeKind::GitHub => "GitHub CLI (gh)",
            ForgeKind::GitLab => "GitLab CLI (glab)",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ForgeKind::GitHub => "GitHub",
            ForgeKind::GitLab => "GitLab",
        }
    }

    pub fn nouns(self) -> &'static str {
        match self {
            ForgeKind::GitHub => "pull requests",
            ForgeKind::GitLab => "merge requests",
        }
    }

    /// How the forge writes a number: `#12` or `!12`
    pub fn reference(self, number: u64) -> String {
        match self {
            ForgeKind::GitHub => format!("#{number}"),
            ForgeKind::GitLab => format!("!{number}"),
        }
    }

    /// Whether a review can request changes
    pub fn can_request_changes(self) -> bool {
        self == ForgeKind::GitHub
    }
}

/// A git remote hosted on a forge Meld can talk to
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ForgeRemote {
    /// The remote's name, e.g. `origin`
    pub name: String,
    pub url: String,
    pub kind: ForgeKind,
    /// e.g. `github.com`, `gitlab.gnome.org`
    pub host: String,
    /// `owner/repo`, or a GitLab project path `group/sub/project`
    pub path: String,
}

impl ForgeRemote {
    /// e.g. `github · melandur/meld`
    pub fn label(&self) -> String {
        format!("{} · {}", self.name, self.path)
    }

    /// The project's web page
    pub fn web_url(&self) -> String {
        format!("https://{}/{}", self.host, self.path)
    }

    /// The pull request's web page
    pub fn pr_url(&self, number: u64) -> String {
        match self.kind {
            ForgeKind::GitHub => format!("{}/pull/{number}", self.web_url()),
            ForgeKind::GitLab => format!("{}/-/merge_requests/{number}", self.web_url()),
        }
    }
}

/// Which pull requests a list shows
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StateFilter {
    #[default]
    Open,
    Closed,
    Merged,
    All,
}

impl StateFilter {
    pub const ALL: [StateFilter; 4] = [
        StateFilter::Open,
        StateFilter::Closed,
        StateFilter::Merged,
        StateFilter::All,
    ];

    pub fn label(self) -> &'static str {
        match self {
            StateFilter::Open => "Open",
            StateFilter::Closed => "Closed",
            StateFilter::Merged => "Merged",
            StateFilter::All => "All",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PrState {
    #[default]
    Open,
    Closed,
    Merged,
}

/// A pull request as lists show it
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub state: PrState,
    pub draft: bool,
    pub updated_at: String,
    pub url: String,
    /// The branch with the changes
    pub head_branch: String,
    /// The branch it is to be merged into
    pub base_branch: String,
    /// The head commit, when the forge says
    pub head_sha: String,
    pub labels: Vec<String>,
}

impl PullRequest {
    /// Open, Draft, Closed or Merged
    pub fn state_label(&self) -> &'static str {
        match self.state {
            PrState::Open if self.draft => "Draft",
            PrState::Open => "Open",
            PrState::Closed => "Closed",
            PrState::Merged => "Merged",
        }
    }

    /// Lower-cased text a filter matches against
    pub fn search_text(&self) -> String {
        format!(
            "#{} !{} {} {} {} {} {} {}",
            self.number,
            self.number,
            self.title,
            self.author,
            self.state_label(),
            self.head_branch,
            self.base_branch,
            self.labels.join(" ")
        )
        .to_lowercase()
    }
}

/// GitLab's commits of a merge request's current diff
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiffRefs {
    /// The merge base
    pub base_sha: String,
    /// The target branch when the diff was made
    pub start_sha: String,
    pub head_sha: String,
}

/// Everything about a pull request but its comments
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PullRequestDetails {
    pub pr: PullRequest,
    /// The base branch's commit the forge compares with (GitHub), or the
    /// merge base (GitLab)
    pub base_sha: Option<String>,
    /// The commit that merged it, for merged pull requests
    pub merge_commit: Option<String>,
    pub diff_refs: Option<DiffRefs>,
}

/// A line comment written in Meld, posted with the review
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftComment {
    /// The file's path in the head version
    pub path: String,
    /// Its path in the base version, when it was renamed
    pub original_path: Option<String>,
    pub side: Side,
    pub line: u32,
    pub body: String,
}

impl DraftComment {
    /// e.g. `src/main.rs line 12 (new)`
    pub fn location(&self) -> String {
        let side = match self.side {
            Side::Old => "old",
            Side::New => "new",
        };
        format!("{} line {} ({side})", self.path, self.line)
    }
}

/// How a review ends
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReviewEvent {
    Comment,
    Approve,
    RequestChanges,
}

impl ReviewEvent {
    pub const ALL: [ReviewEvent; 3] = [
        ReviewEvent::Comment,
        ReviewEvent::Approve,
        ReviewEvent::RequestChanges,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ReviewEvent::Comment => "Comment",
            ReviewEvent::Approve => "Approve",
            ReviewEvent::RequestChanges => "Request changes",
        }
    }

    /// GitHub's name for it
    pub fn github_event(self) -> &'static str {
        match self {
            ReviewEvent::Comment => "COMMENT",
            ReviewEvent::Approve => "APPROVE",
            ReviewEvent::RequestChanges => "REQUEST_CHANGES",
        }
    }
}

/// A review to post: an overall comment, line comments and a verdict
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSubmission {
    pub event: ReviewEvent,
    pub body: String,
    pub comments: Vec<DraftComment>,
}

impl ReviewSubmission {
    /// Why the review can't be posted as it is, if it can't
    pub fn problem(&self, kind: ForgeKind) -> Option<String> {
        if self.comments.iter().any(|c| c.body.trim().is_empty()) {
            return Some("A line comment is empty.".into());
        }
        if self.comments.iter().any(|c| c.line == 0) {
            return Some("A line comment has no line.".into());
        }
        match self.event {
            ReviewEvent::Comment if self.body.trim().is_empty() && self.comments.is_empty() => {
                Some("Write a comment or add line comments first.".into())
            }
            ReviewEvent::RequestChanges if !kind.can_request_changes() => Some(format!(
                "{} merge requests can’t request changes from here; comment instead.",
                kind.name()
            )),
            ReviewEvent::RequestChanges if self.body.trim().is_empty() => {
                Some("Say what should change in the review’s comment.".into())
            }
            _ => None,
        }
    }

    /// One line saying what posting does, e.g. "Approve with 2 line comments"
    pub fn summary(&self) -> String {
        let comments = match self.comments.len() {
            0 => String::new(),
            1 => " with 1 line comment".into(),
            n => format!(" with {n} line comments"),
        };
        let what = match self.event {
            ReviewEvent::Comment => "Comment",
            ReviewEvent::Approve => "Approve",
            ReviewEvent::RequestChanges => "Request changes",
        };
        format!("{what}{comments}")
    }
}

/// A pull request's head and the commit to compare it with, fetched into
/// the repository
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedPullRequest {
    /// Where the pull request branched off its base branch
    pub base: String,
    /// The pull request's head commit
    pub head: String,
    /// The private ref that keeps the head, `refs/meld/pr/<remote>/<n>/head`
    pub head_ref: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(body: &str, line: u32) -> DraftComment {
        DraftComment {
            path: "a.rs".into(),
            original_path: None,
            side: Side::New,
            line,
            body: body.into(),
        }
    }

    #[test]
    fn reviews_are_checked_before_posting() {
        let mut review = ReviewSubmission {
            event: ReviewEvent::Comment,
            body: " ".into(),
            comments: Vec::new(),
        };
        assert!(review.problem(ForgeKind::GitHub).is_some());
        review.comments.push(draft("Nice", 3));
        assert_eq!(review.problem(ForgeKind::GitHub), None);
        review.comments.push(draft("", 4));
        assert_eq!(
            review.problem(ForgeKind::GitHub).as_deref(),
            Some("A line comment is empty.")
        );
        review.comments.pop();
        review.event = ReviewEvent::RequestChanges;
        assert!(review.problem(ForgeKind::GitHub).is_some());
        review.body = "Please rename".into();
        assert_eq!(review.problem(ForgeKind::GitHub), None);
        assert!(
            review
                .problem(ForgeKind::GitLab)
                .unwrap()
                .contains("can’t request changes")
        );
        review.event = ReviewEvent::Approve;
        review.body.clear();
        review.comments.clear();
        assert_eq!(review.problem(ForgeKind::GitLab), None);
    }

    #[test]
    fn summaries_count_line_comments() {
        let mut review = ReviewSubmission {
            event: ReviewEvent::Approve,
            body: String::new(),
            comments: vec![draft("x", 1)],
        };
        assert_eq!(review.summary(), "Approve with 1 line comment");
        review.comments.push(draft("y", 2));
        review.event = ReviewEvent::Comment;
        assert_eq!(review.summary(), "Comment with 2 line comments");
        review.comments.clear();
        assert_eq!(review.summary(), "Comment");
    }

    #[test]
    fn states_and_urls() {
        let pr = PullRequest {
            number: 7,
            draft: true,
            ..Default::default()
        };
        assert_eq!(pr.state_label(), "Draft");
        assert!(pr.search_text().contains("#7"));
        let remote = ForgeRemote {
            name: "origin".into(),
            url: String::new(),
            kind: ForgeKind::GitLab,
            host: "gitlab.gnome.org".into(),
            path: "GNOME/meld".into(),
        };
        assert_eq!(
            remote.pr_url(5),
            "https://gitlab.gnome.org/GNOME/meld/-/merge_requests/5"
        );
        assert_eq!(ForgeKind::GitLab.reference(5), "!5");
        assert_eq!(remote.label(), "origin · GNOME/meld");
    }
}
