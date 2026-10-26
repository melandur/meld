//! Data shared by the AI Lens modules (port of `models.py`)
//!
//! Line numbers here are 1-based and ranges inclusive, as git prints them
//! and as the notes and build files store them.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct GitIdentity {
    pub name: String,
    pub email: String,
    /// Strict ISO 8601
    pub date: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GitTrailer {
    pub key: String,
    pub value: String,
}

impl GitTrailer {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct GitCommit {
    pub sha: String,
    pub parents: Vec<String>,
    pub author: GitIdentity,
    pub committer: GitIdentity,
    pub subject: String,
    /// Message without the subject line; includes the trailer block
    pub body: String,
    pub trailers: Vec<GitTrailer>,
    /// The commit's note in the notes ref the log was read with, if any
    pub note: Option<String>,
}

/// `git diff --name-status` letters
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GitFileStatus {
    Added,
    Copied,
    Deleted,
    Modified,
    Renamed,
    TypeChanged,
    Unmerged,
    Unknown,
}

impl GitFileStatus {
    pub fn from_letter(letter: char) -> Option<Self> {
        Some(match letter {
            'A' => Self::Added,
            'C' => Self::Copied,
            'D' => Self::Deleted,
            'M' => Self::Modified,
            'R' => Self::Renamed,
            'T' => Self::TypeChanged,
            'U' => Self::Unmerged,
            'X' => Self::Unknown,
            _ => return None,
        })
    }

    pub fn letter(self) -> char {
        match self {
            Self::Added => 'A',
            Self::Copied => 'C',
            Self::Deleted => 'D',
            Self::Modified => 'M',
            Self::Renamed => 'R',
            Self::TypeChanged => 'T',
            Self::Unmerged => 'U',
            Self::Unknown => 'X',
        }
    }
}

impl std::fmt::Display for GitFileStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.letter())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GitFileChange {
    pub status: GitFileStatus,
    /// Path in the target version (for deletions: the deleted path)
    pub path: String,
    /// Path in the base version, for renames and copies
    pub original_path: Option<String>,
    /// Rename/copy similarity, 0-100
    pub similarity: Option<u32>,
    /// Not tracked by git: only in the working tree (status `A`)
    pub untracked: bool,
}

impl GitFileChange {
    pub fn new(status: GitFileStatus, path: impl Into<String>) -> Self {
        Self {
            status,
            path: path.into(),
            original_path: None,
            similarity: None,
            untracked: false,
        }
    }

    pub fn renamed(
        status: GitFileStatus,
        path: impl Into<String>,
        original_path: impl Into<String>,
        similarity: Option<u32>,
    ) -> Self {
        Self {
            status,
            path: path.into(),
            original_path: Some(original_path.into()),
            similarity,
            untracked: false,
        }
    }

    pub fn untracked(path: impl Into<String>) -> Self {
        Self {
            untracked: true,
            ..Self::new(GitFileStatus::Added, path)
        }
    }

    /// Every path the change touches, target path first
    pub fn paths(&self) -> Vec<String> {
        match &self.original_path {
            Some(original) if !original.is_empty() && *original != self.path => {
                vec![self.path.clone(), original.clone()]
            }
            _ => vec![self.path.clone()],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiffLineKind {
    Context,
    Add,
    Delete,
}

impl DiffLineKind {
    /// The line's prefix in a unified diff
    pub fn prefix(self) -> char {
        match self {
            Self::Context => ' ',
            Self::Add => '+',
            Self::Delete => '-',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GitDiffLine {
    pub kind: DiffLineKind,
    pub content: String,
    /// Line in the old version; None for added lines
    pub old_line: Option<usize>,
    /// Line in the new version; None for deleted lines
    pub new_line: Option<usize>,
    pub no_newline_at_end: bool,
}

impl GitDiffLine {
    pub fn new(
        kind: DiffLineKind,
        content: impl Into<String>,
        old_line: Option<usize>,
        new_line: Option<usize>,
    ) -> Self {
        Self {
            kind,
            content: content.into(),
            old_line,
            new_line,
            no_newline_at_end: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GitDiffHunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    /// Text after the closing `@@`, usually the enclosing function
    pub section: String,
    pub lines: Vec<GitDiffLine>,
}

/// Context lines a change block keeps on each side, as `git diff -U3`
const BLOCK_CONTEXT: usize = 3;

impl GitDiffHunk {
    /// The hunk split into one hunk per change block: each run of added and
    /// deleted lines, with the context around it
    ///
    /// git joins changes whose context touches (up to twice the context
    /// apart) into one hunk, even with `--inter-hunk-context=0`; each block
    /// is a change of its own, explained apart. Context between two blocks
    /// is shared out so their ranges don't overlap. A hunk with one block
    /// comes back as it is.
    pub fn blocks(&self) -> Vec<GitDiffHunk> {
        // Each run of changed lines as [start, end) in `lines`
        let mut runs: Vec<(usize, usize)> = Vec::new();
        for (index, line) in self.lines.iter().enumerate() {
            if line.kind == DiffLineKind::Context {
                continue;
            }
            match runs.last_mut() {
                Some((_, end)) if *end == index => *end = index + 1,
                _ => runs.push((index, index + 1)),
            }
        }
        if runs.len() <= 1 {
            return vec![self.clone()];
        }

        // Where each line starts on each side; an empty side's start is the
        // line before, as git numbers it
        let mut old_line = if self.old_count == 0 {
            self.old_start + 1
        } else {
            self.old_start
        };
        let mut new_line = if self.new_count == 0 {
            self.new_start + 1
        } else {
            self.new_start
        };
        let mut positions = Vec::with_capacity(self.lines.len());
        for line in &self.lines {
            positions.push((old_line, new_line));
            if line.kind != DiffLineKind::Add {
                old_line += 1;
            }
            if line.kind != DiffLineKind::Delete {
                new_line += 1;
            }
        }

        let mut blocks = Vec::with_capacity(runs.len());
        let mut from = 0;
        for (index, &(_, end)) in runs.iter().enumerate() {
            let to = match runs.get(index + 1) {
                Some(&(next_start, _)) => end + BLOCK_CONTEXT.min((next_start - end).div_ceil(2)),
                None => self.lines.len(),
            };
            let lines = self.lines[from..to].to_vec();
            let old_count = lines.iter().filter(|l| l.kind != DiffLineKind::Add).count();
            let new_count = lines
                .iter()
                .filter(|l| l.kind != DiffLineKind::Delete)
                .count();
            let (old_start, new_start) = positions[from];
            blocks.push(GitDiffHunk {
                old_start: if old_count == 0 {
                    old_start - 1
                } else {
                    old_start
                },
                old_count,
                new_start: if new_count == 0 {
                    new_start - 1
                } else {
                    new_start
                },
                new_count,
                section: self.section.clone(),
                lines,
            });
            if let Some(&(next_start, _)) = runs.get(index + 1) {
                from = to.max(next_start.saturating_sub(BLOCK_CONTEXT));
            }
        }
        blocks
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GitFileDiff {
    /// None for added files
    pub old_path: Option<String>,
    /// None for deleted files
    pub new_path: Option<String>,
    pub binary: bool,
    pub hunks: Vec<GitDiffHunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GitBlameLine {
    /// For a normal blame, the commit that last changed the line; for
    /// `--reverse`, the last commit it existed in
    pub sha: String,
    /// Line number in the version of the file at `sha`
    pub original_line: usize,
    /// Line number in the blamed version of the file
    pub final_line: usize,
    /// The line predates the blamed range
    pub boundary: bool,
    /// Path of the file at `sha`
    pub filename: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GitRefType {
    Branch,
    Remote,
    Tag,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GitRef {
    pub kind: GitRefType,
    /// Short name, e.g. `main`, `origin/main`, `v1.0`
    pub name: String,
    /// Full name, e.g. `refs/heads/main`
    pub full_name: String,
    /// Commit the ref points to (annotated tags are peeled)
    pub sha: String,
    pub subject: String,
    /// Committer date of `sha`, strict ISO 8601
    pub date: String,
    /// The branch checked out in the repository's own worktree
    pub current: bool,
    /// Path of the worktree that has this branch checked out, if any
    pub worktree: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct GitWorktree {
    pub path: PathBuf,
    /// None for a worktree on an unborn branch
    pub head: Option<String>,
    /// Short branch name; None when detached
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    pub locked: bool,
    pub prunable: bool,
    /// The main worktree (the first one `git worktree list` prints)
    pub main: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RevisionKind {
    /// Any revision git can resolve to a commit
    Commit,
    /// The index of a worktree: its staged changes
    Index,
    /// The files on disk in a worktree: staged, unstaged and untracked
    WorkingTree,
}

/// One side of a comparison, as the user picked it
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Revision {
    pub kind: RevisionKind,
    /// For `Commit`: the ref or SHA as typed or picked
    pub ref_name: Option<String>,
    /// For `Index` and `WorkingTree`: the worktree; None means the
    /// repository's own worktree (the one the comparison was opened in)
    pub worktree: Option<PathBuf>,
}

impl Revision {
    pub fn commit(ref_name: impl Into<String>) -> Self {
        Self {
            kind: RevisionKind::Commit,
            ref_name: Some(ref_name.into()),
            worktree: None,
        }
    }

    pub fn index(worktree: Option<PathBuf>) -> Self {
        Self {
            kind: RevisionKind::Index,
            ref_name: None,
            worktree,
        }
    }

    pub fn working_tree(worktree: Option<PathBuf>) -> Self {
        Self {
            kind: RevisionKind::WorkingTree,
            ref_name: None,
            worktree,
        }
    }

    pub fn uncommitted(&self) -> bool {
        self.kind != RevisionKind::Commit
    }
}

/// Where an uncommitted change is: staged in the index, or only in the
/// working tree (untracked files too)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum UncommittedLayer {
    Staged,
    Unstaged,
}

impl UncommittedLayer {
    pub const ALL: [Self; 2] = [Self::Staged, Self::Unstaged];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Unstaged => "unstaged",
        }
    }
}

/// How base and target relate
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ComparisonRelation {
    /// Base and target are the same commit
    Same,
    /// Base is an ancestor of target (the usual case)
    Ancestor,
    /// Different branches; changes are shown from their merge base
    Diverged,
    /// Target is an ancestor of base
    Reversed,
    /// No common history
    Unrelated,
}

impl ComparisonRelation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Same => "same",
            Self::Ancestor => "ancestor",
            Self::Diverged => "diverged",
            Self::Reversed => "reversed",
            Self::Unrelated => "unrelated",
        }
    }
}

impl std::fmt::Display for ComparisonRelation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A resolved base/target pair
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    pub base: Revision,
    pub target: Revision,
    /// Full SHA of the base commit; for an INDEX base, the worktree's HEAD
    pub base_sha: String,
    /// Full SHA of the target commit; for an uncommitted target, the
    /// worktree's HEAD
    pub target_sha: String,
    pub merge_base: Option<String>,
    pub relation: ComparisonRelation,
    /// Where the diff and attribution start: `base_sha`, or `merge_base`
    /// when the branches diverged
    pub from_sha: String,
    /// Directory git runs in for the uncommitted side (index / working
    /// tree); the repository's own worktree otherwise
    pub worktree_path: PathBuf,
}

impl Comparison {
    pub fn uncommitted(&self) -> bool {
        self.target.uncommitted()
    }
}

/// 1-based, inclusive
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LineRange {
    pub start: usize,
    pub end: usize,
}

impl LineRange {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Old,
    New,
}

/// A line as the commit that changed it has it
///
/// `New` in the commit's version, `Old` in its first parent's.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LineOrigin {
    pub path: String,
    pub side: Side,
    pub line: usize,
}

impl LineOrigin {
    pub fn new(path: impl Into<String>, side: Side, line: usize) -> Self {
        Self {
            path: path.into(),
            side,
            line,
        }
    }
}

/// The lines of one change block that one commit is credited with
///
/// A block changed by several commits yields one group per commit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChangeGroup {
    /// The commit in the range that made the change; None when git cannot
    /// tell or it is uncommitted
    pub sha: Option<String>,
    /// Index into the file diff's hunks
    pub hunk_index: usize,
    /// Lines this group added, numbered in the target version
    pub added: Vec<LineRange>,
    /// Lines this group removed, numbered in the base version
    pub deleted: Vec<LineRange>,
    /// Where the lines were in the commit's own diff (against its first
    /// parent), or, when uncommitted, in its layer's diff
    pub origins: Vec<LineOrigin>,
    /// Set for a change not committed yet
    pub uncommitted: Option<UncommittedLayer>,
}

/// The diffs of each uncommitted layer that a file's groups point into
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LayerDiffsUsed {
    pub staged: Vec<GitFileDiff>,
    pub unstaged: Vec<GitFileDiff>,
}

impl LayerDiffsUsed {
    pub fn get(&self, layer: UncommittedLayer) -> &[GitFileDiff] {
        match layer {
            UncommittedLayer::Staged => &self.staged,
            UncommittedLayer::Unstaged => &self.unstaged,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAttribution {
    pub change: GitFileChange,
    /// None when git shows no textual difference (e.g. a mode-only change)
    pub diff: Option<GitFileDiff>,
    /// In diff order
    pub groups: Vec<ChangeGroup>,
    /// Commits in the range that touched the file, newest first; the only
    /// attribution binary files get
    pub touched_by: Vec<String>,
    /// For uncommitted targets: the file's diffs in each layer that its
    /// uncommitted groups' origins point into
    pub layers: Option<LayerDiffsUsed>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExplanationSource {
    Commit,
    Note,
    Inferred,
}

impl ExplanationSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Note => "note",
            Self::Inferred => "inferred",
        }
    }
}

impl std::fmt::Display for ExplanationSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitExplanation {
    pub what: String,
    pub why: Option<String>,
    /// Why this change is the right one: the evidence for it and the
    /// alternatives it rules out; from the same source as `why`, None when
    /// not given (older notes, plain messages)
    pub justification: Option<String>,
    pub source: ExplanationSource,
    /// Model that inferred it, for `Inferred`
    pub model: Option<String>,
}

/// A hunk of a commit's own diff (against its first parent)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CommitHunk {
    /// Path in the commit; for a deleted file, the path it had
    pub path: String,
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    /// Path in the first parent, when the commit renamed the file
    pub original_path: Option<String>,
}

/// What one hunk changed and why; always inferred by a model
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeExplanation {
    pub hunk: CommitHunk,
    pub what: String,
    pub why: String,
    /// Why this change is the right one; None in notes written before
    /// justifications were asked for
    pub justification: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitAgent {
    /// Always `claude-code`
    pub kind: String,
    /// As written in the trailer, e.g. `Claude Opus 5.5`
    pub display_name: String,
    /// Model ID derived from the trailer, e.g. `claude-opus-5-5`
    pub model: Option<String>,
}
