//! One open comparison: its files, their Why cards, explaining and builds
//! (port of `session.py`, plus the file tree of `gitcompare.py`)
//!
//! Every method here blocks on git or Claude Code, so the UI calls them on
//! a background task. A session can be shared between threads (`Arc`); its
//! caches are guarded by a lock so that an explain job and a file load can
//! overlap.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::agent::detect_agent;
use crate::agent_setup::{AgentSetupOptions, plan_agent_setup, write_agent_setup};
use crate::attribution::ComparisonAttributor;
use crate::build_facts::{
    BuildAgent, BuildChange, BuildCommit, BuildFacts, BuildFile, BuildFileEntry, BuildRevision,
    BuildStats, merge_ranges,
};
use crate::build_file::BuildHeader;
use crate::build_index::{find_builds, read_build_headers, resolve_build_folder};
use crate::build_narrative::write_narrative;
use crate::cards::{Card, CardKind, ExplainAction};
use crate::claude::RunClaude;
use crate::comparison::{changed_files_with_stats, resolve_comparison};
use crate::error::{Error, Result};
use crate::explanation::{
    MessageFormat, ParsedNote, explain_commit, find_change_explanations, parse_note,
};
use crate::generate_build::write_build;
use crate::git::{GitOptions, GitRepository, LogQuery, realpath};
use crate::ignore::{create_ignore_rules, is_ignored};
use crate::infer::{
    InferOptions, compose_inferred_note, infer_explanation, infer_uncommitted_explanation,
};
use crate::models::{
    ChangeGroup, Comparison, ComparisonRelation, ExplanationSource, FileAttribution, GitCommit,
    GitFileChange, GitFileStatus, LayerDiffsUsed, Revision, RevisionKind, UncommittedLayer,
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

/// Reports what a long operation is doing, e.g. "Explaining commit …"
pub type Progress<'a> = &'a dyn Fn(&str);

fn no_progress(_: &str) {}

/// The settings a session reads; the app fills it from its preferences
#[derive(Clone)]
pub struct SessionConfig {
    /// The Claude Code CLI (`ailens-claude-path`)
    pub claude_path: String,
    /// Whether Claude Code may be asked (`ailens-infer-enabled`)
    pub infer_enabled: bool,
    /// Model that explains commits (`ailens-infer-model`)
    pub infer_model: String,
    /// Model that writes builds (`ailens-build-model`)
    pub build_model: String,
    /// Build folder relative to the repository (`ailens-build-folder`)
    pub build_folder: String,
    /// Notes ref name (`ailens-notes-ref`)
    pub notes_ref: String,
    pub message_format: MessageFormat,
    /// Extra agent author regex (`ailens-author-pattern`); None when empty
    pub author_pattern: Option<String>,
    /// Written as `generator:` into builds
    pub generator: String,
    /// Replaces the Claude Code call, e.g. in tests
    pub run_claude: Option<RunClaude>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            claude_path: "claude".into(),
            infer_enabled: true,
            infer_model: "claude-sonnet-5-5".into(),
            build_model: "claude-opus-5-5".into(),
            build_folder: "agent".into(),
            notes_ref: "ai-lens".into(),
            message_format: MessageFormat::default(),
            author_pattern: None,
            generator: "meld".into(),
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
            .field("build_model", &self.build_model)
            .field("build_folder", &self.build_folder)
            .field("notes_ref", &self.notes_ref)
            .field("message_format", &self.message_format)
            .field("author_pattern", &self.author_pattern)
            .field("generator", &self.generator)
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
}

impl RepositoryInfo {
    pub fn new(repo: GitRepository) -> Self {
        Self {
            root: repo.root.clone(),
            repo,
            uncommitted: UncommittedExplanations::default(),
            worktree_heads: Mutex::new(HashMap::new()),
        }
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

    pub fn build_folder(&self, config: &SessionConfig) -> Result<PathBuf> {
        resolve_build_folder(&self.root, &config.build_folder)
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

    /// Write the CLAUDE.md section and the /why skill; returns the changed
    /// files relative to the repository
    pub fn set_up_agents(
        &self,
        config: &SessionConfig,
        cancel: &CancelToken,
    ) -> Result<Vec<String>> {
        let options = AgentSetupOptions {
            build_folder: config.build_folder.clone(),
            notes_ref: config.notes_ref.clone(),
        };
        let files = plan_agent_setup(&self.root, &options)?;
        cancel.check()?;
        write_agent_setup(&files)?;
        Ok(files
            .iter()
            .filter(|file| file.changed)
            .map(|file| {
                file.path
                    .strip_prefix(&self.root)
                    .unwrap_or(&file.path)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect())
    }
}

#[derive(Default)]
struct SessionState {
    entries: Option<Vec<FileEntry>>,
    commits: HashMap<String, GitCommit>,
    range: Option<Vec<GitCommit>>,
    builds: Option<Arc<Vec<BuildHeader>>>,
    patch_ids: HashMap<String, Option<String>>,
}

/// A resolved comparison, its files, and their explanations
///
/// What the Git Compare tab drives: [`Self::files`] for the file list,
/// [`Self::file_view`] for both versions of a file and its cards,
/// [`Self::explain_card`] / [`Self::explain_all`] for the Explain button
/// and [`Self::generate_build`] for Explain & Build.
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

    pub fn can_build(&self) -> bool {
        self.build_unavailable_reason().is_empty()
    }

    /// Why Explain & Build is off; empty when it is available
    pub fn build_unavailable_reason(&self) -> String {
        let c = &self.comparison;
        if c.uncommitted() || c.base.uncommitted() {
            return "Builds cover commits only, not staged or unstaged changes".into();
        }
        if c.relation == ComparisonRelation::Same {
            return "Base and target are the same commit".into();
        }
        String::new()
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

    fn refresh_commit(&self, sha: &str, cancel: &CancelToken) -> Result<()> {
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
        Ok(())
    }

    /// The commit as `git log` shows it, with its AI Lens note
    pub fn commit_message(&self, sha: &str, cancel: &CancelToken) -> Result<String> {
        let commit = self.get_commit(sha, cancel)?;
        let mut lines = vec![
            format!("commit {}", commit.sha),
            format!("Author: {} <{}>", commit.author.name, commit.author.email),
            format!("Date:   {}", commit.author.date),
            String::new(),
            commit.subject.clone(),
        ];
        if !commit.body.trim().is_empty() {
            lines.extend([String::new(), commit.body.trim_end().to_string()]);
        }
        if let Some(note) = &commit.note {
            lines.extend([
                String::new(),
                format!("Notes ({}):", self.config.notes_ref),
                note.trim_end().to_string(),
            ]);
        }
        Ok(lines.join("\n"))
    }

    fn builds_for(
        &self,
        shas: &[String],
        cancel: &CancelToken,
    ) -> Result<HashMap<String, Vec<String>>> {
        let cached = lock(&self.state).builds.clone();
        let headers = match cached {
            Some(headers) => headers,
            None => {
                let headers = match self.info.build_folder(&self.config) {
                    Ok(folder) if folder.is_dir() => read_build_headers(&folder),
                    Ok(_) => Ok(Vec::new()),
                    Err(err) => Err(err),
                };
                let headers = Arc::new(headers.unwrap_or_else(|err| {
                    log::info!("Can't read builds: {err}");
                    Vec::new()
                }));
                lock(&self.state).builds = Some(headers.clone());
                headers
            }
        };
        if headers.is_empty() {
            return Ok(HashMap::new());
        }
        let known: HashSet<&str> = headers
            .iter()
            .flat_map(|header| &header.commits)
            .map(|commit| commit.sha.as_str())
            .collect();
        let with_patch_ids = headers
            .iter()
            .flat_map(|header| &header.commits)
            .any(|commit| commit.patch_id.is_some());
        let missing: Vec<&str> = {
            let state = lock(&self.state);
            shas.iter()
                .map(String::as_str)
                .filter(|sha| !known.contains(sha) && !state.patch_ids.contains_key(*sha))
                .collect()
        };
        if with_patch_ids && !missing.is_empty() {
            let ids = self.repo.patch_ids(&missing, cancel)?;
            let mut state = lock(&self.state);
            for sha in missing {
                state
                    .patch_ids
                    .insert(sha.to_string(), ids.get(sha).cloned());
            }
        }
        let state = lock(&self.state);
        Ok(shas
            .iter()
            .map(|sha| {
                let patch_id = state.patch_ids.get(sha).cloned().flatten();
                (sha.clone(), find_builds(&headers, sha, patch_id.as_deref()))
            })
            .collect())
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
        let range_order: HashMap<String, i32> = self
            .range_commits(cancel)?
            .into_iter()
            .enumerate()
            .map(|(index, commit)| (commit.sha, index as i32))
            .collect();
        let mut shas: Vec<String> = attribution
            .groups
            .iter()
            .filter_map(|group| group.sha.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        shas.sort();
        let builds = self.builds_for(&shas, cancel)?;
        let empty = LayerDiffsUsed::default();
        let layers = attribution.layers.as_ref().unwrap_or(&empty);
        let placed: HashMap<UncommittedLayer, Vec<PlacedExplanation>> = UncommittedLayer::ALL
            .iter()
            .map(|&layer| (layer, self.info.uncommitted.place(layer, layers.get(layer))))
            .collect();
        let mut cards = Vec::new();
        for (index, group) in attribution.groups.iter().enumerate() {
            cards.push(self.card(group, index, &range_order, &builds, layers, &placed, cancel)?);
        }
        Ok(cards)
    }

    #[allow(clippy::too_many_arguments)]
    fn card(
        &self,
        group: &ChangeGroup,
        index: usize,
        range_order: &HashMap<String, i32>,
        builds: &HashMap<String, Vec<String>>,
        layers: &LayerDiffsUsed,
        placed: &HashMap<UncommittedLayer, Vec<PlacedExplanation>>,
        cancel: &CancelToken,
    ) -> Result<Card> {
        let infer = self.config.infer_enabled;
        let by = match (group.uncommitted, &group.sha) {
            (Some(layer), _) => layer.as_str().to_string(),
            (None, Some(sha)) => sha.clone(),
            (None, None) => "unknown".to_string(),
        };
        let common = Card {
            key: format!("{}:{by}:{index}", group.hunk_index),
            added: group.added.clone(),
            deleted: group.deleted.clone(),
            color_index: -1,
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
            return Ok(Card {
                kind: match layer {
                    UncommittedLayer::Staged => CardKind::Staged,
                    UncommittedLayer::Unstaged => CardKind::Unstaged,
                },
                change_what: join_texts(changes.iter().map(|c| c.what.as_str())),
                change_why: join_texts(changes.iter().map(|c| c.why.as_str())),
                change_model: changes.first().map(|c| c.model.clone()).unwrap_or_default(),
                explain: (infer && !explained).then_some(ExplainAction::Uncommitted),
                ..common
            });
        }
        let Some(sha) = &group.sha else {
            return Ok(Card {
                kind: CardKind::Unknown,
                ..common
            });
        };

        let commit = self.get_commit(sha, cancel)?;
        let explanation = explain_commit(&commit, &self.config.message_format);
        let agent = detect_agent(&commit, self.config.author_pattern.as_deref());
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
        } else if !changes_explained {
            Some(ExplainAction::Changes)
        } else {
            None
        };
        let message = if commit.body.is_empty() {
            commit.subject.clone()
        } else {
            format!("{}\n\n{}", commit.subject, commit.body)
        };
        Ok(Card {
            kind: CardKind::Commit,
            sha: Some(commit.sha.clone()),
            short_sha: short_sha(&commit.sha).to_string(),
            subject: commit.subject.clone(),
            message,
            author: commit.author.name.clone(),
            date: commit.author.date.clone(),
            agent_name: agent
                .as_ref()
                .map(|a| a.display_name.clone())
                .unwrap_or_default(),
            agent_model: agent
                .as_ref()
                .and_then(|a| a.model.clone())
                .unwrap_or_default(),
            what: explanation.what.clone(),
            why: explanation.why.clone().unwrap_or_default(),
            source: Some(explanation.source),
            inferred_by: explanation.model.clone().unwrap_or_default(),
            change_what: join_texts(changes.iter().map(|c| c.what.as_str())),
            change_why: join_texts(changes.iter().map(|c| c.why.as_str())),
            change_model: match &note {
                Some(note) if !changes.is_empty() => note.change_model.clone().unwrap_or_default(),
                _ => String::new(),
            },
            explain,
            color_index: range_order.get(&commit.sha).copied().unwrap_or(-1),
            builds: builds.get(&commit.sha).cloned().unwrap_or_default(),
            ..common
        })
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
        self.require_infer()?;
        let commit = self.get_commit(sha, cancel)?;
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
        self.refresh_commit(sha, cancel)
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
            kept += self
                .info
                .uncommitted
                .remember(layer, diff, &result.changes, &result.model);
        }
        Ok(kept)
    }

    fn commit_needs_explaining(&self, commit: &GitCommit) -> bool {
        let explanation = explain_commit(commit, &self.config.message_format);
        let changes_explained = Self::note(commit).is_some_and(|note| note.changes_explained());
        explanation.why.is_none() || !changes_explained
    }

    /// Explain every commit and uncommitted hunk not explained yet
    ///
    /// Keeps going past single failures; a missing Claude Code or a cancel
    /// stops at once. Returns how many were explained, or fails naming what
    /// failed.
    pub fn explain_all(
        &self,
        cancel: &CancelToken,
        progress: Option<Progress<'_>>,
        commits_only: bool,
    ) -> Result<usize> {
        self.require_infer()?;
        let progress = progress.unwrap_or(&no_progress);
        let mut failures: Vec<String> = Vec::new();
        let mut done = 0;
        let todo: Vec<GitCommit> = self
            .range_commits(cancel)?
            .into_iter()
            .filter(|c| self.commit_needs_explaining(c))
            .collect();
        for (index, commit) in todo.iter().enumerate() {
            cancel.check()?;
            progress(&format!(
                "Explaining commit {} ({} of {})…",
                short_sha(&commit.sha),
                index + 1,
                todo.len()
            ));
            match self.explain_commit(&commit.sha, cancel) {
                Ok(()) => done += 1,
                Err(err) if err.is_claude_not_found() || err.is_cancelled() => return Err(err),
                Err(err) => failures.push(format!("{}: {err}", short_sha(&commit.sha))),
            }
        }

        let c = &self.comparison;
        if !commits_only && (c.uncommitted() || c.base.uncommitted()) {
            let entries = self.files(cancel)?;
            for (index, entry) in entries.iter().enumerate() {
                cancel.check()?;
                let attribution = self.attributor.attribute(&entry.change, cancel)?;
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
                    progress(&format!(
                        "Explaining {} changes in {} ({} of {})…",
                        layer.as_str(),
                        entry.change.path,
                        index + 1,
                        entries.len()
                    ));
                    match self.explain_layer(&attribution, layer, cancel) {
                        Ok(kept) => {
                            if kept > 0 {
                                done += 1;
                            }
                        }
                        Err(Error::IgnoredFile(_) | Error::NothingToExplain(_)) => continue,
                        Err(err) if err.is_claude_not_found() || err.is_cancelled() => {
                            return Err(err);
                        }
                        Err(err) => failures.push(format!("{}: {err}", entry.change.path)),
                    }
                }
            }
        }

        if let Some(first) = failures.first() {
            return Err(Error::msg(format!(
                "Explained {done}; {} failed. First: {first}",
                failures.len()
            )));
        }
        Ok(done)
    }

    // -- builds --

    /// Everything the build file states as fact (a port of AI Lens'
    /// `gatherBuildFacts`)
    pub fn gather_build_facts(
        &self,
        cancel: &CancelToken,
        progress: Option<Progress<'_>>,
    ) -> Result<BuildFacts> {
        if !self.can_build() {
            return Err(Error::msg(self.build_unavailable_reason()));
        }
        let progress = progress.unwrap_or(&no_progress);
        let ignore_text = self.info.ignore_rules();
        let rules = create_ignore_rules(ignore_text.as_deref());
        let entries = self.files(cancel)?;
        let mut files: Vec<BuildFile> = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            cancel.check()?;
            progress(&format!(
                "Reading the changes ({index}/{} files)…",
                entries.len()
            ));
            let change = &entry.change;
            let ignored = is_ignored(&rules, &change.path, change.original_path.as_deref());
            let attribution = if ignored {
                None
            } else {
                Some(self.attributor.attribute(change, cancel)?)
            };
            let mut file_entries: Vec<BuildFileEntry> = Vec::new();
            let binary = attribution
                .as_ref()
                .and_then(|a| a.diff.as_ref())
                .is_some_and(|d| d.binary);
            if let (Some(attribution), false) = (&attribution, binary) {
                for card in self.build_cards(attribution, cancel)? {
                    let changes = (!card.change_what.is_empty() || !card.change_why.is_empty())
                        .then(|| {
                            vec![BuildChange {
                                what: card.change_what.clone(),
                                why: card.change_why.clone(),
                            }]
                        });
                    let position = file_entries
                        .iter()
                        .position(|e| e.sha == card.sha && e.changes == changes);
                    let found = match position {
                        Some(position) => &mut file_entries[position],
                        None => {
                            file_entries.push(BuildFileEntry {
                                sha: card.sha.clone(),
                                changes,
                                ..Default::default()
                            });
                            file_entries.last_mut().expect("just pushed")
                        }
                    };
                    let added: Vec<_> = found.added.iter().chain(&card.added).copied().collect();
                    found.added = merge_ranges(&added);
                    let deleted: Vec<_> =
                        found.deleted.iter().chain(&card.deleted).copied().collect();
                    found.deleted = merge_ranges(&deleted);
                }
            }
            files.push(BuildFile {
                path: change.path.clone(),
                original_path: change.original_path.clone(),
                status: change.status,
                additions: entry.additions,
                deletions: entry.deletions,
                binary: binary || entry.binary(),
                ignored,
                entries: file_entries,
                touched_by: attribution
                    .as_ref()
                    .map(|a| a.touched_by.clone())
                    .unwrap_or_default(),
            });
        }

        let commits = self.range_commits(cancel)?;
        let shas: Vec<&str> = commits.iter().map(|c| c.sha.as_str()).collect();
        let patch_ids = self.repo.patch_ids(&shas, cancel)?;
        let mut build_commits = Vec::new();
        for commit in &commits {
            let explanation = explain_commit(commit, &self.config.message_format);
            let agent = detect_agent(commit, self.config.author_pattern.as_deref());
            let note = Self::note(commit);
            let changes_inferred_by =
                note.as_ref()
                    .filter(|note| note.changes_explained())
                    .map(|note| {
                        note.change_model
                            .clone()
                            .unwrap_or_else(|| "a model".into())
                    });
            build_commits.push(BuildCommit {
                sha: commit.sha.clone(),
                short_sha: self.repo.short_sha(&commit.sha, cancel)?,
                patch_id: patch_ids.get(&commit.sha).cloned(),
                author: commit.author.name.clone(),
                agent: if agent.is_some() {
                    BuildAgent::ClaudeCode
                } else {
                    BuildAgent::Human
                },
                agent_model: agent.as_ref().and_then(|a| a.model.clone()),
                agent_name: agent.as_ref().map(|a| a.display_name.clone()),
                what: explanation.what.clone(),
                why: explanation.why.clone(),
                why_source: explanation.source,
                inferred_by: if explanation.source == ExplanationSource::Inferred {
                    explanation.model.clone()
                } else {
                    None
                },
                changes_inferred_by,
            });
        }

        let c = &self.comparison;
        let revision = |sha: &str, label: &str| -> Result<BuildRevision> {
            let ref_name = if sha.starts_with(label) || label == short_sha(sha) {
                None
            } else {
                Some(label.to_string())
            };
            Ok(BuildRevision {
                sha: sha.to_string(),
                short_sha: self.repo.short_sha(sha, cancel)?,
                ref_name,
            })
        };

        Ok(BuildFacts {
            base: revision(&c.base_sha, &self.base_label)?,
            target: revision(&c.target_sha, &self.target_label)?,
            merge_base: c.merge_base.clone(),
            relation: c.relation,
            stats: BuildStats {
                files: files.len(),
                insertions: files.iter().map(|f| f.additions.unwrap_or(0)).sum(),
                deletions: files.iter().map(|f| f.deletions.unwrap_or(0)).sum(),
                commits: build_commits.len(),
            },
            commits: build_commits,
            files,
        })
    }

    /// Explain & Build: explain what isn't yet (if allowed), then write the
    /// build file; returns its path
    pub fn generate_build(
        &self,
        cancel: &CancelToken,
        progress: Option<Progress<'_>>,
    ) -> Result<PathBuf> {
        if !self.can_build() {
            return Err(Error::msg(self.build_unavailable_reason()));
        }
        let progress = progress.unwrap_or(&no_progress);
        if self.config.infer_enabled {
            self.explain_all(cancel, Some(progress), true)?;
        }
        let facts = self.gather_build_facts(cancel, Some(progress))?;
        progress(&format!(
            "Writing the summary with {}…",
            self.config.build_model
        ));
        let narrative = write_narrative(
            &facts,
            &self.config.build_model,
            Some(&self.config.claude_path),
            self.config.run_claude.clone(),
            cancel,
        )?;
        cancel.check()?;
        let built = write_build(
            &self.root,
            &self.config.build_folder,
            &facts,
            &narrative,
            &self.config.generator,
            crate::time::now_timestamp(),
        )?;
        lock(&self.state).builds = None;
        Ok(built.path)
    }
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
