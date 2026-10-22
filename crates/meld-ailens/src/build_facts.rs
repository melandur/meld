//! What a build file states as fact (port of `build_facts.py`)
//!
//! Everything here is read from git the same way every time, so the same
//! comparison always yields the same facts. See `docs/build-format.md` in
//! AI Lens for how [`crate::build_file`] renders them. The facts of an open
//! comparison are gathered by [`crate::session::CompareSession::gather_build_facts`].

use crate::models::{ComparisonRelation, ExplanationSource, GitFileStatus, LineRange};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildRevision {
    pub sha: String,
    /// `git rev-parse --short`; longer than 7 when 7 would be ambiguous
    pub short_sha: String,
    /// How the user picked it (branch, tag, `HEAD`); None when by SHA
    pub ref_name: Option<String>,
}

/// Who wrote a commit
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuildAgent {
    ClaudeCode,
    Human,
}

impl BuildAgent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Human => "human",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildCommit {
    pub sha: String,
    pub short_sha: String,
    /// `git patch-id --stable`; None for merges
    pub patch_id: Option<String>,
    pub author: String,
    pub agent: BuildAgent,
    /// From the Co-Authored-By trailer, when it names one
    pub agent_model: Option<String>,
    /// e.g. `Claude Opus 5.5`
    pub agent_name: Option<String>,
    pub what: String,
    pub why: Option<String>,
    pub why_source: ExplanationSource,
    /// Model that inferred the Why, for `Inferred`
    pub inferred_by: Option<String>,
    /// Model that explained the commit's changes one by one
    pub changes_inferred_by: Option<String>,
}

/// What one change does and why; always inferred
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildChange {
    pub what: String,
    pub why: String,
}

/// The lines of a file one commit is credited with
///
/// One entry per explained change of the commit, or one for all its lines
/// when its changes weren't explained one by one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BuildFileEntry {
    /// None when git can't tell which commit made the change
    pub sha: Option<String>,
    /// Numbered in the target version
    pub added: Vec<LineRange>,
    /// Numbered in the base version
    pub deleted: Vec<LineRange>,
    /// What the change does and why, inferred; None when not explained one
    /// by one
    pub changes: Option<Vec<BuildChange>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildFile {
    pub path: String,
    pub original_path: Option<String>,
    pub status: GitFileStatus,
    /// None for binary files
    pub additions: Option<usize>,
    pub deletions: Option<usize>,
    pub binary: bool,
    /// Matched by `.ailensignore` or the generated-file rules: listed by
    /// path only
    pub ignored: bool,
    /// In the order each commit first appears in the file's diff
    pub entries: Vec<BuildFileEntry>,
    /// Commits that touched the file, newest first; what binary files get
    /// instead of entries
    pub touched_by: Vec<String>,
}

impl BuildFile {
    pub fn new(
        path: impl Into<String>,
        status: GitFileStatus,
        additions: Option<usize>,
        deletions: Option<usize>,
    ) -> Self {
        Self {
            path: path.into(),
            original_path: None,
            status,
            additions,
            deletions,
            binary: false,
            ignored: false,
            entries: Vec::new(),
            touched_by: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BuildStats {
    pub files: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub commits: usize,
}

/// Everything a build file states as fact
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildFacts {
    pub base: BuildRevision,
    pub target: BuildRevision,
    pub merge_base: Option<String>,
    pub relation: ComparisonRelation,
    pub stats: BuildStats,
    /// Oldest first
    pub commits: Vec<BuildCommit>,
    pub files: Vec<BuildFile>,
}

/// Sorted, with overlapping and adjacent ranges joined
pub fn merge_ranges(ranges: &[LineRange]) -> Vec<LineRange> {
    let mut sorted = ranges.to_vec();
    sorted.sort_by_key(|r| r.start);
    let mut merged: Vec<LineRange> = Vec::new();
    for range in sorted {
        match merged.last_mut() {
            Some(last) if range.start <= last.end + 1 => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}
