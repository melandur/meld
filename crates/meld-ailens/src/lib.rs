//! meld-ailens: the GTK-free AI Lens core behind Meld's Git Compare tab
//!
//! Replaces the Python package `meld/ailens/` (every module, one Rust
//! module each: `agent`, `agent_setup`, `attribution`, `build_facts`,
//! `build_file`, `build_index`, `build_narrative`, `cards`, `claude`,
//! `comparison`, `explanation`, `generate_build`, `git`, `ignore`, `infer`,
//! `linemap`, `models`, `parsers`, `picker`, `process`, `session`,
//! `uncommitted`, `worker`) and the GTK-free `build_file_tree` of
//! `meld/gitcompare.py` (in [`session`]).
//!
//! Git Compare compares two revisions of a git repository (branches, tags,
//! commits, the staged or unstaged changes, or another worktree's files).
//! Each change gets a card in a Why column saying which commit made it and
//! why: from labeled commit messages, git notes in `refs/notes/ai-lens`, or
//! the plain message. Explain asks Claude Code (`claude -p`) for missing
//! reasons, with a justification of each (why it is the right change: its
//! evidence and the alternatives it rules out), and saves them as notes
//! (`Justification:` after each `Why:`); Explain & Build writes
//! `agent/<base>_<target>_build.md`. Notes and build files are byte-for-byte
//! compatible with the AI Lens VS Code extension.
//!
//! # What the Git Compare UI uses
//!
//! Everything is synchronous and blocking; run it on background tasks.
//! Long operations take a [`CancelToken`] (cancelling kills the running
//! `git` or `claude` process and returns [`Error::Cancelled`]) and report
//! progress through a `&dyn Fn(&str)` callback (send it to a channel).
//!
//! - [`RepositoryInfo`]: one per tab.
//!   - `RepositoryInfo::open(path: &Path, git: &GitOptions, cancel) -> Result<RepositoryInfo>`
//!     (wrap it in an `Arc`), `display_name()`, `watch_paths() -> Vec<PathBuf>`
//!     (files to monitor for reloads), `set_up_agents(&SessionConfig, cancel)`
//!     (CLAUDE.md section and `/why` skill), `build_folder(&SessionConfig)`.
//!   - Picker data: `picker_entries(PickerSide, cancel) -> Vec<PickerEntry>`
//!     (HEAD, Working Tree / Unstaged / Staged per worktree, branches,
//!     remotes, tags, recent commits), `search_commits(query, cancel)`,
//!     `worktree_heads()`. Filter with [`picker::filter_entries`] and apply a
//!     pick with [`picker::apply_pick`]; [`picker::revisions_from_args`]
//!     parses `meld --git --base … --target …`.
//! - [`CompareSession`]: one per resolved comparison.
//!   - `CompareSession::open(Arc<RepositoryInfo>, &Revision, &Revision, SessionConfig, cancel)`
//!   - Header: `base_label()`, `target_label()`, `base_tooltip()`,
//!     `target_tooltip()`, `relation_text()`, `can_build()`,
//!     `build_unavailable_reason()`, `follows_repository_changes()`.
//!   - File list: `files(cancel) -> Vec<FileEntry>` (a [`GitFileChange`] with
//!     status letter, paths, untracked flag, and line counts);
//!     [`build_file_tree`] turns it into folder rows.
//!   - A file: `file_view(&GitFileChange, cancel) -> FileView` with the
//!     left/right contents (`base_text`/`target_text`, bytes), labels, the
//!     on-disk path when editable, a message instead of the comparison
//!     (binary, too large, submodule), and `cards: Vec<Card>`. Each
//!     [`Card`] has 1-based inclusive `added` (target) and `deleted` (base)
//!     line ranges and [`Card::anchor`] (side and line it sits level with);
//!     [`cards::layout_cards`] stacks them without overlap.
//!   - Explain: `explain_card(&FileView, key, cancel)` then
//!     `cards(&FileView, cancel)` for the refreshed cards;
//!     `explain_all(cancel, Some(&progress), commits_only) -> usize`.
//!   - Explain & Build: `generate_build(cancel, Some(&progress)) -> PathBuf`.
//!   - `commit_message(sha, cancel)` for a card's full commit.
//! - [`SessionConfig`]: the settings (Claude path, infer on/off, infer and
//!   build models, build folder, notes ref, author pattern), filled by the
//!   app from its preferences; `run_claude` swaps the Claude Code client.
//! - [`worker::Worker`]: an optional thread pool that runs jobs with their
//!   own tokens, keeps only the newest result of an exclusive job, and hands
//!   results to a dispatch function (e.g. [`worker::channel_dispatch`]).
//!
//! Line numbers in the models are 1-based and ranges inclusive, as git, the
//! notes and the build files have them.

pub mod agent;
pub mod agent_setup;
pub mod attribution;
pub mod build_facts;
pub mod build_file;
pub mod build_index;
pub mod build_narrative;
pub mod cards;
pub mod claude;
pub mod comparison;
pub mod error;
pub mod explanation;
pub mod generate_build;
pub mod git;
pub mod ignore;
pub mod infer;
pub mod linemap;
pub mod models;
pub mod parsers;
pub mod picker;
pub mod process;
pub mod session;
pub mod time;
pub mod uncommitted;
pub mod worker;

pub use cards::{Card, CardKind, ExplainAction};
pub use claude::{ClaudeRequest, ClaudeResponse, RunClaude};
pub use error::{ClaudeErrorReason, Error, Result};
pub use git::{GitOptions, GitRepository};
pub use models::*;
pub use picker::{PickerEntry, PickerSide};
pub use process::CancelToken;
pub use session::{
    CompareSession, FileEntry, FileRow, FileView, RepositoryInfo, SessionConfig, build_file_tree,
};
