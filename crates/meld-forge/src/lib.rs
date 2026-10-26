//! meld-forge: pull requests (GitHub) and merge requests (GitLab) for Git
//! Compare
//!
//! New in the Rust port; no Python module corresponds. Talks to the forges
//! only through their command-line tools, `gh` and `glab`, which keep the
//! user's login; never through the network directly.
//!
//! Everything is synchronous and blocking; run it on background tasks.
//! Calls take a [`CancelToken`] (cancelling kills the running tool or git).
//!
//! # API
//!
//! - Remotes: [`remote::forge_remotes`]`(repo, cancel) -> Vec<ForgeRemote>`
//!   (the repository's remotes on github.com / `github.*` hosts and on hosts
//!   named `gitlab…`, in configuration order); [`remote::parse_remote`].
//! - [`Forge`]: `Forge::new(ForgeRemote, Arc<dyn Runner>)`, then
//!   - `list(StateFilter, search, limit, cancel) -> Vec<PullRequest>`
//!   - `details(number, cancel) -> PullRequestDetails` (state, labels, base
//!     and merge commits)
//!   - `submit_review(&details, &ReviewSubmission, cancel)` (Comment,
//!     Approve or Request changes, with [`DraftComment`]s on lines)
//!
//!   Posting only happens through `submit_review`; callers
//!   must have the user confirm exactly what is posted first.
//! - [`default_remote`]: the first remote with pull requests.
//! - Fetching: [`fetch::fetch_pull_request`]`(repo, remote, &details,
//!   cancel, progress) -> FetchedPullRequest` fetches the head (and base
//!   branch) into `refs/meld/pr/<remote>/<n>/…` ([`fetch::pr_ref`],
//!   [`fetch::parse_pr_ref`]) and finds the merge base to compare with.
//! - Runners: [`ProcessRunner`] runs the real tools; [`ScriptedRunner`]
//!   answers from a script and records calls, for tests.
//!
//! Line numbers are 1-based, as the forges count them.

pub mod error;
pub mod fetch;
pub mod forge;
pub mod github;
pub mod gitlab;
pub mod json;
pub mod models;
pub mod remote;
pub mod runner;

pub use error::{Error, Result};
pub use forge::{Forge, default_remote};
pub use meld_ailens::CancelToken;
pub use models::*;
pub use runner::{ProcessRunner, Runner, ScriptedRunner};
