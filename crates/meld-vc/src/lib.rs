//! Version control backends for Meld.
//!
//! Replaces the Python modules `meld/vc/__init__.py`, `meld/vc/_vc.py`,
//! `meld/vc/_null.py`, `meld/vc/git.py`, `meld/vc/mercurial.py`,
//! `meld/vc/svn.py`, `meld/vc/bzr.py`, `meld/vc/cvs.py`,
//! `meld/vc/darcs.py`, the toolkit-independent parts of `meld/vcview.py`,
//! and the commit dialog logic of `meld/ui/vcdialogs.py` /
//! `meld/ui/historyentry.py`.
//!
//! Everything is synchronous and `Send`; calls that spawn VC programs
//! block, so the application runs them on background tasks.
//!
//! # Public API
//!
//! **Types** ([`entry`]): [`State`] (the `STATE_*` constants, same order),
//! [`ConflictSide`] (`CONFLICT_*`), [`Action`] (`"compare"`, `"commit"`,
//! ...), [`Entry`] (`path`, `name`, `state`, `isdir`, `options`) with the
//! filter predicates `is_modified`/`is_normal`/`is_nonvc`/`is_ignored`.
//! Errors: [`VcError`] / [`Result`].
//!
//! **Discovery** ([`vc`]): [`VcKind`] (`name`, `cmd`, `vc_dir`,
//! `is_in_repo`, `find_repo_root`, `check_repo_root`, `is_installed`,
//! `valid_repo`, `open`), [`VC_PLUGINS`] (preference order),
//! [`get_vcs`]`(location) -> Vec<VcCandidate>` and [`open_vc`].
//!
//! **Backends**: the [`Vc`] trait, implemented by [`Git`], [`Mercurial`],
//! [`Bazaar`], [`Subversion`], [`Darcs`], [`Cvs`] and [`NullVc`]:
//! - state: `root()`, `location()`, `refresh_vc_state(Option<&Path>)`,
//!   `get_entries(dir) -> Result<Vec<Entry>>`, `get_entry(path) -> Entry`;
//! - files: `get_path_for_repo_file(path, commit) -> Result<PathBuf>` (base
//!   revision in a temp file), `get_path_for_conflict(path, side) ->
//!   Result<(PathBuf, is_temp)>`;
//! - commit info: `get_files_to_commit`, `get_commit_message_prefill`,
//!   `get_commits_to_push_summary`;
//! - actions: `get_valid_actions(&[(PathBuf, State)]) -> BTreeSet<Action>`,
//!   `supports(Action)`, and command builders `update`, `push`,
//!   `commit(files, message)`, `add`, `remove`, `revert`, `resolve`,
//!   `unstage` (Git), `action_commands(action, files, message)`, each
//!   returning `Vec<`[`VcCommand`]`>`.
//!
//! **Commands** ([`command`]): [`VcCommand`] (`program`, `args`, `files`,
//! `working_dir`, `refresh`) with `console_line()`, `argv()`,
//! `to_std_command()` and `run() -> Result<CommandOutput>`; [`run_all`].
//!
//! **View logic** ([`view`]): [`StatusFilters`] (`vc-status-filters`),
//! [`scan_tree`] / [`scan_tree_with_progress`] building a [`VcRow`] tree
//! (recursive or flattened), [`choose_vcs`] (backend chooser with
//! "(not found)" etc. labels, default selection and null fallback),
//! [`action_sensitivity`], [`prepare_diff`] building a [`DiffRequest`]
//! (base comparison honouring `vc-left-is-local`, 3-way conflicts honouring
//! `vc-merge-file-order`), [`TempFiles`] cleanup, [`auto_compare_paths`].
//!
//! **Commit dialog** ([`commit`]): [`files_to_commit_display`],
//! [`finish_commit_message`] / [`wrap_commit_message`] / [`commit::fill`]
//! (`textwrap.fill` port) and [`MessageHistory`] ("Previous logs",
//! persisted in `history.ini`).

pub mod bzr;
pub mod command;
pub mod commit;
pub mod cvs;
pub mod darcs;
pub mod entry;
pub mod error;
pub mod git;
pub mod mercurial;
pub mod null;
pub mod paths;
pub mod svn;
pub mod vc;
pub mod view;

pub use bzr::Bazaar;
pub use command::{CommandOutput, VcCommand, run_all};
pub use commit::{
    CommitSettings, MessageHistory, files_to_commit_display, finish_commit_message,
    wrap_commit_message,
};
pub use cvs::Cvs;
pub use darcs::Darcs;
pub use entry::{Action, ConflictSide, Entry, State};
pub use error::{Result, VcError};
pub use git::Git;
pub use mercurial::Mercurial;
pub use null::NullVc;
pub use svn::Subversion;
pub use vc::{TreeCache, VC_PLUGINS, Vc, VcCandidate, VcCore, VcKind, get_vcs, open_vc};
pub use view::{
    ActionSensitivity, DiffOptions, DiffRequest, DiffSide, MergeFileOrder, StatusFilters,
    TempFiles, VcChoice, VcChoices, VcRow, action_sensitivity, auto_compare_paths, choose_vcs,
    prepare_diff, scan_tree, scan_tree_with_progress,
};

#[cfg(test)]
mod send_tests {
    #[test]
    fn backends_are_send() {
        fn is_send<T: Send>() {}
        is_send::<Box<dyn super::Vc>>();
        is_send::<super::VcCommand>();
        is_send::<super::VcError>();
    }
}
