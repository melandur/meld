//! One client for a remote's forge, whichever it is

use std::fmt;
use std::sync::Arc;

use meld_ailens::CancelToken;

use crate::error::{Error, Result};
use crate::models::*;
use crate::runner::Runner;
use crate::{github, gitlab};

/// Talks to the forge of one remote through its tool
#[derive(Clone)]
pub struct Forge {
    pub remote: ForgeRemote,
    runner: Arc<dyn Runner>,
}

impl fmt::Debug for Forge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Forge")
            .field("remote", &self.remote)
            .finish_non_exhaustive()
    }
}

impl Forge {
    pub fn new(remote: ForgeRemote, runner: Arc<dyn Runner>) -> Self {
        Self { remote, runner }
    }

    pub fn kind(&self) -> ForgeKind {
        self.remote.kind
    }

    /// Up to `limit` pull requests in `filter`'s state matching `search`,
    /// most recently updated first; a number (`12`, `#12`, `!12`) finds
    /// that one
    pub fn list(
        &self,
        filter: StateFilter,
        search: &str,
        limit: usize,
        cancel: &CancelToken,
    ) -> Result<Vec<PullRequest>> {
        match self.kind() {
            ForgeKind::GitHub => {
                github::list(&*self.runner, &self.remote, filter, search, limit, cancel)
            }
            ForgeKind::GitLab => {
                gitlab::list(&*self.runner, &self.remote, filter, search, limit, cancel)
            }
        }
    }

    pub fn details(&self, number: u64, cancel: &CancelToken) -> Result<PullRequestDetails> {
        match self.kind() {
            ForgeKind::GitHub => github::details(&*self.runner, &self.remote, number, cancel),
            ForgeKind::GitLab => gitlab::details(&*self.runner, &self.remote, number, cancel),
        }
    }

    /// The conversation and the line comments
    pub fn discussion(
        &self,
        details: &PullRequestDetails,
        cancel: &CancelToken,
    ) -> Result<Discussion> {
        match self.kind() {
            ForgeKind::GitHub => {
                github::discussion(&*self.runner, &self.remote, details.pr.number, cancel)
            }
            ForgeKind::GitLab => gitlab::discussion(&*self.runner, &self.remote, details, cancel),
        }
    }

    /// Posts a review. Only call this after the user confirmed exactly
    /// this review.
    pub fn submit_review(
        &self,
        details: &PullRequestDetails,
        review: &ReviewSubmission,
        cancel: &CancelToken,
    ) -> Result<()> {
        match self.kind() {
            ForgeKind::GitHub => {
                github::submit_review(&*self.runner, &self.remote, details, review, cancel)
            }
            ForgeKind::GitLab => {
                gitlab::submit_review(&*self.runner, &self.remote, details, review, cancel)
            }
        }
    }

    /// Posts a reply to a thread. Only call this after the user confirmed
    /// exactly this reply.
    pub fn reply(
        &self,
        details: &PullRequestDetails,
        thread: &ReviewThread,
        body: &str,
        cancel: &CancelToken,
    ) -> Result<()> {
        match self.kind() {
            ForgeKind::GitHub => {
                github::reply(&*self.runner, &self.remote, details, thread, body, cancel)
            }
            ForgeKind::GitLab => {
                gitlab::reply(&*self.runner, &self.remote, details, thread, body, cancel)
            }
        }
    }
}

/// Which of `forges` to show first: the first that has pull requests,
/// else the first whose tool works, else the first
///
/// None when there are none (or it was cancelled).
pub fn default_remote(forges: &[Forge], cancel: &CancelToken) -> Option<usize> {
    let mut working = None;
    for (index, forge) in forges.iter().enumerate() {
        match forge.list(StateFilter::All, "", 1, cancel) {
            Ok(prs) if !prs.is_empty() => return Some(index),
            Ok(_) => {
                working.get_or_insert(index);
            }
            Err(Error::Cancelled) => return None,
            Err(err) => log::info!("{}: {err}", forge.remote.name),
        }
    }
    working.or((!forges.is_empty()).then_some(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::parse_remote;
    use crate::runner::ScriptedRunner;

    #[test]
    fn the_default_remote_is_the_first_with_pull_requests() {
        let gitlab = parse_remote("origin", "https://gitlab.gnome.org/GNOME/meld.git").unwrap();
        let github = parse_remote("github", "git@github.com:melandur/meld.git").unwrap();
        let empty = parse_remote("fork", "git@github.com:me/fork.git").unwrap();
        let runner: Arc<dyn Runner> = Arc::new(
            ScriptedRunner::new()
                .fail(
                    "merge_requests",
                    Error::ToolMissing {
                        program: "glab".into(),
                        cli: "GitLab CLI".into(),
                        nouns: "merge requests".into(),
                    },
                )
                .when("me/fork", "[]")
                .when(
                    "melandur/meld",
                    r#"[{"number":1,"title":"x","state":"OPEN"}]"#,
                ),
        );
        let forges = |remotes: Vec<ForgeRemote>| -> Vec<Forge> {
            remotes
                .into_iter()
                .map(|r| Forge::new(r, runner.clone()))
                .collect()
        };
        let cancel = CancelToken::new();
        assert_eq!(
            default_remote(
                &forges(vec![gitlab.clone(), empty.clone(), github.clone()]),
                &cancel
            ),
            Some(2)
        );
        // Nobody has any: the first that answered
        assert_eq!(
            default_remote(&forges(vec![gitlab.clone(), empty]), &cancel),
            Some(1)
        );
        assert_eq!(default_remote(&forges(vec![gitlab]), &cancel), Some(0));
        assert_eq!(default_remote(&[], &cancel), None);
    }
}
