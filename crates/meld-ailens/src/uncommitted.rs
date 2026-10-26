//! Explanations of uncommitted hunks, kept in memory (port of
//! `uncommitted.py`)
//!
//! There is no commit to save them on as a note, so they are keyed by the
//! hunk's layer, file and lines (not its position): an explanation lasts
//! until its hunk changes.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use crate::models::{ChangeExplanation, CommitHunk, GitDiffHunk, GitFileDiff, UncommittedLayer};

pub const MAX_ENTRIES: usize = 2000;

/// What an uncommitted hunk does, why, why it is the right change, and the
/// model that inferred it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkExplanation {
    pub what: String,
    pub why: String,
    pub justification: Option<String>,
    pub model: String,
}

/// A hunk's explanation, placed where the hunk is now
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedExplanation {
    pub hunk: CommitHunk,
    pub what: String,
    pub why: String,
    pub justification: Option<String>,
    pub model: String,
}

impl PlacedExplanation {
    pub fn as_change(&self) -> ChangeExplanation {
        ChangeExplanation {
            hunk: self.hunk.clone(),
            what: self.what.clone(),
            why: self.why.clone(),
            justification: self.justification.clone(),
        }
    }
}

/// Anything that explains a hunk, for
/// [`crate::explanation::find_change_explanations`]
pub trait HunkChange {
    fn hunk(&self) -> &CommitHunk;
}

impl HunkChange for ChangeExplanation {
    fn hunk(&self) -> &CommitHunk {
        &self.hunk
    }
}

impl HunkChange for PlacedExplanation {
    fn hunk(&self) -> &CommitHunk {
        &self.hunk
    }
}

fn hunk_key(layer: UncommittedLayer, diff: &GitFileDiff, hunk: &GitDiffHunk) -> String {
    let mut digest = sha1_smol::Sha1::new();
    digest.update(
        format!(
            "{}\0{}\0{}\0",
            layer.as_str(),
            diff.old_path.as_deref().unwrap_or(""),
            diff.new_path.as_deref().unwrap_or("")
        )
        .as_bytes(),
    );
    for line in &hunk.lines {
        digest.update(format!("{}{}\n", line.kind.prefix(), line.content).as_bytes());
    }
    digest.digest().to_string()
}

#[derive(Default)]
struct Entries {
    by_hunk: HashMap<String, (u64, HunkExplanation)>,
    order: BTreeMap<u64, String>,
    serial: u64,
}

/// Explanations of uncommitted hunks; safe to share between threads
pub struct UncommittedExplanations {
    entries: Mutex<Entries>,
    max_entries: usize,
}

impl Default for UncommittedExplanations {
    fn default() -> Self {
        Self::new(MAX_ENTRIES)
    }
}

impl UncommittedExplanations {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Mutex::new(Entries::default()),
            max_entries,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Entries> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn len(&self) -> usize {
        self.lock().by_hunk.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(
        &self,
        layer: UncommittedLayer,
        diff: &GitFileDiff,
        hunk: &GitDiffHunk,
    ) -> Option<HunkExplanation> {
        let key = hunk_key(layer, diff, hunk);
        self.lock()
            .by_hunk
            .get(&key)
            .map(|(_, found)| found.clone())
    }

    pub fn set(
        &self,
        layer: UncommittedLayer,
        diff: &GitFileDiff,
        hunk: &GitDiffHunk,
        explanation: HunkExplanation,
    ) {
        let key = hunk_key(layer, diff, hunk);
        let mut entries = self.lock();
        if let Some((serial, _)) = entries.by_hunk.remove(&key) {
            entries.order.remove(&serial);
        }
        entries.serial += 1;
        let serial = entries.serial;
        entries.order.insert(serial, key.clone());
        entries.by_hunk.insert(key, (serial, explanation));
        while entries.by_hunk.len() > self.max_entries {
            let Some((_, oldest)) = entries.order.pop_first() else {
                break;
            };
            entries.by_hunk.remove(&oldest);
        }
    }

    /// Keep the explanations a model gave of `diff`'s hunks
    ///
    /// Each change is matched to the change block (see
    /// [`GitDiffHunk::blocks`]) at its position in `diff`.
    /// Returns how many were kept.
    pub fn remember(
        &self,
        layer: UncommittedLayer,
        diff: &GitFileDiff,
        changes: &[ChangeExplanation],
        model: &str,
    ) -> usize {
        let mut kept = 0;
        for change in changes {
            let hunk = diff
                .hunks
                .iter()
                .flat_map(GitDiffHunk::blocks)
                .find(|candidate| {
                    candidate.old_start == change.hunk.old_start
                        && candidate.new_start == change.hunk.new_start
                });
            if let Some(hunk) = hunk {
                self.set(
                    layer,
                    diff,
                    &hunk,
                    HunkExplanation {
                        what: change.what.clone(),
                        why: change.why.clone(),
                        justification: change.justification.clone(),
                        model: model.to_string(),
                    },
                );
                kept += 1;
            }
        }
        kept
    }

    /// Whether every change block of `diff` has an explanation
    pub fn is_explained(&self, layer: UncommittedLayer, diff: &GitFileDiff) -> bool {
        diff.hunks
            .iter()
            .flat_map(GitDiffHunk::blocks)
            .all(|hunk| self.get(layer, diff, &hunk).is_some())
    }

    /// The explained hunks of a layer's diffs, where they are now
    pub fn place(&self, layer: UncommittedLayer, diffs: &[GitFileDiff]) -> Vec<PlacedExplanation> {
        let mut placed = Vec::new();
        for diff in diffs {
            let Some(path) = diff.new_path.as_ref().or(diff.old_path.as_ref()) else {
                continue;
            };
            let original_path = diff.old_path.clone().filter(|old| old != path);
            for hunk in diff.hunks.iter().flat_map(GitDiffHunk::blocks) {
                let Some(found) = self.get(layer, diff, &hunk) else {
                    continue;
                };
                placed.push(PlacedExplanation {
                    hunk: CommitHunk {
                        path: path.clone(),
                        old_start: hunk.old_start,
                        old_count: hunk.old_count,
                        new_start: hunk.new_start,
                        new_count: hunk.new_count,
                        original_path: original_path.clone(),
                    },
                    what: found.what,
                    why: found.why,
                    justification: found.justification,
                    model: found.model,
                });
            }
        }
        placed
    }

    pub fn clear(&self) {
        let mut entries = self.lock();
        entries.by_hunk.clear();
        entries.order.clear();
    }
}
