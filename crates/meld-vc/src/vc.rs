//! The [`Vc`] trait, backend kinds and discovery (port of the `Vc` base
//! class in `meld/vc/_vc.py` and of `meld/vc/__init__.py`).

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::command::{self, VcCommand};
use crate::entry::{Action, ConflictSide, Entry, State};
use crate::error::{Result, VcError};
use crate::paths;
use crate::{bzr, cvs, darcs, git, mercurial, null, svn};

/// The available version control backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum VcKind {
    Git,
    Mercurial,
    Bazaar,
    Subversion,
    Darcs,
    Cvs,
    /// Fallback used when no version control system is found; shows plain
    /// files and compares them against themselves.
    Null,
}

/// Backends ordered by best guess as to which VC a user is likely to want
/// by default in a multiple-VC situation (`VC_PLUGINS`).
pub const VC_PLUGINS: [VcKind; 6] = [
    VcKind::Git,
    VcKind::Mercurial,
    VcKind::Bazaar,
    VcKind::Subversion,
    VcKind::Darcs,
    VcKind::Cvs,
];

impl VcKind {
    /// Displayed name (`Vc.NAME`).
    pub fn name(self) -> &'static str {
        match self {
            VcKind::Git => "Git",
            VcKind::Mercurial => "Mercurial",
            VcKind::Bazaar => "Bazaar",
            VcKind::Subversion => "Subversion",
            VcKind::Darcs => "Darcs",
            VcKind::Cvs => "CVS",
            VcKind::Null => "None",
        }
    }

    /// The command line program (`Vc.CMD`); `None` for [`VcKind::Null`].
    /// CVSNT is a drop-in replacement for CVS and preferred if installed.
    /// Breezy, Bazaar's maintained successor, installs only `brz` unless
    /// packaged with a `bzr` alias; `brz` is used when there is no `bzr`.
    pub fn cmd(self) -> Option<&'static str> {
        static CVS_CMD: OnceLock<&'static str> = OnceLock::new();
        static BZR_CMD: OnceLock<&'static str> = OnceLock::new();
        Some(match self {
            VcKind::Git => "git",
            VcKind::Mercurial => "hg",
            VcKind::Bazaar => BZR_CMD.get_or_init(|| {
                if paths::which("bzr").is_none() && paths::which("brz").is_some() {
                    "brz"
                } else {
                    "bzr"
                }
            }),
            VcKind::Subversion => "svn",
            VcKind::Darcs => "darcs",
            VcKind::Cvs => CVS_CMD.get_or_init(|| {
                if paths::which("cvsnt").is_some() {
                    "cvsnt"
                } else {
                    "cvs"
                }
            }),
            VcKind::Null => return None,
        })
    }

    /// Name of the metadata folder (`Vc.VC_DIR`).
    pub fn vc_dir(self) -> &'static str {
        match self {
            VcKind::Git => ".git",
            VcKind::Mercurial => ".hg",
            VcKind::Bazaar => ".bzr",
            VcKind::Subversion => ".svn",
            VcKind::Darcs => "_darcs",
            VcKind::Cvs => "CVS",
            VcKind::Null => ".",
        }
    }

    /// Whether to walk parents to find the repository root
    /// (`VC_ROOT_WALK`); only CVS doesn't.
    pub fn root_walk(self) -> bool {
        self != VcKind::Cvs
    }

    /// Whether `location` is a repository root of this kind. For Git the
    /// `.git` entry may be a file (worktrees, submodules).
    pub fn check_repo_root(self, location: &Path) -> bool {
        let candidate = location.join(self.vc_dir());
        match self {
            VcKind::Git => candidate.exists(),
            _ => candidate.is_dir(),
        }
    }

    /// Walk upwards from `location` to find the repository root.
    pub fn find_repo_root(self, location: &Path) -> Option<PathBuf> {
        let mut current = Some(location);
        while let Some(loc) = current {
            if loc.as_os_str().is_empty() {
                break;
            }
            if self.check_repo_root(loc) {
                return Some(loc.to_path_buf());
            }
            current = loc.parent();
        }
        None
    }

    /// Returns `(root, location)` where `location` is `path` if it is a
    /// folder, else its parent, and `root` the repository root, if any.
    pub fn is_in_repo(self, path: &Path) -> (Option<PathBuf>, PathBuf) {
        let path = paths::abspath(path);
        let location = if path.is_dir() {
            path
        } else {
            paths::parent_or_self(&path)
        };
        let root = if self.root_walk() {
            self.find_repo_root(&location)
        } else if self.check_repo_root(&location) {
            Some(location.clone())
        } else {
            None
        };
        (root, location)
    }

    /// Whether the backend's program is installed and usable.
    pub fn is_installed(self) -> bool {
        let Some(cmd) = self.cmd() else {
            return false;
        };
        match self {
            VcKind::Git => command::output_text(cmd, &["--version"], None)
                .map(|out| out.starts_with("git version"))
                .unwrap_or(false),
            VcKind::Darcs => command::output_text(cmd, &["--version"], None)
                .map(|out| darcs::version_supported(&out))
                .unwrap_or(false),
            _ => command::call_status(cmd, &[], None).is_some(),
        }
    }

    /// Whether `path` is a valid (usable) repository for this backend.
    pub fn valid_repo(self, path: &Path) -> bool {
        let Some(cmd) = self.cmd() else {
            return false;
        };
        let cwd = Some(path);
        match self {
            VcKind::Git => command::call_ok(cmd, &["branch"], cwd),
            VcKind::Mercurial | VcKind::Bazaar => command::call_ok(cmd, &["root"], cwd),
            VcKind::Subversion => svn::valid_repo(cmd, path),
            // Python passed --no-files, which Darcs 2.18 rejects
            VcKind::Darcs => command::call_ok(cmd, &["show", "repo"], cwd),
            VcKind::Cvs => command::call_ok(cmd, &["ls"], cwd),
            VcKind::Null => false,
        }
    }

    /// Open a backend instance for `path`.
    pub fn open(self, path: &Path) -> Result<Box<dyn Vc>> {
        Ok(match self {
            VcKind::Git => Box::new(git::Git::new(path)?),
            VcKind::Mercurial => Box::new(mercurial::Mercurial::new(path)?),
            VcKind::Bazaar => Box::new(bzr::Bazaar::new(path)?),
            VcKind::Subversion => Box::new(svn::Subversion::new(path)?),
            VcKind::Darcs => Box::new(darcs::Darcs::new(path)?),
            VcKind::Cvs => Box::new(cvs::Cvs::new(path)?),
            VcKind::Null => Box::new(null::NullVc::new(path)),
        })
    }
}

impl fmt::Display for VcKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A backend found (or not) for a location by [`get_vcs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VcCandidate {
    pub kind: VcKind,
    /// Repository root, when `location` is inside a repository of this kind.
    pub root: Option<PathBuf>,
}

impl VcCandidate {
    /// Whether a repository of this kind was found (`enabled`).
    pub fn enabled(&self) -> bool {
        self.root.is_some()
    }
}

/// All backends in [`VC_PLUGINS`] order, each with the repository root
/// found for `location` (if any).
pub fn get_vcs(location: &Path) -> Vec<VcCandidate> {
    VC_PLUGINS
        .iter()
        .map(|&kind| VcCandidate {
            kind,
            root: kind.is_in_repo(location).0,
        })
        .collect()
}

/// Cached version control state for the paths below a location.
#[derive(Debug, Clone, Default)]
pub struct TreeCache {
    /// State per absolute path; paths not present are [`State::Normal`].
    pub states: HashMap<PathBuf, State>,
    /// Extra information per path ("Staged", "Rev 3", ...).
    pub meta: HashMap<PathBuf, String>,
    /// Names of removed/missing entries per parent folder; these do not
    /// exist on disk so folder listings must add them.
    pub missing: HashMap<PathBuf, BTreeSet<String>>,
}

impl TreeCache {
    /// Record `path` as missing from disk if `state` is removed/missing
    /// (`_add_missing_cache_entry`).
    pub fn add_missing_entry(&mut self, path: &Path, state: State) {
        if matches!(state, State::Removed | State::Missing) {
            let folder = paths::parent_or_self(path);
            if let Some(name) = path.file_name() {
                self.missing
                    .entry(folder)
                    .or_default()
                    .insert(name.to_string_lossy().into_owned());
            }
        }
    }

    pub fn state(&self, path: &Path) -> State {
        self.states.get(path).copied().unwrap_or(State::Normal)
    }

    pub fn meta(&self, path: &Path) -> String {
        self.meta.get(path).cloned().unwrap_or_default()
    }
}

/// State shared by every backend instance: where it is and what it knows.
#[derive(Debug, Clone)]
pub struct VcCore {
    /// Repository root.
    pub root: PathBuf,
    /// The requested comparison location; may be a sub-folder of `root`
    /// (or the containing folder of a requested file).
    pub location: PathBuf,
    pub cache: TreeCache,
}

impl VcCore {
    /// Port of `Vc.__init__`: fails if `path` is not in a repository.
    pub fn new(kind: VcKind, path: &Path) -> Result<VcCore> {
        match kind.is_in_repo(path) {
            (Some(root), location) => Ok(VcCore {
                root,
                location,
                cache: TreeCache::default(),
            }),
            (None, _) => Err(VcError::NotInRepo {
                vc: kind.name(),
                path: path.to_path_buf(),
            }),
        }
    }

    /// Resolve a (possibly location-relative) path to an absolute,
    /// normalised path.
    pub fn resolve(&self, path: &Path) -> PathBuf {
        paths::absolutize(&self.location, path)
    }

    /// Strip the repository root (plus separator) from `path`, failing like
    /// Python's `InvalidVCPath` if `path` is not inside it.
    pub fn repo_relative(&self, vc: &'static str, path: &Path) -> Result<String> {
        let abs = paths::abspath(path);
        match abs.strip_prefix(&self.root) {
            Ok(rel) if !rel.as_os_str().is_empty() => Ok(rel.to_string_lossy().replace('\\', "/")),
            _ => Err(VcError::InvalidPath {
                vc,
                path: path.to_path_buf(),
                reason: "Path not in repository".into(),
            }),
        }
    }
}

pub(crate) fn unsupported<T>(kind: VcKind, operation: &'static str) -> Result<T> {
    Err(VcError::Unsupported {
        vc: kind.name(),
        operation,
    })
}

/// The actions valid for every path in `path_states` according to the
/// generic rules of `Vc.get_valid_actions` (before backend filtering).
pub fn base_valid_actions(root: &Path, path_states: &[(PathBuf, State)]) -> BTreeSet<Action> {
    use State::*;
    let mut valid = BTreeSet::new();
    let all = |pred: &dyn Fn(State) -> bool| path_states.iter().all(|(_, s)| pred(*s));

    if !path_states.is_empty() {
        valid.insert(Action::Compare);
    }
    valid.insert(Action::Update);
    valid.insert(Action::Push);

    if all(&|s| !matches!(s, Unversioned | Ignored)) {
        valid.insert(Action::Commit);
    }
    if all(&|s| !matches!(s, Normal | Removed)) {
        valid.insert(Action::Add);
    }
    if all(&|s| s == Conflict) {
        valid.insert(Action::Resolve);
    }
    if all(&|s| !matches!(s, Unversioned | Ignored | Removed))
        && !path_states.iter().any(|(p, _)| p == root)
    {
        valid.insert(Action::Remove);
    }
    if all(&|s| !matches!(s, Unversioned | Normal | Ignored)) {
        valid.insert(Action::Revert);
    }
    valid
}

/// A version control backend bound to a repository location.
///
/// Instances are `Send` and all methods are synchronous; process calls
/// block, so the application should call them from background tasks.
pub trait Vc: Send + fmt::Debug {
    /// Which backend this is.
    fn kind(&self) -> VcKind;
    /// Shared root/location/cache state.
    fn core(&self) -> &VcCore;
    /// Mutable shared state.
    fn core_mut(&mut self) -> &mut VcCore;

    /// Update the cached state of the file(s) at `path`, which is either
    /// relative to [`Vc::location`] (`./` for everything) or absolute.
    fn update_tree_state_cache(&mut self, path: &Path) -> Result<()>;

    /// Actions this backend implements (Python: methods present on the
    /// plugin class).
    fn supported_actions(&self) -> &'static [Action];

    /// A temporary file with the content of `path` at `commit` (the current
    /// base revision when `None`). The caller owns (and should delete) the
    /// file.
    fn get_path_for_repo_file(&self, path: &Path, commit: Option<&str>) -> Result<PathBuf>;

    /// A file for one side of a conflicted `path`, and whether it is a
    /// temporary file the caller should delete.
    fn get_path_for_conflict(&self, _path: &Path, _side: ConflictSide) -> Result<(PathBuf, bool)> {
        unsupported(self.kind(), "conflict files")
    }

    /// Repository-relative files that committing `paths` would commit
    /// (directories expanded, unchanged files excluded), sorted.
    fn get_files_to_commit(&self, _paths: &[PathBuf]) -> Result<Vec<String>> {
        unsupported(self.kind(), "listing files to commit")
    }

    /// A VC-provided pre-filled commit message (e.g. during merges).
    fn get_commit_message_prefill(&self) -> Option<String> {
        None
    }

    /// One-line description of unpushed commits, e.g.
    /// "2 unpushed commits in 3 branches"; empty if nothing to push.
    fn get_commits_to_push_summary(&self) -> Result<String> {
        unsupported(self.kind(), "push summaries")
    }

    fn update(&self) -> Result<Vec<VcCommand>> {
        unsupported(self.kind(), "update")
    }
    fn push(&self) -> Result<Vec<VcCommand>> {
        unsupported(self.kind(), "push")
    }
    fn commit(&self, _files: &[PathBuf], _message: &str) -> Result<Vec<VcCommand>> {
        unsupported(self.kind(), "commit")
    }
    fn add(&self, _files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        unsupported(self.kind(), "add")
    }
    fn remove(&self, _files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        unsupported(self.kind(), "remove")
    }
    fn revert(&self, _files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        unsupported(self.kind(), "revert")
    }
    fn resolve(&self, _files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        unsupported(self.kind(), "resolve")
    }
    fn unstage(&self, _files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        unsupported(self.kind(), "unstage")
    }

    // ----- Provided methods -----

    /// Displayed backend name.
    fn name(&self) -> &'static str {
        self.kind().name()
    }

    /// Repository root.
    fn root(&self) -> &Path {
        &self.core().root
    }

    /// Requested comparison location.
    fn location(&self) -> &Path {
        &self.core().location
    }

    /// Whether this backend implements `action`.
    fn supports(&self, action: Action) -> bool {
        action == Action::Compare || self.supported_actions().contains(&action)
    }

    /// Update cached state: for one `path` (e.g. a file saved in a
    /// comparison) or, with `None`, the whole tree at the location.
    fn refresh_vc_state(&mut self, path: Option<&Path>) -> Result<()> {
        match path {
            Some(p) => self.update_tree_state_cache(p),
            None => {
                let cache = &mut self.core_mut().cache;
                cache.states.clear();
                cache.missing.clear();
                self.update_tree_state_cache(Path::new("./"))
            }
        }
    }

    /// Entries of folder `base`: its children on disk (except the VC
    /// metadata folder) plus removed/missing entries known to the VC that
    /// are not on disk (Python listed those twice). Symlinks are not
    /// followed.
    fn get_entries(&self, base: &Path) -> Result<Vec<Entry>> {
        let base = paths::abspath(base);
        let core = self.core();
        let vc_dir = self.kind().vc_dir();
        let mut entries = Vec::new();
        match std::fs::read_dir(&base) {
            Ok(iter) => {
                for item in iter {
                    let item = item?;
                    let name = item.file_name().to_string_lossy().into_owned();
                    if name == vc_dir {
                        continue;
                    }
                    let path = base.join(item.file_name());
                    let isdir = item.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    entries.push(Entry::new(
                        path.clone(),
                        name,
                        core.cache.state(&path),
                        isdir,
                        core.cache.meta(&path),
                    ));
                }
            }
            Err(e) if e.kind() == ErrorKind::PermissionDenied => {
                log::error!("Failed to scan folder {base:?}; permission denied");
                return Ok(entries);
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }

        // Removed entries may still be on disk (e.g. `darcs remove` keeps
        // the file); those are already listed.
        if let Some(names) = core.cache.missing.get(&base) {
            for name in names {
                if entries.iter().any(|e| &e.name == name) {
                    continue;
                }
                let path = base.join(name);
                entries.push(Entry::new(
                    path.clone(),
                    name.clone(),
                    core.cache.state(&path),
                    false,
                    core.cache.meta(&path),
                ));
            }
        }
        Ok(entries)
    }

    /// The entry for `path`. Paths that can't be queried (e.g. removed
    /// files) get their full path as name and `isdir == false`.
    fn get_entry(&self, path: &Path) -> Entry {
        let path = paths::abspath(path);
        let core = self.core();
        let (name, isdir) = match std::fs::symlink_metadata(&path) {
            Ok(meta) => (
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| paths::lossy(&path)),
                meta.is_dir(),
            ),
            Err(_) => (paths::lossy(&path), false),
        };
        Entry::new(
            path.clone(),
            name,
            core.cache.state(&path),
            isdir,
            core.cache.meta(&path),
        )
    }

    /// Actions that could reasonably be taken on *all* of `path_states`.
    /// Actions the backend does not implement are left out.
    fn get_valid_actions(&self, path_states: &[(PathBuf, State)]) -> BTreeSet<Action> {
        let mut valid = base_valid_actions(self.root(), path_states);
        if path_states
            .iter()
            .all(|(_, s)| matches!(s, State::Modified | State::New))
        {
            valid.insert(Action::Unstage);
        }
        valid.retain(|a| self.supports(*a));
        valid
    }

    /// Build the commands for `action`; `message` is used by commit.
    fn action_commands(
        &self,
        action: Action,
        files: &[PathBuf],
        message: &str,
    ) -> Result<Vec<VcCommand>> {
        match action {
            Action::Compare => unsupported(self.kind(), "compare as a command"),
            Action::Update => self.update(),
            Action::Push => self.push(),
            Action::Commit => self.commit(files, message),
            Action::Add => self.add(files),
            Action::Remove => self.remove(files),
            Action::Revert => self.revert(files),
            Action::Resolve => self.resolve(files),
            Action::Unstage => self.unstage(files),
        }
    }
}

/// Open the backend `kind` for `path` (shorthand for [`VcKind::open`]).
pub fn open_vc(kind: VcKind, path: &Path) -> Result<Box<dyn Vc>> {
    kind.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ps(items: &[(&str, State)]) -> Vec<(PathBuf, State)> {
        items.iter().map(|(p, s)| (PathBuf::from(p), *s)).collect()
    }

    #[test]
    fn base_actions_rules() {
        let root = Path::new("/r");
        let v = base_valid_actions(root, &[]);
        assert!(!v.contains(&Action::Compare));
        assert!(v.contains(&Action::Update) && v.contains(&Action::Push));

        let v = base_valid_actions(root, &ps(&[("/r/a", State::Modified)]));
        for a in [
            Action::Compare,
            Action::Commit,
            Action::Add,
            Action::Remove,
            Action::Revert,
        ] {
            assert!(v.contains(&a), "{a}");
        }
        assert!(!v.contains(&Action::Resolve));

        let v = base_valid_actions(root, &ps(&[("/r/a", State::Normal)]));
        assert!(v.contains(&Action::Commit) && v.contains(&Action::Remove));
        assert!(!v.contains(&Action::Add) && !v.contains(&Action::Revert));

        let v = base_valid_actions(root, &ps(&[("/r/a", State::Unversioned)]));
        assert!(v.contains(&Action::Add));
        assert!(!v.contains(&Action::Commit) && !v.contains(&Action::Remove));

        let v = base_valid_actions(
            root,
            &ps(&[("/r/a", State::Conflict), ("/r/b", State::Conflict)]),
        );
        assert!(v.contains(&Action::Resolve));

        // The root can never be removed
        let v = base_valid_actions(root, &ps(&[("/r", State::Modified)]));
        assert!(!v.contains(&Action::Remove));
    }

    #[test]
    fn tree_cache_missing() {
        let mut c = TreeCache::default();
        c.add_missing_entry(Path::new("/r/a/x.txt"), State::Removed);
        c.add_missing_entry(Path::new("/r/a/y.txt"), State::Modified);
        assert_eq!(
            c.missing
                .get(Path::new("/r/a"))
                .unwrap()
                .iter()
                .collect::<Vec<_>>(),
            vec!["x.txt"]
        );
    }
}
