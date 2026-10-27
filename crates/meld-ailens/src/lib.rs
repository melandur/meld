//! meld-ailens: the GTK-free AI Lens core behind Meld's Git Compare tab
//!
//! Replaces the Python package `meld/ailens/` (every module but `worker`,
//! whose role the app's own jobs take, and the build files, which Meld
//! doesn't write; one Rust module each: `attribution`, `cards`,
//! `claude`, `comparison`, `explanation`, `git`, `ignore`, `infer`,
//! `linemap`, `models`, `parsers`, `picker`, `process`, `session`,
//! `uncommitted`) and the GTK-free `build_file_tree` of
//! `meld/gitcompare.py` (in [`session`]).
//!
//! Git Compare compares two revisions of a git repository (branches, tags,
//! commits, the staged or unstaged changes, or another worktree's files).
//! Each change gets a card in a Why column saying what it does and how
//! much it needs checking, line by line: from labeled commit messages, git
//! notes in `refs/notes/ai-lens`, or the plain message. Explain asks Claude Code (`claude -p`) for missing
//! reasons, with a justification of each (why it is the right change: its
//! evidence and the alternatives it rules out), and saves them as notes
//! (`Justification:` after each `Why:`). Notes are byte-for-byte compatible
//! with the AI Lens VS Code extension.
//!
//! # What the Git Compare UI uses
//!
//! Everything is synchronous and blocking; run it on background tasks.
//! Long operations take a [`CancelToken`] (cancelling kills the running
//! `git` or `claude` process and returns [`Error::Cancelled`]) and report
//! progress through a callback (send it to a channel).
//!
//! - [`RepositoryInfo`]: one per tab.
//!   - `RepositoryInfo::open(path: &Path, git: &GitOptions, cancel) -> Result<RepositoryInfo>`
//!     (wrap it in an `Arc`), `display_name()`, `watch_paths() -> Vec<PathBuf>`
//!     (files to monitor for reloads).
//!   - Picker data: `picker_entries(PickerSide, cancel) -> Vec<PickerEntry>`
//!     (HEAD, Working Tree / Unstaged / Staged per worktree, branches,
//!     remotes, tags, recent commits), `search_commits(query, cancel)`,
//!     `worktree_heads()`. Filter with [`picker::filter_entries`] and apply a
//!     pick with [`picker::apply_pick`]; [`picker::revisions_from_args`]
//!     parses `meld --git --base … --target …`.
//! - [`CompareSession`]: one per resolved comparison.
//!   - `CompareSession::open(Arc<RepositoryInfo>, &Revision, &Revision, SessionConfig, cancel)`
//!   - Header: `base_label()`, `target_label()`, `base_tooltip()`,
//!     `target_tooltip()`, `relation_text()`, `follows_repository_changes()`.
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
//!     `explain_all(cancel, Some(&progress)) -> usize`, its progress an
//!     [`ExplainProgress`].
//! - [`SessionConfig`]: the settings (Claude path, infer on/off, infer
//!   model, notes ref), filled by the
//!   app from its preferences; `run_claude` swaps the Claude Code client.
//!
//! Line numbers in the models are 1-based and ranges inclusive, as git and
//! the notes have them.

pub mod attribution;
pub mod cards;
pub mod claude;
pub mod comparison;
pub mod error;
pub mod explanation;
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

pub use cards::{Card, CardKind, CardStep, ExplainAction};
pub use claude::{ClaudeRequest, ClaudeResponse, RunClaude};
pub use error::{ClaudeErrorReason, Error, Result};
pub use git::{GitOptions, GitRepository};
pub use models::*;
pub use picker::{PickerEntry, PickerSide};
pub use process::CancelToken;
pub use session::{
    CompareSession, ExplainProgress, FileEntry, FileRow, FileView, RepositoryInfo, SessionConfig,
    build_file_tree,
};
