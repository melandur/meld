//! Toolkit-independent logic of the version control view (the non-GTK
//! parts of `meld/vcview.py`): choosing a backend for a location, building
//! the file tree with status filters (recursive or flattened), action
//! sensitivity, and preparing file comparisons ("compare to base", 3-way
//! conflict merges, "left is local").

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::entry::{Action, ConflictSide, Entry, State};
use crate::error::{Result, VcError};
use crate::paths;
use crate::vc::{Vc, VcKind, get_vcs};

// ----- Status filters (`vc-status-filters`) -----

/// The `vc-status-filters` setting: which entries are shown, and whether
/// the tree is flattened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusFilters {
    pub flatten: bool,
    pub modified: bool,
    pub normal: bool,
    pub unknown: bool,
    pub ignored: bool,
}

impl Default for StatusFilters {
    /// The gschema default `['flatten', 'modified']`.
    fn default() -> Self {
        StatusFilters {
            flatten: true,
            modified: true,
            normal: false,
            unknown: false,
            ignored: false,
        }
    }
}

impl StatusFilters {
    /// Filter keys in the order of `VcView.state_actions`.
    pub const KEYS: [&'static str; 5] = ["flatten", "modified", "normal", "unknown", "ignored"];

    /// No filters enabled.
    pub fn none() -> StatusFilters {
        StatusFilters {
            flatten: false,
            modified: false,
            normal: false,
            unknown: false,
            ignored: false,
        }
    }

    /// Build from setting keys; unknown keys are ignored.
    pub fn from_keys<S: AsRef<str>>(keys: &[S]) -> StatusFilters {
        let mut f = StatusFilters::none();
        for k in keys {
            match k.as_ref() {
                "flatten" => f.flatten = true,
                "modified" => f.modified = true,
                "normal" => f.normal = true,
                "unknown" => f.unknown = true,
                "ignored" => f.ignored = true,
                _ => {}
            }
        }
        f
    }

    /// Setting keys for the enabled filters.
    pub fn keys(&self) -> Vec<String> {
        let flags = [
            self.flatten,
            self.modified,
            self.normal,
            self.unknown,
            self.ignored,
        ];
        Self::KEYS
            .iter()
            .zip(flags)
            .filter(|(_, on)| *on)
            .map(|(k, _)| k.to_string())
            .collect()
    }

    /// Whether `entry` passes any enabled state filter (`flatten` is not a
    /// state filter). With no state filters enabled nothing is shown.
    pub fn matches(&self, entry: &Entry) -> bool {
        (self.modified && entry.is_modified())
            || (self.normal && entry.is_normal())
            || (self.unknown && entry.is_nonvc())
            || (self.ignored && entry.is_ignored())
    }
}

// ----- Tree building (`_search_recursively_iter`) -----

/// A row of the version control tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VcRow {
    /// A file or folder.
    Entry {
        entry: Entry,
        children: Vec<VcRow>,
        /// Whether the view should expand this row (and its ancestors):
        /// set for the root and for folders containing changed entries.
        expanded: bool,
    },
    /// The "(Empty)" placeholder for a folder without visible entries.
    Empty(String),
    /// A folder that could not be read, e.g. a broken symlink.
    Error(String),
}

impl VcRow {
    /// The entry of an entry row.
    pub fn entry(&self) -> Option<&Entry> {
        match self {
            VcRow::Entry { entry, .. } => Some(entry),
            _ => None,
        }
    }

    /// Child rows (empty for placeholders).
    pub fn children(&self) -> &[VcRow] {
        match self {
            VcRow::Entry { children, .. } => children,
            _ => &[],
        }
    }

    /// All entries of this subtree in display (pre-)order, including self.
    pub fn entries(&self) -> Vec<&Entry> {
        let mut out = Vec::new();
        fn walk<'a>(row: &'a VcRow, out: &mut Vec<&'a Entry>) {
            if let VcRow::Entry {
                entry, children, ..
            } = row
            {
                out.push(entry);
                for c in children {
                    walk(c, out);
                }
            }
        }
        walk(self, &mut out);
        out
    }
}

/// Filter and order entries like the view: filtered by `filters`, then
/// folders first, each group sorted by name.
pub fn filter_and_sort(entries: Vec<Entry>, filters: &StatusFilters) -> Vec<Entry> {
    let mut entries: Vec<Entry> = entries.into_iter().filter(|e| filters.matches(e)).collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries.sort_by_key(|e| !e.isdir);
    entries
}

/// Placeholder text for empty folders.
pub const EMPTY_LABEL: &str = "(Empty)";

struct Scanner<'a> {
    vc: &'a dyn Vc,
    filters: StatusFilters,
    display_prefix: usize,
    symlinks_followed: HashSet<(u64, u64)>,
    progress: &'a mut dyn FnMut(&str),
    cancel: Option<&'a AtomicBool>,
}

impl Scanner<'_> {
    fn check_cancelled(&self) -> Result<()> {
        if self.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            Err(VcError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn progress_for(&mut self, path: &Path) -> Result<()> {
        self.check_cancelled()?;
        let s = paths::lossy(path);
        let rel = s.get(self.display_prefix..).unwrap_or("");
        (self.progress)(&format!("Scanning {rel}"));
        Ok(())
    }

    /// Returns Ok(false) if the folder should be skipped (already followed
    /// symlink), Err(message) if it can't be stat'ed.
    fn check_folder(&mut self, entry: &Entry) -> std::result::Result<bool, String> {
        let meta = std::fs::symlink_metadata(&entry.path)
            .map_err(|e| format!("{:?}: {}", paths::lossy(&entry.path), e))?;
        if meta.file_type().is_symlink()
            && let Some(key) = file_key(&meta)
            && !self.symlinks_followed.insert(key)
        {
            return Ok(false);
        }
        Ok(true)
    }

    /// Non-flattened scan of folder `path` into `row`.
    fn scan_nested(&mut self, path: &Path) -> Result<(Vec<VcRow>, bool)> {
        self.progress_for(path)?;
        let entries = filter_and_sort(self.vc.get_entries(path)?, &self.filters);
        let mut rows = Vec::new();
        for e in &entries {
            self.check_cancelled()?;
            if e.isdir && e.is_present() {
                match self.check_folder(e) {
                    Err(msg) => {
                        rows.push(VcRow::Error(msg));
                        continue;
                    }
                    Ok(false) => continue,
                    Ok(true) => {}
                }
            }
            let (children, expanded) = if e.isdir && e.state != State::Ignored {
                self.scan_nested(&e.path)?
            } else {
                (Vec::new(), false)
            };
            rows.push(VcRow::Entry {
                entry: e.clone(),
                children,
                expanded,
            });
        }
        let expanded = entries.iter().any(|e| e.state != State::Normal);
        if entries.is_empty() {
            rows.push(VcRow::Empty(EMPTY_LABEL.to_string()));
        }
        Ok((rows, expanded))
    }

    /// Flattened scan: every visible file (and changed folder) below
    /// `start` becomes a direct child of the root. Folders are visited in
    /// path order, as the Python todo list is sorted by path.
    fn scan_flat(&mut self, start: &Path) -> Result<Vec<VcRow>> {
        let mut rows = Vec::new();
        let mut todo: BTreeSet<String> = BTreeSet::new();
        todo.insert(paths::lossy(start));
        while let Some(current) = todo.pop_first() {
            let current = PathBuf::from(current);
            self.progress_for(&current)?;
            let entries = filter_and_sort(self.vc.get_entries(&current)?, &self.filters);
            for e in entries {
                self.check_cancelled()?;
                if e.isdir && e.is_present() {
                    match self.check_folder(&e) {
                        Err(msg) => {
                            rows.push(VcRow::Error(msg));
                            continue;
                        }
                        Ok(false) => continue,
                        Ok(true) => {}
                    }
                    if e.state != State::Ignored {
                        // Render folders whose own state changed
                        if e.state != State::Normal {
                            rows.push(VcRow::Entry {
                                entry: e.clone(),
                                children: Vec::new(),
                                expanded: false,
                            });
                        }
                        todo.insert(paths::lossy(&e.path));
                    }
                    continue;
                }
                let children = if e.isdir && e.state != State::Ignored {
                    // A removed folder: Python scans it below its own row
                    self.scan_flat_missing(&e.path)?
                } else {
                    Vec::new()
                };
                rows.push(VcRow::Entry {
                    entry: e,
                    children,
                    expanded: false,
                });
            }
        }
        Ok(rows)
    }

    fn scan_flat_missing(&mut self, path: &Path) -> Result<Vec<VcRow>> {
        Ok(self.scan_nested(path)?.0)
    }
}

#[cfg(unix)]
fn file_key(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_key(_meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// Build the tree of rows below `start` (normally [`Vc::location`]) from
/// the backend's cached state; call [`Vc::refresh_vc_state`] first.
pub fn scan_tree(vc: &dyn Vc, start: &Path, filters: &StatusFilters) -> Result<VcRow> {
    scan(vc, start, filters, &mut |_| {}, None)
}

/// [`scan_tree`] reporting progress messages ("Scanning repository",
/// "Scanning sub/dir") for a status bar, that stops with
/// [`VcError::Cancelled`] soon after `cancel` is set; it is checked
/// before each folder and entry.
pub fn scan_tree_cancellable(
    vc: &dyn Vc,
    start: &Path,
    filters: &StatusFilters,
    progress: &mut dyn FnMut(&str),
    cancel: &AtomicBool,
) -> Result<VcRow> {
    scan(vc, start, filters, progress, Some(cancel))
}

fn scan(
    vc: &dyn Vc,
    start: &Path,
    filters: &StatusFilters,
    progress: &mut dyn FnMut(&str),
    cancel: Option<&AtomicBool>,
) -> Result<VcRow> {
    progress("Scanning repository");
    let start = paths::abspath(start);
    let name = start
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| paths::lossy(&start));
    let root_entry = Entry::new(start.clone(), name, State::Normal, true, "");
    let mut scanner = Scanner {
        vc,
        filters: *filters,
        display_prefix: paths::lossy(&start).len() + 1,
        symlinks_followed: HashSet::new(),
        progress,
        cancel,
    };
    let children = if filters.flatten {
        scanner.scan_flat(&start)?
    } else {
        scanner.scan_nested(&start)?.0
    };
    Ok(VcRow::Entry {
        entry: root_entry,
        children,
        expanded: true,
    })
}

/// Paths to open automatically (`auto_compare`): existing modified or
/// conflicted entries.
pub fn auto_compare_paths(tree: &VcRow) -> Vec<PathBuf> {
    tree.entries()
        .into_iter()
        .filter(|e| matches!(e.state, State::Modified | State::Conflict) && e.path.exists())
        .map(|e| e.path.clone())
        .collect()
}

// ----- Backend selection (`populate_vcs_for_location`) -----

/// Why a backend can't be used for a location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VcAvailability {
    Available,
    /// No repository of this kind found.
    NotFound,
    /// The program isn't installed.
    NotInstalled,
    /// The repository is invalid (or unsupported).
    InvalidRepository,
}

/// One entry of the backend chooser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VcChoice {
    pub kind: VcKind,
    /// "Git", "Git (not found)", "CVS (cvs not installed)", ...
    pub label: String,
    pub availability: VcAvailability,
    pub root: Option<PathBuf>,
}

impl VcChoice {
    pub fn enabled(&self) -> bool {
        self.availability == VcAvailability::Available
    }

    /// Open this backend for `location`.
    pub fn open(&self, location: &Path) -> Result<Box<dyn Vc>> {
        self.kind.open(location)
    }
}

/// The backend chooser for a location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VcChoices {
    /// The folder the backends were checked at (nearest existing folder).
    pub location: PathBuf,
    pub choices: Vec<VcChoice>,
    /// Index of the default choice.
    pub active: usize,
    pub tooltip: &'static str,
}

impl VcChoices {
    /// Open the active choice for [`VcChoices::location`].
    pub fn open_active(&self) -> Result<Box<dyn Vc>> {
        let choice = self.choices.get(self.active).ok_or(VcError::NotInRepo {
            vc: "None",
            path: self.location.clone(),
        })?;
        choice.open(&self.location)
    }
}

/// The nearest existing folder for `location` (VC systems run at folder
/// level, so files and deleted paths are replaced by their parent).
/// `None` if no existing parent exists.
pub fn vc_location(location: Option<&Path>) -> Option<PathBuf> {
    let mut loc = paths::abspath(location.unwrap_or(Path::new(".")));
    while !loc.is_dir() {
        let parent = loc.parent()?.to_path_buf();
        if parent.as_os_str().len() >= loc.as_os_str().len() {
            return None;
        }
        loc = parent;
    }
    Some(loc)
}

/// The default backend: the currently used one if still enabled, else the
/// enabled backend with the deepest (longest) root, else the first.
pub fn default_vc_index(choices: &[VcChoice], current: Option<VcKind>) -> usize {
    if let Some(target) = current
        && let Some(i) = choices.iter().position(|c| c.enabled() && c.kind == target)
    {
        return i;
    }
    let depth = |c: &VcChoice| {
        if c.enabled() {
            c.root.as_ref().map_or(0, |r| r.as_os_str().len())
        } else {
            0
        }
    };
    let target = choices.iter().map(depth).max().unwrap_or(0);
    if target > 0
        && let Some(i) = choices
            .iter()
            .position(|c| c.enabled() && depth(c) == target)
    {
        return i;
    }
    0
}

/// Check every backend for `location`, labelling unusable ones, and pick
/// the default. Falls back to the null backend (inserted first) when none
/// is usable. Spawns the VC programs; run on a background task.
pub fn choose_vcs(location: Option<&Path>, current: Option<VcKind>) -> VcChoices {
    let mut choices = Vec::new();
    let loc = vc_location(location);
    let location = loc
        .clone()
        .unwrap_or_else(|| paths::abspath(location.unwrap_or(Path::new("."))));
    if let Some(loc) = &loc {
        for cand in get_vcs(loc) {
            let kind = cand.kind;
            let name = kind.name();
            let cmd = kind.cmd().unwrap_or_default();
            let (availability, label) = if !cand.enabled() {
                (VcAvailability::NotFound, format!("{name} (not found)"))
            } else if !kind.is_installed() {
                (
                    VcAvailability::NotInstalled,
                    format!("{name} ({cmd} not installed)"),
                )
            } else if !kind.valid_repo(loc) {
                (
                    VcAvailability::InvalidRepository,
                    format!("{name} (invalid repository)"),
                )
            } else {
                (VcAvailability::Available, name.to_string())
            };
            choices.push(VcChoice {
                kind,
                label,
                availability,
                root: cand.root,
            });
        }
    }
    let active = default_vc_index(&choices, current);
    let tooltip = if choices.iter().any(VcChoice::enabled) {
        "Choose which version control system to use"
    } else {
        choices.insert(
            0,
            VcChoice {
                kind: VcKind::Null,
                label: VcKind::Null.name().to_string(),
                availability: VcAvailability::Available,
                root: Some(location.clone()),
            },
        );
        "No valid version control system found in this folder"
    };
    VcChoices {
        location,
        choices,
        active,
        tooltip,
    }
}

/// Tab title and tooltip for a VC view (`recompute_label`).
pub fn tab_labels(vc: &dyn Vc, location: &Path) -> (String, String) {
    let title = location
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| paths::lossy(location));
    let tooltip = format!("{} comparison:\n{}", vc.name(), location.display());
    (title, tooltip)
}

// ----- Action sensitivity (`on_treeview_selection_changed`) -----

/// Which view actions are enabled for the current selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ActionSensitivity {
    pub compare: bool,
    pub open_external: bool,
    pub add: bool,
    pub unstage: bool,
    pub commit: bool,
    pub delete_locally: bool,
    pub push: bool,
    pub remove: bool,
    pub resolve: bool,
    pub revert: bool,
    pub update: bool,
}

/// Action sensitivity for the selected `(path, state)` rows.
pub fn action_sensitivity(
    vc: Option<&dyn Vc>,
    selection: &[(PathBuf, State)],
) -> ActionSensitivity {
    let valid = vc
        .map(|v| v.get_valid_actions(selection))
        .unwrap_or_default();
    let has = |a: Action| valid.contains(&a);
    let has_paths = !selection.is_empty();
    ActionSensitivity {
        compare: has(Action::Compare),
        open_external: has_paths,
        add: has(Action::Add),
        unstage: has(Action::Unstage),
        commit: has(Action::Commit),
        delete_locally: has_paths
            && vc.is_some_and(|v| !selection.iter().any(|(p, _)| p == v.root())),
        push: has(Action::Push),
        remove: has(Action::Remove),
        resolve: has(Action::Resolve),
        revert: has(Action::Revert),
        update: has(Action::Update),
    }
}

/// Selected paths with trailing slashes removed (`_get_selected_files`).
pub fn clean_selection<I: IntoIterator<Item = PathBuf>>(paths_in: I) -> Vec<PathBuf> {
    paths_in.into_iter().map(|p| paths::normalize(&p)).collect()
}

/// Removing folders asks for confirmation ("Remove folder and all its
/// files?"); removing only files does not.
pub fn remove_needs_confirmation(selected: &[PathBuf]) -> bool {
    selected.iter().any(|p| p.is_dir())
}

/// Folder to refresh after deleting `files` locally.
pub fn delete_workdir(files: &[PathBuf]) -> PathBuf {
    paths::common_dir(files)
}

// ----- Comparisons (`run_diff`) -----

/// The `vc-merge-file-order` setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MergeFileOrder {
    LocalMergeRemote,
    /// The gschema default.
    #[default]
    RemoteMergeLocal,
}

impl MergeFileOrder {
    pub fn from_key(key: &str) -> MergeFileOrder {
        match key {
            "local-merge-remote" => MergeFileOrder::LocalMergeRemote,
            _ => MergeFileOrder::RemoteMergeLocal,
        }
    }

    #[cfg(test)]
    pub fn key(self) -> &'static str {
        match self {
            MergeFileOrder::LocalMergeRemote => "local-merge-remote",
            MergeFileOrder::RemoteMergeLocal => "remote-merge-local",
        }
    }
}

/// Settings affecting how comparisons are opened.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiffOptions {
    /// `vc-left-is-local`: show the working copy on the left.
    pub left_is_local: bool,
    /// `vc-merge-file-order`.
    pub merge_file_order: MergeFileOrder,
}

/// Which pane holds the newer version, for 2-way comparisons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffSide {
    New,
    Old,
}

/// A comparison to open for a selected path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffRequest {
    /// Open a folder comparison of this folder.
    Folder(PathBuf),
    /// Open a 2- or 3-way file comparison.
    Files {
        /// Files per pane.
        paths: Vec<PathBuf>,
        /// Custom pane labels (`None` keeps the default label).
        labels: Vec<Option<String>>,
        tab_label: String,
        /// Where a 3-way merge saves its result (the conflicted file).
        merge_output: Option<PathBuf>,
        /// Whether a 3-way merge should auto-merge non-conflicting changes
        /// (false for VC conflicts, which arrive pre-merged; meaningless
        /// for 2-way comparisons).
        auto_merge: bool,
        /// Whether saving should offer to mark the conflict resolved.
        prompt_resolve: bool,
        /// Old/new pane roles when they differ from the default
        /// (left-is-local gives `[New, Old]`).
        diff_sides: Option<[DiffSide; 2]>,
        /// Temporary files created for this comparison; already made
        /// read-only. Delete them with [`TempFiles::cleanup`].
        temp_files: Vec<PathBuf>,
    },
}

/// Build the comparison for `path`: a folder comparison, a 3-way merge for
/// conflicts, or the base revision against the working file.
pub fn prepare_diff(vc: &dyn Vc, path: &Path, options: &DiffOptions) -> Result<DiffRequest> {
    if path.is_dir() {
        return Ok(DiffRequest::Folder(path.to_path_buf()));
    }
    let basename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| paths::lossy(path));
    let entry = vc.get_entry(path);

    if entry.state == State::Conflict {
        match prepare_conflict(vc, path, &basename, options) {
            Err(VcError::Unsupported { .. }) => {}
            other => return other,
        }
    }

    let remote_label = format!("{basename} — repository");
    let comp_path = vc.get_path_for_repo_file(path, None)?;
    make_read_only(&comp_path);
    let (paths_v, labels, diff_sides, tab_label) = if options.left_is_local {
        (
            vec![path.to_path_buf(), comp_path.clone()],
            vec![None, Some(remote_label)],
            Some([DiffSide::New, DiffSide::Old]),
            format!("{basename} (working, repository)"),
        )
    } else {
        (
            vec![comp_path.clone(), path.to_path_buf()],
            vec![Some(remote_label), None],
            None,
            format!("{basename} (repository, working)"),
        )
    };
    Ok(DiffRequest::Files {
        paths: paths_v,
        labels,
        tab_label,
        merge_output: None,
        auto_merge: false,
        prompt_resolve: false,
        diff_sides,
        temp_files: vec![comp_path],
    })
}

fn prepare_conflict(
    vc: &dyn Vc,
    path: &Path,
    basename: &str,
    options: &DiffOptions,
) -> Result<DiffRequest> {
    let local_label = format!("{basename} — local");
    let remote_label = format!("{basename} — remote");
    let (sides, labels, tab_label) = match options.merge_file_order {
        MergeFileOrder::LocalMergeRemote => (
            [
                ConflictSide::Local,
                ConflictSide::Merged,
                ConflictSide::Remote,
            ],
            vec![Some(local_label), None, Some(remote_label)],
            format!("{basename} (local, merge, remote)"),
        ),
        MergeFileOrder::RemoteMergeLocal => (
            [
                ConflictSide::Remote,
                ConflictSide::Merged,
                ConflictSide::Local,
            ],
            vec![Some(remote_label), None, Some(local_label)],
            format!("{basename} (remote, merge, local)"),
        ),
    };
    let mut files = Vec::new();
    let mut temps = Vec::new();
    for side in sides {
        match vc.get_path_for_conflict(path, side) {
            Ok((p, is_temp)) => {
                if is_temp {
                    make_read_only(&p);
                    temps.push(p.clone());
                }
                files.push(p);
            }
            Err(e) => {
                for t in &temps {
                    let _ = std::fs::remove_file(t);
                }
                return Err(e);
            }
        }
    }
    Ok(DiffRequest::Files {
        paths: files,
        labels,
        tab_label,
        merge_output: Some(path.to_path_buf()),
        auto_merge: false,
        prompt_resolve: true,
        diff_sides: None,
        temp_files: temps,
    })
}

fn make_read_only(path: &Path) {
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_readonly(true);
        let _ = std::fs::set_permissions(path, perms);
    }
}

/// Registry of temporary files handed to comparisons, deleted at exit
/// (`_temp_files` + `cleanup_temp`). Only files directly inside the
/// system temporary folder are removed.
#[derive(Debug, Default)]
pub struct TempFiles {
    files: Vec<PathBuf>,
}

impl TempFiles {
    pub fn new() -> TempFiles {
        TempFiles::default()
    }

    /// Track `paths` for later deletion.
    pub fn register<I: IntoIterator<Item = PathBuf>>(&mut self, paths_in: I) {
        self.files.extend(paths_in);
    }

    /// Tracked files.
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    /// Delete all tracked files, returning those that could not be removed
    /// (with the reason).
    pub fn cleanup(&mut self) -> Vec<(PathBuf, String)> {
        let temp_dir = std::env::temp_dir();
        let mut failures = Vec::new();
        for f in self.files.drain(..) {
            let ok_location =
                f.is_absolute() && f.parent() == Some(temp_dir.as_path()) && f.exists();
            if !ok_location {
                failures.push((f, "not a temporary file".to_string()));
                continue;
            }
            // Windows refuses to delete read-only files
            if let Ok(meta) = std::fs::metadata(&f) {
                let mut perms = meta.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                perms.set_readonly(false);
                let _ = std::fs::set_permissions(&f, perms);
            }
            if let Err(e) = std::fs::remove_file(&f) {
                failures.push((f, e.to_string()));
            }
        }
        failures
    }
}

impl Drop for TempFiles {
    fn drop(&mut self) {
        for (f, why) in self.cleanup() {
            log::warn!("File {f:?} not removed due to {why}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_round_trip() {
        let f = StatusFilters::default();
        assert_eq!(f.keys(), vec!["flatten", "modified"]);
        let f = StatusFilters::from_keys(&["ignored", "normal", "bogus"]);
        assert!(f.ignored && f.normal && !f.flatten && !f.modified);
        assert_eq!(f.keys(), vec!["normal", "ignored"]);
        let e = Entry::new("/r/a", "a", State::Modified, false, "");
        assert!(!StatusFilters::none().matches(&e));
        assert!(StatusFilters::default().matches(&e));
    }

    #[test]
    fn sorting_dirs_first() {
        let es = vec![
            Entry::new("/r/b", "b", State::Modified, false, ""),
            Entry::new("/r/z", "z", State::Modified, true, ""),
            Entry::new("/r/a", "a", State::Modified, false, ""),
            Entry::new("/r/c", "c", State::Normal, true, ""),
        ];
        let names: Vec<_> = filter_and_sort(es, &StatusFilters::default())
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["c", "z", "a", "b"]);
    }

    fn choice(kind: VcKind, root: Option<&str>, ok: bool) -> VcChoice {
        VcChoice {
            kind,
            label: kind.name().into(),
            availability: if ok {
                VcAvailability::Available
            } else {
                VcAvailability::NotFound
            },
            root: root.map(PathBuf::from),
        }
    }

    #[test]
    fn default_vc_prefers_current_then_deepest() {
        let cs = vec![
            choice(VcKind::Git, Some("/a"), true),
            choice(VcKind::Mercurial, Some("/a/b/c"), true),
            choice(VcKind::Bazaar, Some("/a/b/c/d"), false),
        ];
        assert_eq!(default_vc_index(&cs, None), 1);
        assert_eq!(default_vc_index(&cs, Some(VcKind::Git)), 0);
        assert_eq!(default_vc_index(&cs, Some(VcKind::Bazaar)), 1);
        assert_eq!(default_vc_index(&[], None), 0);
    }

    #[test]
    fn cancelled_scans_stop() {
        let dir = tempfile::tempdir().unwrap();
        for d in ["a", "b", "c"] {
            std::fs::create_dir_all(dir.path().join(d).join("deeper")).unwrap();
            std::fs::write(dir.path().join(d).join("f"), "x").unwrap();
        }
        let vc = VcKind::Null.open(dir.path()).unwrap();
        let filters = StatusFilters::from_keys(&["normal"]);
        let cancel = AtomicBool::new(false);
        let full = scan_tree_cancellable(&*vc, dir.path(), &filters, &mut |_| {}, &cancel).unwrap();
        assert_eq!(full.children().len(), 3);

        // Cancelling from the progress callback stops at the next folder
        let mut folders = 0;
        let result = scan_tree_cancellable(
            &*vc,
            dir.path(),
            &filters,
            &mut |message| {
                if message.starts_with("Scanning ") && message != "Scanning repository" {
                    folders += 1;
                    if folders == 2 {
                        cancel.store(true, Ordering::Relaxed);
                    }
                }
            },
            &cancel,
        );
        assert!(matches!(result, Err(VcError::Cancelled)));
        assert_eq!(folders, 2);
    }

    #[test]
    fn location_walks_up_to_existing_folder() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("gone/deeper/file.txt");
        assert_eq!(vc_location(Some(&missing)).unwrap(), dir.path());
    }

    #[test]
    fn null_fallback_when_nothing_found() {
        let dir = tempfile::tempdir().unwrap();
        let choices = choose_vcs(Some(dir.path()), None);
        // The temp dir may live inside some repository on odd setups;
        // only assert the fallback when nothing was found.
        if !choices.choices.iter().skip(1).any(|c| c.root.is_some()) {
            assert_eq!(choices.choices[0].kind, VcKind::Null);
            assert_eq!(choices.active, 0);
            assert_eq!(choices.choices.len(), 7);
            assert!(choices.choices[1].label.ends_with("(not found)"));
        }
    }

    #[test]
    fn misc_helpers() {
        assert_eq!(
            MergeFileOrder::from_key("local-merge-remote").key(),
            "local-merge-remote"
        );
        assert_eq!(MergeFileOrder::default().key(), "remote-merge-local");
        assert_eq!(
            delete_workdir(&[PathBuf::from("/r/a/x"), PathBuf::from("/r/a/y")]),
            PathBuf::from("/r/a")
        );
        assert_eq!(
            clean_selection(vec![PathBuf::from("/r/a/")]),
            vec![PathBuf::from("/r/a")]
        );
    }
}
