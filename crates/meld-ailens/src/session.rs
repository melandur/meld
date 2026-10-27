//! One open comparison: its files, their Why cards and explaining them
//! (port of `session.py`, plus the file tree of `gitcompare.py`)
//!
//! Every method here blocks on git or Claude Code, so the UI calls them on
//! a background task. A session can be shared between threads (`Arc`); its
//! caches are guarded by a lock so that an explain job and a file load can
//! overlap.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::attribution::ComparisonAttributor;
use crate::cards::{Card, CardKind, CardStep, ExplainAction};
use crate::claude::RunClaude;
use crate::comparison::{changed_files_with_stats, resolve_comparison};
use crate::error::{Error, Result};
use crate::explanation::{
    MessageFormat, ParsedNote, compose_note, explain_commit, find_change_explanations, parse_note,
};
use crate::git::{GitOptions, GitRepository, LogQuery, realpath};
use crate::ignore::{IgnoreRules, create_ignore_rules, is_ignored};
use crate::infer::{
    InferOptions, compose_inferred_note, infer_commit_changes, infer_explanation,
    infer_uncommitted_explanation,
};
use crate::models::{
    ChangeExplanation, ChangeGroup, ChangeGuide, ChangeStep, CommitHunk, Comparison,
    ComparisonRelation, DiffLineKind, FileAttribution, GitCommit, GitDiffHunk, GitFileChange,
    GitFileDiff, GitFileStatus, LayerDiffsUsed, LineOrigin, LineRange, Revision, RevisionKind,
    Side, UncommittedLayer,
};
use crate::picker::{
    COMMIT_PAGE_SIZE, PickerEntry, PickerSide, PickerSources, build_entries, short_sha,
};
use crate::process::CancelToken;
use crate::uncommitted::{PlacedExplanation, UncommittedExplanations};

/// Larger files are shown as "too large" instead of compared
pub const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;

/// How many bytes are checked for NUL to call a file binary, as git does
pub const BINARY_SNIFF_BYTES: usize = 8000;

/// Reports how far explaining everything is: what it is doing, and the
/// share of the work done, from 0 to 1
pub type ExplainProgress<'a> = &'a dyn Fn(&str, f32);

/// The settings a session reads; the app fills it from its preferences
#[derive(Clone)]
pub struct SessionConfig {
    /// The Claude Code CLI (`ailens-claude-path`)
    pub claude_path: String,
    /// Whether Claude Code may be asked (`ailens-infer-enabled`)
    pub infer_enabled: bool,
    /// Model that explains commits (`ailens-infer-model`)
    pub infer_model: String,
    /// Notes ref name (`ailens-notes-ref`)
    pub notes_ref: String,
    pub message_format: MessageFormat,
    /// Replaces the Claude Code call, e.g. in tests
    pub run_claude: Option<RunClaude>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            claude_path: "claude".into(),
            infer_enabled: true,
            infer_model: "claude-opus-5-5".into(),
            notes_ref: "ai-lens".into(),
            message_format: MessageFormat::default(),
            run_claude: None,
        }
    }
}

impl std::fmt::Debug for SessionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionConfig")
            .field("claude_path", &self.claude_path)
            .field("infer_enabled", &self.infer_enabled)
            .field("infer_model", &self.infer_model)
            .field("notes_ref", &self.notes_ref)
            .field("message_format", &self.message_format)
            .field("run_claude", &self.run_claude.is_some())
            .finish()
    }
}

/// A changed file in the comparison's file list
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub change: GitFileChange,
    pub additions: Option<usize>,
    pub deletions: Option<usize>,
}

impl FileEntry {
    pub fn new(change: GitFileChange, additions: Option<usize>, deletions: Option<usize>) -> Self {
        Self {
            change,
            additions,
            deletions,
        }
    }

    pub fn binary(&self) -> bool {
        self.additions.is_none() && self.deletions.is_none()
    }
}

/// A file ready to compare: both versions and its cards
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileView {
    pub entry: FileEntry,
    /// Paths in the base and target versions, for labels and file names
    pub base_name: String,
    pub target_name: String,
    /// Contents; None when that side has no such file
    pub base_text: Option<Vec<u8>>,
    pub target_text: Option<Vec<u8>>,
    pub base_label: String,
    pub target_label: String,
    /// The file on disk, when the target is a working tree (it may be
    /// edited there)
    pub target_disk_path: Option<PathBuf>,
    pub attribution: Option<FileAttribution>,
    /// In diff order; see [`Card::anchor`] for where each one sits
    pub cards: Vec<Card>,
    /// Shown in the Why column when there are no cards
    pub cards_placeholder: String,
    /// Shown instead of the comparison (binary, too large, ...), if set
    pub message: String,
}

impl FileView {
    fn new(entry: FileEntry, attribution: Option<FileAttribution>) -> Self {
        Self {
            base_name: entry.change.path.clone(),
            target_name: entry.change.path.clone(),
            entry,
            base_text: None,
            target_text: None,
            base_label: String::new(),
            target_label: String::new(),
            target_disk_path: None,
            attribution,
            cards: Vec::new(),
            cards_placeholder: String::new(),
            message: String::new(),
        }
    }

    pub fn change(&self) -> &GitFileChange {
        &self.entry.change
    }
}

fn is_binary(data: Option<&[u8]>) -> bool {
    data.is_some_and(|data| data[..data.len().min(BINARY_SNIFF_BYTES)].contains(&0))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn basename(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

/// A repository (or worktree) a Git Compare tab is open on
///
/// Outlives the tab's comparisons, so explanations of uncommitted hunks,
/// which can't be saved, last until the tab is closed.
pub struct RepositoryInfo {
    pub repo: GitRepository,
    pub root: PathBuf,
    pub uncommitted: UncommittedExplanations,
    /// HEAD of each worktree (None: this one), as the pickers last saw it
    worktree_heads: Mutex<HashMap<Option<PathBuf>, String>>,
    /// One lock per commit whose note is being explained: its note is
    /// read, added to and written by one explanation at a time
    note_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Notes found to explain every change block of their commit, by
    /// commit: looked at again once the note changes
    complete_notes: Mutex<HashMap<String, Option<String>>>,
}

impl RepositoryInfo {
    pub fn new(repo: GitRepository) -> Self {
        Self {
            root: repo.root.clone(),
            repo,
            uncommitted: UncommittedExplanations::default(),
            worktree_heads: Mutex::new(HashMap::new()),
            note_locks: Mutex::new(HashMap::new()),
            complete_notes: Mutex::new(HashMap::new()),
        }
    }

    /// The lock to hold while the note of `sha` is read, explained and
    /// written
    fn note_lock(&self, sha: &str) -> Arc<Mutex<()>> {
        lock(&self.note_locks)
            .entry(sha.to_string())
            .or_default()
            .clone()
    }

    /// The repository of the worktree that holds `path` (a file or folder)
    pub fn open(path: &Path, git: &GitOptions, cancel: &CancelToken) -> Result<Self> {
        if !path.exists() {
            return Err(Error::msg(format!("“{}” doesn’t exist.", path.display())));
        }
        let folder = if path.is_file() {
            match path.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
                _ => PathBuf::from("."),
            }
        } else {
            path.to_path_buf()
        };
        Ok(Self::new(GitRepository::discover(&folder, git, cancel)?))
    }

    /// The tab's title: the worktree folder's name
    pub fn display_name(&self) -> String {
        basename(&self.root)
    }

    /// HEAD of each worktree (None: this one), as [`Self::picker_entries`]
    /// last saw them; for [`crate::picker::apply_pick`]
    pub fn worktree_heads(&self) -> HashMap<Option<PathBuf>, String> {
        lock(&self.worktree_heads).clone()
    }

    /// The text of the repository's `.ailensignore`, if any
    pub fn ignore_rules(&self) -> Option<String> {
        match std::fs::read(self.root.join(".ailensignore")) {
            Ok(data) => Some(String::from_utf8_lossy(&data).into_owned()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                log::warn!("Can't read .ailensignore: {err}");
                None
            }
        }
    }

    /// Every entry of the Base or Target picker
    pub fn picker_entries(
        &self,
        side: PickerSide,
        cancel: &CancelToken,
    ) -> Result<Vec<PickerEntry>> {
        let repo = &self.repo;
        let worktrees = repo.worktrees(cancel)?;
        let refs = repo.refs(cancel)?;
        let head_sha = repo.head(None, cancel)?;
        let head = match &head_sha {
            Some(sha) => Some(repo.commit(sha, None, cancel)?),
            None => None,
        };
        let commits = if head_sha.is_some() || !refs.is_empty() {
            repo.log(
                &["HEAD"],
                &LogQuery {
                    all_refs: true,
                    max_count: Some(COMMIT_PAGE_SIZE),
                    ..Default::default()
                },
                cancel,
            )?
        } else {
            Vec::new()
        };
        let head_branch = refs.iter().find(|r| r.current).map(|r| r.name.clone());
        let own = realpath(&self.root);
        let heads: HashMap<Option<PathBuf>, String> = worktrees
            .iter()
            .filter_map(|worktree| {
                let head = worktree.head.clone()?;
                let key = (realpath(&worktree.path) != own).then(|| worktree.path.clone());
                Some((key, head))
            })
            .collect();
        *lock(&self.worktree_heads) = heads;
        Ok(build_entries(
            &PickerSources {
                refs: &refs,
                worktrees: &worktrees,
                commits: &commits,
                current_worktree: Some(&self.root),
                head: head.as_ref(),
                head_branch: head_branch.as_deref(),
                now: None,
            },
            side,
        ))
    }

    /// Commits anywhere in history whose message or author matches
    pub fn search_commits(&self, query: &str, cancel: &CancelToken) -> Result<Vec<PickerEntry>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let commits = self.repo.log(
            &[],
            &LogQuery {
                all_refs: true,
                grep: Some(query),
                max_count: Some(COMMIT_PAGE_SIZE),
                ..Default::default()
            },
            cancel,
        )?;
        Ok(build_entries(
            &PickerSources {
                commits: &commits,
                ..Default::default()
            },
            PickerSide::Target,
        ))
    }

    /// Files and folders whose changes may change the comparison
    pub fn watch_paths(&self) -> Vec<PathBuf> {
        let cancel = CancelToken::new();
        let found = self.repo.git_dir(None, &cancel).and_then(|git_dir| {
            let result = self.repo.run(
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
                &cancel,
            )?;
            let common = String::from_utf8_lossy(&result.stdout).trim().to_string();
            Ok((git_dir, common))
        });
        let Ok((git_dir, common)) = found else {
            return vec![self.root.clone()];
        };
        let common = if common.is_empty() {
            git_dir.clone()
        } else {
            PathBuf::from(common)
        };
        let mut paths: Vec<PathBuf> = Vec::new();
        let folders: HashSet<&PathBuf> = [&git_dir, &common].into_iter().collect();
        for folder in folders {
            for name in ["HEAD", "index", "packed-refs", "refs/heads", "refs/tags"] {
                let path = folder.join(name);
                if path.exists() {
                    paths.push(path);
                }
            }
        }
        let notes = common.join("refs").join("notes");
        if notes.is_dir() {
            paths.push(notes);
        }
        paths.sort();
        paths.dedup();
        paths
    }
}

#[derive(Default)]
struct SessionState {
    entries: Option<Vec<FileEntry>>,
    commits: HashMap<String, GitCommit>,
    range: Option<Vec<GitCommit>>,
}

/// A resolved comparison, its files, and their explanations
///
/// What the Git Compare tab drives: [`Self::files`] for the file list,
/// [`Self::file_view`] for both versions of a file and its cards,
/// [`Self::explain_card`] / [`Self::explain_all`] for the Explain buttons.
pub struct CompareSession {
    pub info: Arc<RepositoryInfo>,
    pub repo: GitRepository,
    pub comparison: Comparison,
    pub config: SessionConfig,
    pub root: PathBuf,
    attributor: ComparisonAttributor,
    state: Mutex<SessionState>,
    base_label: String,
    target_label: String,
}

impl CompareSession {
    /// Resolve `base` → `target` and open a session on it
    pub fn open(
        info: Arc<RepositoryInfo>,
        base: &Revision,
        target: &Revision,
        config: SessionConfig,
        cancel: &CancelToken,
    ) -> Result<Self> {
        let comparison = resolve_comparison(&info.repo, base, target, cancel)?;
        let repo = info.repo.clone();
        let base_label = label(&repo, &comparison.base, &comparison.base_sha, cancel)?;
        let target_label = label(&repo, &comparison.target, &comparison.target_sha, cancel)?;
        Ok(Self {
            attributor: ComparisonAttributor::new(repo.clone(), comparison.clone()),
            root: info.root.clone(),
            info,
            repo,
            comparison,
            config,
            state: Mutex::new(SessionState::default()),
            base_label,
            target_label,
        })
    }

    // -- labels --

    /// e.g. `main`, `a1b2c3d`, `Staged`, `Working Tree (wt)`
    pub fn base_label(&self) -> &str {
        &self.base_label
    }

    pub fn target_label(&self) -> &str {
        &self.target_label
    }

    pub fn base_tooltip(&self) -> String {
        let c = &self.comparison;
        if c.base.kind == RevisionKind::Index {
            return format!("The index of {}", c.worktree_path.display());
        }
        format!("{}\n{}", self.base_label, c.base_sha)
    }

    pub fn target_tooltip(&self) -> String {
        let c = &self.comparison;
        match c.target.kind {
            RevisionKind::Index => format!("Staged changes in {}", c.worktree_path.display()),
            RevisionKind::WorkingTree => format!(
                "Files on disk in {}, untracked ones included",
                c.worktree_path.display()
            ),
            RevisionKind::Commit => format!("{}\n{}", self.target_label, c.target_sha),
        }
    }

    /// How base and target relate, when that needs saying; empty otherwise
    pub fn relation_text(&self) -> String {
        let c = &self.comparison;
        if c.base.kind == RevisionKind::Index {
            return "Unstaged changes".into();
        }
        match (c.relation, &c.merge_base) {
            (ComparisonRelation::Diverged, Some(merge_base)) => format!(
                "Diverged: showing changes since the merge base {}",
                short_sha(merge_base)
            ),
            (ComparisonRelation::Reversed, _) => {
                "The target is older than the base; swap them to see what was added".into()
            }
            (ComparisonRelation::Unrelated, _) => {
                "No shared history: comparing the trees directly".into()
            }
            (ComparisonRelation::Same, _) if !c.uncommitted() => {
                "Base and target are the same commit".into()
            }
            _ => String::new(),
        }
    }

    /// Whether index, worktree or ref changes can change this comparison
    pub fn follows_repository_changes(&self) -> bool {
        let c = &self.comparison;
        if c.uncommitted() || c.base.uncommitted() {
            return true;
        }
        [&c.base, &c.target].iter().any(|r| {
            r.kind == RevisionKind::Commit
                && r.ref_name.as_ref().is_some_and(|name| {
                    !(name.len() >= 7 && name.chars().all(|ch| "0123456789abcdef".contains(ch)))
                })
        })
    }

    // -- files --

    /// The changed files with their line counts, in git's path order
    pub fn files(&self, cancel: &CancelToken) -> Result<Vec<FileEntry>> {
        if let Some(entries) = &lock(&self.state).entries {
            return Ok(entries.clone());
        }
        let (changes, stats) = changed_files_with_stats(&self.repo, &self.comparison, cancel)?;
        let by_path: HashMap<&str, _> = stats.iter().map(|s| (s.path.as_str(), s)).collect();
        let entries: Vec<FileEntry> = changes
            .into_iter()
            .map(|change| {
                let stat = by_path.get(change.path.as_str());
                FileEntry::new(
                    change,
                    stat.and_then(|s| s.additions),
                    stat.and_then(|s| s.deletions),
                )
            })
            .collect();
        lock(&self.state).entries = Some(entries.clone());
        Ok(entries)
    }

    fn read_side(&self, base: bool, path: &str, cancel: &CancelToken) -> Result<Option<Vec<u8>>> {
        let c = &self.comparison;
        let revision = if base { &c.base } else { &c.target };
        let worktree = Some(c.worktree_path.as_path());
        match revision.kind {
            RevisionKind::Index => self.repo.read_index_file(path, worktree, cancel),
            RevisionKind::WorkingTree => self.repo.read_worktree_file(path, worktree, cancel),
            RevisionKind::Commit => {
                let rev = if base { &c.from_sha } else { &c.target_sha };
                self.repo.read_blob(rev, path, cancel)
            }
        }
    }

    /// Both versions of a changed file, its attribution and its cards
    pub fn file_view(&self, change: &GitFileChange, cancel: &CancelToken) -> Result<FileView> {
        let entry = self
            .files(cancel)?
            .into_iter()
            .find(|e| e.change == *change)
            .unwrap_or_else(|| FileEntry::new(change.clone(), None, None));
        let c = &self.comparison;
        let base_path = change
            .original_path
            .clone()
            .unwrap_or_else(|| change.path.clone());
        let base_text = if change.status == GitFileStatus::Added {
            None
        } else {
            self.read_side(true, &base_path, cancel)?
        };
        let target_text = if change.status == GitFileStatus::Deleted {
            None
        } else {
            self.read_side(false, &change.path, cancel)?
        };
        let mut base_label = format!("{base_path} — {}", self.base_label);
        if let (ComparisonRelation::Diverged, Some(merge_base), RevisionKind::Commit) =
            (c.relation, &c.merge_base, c.base.kind)
        {
            base_label = format!("{base_path} — merge base {}", short_sha(merge_base));
        }
        let target_label = format!("{} — {}", change.path, self.target_label);
        let mut disk = None;
        if c.target.kind == RevisionKind::WorkingTree && change.status != GitFileStatus::Deleted {
            let candidate = c.worktree_path.join(&change.path);
            let is_link = candidate
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink());
            if candidate.is_file() && !is_link {
                disk = Some(candidate);
            }
        }

        let mut view = FileView::new(entry, None);
        view.base_name = base_path;
        view.target_name = change.path.clone();
        view.base_label = base_label;
        view.target_label = target_label;
        view.target_disk_path = disk;

        let largest = [&base_text, &target_text]
            .iter()
            .filter_map(|text| text.as_ref().map(Vec::len))
            .max();
        view.base_text = base_text;
        view.target_text = target_text;
        if change.untracked && change.path.ends_with('/') {
            view.message = format!("{} is a nested repository or submodule", change.path);
            return Ok(view);
        }
        if let Some(largest) = largest.filter(|&largest| largest > MAX_FILE_BYTES) {
            view.message = format!(
                "{} is too large to compare here ({} MB)",
                change.path,
                largest / (1024 * 1024)
            );
        }
        let attribution = self.attributor.attribute(change, cancel)?;
        let diff_binary = attribution.diff.as_ref().is_some_and(|d| d.binary);
        let binary = diff_binary
            || is_binary(view.base_text.as_deref())
            || is_binary(view.target_text.as_deref());
        let submodule = attribution.diff.as_ref().is_some_and(|diff| {
            diff.hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .any(|line| line.content.starts_with("Subproject commit "))
        });
        if submodule {
            view.message = format!("{} is a submodule; its commit changed", change.path);
        } else if binary && view.message.is_empty() {
            view.message = format!("{} is a binary file", change.path);
        }
        view.cards = self.build_cards(&attribution, cancel)?;
        if view.cards.is_empty() {
            view.cards_placeholder = if attribution.diff.as_ref().is_none_or(|d| d.hunks.is_empty())
            {
                "No text changes (e.g. only the mode)".into()
            } else {
                "Git can’t tell which commit made these changes".into()
            };
        }
        if !view.message.is_empty() && !attribution.touched_by.is_empty() {
            let names: Vec<&str> = attribution
                .touched_by
                .iter()
                .take(8)
                .map(|sha| short_sha(sha))
                .collect();
            view.message
                .push_str(&format!("\nChanged by {}", names.join(", ")));
        }
        view.attribution = Some(attribution);
        Ok(view)
    }

    // -- commits --

    /// The comparison's commits, oldest first
    pub fn range_commits(&self, cancel: &CancelToken) -> Result<Vec<GitCommit>> {
        if let Some(range) = &lock(&self.state).range {
            return Ok(range.clone());
        }
        let c = &self.comparison;
        let query = LogQuery {
            notes_ref: Some(&self.config.notes_ref),
            ..Default::default()
        };
        let mut commits = match c.relation {
            ComparisonRelation::Same | ComparisonRelation::Reversed => Vec::new(),
            _ if c.base.uncommitted() => Vec::new(),
            ComparisonRelation::Unrelated => self.repo.log(&[&c.target_sha], &query, cancel)?,
            _ => {
                let range = format!("{}..{}", c.from_sha, c.target_sha);
                self.repo.log(&[&range], &query, cancel)?
            }
        };
        commits.reverse();
        let mut state = lock(&self.state);
        for commit in &commits {
            state
                .commits
                .entry(commit.sha.clone())
                .or_insert_with(|| commit.clone());
        }
        state.range = Some(commits.clone());
        Ok(commits)
    }

    pub fn get_commit(&self, sha: &str, cancel: &CancelToken) -> Result<GitCommit> {
        if let Some(commit) = lock(&self.state).commits.get(sha) {
            return Ok(commit.clone());
        }
        let commit = self
            .repo
            .commit(sha, Some(&self.config.notes_ref), cancel)?;
        lock(&self.state)
            .commits
            .insert(sha.to_string(), commit.clone());
        Ok(commit)
    }

    /// The commit as git has it now, its note included, cached again
    fn refresh_commit(&self, sha: &str, cancel: &CancelToken) -> Result<GitCommit> {
        let commit = self
            .repo
            .commit(sha, Some(&self.config.notes_ref), cancel)?;
        let mut state = lock(&self.state);
        state.commits.insert(sha.to_string(), commit.clone());
        if let Some(range) = &mut state.range {
            for item in range.iter_mut().filter(|c| c.sha == sha) {
                *item = commit.clone();
            }
        }
        Ok(commit)
    }

    // -- cards --

    fn note(commit: &GitCommit) -> Option<ParsedNote> {
        commit
            .note
            .as_deref()
            .filter(|n| !n.is_empty())
            .map(parse_note)
    }

    /// The view's cards again, e.g. after an explanation was added
    pub fn cards(&self, view: &FileView, cancel: &CancelToken) -> Result<Vec<Card>> {
        match &view.attribution {
            Some(attribution) => self.build_cards(attribution, cancel),
            None => Ok(Vec::new()),
        }
    }

    fn build_cards(
        &self,
        attribution: &FileAttribution,
        cancel: &CancelToken,
    ) -> Result<Vec<Card>> {
        if attribution.diff.as_ref().is_none_or(|diff| diff.binary) {
            return Ok(Vec::new());
        }
        // Loads the comparison's commits with one git call, for the cards
        self.range_commits(cancel)?;
        let empty = LayerDiffsUsed::default();
        let layers = attribution.layers.as_ref().unwrap_or(&empty);
        let placed: HashMap<UncommittedLayer, Vec<PlacedExplanation>> = UncommittedLayer::ALL
            .iter()
            .map(|&layer| (layer, self.info.uncommitted.place(layer, layers.get(layer))))
            .collect();
        let covered = covered_lines(&attribution.groups);
        let mut cards = Vec::new();
        for index in 0..attribution.groups.len() {
            cards.push(self.card(
                &attribution.change.path,
                &attribution.groups,
                index,
                &covered,
                layers,
                &placed,
                cancel,
            )?);
        }
        Ok(cards)
    }

    #[allow(clippy::too_many_arguments)]
    fn card(
        &self,
        path: &str,
        groups: &[ChangeGroup],
        index: usize,
        covered: &CoveredLines,
        layers: &LayerDiffsUsed,
        placed: &HashMap<UncommittedLayer, Vec<PlacedExplanation>>,
        cancel: &CancelToken,
    ) -> Result<Card> {
        let group = &groups[index];
        let infer = self.config.infer_enabled;
        let by = match (group.uncommitted, &group.sha) {
            (Some(layer), _) => layer.as_str().to_string(),
            (None, Some(sha)) => sha.clone(),
            (None, None) => "unknown".to_string(),
        };
        let common = Card {
            // The path too: keys stay apart across files, e.g. for what
            // is being explained while another file is shown
            key: format!("{path}:{}:{by}:{index}", group.hunk_index),
            added: group.added.clone(),
            deleted: group.deleted.clone(),
            ..Default::default()
        };

        if let Some(layer) = group.uncommitted {
            let empty = Vec::new();
            let explanations = placed.get(&layer).unwrap_or(&empty);
            let changes = find_change_explanations(&group.origins, explanations);
            let explained = layers
                .get(layer)
                .iter()
                .all(|diff| self.info.uncommitted.is_explained(layer, diff));
            let mut card = Card {
                kind: match layer {
                    UncommittedLayer::Staged => CardKind::Staged,
                    UncommittedLayer::Unstaged => CardKind::Unstaged,
                },
                change_what: join_texts(changes.iter().map(|c| c.what.as_str())),
                change_why: join_texts(changes.iter().map(|c| c.why.as_str())),
                change_justification: join_texts(
                    changes.iter().filter_map(|c| c.justification.as_deref()),
                ),
                explain: (infer && !explained).then_some(ExplainAction::Uncommitted),
                ..common
            };
            set_guide(&mut card, changes.iter().map(|c| &c.guide));
            card.change_steps = card_steps(
                groups,
                index,
                covered,
                changes.iter().map(|c| (&c.hunk, c.guide.steps.as_slice())),
            );
            return Ok(card);
        }
        let Some(sha) = &group.sha else {
            return Ok(Card {
                kind: CardKind::Unknown,
                ..common
            });
        };

        let commit = self.get_commit(sha, cancel)?;
        let explanation = explain_commit(&commit, &self.config.message_format);
        let note = Self::note(&commit);
        let changes = match &note {
            Some(note) => find_change_explanations(&group.origins, &note.changes),
            None => Vec::new(),
        };
        let changes_explained = note.as_ref().is_some_and(ParsedNote::changes_explained);
        let explain = if !infer {
            None
        } else if explanation.why.is_none() {
            Some(ExplainAction::Commit)
        } else if !changes_explained
            || note.as_ref().is_some_and(|note| {
                note.changes_outdated() || shares_a_joined_change(note, &changes, groups, index)
            })
        {
            Some(ExplainAction::Changes)
        } else if changes.is_empty() && !group.origins.is_empty() {
            // Its hunks were left out when the commit was explained
            Some(ExplainAction::Block)
        } else {
            None
        };
        let mut card = Card {
            kind: CardKind::Commit,
            sha: Some(commit.sha.clone()),
            what: explanation.what.clone(),
            change_what: join_texts(changes.iter().map(|c| c.what.as_str())),
            change_why: join_texts(changes.iter().map(|c| c.why.as_str())),
            change_justification: join_texts(
                changes.iter().filter_map(|c| c.justification.as_deref()),
            ),
            explain,
            ..common
        };
        set_guide(&mut card, changes.iter().map(|c| &c.guide));
        card.change_steps = card_steps(
            groups,
            index,
            covered,
            changes.iter().map(|c| (&c.hunk, c.guide.steps.as_slice())),
        );
        Ok(card)
    }

    // -- explaining --

    fn require_infer(&self) -> Result<()> {
        if !self.config.infer_enabled {
            return Err(Error::msg(
                "Explaining with Claude Code is turned off in the preferences.",
            ));
        }
        Ok(())
    }

    fn infer_options<'a>(&'a self, ignore_rules: Option<&'a str>) -> InferOptions<'a> {
        let mut options = InferOptions::new(&self.config.infer_model);
        options.claude_path = &self.config.claude_path;
        options.run = self.config.run_claude.clone();
        options.ignore_rules = ignore_rules;
        options
    }

    /// What a card's explain button does; afterwards, [`Self::cards`] gives
    /// the updated cards
    pub fn explain_card(&self, view: &FileView, key: &str, cancel: &CancelToken) -> Result<()> {
        let Some(card) = view.cards.iter().find(|c| c.key == key) else {
            return Err(Error::msg("That change isn’t in this file any more."));
        };
        match (card.kind, &card.sha) {
            (CardKind::Commit, Some(sha)) if card.explain == Some(ExplainAction::Block) => {
                let index = view.cards.iter().position(|c| c.key == key).unwrap_or(0);
                let group = view
                    .attribution
                    .as_ref()
                    .and_then(|attribution| attribution.groups.get(index))
                    .ok_or_else(|| Error::msg("That change isn’t in this file any more."))?;
                // Only the change blocks with the card's lines
                let files = self
                    .repo
                    .commit_diffs(sha, cancel)?
                    .into_iter()
                    .filter_map(|(change, mut diff)| {
                        let paths = diff_paths(&diff);
                        diff.hunks = diff
                            .hunks
                            .iter()
                            .flat_map(GitDiffHunk::blocks)
                            .filter(|block| block_has_lines(&paths, block, &group.origins))
                            .collect();
                        (!diff.hunks.is_empty()).then_some((change, diff))
                    })
                    .collect();
                self.explain_blocks(sha, files, cancel).map(|_| ())
            }
            (CardKind::Commit, Some(sha)) => self.explain_commit(sha, cancel),
            (CardKind::Staged, _) => self
                .explain_uncommitted(view, UncommittedLayer::Staged, cancel)
                .map(|_| ()),
            (CardKind::Unstaged, _) => self
                .explain_uncommitted(view, UncommittedLayer::Unstaged, cancel)
                .map(|_| ()),
            _ => Err(Error::msg("Git can’t tell which commit made this change.")),
        }
    }

    /// Have Claude Code explain a commit and its changes; saved as a note
    pub fn explain_commit(&self, sha: &str, cancel: &CancelToken) -> Result<()> {
        self.explain_commit_once(sha, cancel).map(|_| ())
    }

    /// [`Self::explain_commit`], unless another explanation of the commit
    /// finished while this one waited for it and left nothing to explain;
    /// returns whether it asked Claude Code
    fn explain_commit_once(&self, sha: &str, cancel: &CancelToken) -> Result<bool> {
        self.require_infer()?;
        let seen = self.get_commit(sha, cancel)?.note;
        let note_lock = self.info.note_lock(sha);
        let _writing = lock(&note_lock);
        let commit = self.refresh_commit(sha, cancel)?;
        if commit.note != seen && !self.commit_needs_explaining(&commit) {
            return Ok(false);
        }
        let files: Vec<_> = self
            .repo
            .commit_diffs(sha, cancel)?
            .into_iter()
            .map(|(change, diff)| (change, Some(diff)))
            .collect();
        let ignore_rules = self.info.ignore_rules();
        let options = self.infer_options(ignore_rules.as_deref());
        let inferred = infer_explanation(&commit, &files, &options, cancel)?;
        let note = compose_inferred_note(&commit, &inferred, &self.config.message_format);
        cancel.check()?;
        self.repo
            .write_note(sha, &note, &self.config.notes_ref, cancel)?;
        self.refresh_commit(sha, cancel)?;
        Ok(true)
    }

    /// Have Claude Code explain the change blocks of the commit `sha` in
    /// `files` (its diffs, cut down to those blocks) that its note doesn't
    /// explain (left out of the prompt for its size), and add them to the
    /// note
    ///
    /// Only those blocks are sent, with the commit's message, so no cap
    /// cuts them. Blocks the note explains by the time it's this call's turn
    /// to write it are left out. Returns how many hunks Claude Code
    /// explained.
    fn explain_blocks(
        &self,
        sha: &str,
        files: Vec<(GitFileChange, GitFileDiff)>,
        cancel: &CancelToken,
    ) -> Result<usize> {
        self.require_infer()?;
        let note_lock = self.info.note_lock(sha);
        let _writing = lock(&note_lock);
        let commit = self.refresh_commit(sha, cancel)?;
        let note = Self::note(&commit);
        let explained_changes = note.as_ref().map_or(&[][..], |note| &note.changes[..]);
        let files: Vec<_> = files
            .into_iter()
            .filter_map(|(change, mut diff)| {
                let paths = diff_paths(&diff);
                diff.hunks
                    .retain(|block| !block_is_explained(&paths, block, explained_changes));
                (!diff.hunks.is_empty()).then_some((change, Some(diff)))
            })
            .collect();
        if files.is_empty() {
            return Ok(0);
        }
        let ignore_rules = self.info.ignore_rules();
        let options = self.infer_options(ignore_rules.as_deref());
        let inferred = infer_commit_changes(&commit, &files, &options, cancel)?;
        let same = |a: &CommitHunk, b: &CommitHunk| {
            a.path == b.path && a.old_start == b.old_start && a.new_start == b.new_start
        };
        let mut changes: Vec<ChangeExplanation> = note
            .as_ref()
            .map(|note| note.changes.clone())
            .unwrap_or_default()
            .into_iter()
            .filter(|old| {
                !inferred
                    .changes
                    .iter()
                    .any(|new| same(&old.hunk, &new.hunk))
            })
            .collect();
        let explained = inferred.changes.len();
        changes.extend(inferred.changes);
        changes.sort_by(|a, b| {
            (&a.hunk.path, a.hunk.new_start, a.hunk.old_start).cmp(&(
                &b.hunk.path,
                b.hunk.new_start,
                b.hunk.old_start,
            ))
        });
        let head = note.map(|note| note.head).unwrap_or_default();
        let text = compose_note(&head, &inferred.model, &changes);
        cancel.check()?;
        self.repo
            .write_note(sha, &text, &self.config.notes_ref, cancel)?;
        self.refresh_commit(sha, cancel)?;
        Ok(explained)
    }

    /// Have Claude Code explain a file's staged or unstaged hunks
    ///
    /// Kept in memory only; returns how many hunks were explained.
    pub fn explain_uncommitted(
        &self,
        view: &FileView,
        layer: UncommittedLayer,
        cancel: &CancelToken,
    ) -> Result<usize> {
        self.require_infer()?;
        match &view.attribution {
            Some(attribution) => self.explain_layer(attribution, layer, cancel),
            None => {
                let attribution = self.attributor.attribute(view.change(), cancel)?;
                self.explain_layer(&attribution, layer, cancel)
            }
        }
    }

    fn explain_layer(
        &self,
        attribution: &FileAttribution,
        layer: UncommittedLayer,
        cancel: &CancelToken,
    ) -> Result<usize> {
        let ignore_rules = self.info.ignore_rules();
        let options = self.infer_options(ignore_rules.as_deref());
        let mut kept = 0;
        let Some(layers) = &attribution.layers else {
            return Ok(0);
        };
        for diff in layers.get(layer) {
            if self.info.uncommitted.is_explained(layer, diff) {
                continue;
            }
            let result = infer_uncommitted_explanation(layer, diff, &options, cancel)?;
            kept += self.info.uncommitted.remember(layer, diff, &result.changes);
        }
        Ok(kept)
    }

    fn commit_needs_explaining(&self, commit: &GitCommit) -> bool {
        let explanation = explain_commit(commit, &self.config.message_format);
        let note = Self::note(commit);
        let changes_explained = note.as_ref().is_some_and(ParsedNote::changes_explained);
        let outdated = note.as_ref().is_some_and(ParsedNote::changes_outdated);
        explanation.why.is_none() || !changes_explained || outdated
    }

    /// Explain every change of every file not explained yet: each commit
    /// and the change blocks its note left out, then the staged, unstaged
    /// and untracked changes
    ///
    /// Keeps going past single failures; a missing Claude Code or a cancel
    /// stops at once. Returns how many were explained, or fails naming what
    /// failed.
    pub fn explain_all(
        &self,
        cancel: &CancelToken,
        progress: Option<ExplainProgress<'_>>,
    ) -> Result<usize> {
        self.require_infer()?;
        let progress = progress.unwrap_or(&|_, _| {});
        let mut failures: Vec<String> = Vec::new();
        let mut done = 0;
        // Counts what was explained, notes what failed; stops for good at a
        // missing Claude Code or a cancel
        let mut settle = |what: String, result: Result<usize>| match result {
            Ok(count) => {
                if count > 0 {
                    done += 1;
                }
                Ok(())
            }
            Err(err) if err.is_claude_not_found() || err.is_cancelled() => Err(err),
            Err(err) => {
                failures.push(format!("{what}: {err}"));
                Ok(())
            }
        };
        progress("Finding what isn’t explained yet…", 0.0);
        let commits = self.range_commits(cancel)?;
        let c = &self.comparison;
        let entries = if c.uncommitted() || c.base.uncommitted() {
            self.files(cancel)?
        } else {
            Vec::new()
        };
        let rules = create_ignore_rules(self.info.ignore_rules().as_deref());
        // Each commit and each file is a step
        let steps = (commits.len() + entries.len()).max(1) as f32;
        for (index, commit) in commits.iter().enumerate() {
            cancel.check()?;
            let sha = short_sha(&commit.sha);
            let share = index as f32 / steps;
            // As cached now: a card's Explain may have changed it
            let commit = self.get_commit(&commit.sha, cancel)?;
            if self.commit_needs_explaining(&commit) {
                progress(
                    &format!(
                        "Explaining commit {sha} ({} of {})…",
                        index + 1,
                        commits.len()
                    ),
                    share,
                );
                settle(
                    sha.to_string(),
                    self.explain_commit_once(&commit.sha, cancel)
                        .map(usize::from),
                )?;
            }
            let starting = |path: Option<&str>| {
                let text = match path {
                    Some(path) => format!(
                        "Explaining the rest of commit {sha} in {path} ({} of {})…",
                        index + 1,
                        commits.len()
                    ),
                    None => format!(
                        "Explaining commit {sha} again, block by block ({} of {})…",
                        index + 1,
                        commits.len()
                    ),
                };
                progress(&text, share);
            };
            for (what, result) in self.complete_note(&commit.sha, &rules, cancel, &starting) {
                settle(format!("{sha} {what}").trim_end().to_string(), result)?;
            }
        }

        for (index, entry) in entries.iter().enumerate() {
            cancel.check()?;
            let path = &entry.change.path;
            progress(
                &format!(
                    "Explaining {path} ({} of {} files)…",
                    index + 1,
                    entries.len()
                ),
                (commits.len() + index) as f32 / steps,
            );
            let attribution = match self.attributor.attribute(&entry.change, cancel) {
                Ok(attribution) => attribution,
                Err(err) => {
                    settle(path.clone(), Err(err))?;
                    continue;
                }
            };
            for layer in UncommittedLayer::ALL {
                let diffs = attribution
                    .layers
                    .as_ref()
                    .map_or(&[][..], |layers| layers.get(layer));
                if diffs
                    .iter()
                    .all(|diff| self.info.uncommitted.is_explained(layer, diff))
                {
                    continue;
                }
                settle(
                    path.clone(),
                    self.explain_layer(&attribution, layer, cancel)
                        .or_else(skip_unexplainable),
                )?;
            }
        }
        progress("Explained", 1.0);

        if let Some(first) = failures.first() {
            return Err(Error::msg(format!(
                "Explained {done}; {} failed. First: {first}",
                failures.len()
            )));
        }
        Ok(done)
    }

    /// Explain what the note of the commit `sha` leaves out: the whole
    /// commit again when the note gives several change blocks one
    /// explanation (it predates explaining them apart), then the change
    /// blocks it doesn't explain, one request per file (they are sent
    /// uncapped). `starting` is told each file first, or None for the whole
    /// commit. Returns each request's outcome, by file (empty for the whole
    /// commit).
    ///
    /// A commit without a note explaining its changes has nothing left out
    /// (all of it is to explain). A note found complete, or after what it
    /// left out was asked for, is remembered, so that it isn't diffed
    /// against its commit again until it changes.
    fn complete_note(
        &self,
        sha: &str,
        rules: &IgnoreRules,
        cancel: &CancelToken,
        starting: &dyn Fn(Option<&str>),
    ) -> Vec<(String, Result<usize>)> {
        let stops = |result: &Result<usize>| {
            result
                .as_ref()
                .is_err_and(|err| err.is_claude_not_found() || err.is_cancelled())
        };
        let mut outcomes = Vec::new();
        let commit = match self.get_commit(sha, cancel) {
            Ok(commit) => commit,
            Err(err) => return vec![(String::new(), Err(err))],
        };
        let Some(mut note) = Self::note(&commit) else {
            return outcomes;
        };
        if !note.changes_explained()
            || note.changes_outdated()
            || lock(&self.info.complete_notes).get(sha) == Some(&commit.note)
        {
            return outcomes;
        }
        let diffs: Vec<_> = match self.repo.commit_diffs(sha, cancel) {
            Ok(diffs) => diffs
                .into_iter()
                .filter(|(change, diff)| {
                    !diff.binary
                        && !is_ignored(rules, &change.path, change.original_path.as_deref())
                })
                .collect(),
            Err(err) => return vec![(String::new(), Err(err))],
        };
        if !note.changes_by_block && joins_blocks(&diffs, &note.changes) {
            starting(None);
            let result = self.explain_commit_once(sha, cancel).map(usize::from);
            let failed = result.is_err();
            outcomes.push((String::new(), result));
            if failed {
                return outcomes;
            }
            // The new note may leave blocks out in turn
            match self
                .get_commit(sha, cancel)
                .map(|commit| Self::note(&commit))
            {
                Ok(Some(new)) if new.changes_explained() => note = new,
                Ok(_) => return outcomes,
                Err(err) => {
                    outcomes.push((String::new(), Err(err)));
                    return outcomes;
                }
            }
        }
        for (change, mut diff) in diffs {
            let paths = diff_paths(&diff);
            diff.hunks = diff
                .hunks
                .iter()
                .flat_map(GitDiffHunk::blocks)
                .filter(|block| !block_is_explained(&paths, block, &note.changes))
                .collect();
            if diff.hunks.is_empty() {
                continue;
            }
            starting(Some(&change.path));
            let path = change.path.clone();
            let result = self
                .explain_blocks(sha, vec![(change, diff)], cancel)
                .or_else(skip_unexplainable);
            let stop = stops(&result);
            outcomes.push((path, result));
            if stop {
                return outcomes;
            }
        }
        // What Claude Code chose not to explain isn't asked for again
        if outcomes.iter().all(|(_, result)| result.is_ok())
            && let Ok(commit) = self.get_commit(sha, cancel)
        {
            lock(&self.info.complete_notes).insert(sha.to_string(), commit.note);
        }
        outcomes
    }
}

/// A file's changes that are ignored or have nothing to send count as
/// nothing explained, not as a failure
fn skip_unexplainable(err: Error) -> Result<usize> {
    match err {
        Error::IgnoredFile(_) | Error::NothingToExplain(_) => Ok(0),
        err => Err(err),
    }
}

/// Whether another of the file's changes by the same commit got one of
/// `changes` too: the note predates explaining change blocks apart and git
/// joined them into one hunk, so both show the same text until the
/// commit's changes are explained again
fn shares_a_joined_change(
    note: &ParsedNote,
    changes: &[&ChangeExplanation],
    groups: &[ChangeGroup],
    index: usize,
) -> bool {
    if note.changes_by_block || changes.is_empty() {
        return false;
    }
    let sha = &groups[index].sha;
    groups.iter().enumerate().any(|(other, group)| {
        other != index
            && group.uncommitted.is_none()
            && group.sha == *sha
            && find_change_explanations(&group.origins, &note.changes)
                .into_iter()
                .any(|found| changes.iter().any(|&mine| std::ptr::eq(mine, found)))
    })
}

fn label(
    repo: &GitRepository,
    revision: &Revision,
    sha: &str,
    cancel: &CancelToken,
) -> Result<String> {
    let suffix = revision
        .worktree
        .as_ref()
        .map_or_else(String::new, |worktree| format!(" ({})", basename(worktree)));
    match revision.kind {
        RevisionKind::Index => return Ok(format!("Staged{suffix}")),
        RevisionKind::WorkingTree => return Ok(format!("Working Tree{suffix}")),
        RevisionKind::Commit => {}
    }
    let ref_name = revision.ref_name.as_deref().unwrap_or(sha);
    if sha.starts_with(ref_name) {
        return repo.short_sha(sha, cancel);
    }
    for prefix in ["refs/heads/", "refs/remotes/", "refs/tags/"] {
        if let Some(short) = ref_name.strip_prefix(prefix) {
            return Ok(short.to_string());
        }
    }
    Ok(ref_name.to_string())
}

/// A card's guide from those of the changes it shows: the highest risk
fn set_guide<'a>(card: &mut Card, guides: impl Iterator<Item = &'a ChangeGuide>) {
    card.change_risk = guides.filter_map(|g| g.risk).max();
}

/// Lines of the commits' own diffs (and the uncommitted layers' diffs) the
/// file's cards show: per commit or layer, side and path, each line's
/// group. Line numbers of one commit's diff mean nothing in another's.
type CoveredLines = HashMap<
    (Option<String>, Option<UncommittedLayer>, Side, String),
    std::collections::BTreeMap<usize, usize>,
>;

/// The [`CoveredLines`] of a file's groups, built once for all its cards
fn covered_lines(groups: &[ChangeGroup]) -> CoveredLines {
    let mut covered = CoveredLines::new();
    for (index, group) in groups.iter().enumerate() {
        for origin in &group.origins {
            covered
                .entry((
                    group.sha.clone(),
                    group.uncommitted,
                    origin.side,
                    origin.path.clone(),
                ))
                .or_default()
                .insert(origin.line, index);
        }
    }
    covered
}

/// The steps of the changes a card shows, with their lines where the
/// comparison has them
///
/// A step's lines are found through the group's origins, so only lines of
/// the card (`groups[index]`) count. A step on lines another card of the
/// same commit or layer shows (one change can span several cards) is left
/// to that card; a step whose lines no such card shows (a later commit
/// changed them again), or without lines, is on every card of its change,
/// without lines.
fn card_steps<'a>(
    groups: &[ChangeGroup],
    index: usize,
    covered: &CoveredLines,
    changes: impl Iterator<Item = (&'a CommitHunk, &'a [ChangeStep])>,
) -> Vec<CardStep> {
    let group = &groups[index];
    let mut steps = Vec::new();
    for (hunk, change_steps) in changes {
        for step in change_steps {
            // None: without lines; Some(None): another card's step;
            // Some(Some(lines)): its lines on this card, if any
            let lines = step.lines.map(|at| {
                let (first, last) = at.in_hunk(hunk);
                let path = match at.side {
                    Side::Old => hunk.original_path.as_deref().unwrap_or(&hunk.path),
                    Side::New => hunk.path.as_str(),
                };
                let covers = |origin: &crate::models::LineOrigin| {
                    origin.side == at.side
                        && origin.path == path
                        && (first..=last).contains(&origin.line)
                };
                let mut places = group
                    .origins
                    .iter()
                    .zip(&group.origin_places)
                    .filter(|(origin, _)| covers(origin))
                    .map(|(_, place)| *place);
                let Some((side, line)) = places.next() else {
                    // Another card's step, or on no card's lines
                    let key = (
                        group.sha.clone(),
                        group.uncommitted,
                        at.side,
                        path.to_string(),
                    );
                    let elsewhere = covered.get(&key).is_some_and(|lines| {
                        lines.range(first..=last).any(|(_, &other)| other != index)
                    });
                    return (!elsewhere).then_some(None);
                };
                let (start, end) = places
                    .filter(|(other, _)| *other == side)
                    .fold((line, line), |(start, end), (_, line)| {
                        (start.min(line), end.max(line))
                    });
                Some(Some((side, LineRange::new(start, end))))
            });
            let lines = match lines {
                // On lines another card shows
                Some(None) => continue,
                Some(Some(found)) => found,
                None => None,
            };
            steps.push(CardStep {
                lines,
                text: step.text.clone(),
            });
        }
    }
    steps
}

/// A diff's paths: in the old version and the new
fn diff_paths(diff: &GitFileDiff) -> (Option<String>, Option<String>) {
    (diff.old_path.clone(), diff.new_path.clone())
}

/// Whether a change block has one of `origins`' lines (in the diff with
/// `paths`, old and new)
fn block_has_lines(
    paths: &(Option<String>, Option<String>),
    block: &GitDiffHunk,
    origins: &[LineOrigin],
) -> bool {
    origins.iter().any(|origin| {
        let (path, start, count) = match origin.side {
            Side::Old => (&paths.0, block.old_start, block.old_count),
            Side::New => (&paths.1, block.new_start, block.new_count),
        };
        path.as_deref() == Some(origin.path.as_str())
            && start <= origin.line
            && origin.line < start + count
    })
}

/// Whether one of `changes` (a note's) explains a changed line of a change
/// block (in the diff with `paths`, old and new), as a card's lines find
/// their explanations
fn block_is_explained(
    paths: &(Option<String>, Option<String>),
    block: &GitDiffHunk,
    changes: &[ChangeExplanation],
) -> bool {
    !find_change_explanations(&block_origins(paths, block), changes).is_empty()
}

/// The added and deleted lines of a change block (in the diff with
/// `paths`, old and new)
fn block_origins(paths: &(Option<String>, Option<String>), block: &GitDiffHunk) -> Vec<LineOrigin> {
    block
        .lines
        .iter()
        .filter_map(|line| match (line.kind, &paths.0, &paths.1) {
            (DiffLineKind::Add, _, Some(path)) => {
                Some(LineOrigin::new(path, Side::New, line.new_line?))
            }
            (DiffLineKind::Delete, Some(path), _) => {
                Some(LineOrigin::new(path, Side::Old, line.old_line?))
            }
            _ => None,
        })
        .collect()
}

/// Whether one of `changes` (a note's) explains two or more change blocks
/// of `diffs` (its commit's): git joined them into one hunk, which the
/// note explained as one, as [`shares_a_joined_change`] finds per card
fn joins_blocks(diffs: &[(GitFileChange, GitFileDiff)], changes: &[ChangeExplanation]) -> bool {
    let mut seen: Vec<&ChangeExplanation> = Vec::new();
    for (_, diff) in diffs {
        let paths = diff_paths(diff);
        for block in diff.hunks.iter().flat_map(GitDiffHunk::blocks) {
            for found in find_change_explanations(&block_origins(&paths, &block), changes) {
                if seen.iter().any(|&other| std::ptr::eq(other, found)) {
                    return true;
                }
                seen.push(found);
            }
        }
    }
    false
}

fn join_texts<'a>(texts: impl Iterator<Item = &'a str>) -> String {
    texts
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

// -- the file list as a tree (from gitcompare.py) --

/// A row of the file list: a folder (with children) or a file
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    /// A file's name, or a folder's path relative to its parent row (chains
    /// of single folders are joined, e.g. `a/b`)
    pub name: String,
    /// None for folders
    pub entry: Option<FileEntry>,
    pub children: Vec<FileRow>,
}

enum TreeNode {
    Folder(Vec<(String, TreeNode)>),
    File(FileEntry),
}

/// Folders and files, folders first; chains of single folders compacted
pub fn build_file_tree(entries: &[FileEntry]) -> Vec<FileRow> {
    let mut root: Vec<(String, TreeNode)> = Vec::new();
    for entry in entries {
        let parts: Vec<&str> = entry.change.path.split('/').collect();
        let mut node = &mut root;
        for part in &parts[..parts.len() - 1] {
            let key = format!("{part}/");
            let position = match node.iter().position(|(name, _)| *name == key) {
                Some(position) => position,
                None => {
                    node.push((key, TreeNode::Folder(Vec::new())));
                    node.len() - 1
                }
            };
            node = match &mut node[position].1 {
                TreeNode::Folder(children) => children,
                TreeNode::File(_) => unreachable!("folder keys end with a slash"),
            };
        }
        let mut name = parts[parts.len() - 1].to_string();
        if node.iter().any(|(existing, _)| *existing == name) {
            // A file deleted from the index but still on disk is listed
            // twice: deleted, and untracked
            let what = if entry.change.untracked {
                "untracked".to_string()
            } else {
                entry.change.status.to_string()
            };
            name = format!("{name} ({what})");
        }
        node.push((name, TreeNode::File(entry.clone())));
    }

    fn rows(mut node: Vec<(String, TreeNode)>) -> Vec<FileRow> {
        node.sort_by_key(|(name, _)| name.to_lowercase());
        let mut folders = Vec::new();
        let mut files = Vec::new();
        for (name, value) in node {
            match value {
                TreeNode::Folder(mut children) => {
                    let mut label = name.trim_end_matches('/').to_string();
                    // Compact a/b/c into one row while each has only one folder
                    while children.len() == 1 && matches!(children[0].1, TreeNode::Folder(_)) {
                        let Some((child_name, TreeNode::Folder(grandchildren))) = children.pop()
                        else {
                            break;
                        };
                        label.push('/');
                        label.push_str(child_name.trim_end_matches('/'));
                        children = grandchildren;
                    }
                    folders.push(FileRow {
                        name: label,
                        entry: None,
                        children: rows(children),
                    });
                }
                TreeNode::File(entry) => files.push(FileRow {
                    name,
                    entry: Some(entry),
                    children: Vec::new(),
                }),
            }
        }
        folders.extend(files);
        folders
    }

    rows(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_takes_the_highest_risk_of_its_changes() {
        let mut card = Card::default();
        let risky = |risk| ChangeGuide {
            risk,
            ..Default::default()
        };
        set_guide(
            &mut card,
            [risky(Some(2)), risky(None), risky(Some(4))].iter(),
        );
        assert_eq!(card.change_risk, Some(4));
        set_guide(&mut card, [risky(None)].iter());
        assert_eq!(card.change_risk, None);
    }
}
