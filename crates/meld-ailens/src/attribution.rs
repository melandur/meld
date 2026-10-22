//! Crediting each changed line of a comparison to the commit that made it
//! (port of `attribution.py`)
//!
//! - Added lines: `git blame from..target` at the target.
//! - Deleted lines: `git blame --reverse from..target` gives the last
//!   commit the line existed in; the commit that removed it is that
//!   commit's child. When the child is a merge, the line went away because
//!   the merge took another parent's side, so the commit on that side that
//!   deleted the same content is credited, or else the merge itself (e.g. a
//!   line dropped while resolving a conflict).
//! - Uncommitted targets are attributed in layers: a changed line is traced
//!   back through the unstaged and staged diffs first, and only a line that
//!   reaches HEAD that way is credited to a commit.
//!
//! Where git's blame and the diff disagree, a line is left uncredited
//! rather than guessed.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::comparison::{changed_files, file_diff};
use crate::error::Result;
use crate::git::{DiffOptions, GitRepository};
use crate::linemap::{new_to_old, old_to_new};
use crate::models::{
    ChangeGroup, Comparison, ComparisonRelation, DiffLineKind, FileAttribution, GitBlameLine,
    GitFileChange, GitFileDiff, LayerDiffsUsed, LineOrigin, LineRange, RevisionKind, Side,
    UncommittedLayer,
};
use crate::process::CancelToken;

/// Every file's staged and unstaged diffs
///
/// No unstaged ones when the comparison ends at the index.
#[derive(Clone)]
struct LayerDiffs {
    staged: Arc<Vec<GitFileDiff>>,
    unstaged: Option<Arc<Vec<GitFileDiff>>>,
}

impl LayerDiffs {
    fn get(&self, layer: UncommittedLayer) -> &[GitFileDiff] {
        match layer {
            UncommittedLayer::Staged => &self.staged,
            UncommittedLayer::Unstaged => self.unstaged.as_deref().map_or(&[], |v| v),
        }
    }
}

/// A changed line that an uncommitted layer made
struct LayerTrace {
    layer: UncommittedLayer,
    origin: LineOrigin,
}

/// A changed line traced back to where it is at HEAD, for blame
struct HeadTrace {
    path: String,
    line: usize,
}

enum Trace {
    Layer(LayerTrace),
    Head(HeadTrace),
}

/// Which layer diffs a file's groups used, in the order first used
#[derive(Default)]
struct Used {
    staged: Vec<usize>,
    unstaged: Vec<usize>,
}

impl Used {
    fn add(&mut self, layer: UncommittedLayer, index: usize) {
        let list = match layer {
            UncommittedLayer::Staged => &mut self.staged,
            UncommittedLayer::Unstaged => &mut self.unstaged,
        };
        if !list.contains(&index) {
            list.push(index);
        }
    }
}

/// Parents and children of the commits from `from` to the target
type Graph = (HashMap<String, Vec<String>>, HashMap<String, Vec<String>>);

#[derive(Default)]
struct Cache {
    /// Parents and children of the commits from `from` to the target
    graph: Option<Arc<Graph>>,
    deleted_content: HashMap<(String, Vec<String>), Arc<HashSet<String>>>,
    layer_diffs: Option<LayerDiffs>,
}

/// Attributes the changed files of one comparison
///
/// Caches what all files of the comparison share (the commit graph,
/// commits' deletions, the uncommitted diffs), so use one instance for all
/// of them. Calls are serialised by a lock: an attributor can be shared
/// between threads, but only one call runs at a time.
pub struct ComparisonAttributor {
    pub repo: GitRepository,
    pub comparison: Comparison,
    cache: Mutex<Cache>,
}

#[derive(PartialEq, Eq, Hash, Clone)]
enum GroupKey {
    Uncommitted(UncommittedLayer),
    Commit(Option<String>),
}

impl ComparisonAttributor {
    pub fn new(repo: GitRepository, comparison: Comparison) -> Self {
        Self {
            repo,
            comparison,
            cache: Mutex::new(Cache::default()),
        }
    }

    /// False when there are no commits from `from` to the target
    fn attributable(&self) -> bool {
        !matches!(
            self.comparison.relation,
            ComparisonRelation::Same | ComparisonRelation::Reversed
        )
    }

    pub fn changed_files(&self, cancel: &CancelToken) -> Result<Vec<GitFileChange>> {
        changed_files(&self.repo, &self.comparison, cancel)
    }

    /// Group the changed lines of `change` by the commit (or uncommitted
    /// layer) that made them
    pub fn attribute(
        &self,
        change: &GitFileChange,
        cancel: &CancelToken,
    ) -> Result<FileAttribution> {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.attribute_locked(&mut cache, change, cancel)
    }

    fn attribute_locked(
        &self,
        cache: &mut Cache,
        change: &GitFileChange,
        cancel: &CancelToken,
    ) -> Result<FileAttribution> {
        let comparison = &self.comparison;
        let (from_sha, target) = (comparison.from_sha.as_str(), comparison.target_sha.as_str());
        let paths = change.paths();

        let diff = file_diff(&self.repo, comparison, change, 3, cancel)?;
        let touched_by = if self.attributable() {
            self.repo
                .commits_touching(&[format!("{from_sha}..{target}")], &paths, false, cancel)?
        } else {
            Vec::new()
        };

        let layers: Option<LayerDiffs> = if comparison.base.kind == RevisionKind::Index {
            // Staged changes are the base, so every change is unstaged
            Some(LayerDiffs {
                staged: Arc::new(Vec::new()),
                unstaged: Some(Arc::new(diff.iter().cloned().collect())),
            })
        } else if comparison.uncommitted() {
            let mut layers = self.layer_diffs(cache, cancel)?;
            if let (true, Some(diff)) = (change.untracked, &diff) {
                // An untracked file is its own unstaged diff
                layers.unstaged = Some(Arc::new(vec![diff.clone()]));
            }
            Some(layers)
        } else {
            None
        };

        let mut used = Used::default();
        let result =
            |groups: Vec<ChangeGroup>, used: &Used, diff: Option<GitFileDiff>| FileAttribution {
                change: change.clone(),
                diff,
                groups,
                touched_by: touched_by.clone(),
                layers: layers.as_ref().map(|layers| LayerDiffsUsed {
                    staged: used
                        .staged
                        .iter()
                        .map(|&i| layers.get(UncommittedLayer::Staged)[i].clone())
                        .collect(),
                    unstaged: used
                        .unstaged
                        .iter()
                        .map(|&i| layers.get(UncommittedLayer::Unstaged)[i].clone())
                        .collect(),
                }),
            };

        let Some(file_diff) = diff.as_ref().filter(|diff| !diff.binary) else {
            return Ok(result(Vec::new(), &used, diff));
        };

        let has_deletions = file_diff
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .any(|line| line.kind == DiffLineKind::Delete);
        let base_path = change
            .original_path
            .clone()
            .unwrap_or_else(|| change.path.clone());

        // Blame at the target, by path: with uncommitted changes, lines may
        // reach HEAD under another path
        let mut blames: HashMap<String, HashMap<usize, GitBlameLine>> = HashMap::new();

        // --reverse needs `from` to be an ancestor of the target; a base at
        // the target (uncommitted changes on top of HEAD) has no history to
        // blame
        let reverse = if from_sha != target
            && matches!(
                comparison.relation,
                ComparisonRelation::Ancestor | ComparisonRelation::Diverged
            )
            && has_deletions
        {
            self.repo
                .blame(from_sha, target, &base_path, true, cancel)?
        } else {
            Vec::new()
        };
        let last_seen_in: HashMap<usize, GitBlameLine> = reverse
            .into_iter()
            .map(|line| (line.final_line, line))
            .collect();

        let mut groups: Vec<ChangeGroup> = Vec::new();
        for (hunk_index, hunk) in file_diff.hunks.iter().enumerate() {
            let mut block: Option<HashMap<GroupKey, usize>> = None;
            for line in &hunk.lines {
                if line.kind == DiffLineKind::Context {
                    block = None;
                    continue;
                }

                let mut sha: Option<String> = None;
                let mut layer: Option<UncommittedLayer> = None;
                let mut origin: Option<LineOrigin> = None;
                if line.kind == DiffLineKind::Add {
                    let new_line = line.new_line.unwrap_or_default();
                    let trace = match &layers {
                        Some(layers) => trace_added(layers, &change.path, new_line, &mut used),
                        None => Trace::Head(HeadTrace {
                            path: change.path.clone(),
                            line: new_line,
                        }),
                    };
                    match trace {
                        Trace::Layer(trace) => {
                            layer = Some(trace.layer);
                            origin = Some(trace.origin);
                        }
                        Trace::Head(trace) => {
                            if !blames.contains_key(&trace.path) {
                                let found = if self.attributable() {
                                    self.repo
                                        .blame(from_sha, target, &trace.path, false, cancel)?
                                } else {
                                    Vec::new()
                                };
                                blames.insert(
                                    trace.path.clone(),
                                    found
                                        .into_iter()
                                        .filter(|line| !line.boundary)
                                        .map(|line| (line.final_line, line))
                                        .collect(),
                                );
                            }
                            if let Some(blamed) =
                                blames.get(&trace.path).and_then(|b| b.get(&trace.line))
                            {
                                sha = Some(blamed.sha.clone());
                                origin = Some(LineOrigin::new(
                                    blamed.filename.clone(),
                                    Side::New,
                                    blamed.original_line,
                                ));
                            }
                        }
                    }
                } else {
                    let old_line = line.old_line.unwrap_or_default();
                    let seen = if from_sha == target {
                        Some(GitBlameLine {
                            sha: target.to_string(),
                            original_line: old_line,
                            final_line: old_line,
                            boundary: false,
                            filename: base_path.clone(),
                        })
                    } else {
                        last_seen_in.get(&old_line).cloned()
                    };
                    match (&layers, &seen) {
                        (Some(layers), Some(seen)) if seen.sha == target => {
                            // Still at HEAD, so an uncommitted change deleted it
                            if let Some(traced) =
                                trace_deleted(layers, &seen.filename, seen.original_line, &mut used)
                            {
                                layer = Some(traced.layer);
                                origin = Some(traced.origin);
                            }
                        }
                        _ => {
                            sha = self.find_deleting_commit(
                                cache,
                                seen.as_ref().map(|seen| seen.sha.as_str()),
                                &line.content,
                                &paths,
                                cancel,
                            )?;
                            // The line's number in the commit's own diff is
                            // known when it was deleted from its first parent
                            if let (Some(sha), Some(seen)) = (&sha, &seen) {
                                let graph = self.graph(cache, cancel)?;
                                let first_parent = graph.0.get(sha).and_then(|p| p.first());
                                if first_parent == Some(&seen.sha) {
                                    origin = Some(LineOrigin::new(
                                        seen.filename.clone(),
                                        Side::Old,
                                        seen.original_line,
                                    ));
                                }
                            }
                        }
                    }
                }

                let block = block.get_or_insert_with(HashMap::new);
                let key = match layer {
                    Some(layer) => GroupKey::Uncommitted(layer),
                    None => GroupKey::Commit(sha.clone()),
                };
                let index = *block.entry(key).or_insert_with(|| {
                    groups.push(ChangeGroup {
                        sha: sha.clone(),
                        hunk_index,
                        uncommitted: layer,
                        ..Default::default()
                    });
                    groups.len() - 1
                });
                let group = &mut groups[index];
                if line.kind == DiffLineKind::Add {
                    add_line(&mut group.added, line.new_line.unwrap_or_default());
                } else {
                    add_line(&mut group.deleted, line.old_line.unwrap_or_default());
                }
                if let Some(origin) = origin {
                    group.origins.push(origin);
                }
            }
        }

        Ok(result(groups, &used, diff.clone()))
    }

    fn layer_diffs(&self, cache: &mut Cache, cancel: &CancelToken) -> Result<LayerDiffs> {
        if let Some(layers) = &cache.layer_diffs {
            return Ok(layers.clone());
        }
        let comparison = &self.comparison;
        let staged = self.repo.uncommitted_diffs(
            UncommittedLayer::Staged,
            Some(&comparison.target_sha),
            Some(&comparison.worktree_path),
            cancel,
        )?;
        let unstaged = if comparison.target.kind == RevisionKind::WorkingTree {
            Some(Arc::new(self.repo.uncommitted_diffs(
                UncommittedLayer::Unstaged,
                None,
                Some(&comparison.worktree_path),
                cancel,
            )?))
        } else {
            None
        };
        let layers = LayerDiffs {
            staged: Arc::new(staged),
            unstaged,
        };
        cache.layer_diffs = Some(layers.clone());
        Ok(layers)
    }

    fn find_deleting_commit(
        &self,
        cache: &mut Cache,
        last_seen_in: Option<&str>,
        content: &str,
        paths: &[String],
        cancel: &CancelToken,
    ) -> Result<Option<String>> {
        // Still present at the target means git's blame and our diff
        // disagree about this line
        let Some(last_seen_in) = last_seen_in else {
            return Ok(None);
        };
        if last_seen_in == self.comparison.target_sha {
            return Ok(None);
        }

        let graph = self.graph(cache, cancel)?;
        let (parents, children) = (&graph.0, &graph.1);
        let empty = Vec::new();
        let kids = children.get(last_seen_in).unwrap_or(&empty);
        if kids.len() == 1 && parents.get(&kids[0]).map_or(0, Vec::len) == 1 {
            return Ok(Some(kids[0].clone()));
        }

        // Several children (the history forked after the line's last
        // commit): the one that deleted it
        for kid in kids {
            let kid_parents = parents.get(kid).unwrap_or(&empty);
            if kid_parents.len() == 1
                && self.deletes(cache, kid, &kid_parents[0], content, paths, cancel)?
            {
                return Ok(Some(kid.clone()));
            }
        }

        for merge in kids {
            let merge_parents = parents.get(merge).unwrap_or(&empty);
            if merge_parents.len() < 2 {
                continue;
            }
            let mut revs: Vec<String> = merge_parents
                .iter()
                .filter(|parent| *parent != last_seen_in)
                .cloned()
                .collect();
            revs.push(format!("^{last_seen_in}"));
            revs.push(format!("^{}", self.comparison.from_sha));
            let other_side = self.repo.commits_touching(&revs, paths, true, cancel)?;
            for sha in &other_side {
                let sha_parents = parents.get(sha).unwrap_or(&empty);
                if sha_parents.len() == 1
                    && self.deletes(cache, sha, &sha_parents[0], content, paths, cancel)?
                {
                    return Ok(Some(sha.clone()));
                }
            }
            // Nothing on the other side deleted it, so the merge itself did
            // (e.g. while resolving a conflict)
            return Ok(Some(merge.clone()));
        }

        Ok(kids.first().cloned())
    }

    /// Whether `sha` (against `parent`) deleted a line with exactly this
    /// content from the file
    fn deletes(
        &self,
        cache: &mut Cache,
        sha: &str,
        parent: &str,
        content: &str,
        paths: &[String],
        cancel: &CancelToken,
    ) -> Result<bool> {
        let key = (sha.to_string(), paths.to_vec());
        if let Some(deleted) = cache.deleted_content.get(&key) {
            return Ok(deleted.contains(content));
        }
        let diffs = self.repo.diff(
            &[parent.to_string(), sha.to_string()],
            paths,
            DiffOptions {
                context: 0,
                ..Default::default()
            },
            cancel,
        )?;
        let deleted: HashSet<String> = diffs
            .iter()
            .flat_map(|diff| &diff.hunks)
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Delete)
            .map(|line| line.content.clone())
            .collect();
        let found = deleted.contains(content);
        cache.deleted_content.insert(key, Arc::new(deleted));
        Ok(found)
    }

    fn graph(&self, cache: &mut Cache, cancel: &CancelToken) -> Result<Arc<Graph>> {
        if let Some(graph) = &cache.graph {
            return Ok(graph.clone());
        }
        let comparison = &self.comparison;
        let range = format!("{}..{}", comparison.from_sha, comparison.target_sha);
        let ordered = self.repo.parents_in_order(&[&range], cancel)?;
        // Children in rev-list's order, newest first
        let mut children: HashMap<String, Vec<String>> = HashMap::new();
        for (sha, sha_parents) in &ordered {
            for parent in sha_parents {
                children
                    .entry(parent.clone())
                    .or_default()
                    .push(sha.clone());
            }
        }
        let graph = Arc::new((ordered.into_iter().collect(), children));
        cache.graph = Some(graph.clone());
        Ok(graph)
    }
}

/// Trace a line added in the target back
///
/// Added by the unstaged diff (into the working tree), or else by the
/// staged diff (into the index), or else to where it is at HEAD.
fn trace_added(layers: &LayerDiffs, path: &str, line: usize, used: &mut Used) -> Trace {
    let (mut index_path, mut index_line) = (path.to_string(), line);
    let unstaged = layers.get(UncommittedLayer::Unstaged);
    if let Some(position) = unstaged
        .iter()
        .position(|diff| diff.new_path.as_deref() == Some(path))
    {
        let diff = &unstaged[position];
        used.add(UncommittedLayer::Unstaged, position);
        let old = new_to_old(diff, line);
        match (old, &diff.old_path) {
            (Some(old), Some(old_path)) => {
                index_path = old_path.clone();
                index_line = old;
            }
            _ => {
                return Trace::Layer(LayerTrace {
                    layer: UncommittedLayer::Unstaged,
                    origin: LineOrigin::new(path, Side::New, line),
                });
            }
        }
    }

    let staged = layers.get(UncommittedLayer::Staged);
    let Some(position) = staged
        .iter()
        .position(|diff| diff.new_path.as_deref() == Some(index_path.as_str()))
    else {
        return Trace::Head(HeadTrace {
            path: index_path,
            line: index_line,
        });
    };
    let diff = &staged[position];
    used.add(UncommittedLayer::Staged, position);
    match (new_to_old(diff, index_line), &diff.old_path) {
        (Some(old), Some(old_path)) => Trace::Head(HeadTrace {
            path: old_path.clone(),
            line: old,
        }),
        _ => Trace::Layer(LayerTrace {
            layer: UncommittedLayer::Staged,
            origin: LineOrigin::new(index_path, Side::New, index_line),
        }),
    }
}

/// Trace a line that is at HEAD but not in the target forward
///
/// Deleted by the staged diff, or else by the unstaged one. None when
/// neither deleted it, i.e. git's blame and the diff disagree.
fn trace_deleted(
    layers: &LayerDiffs,
    path: &str,
    line: usize,
    used: &mut Used,
) -> Option<LayerTrace> {
    let (mut index_path, mut index_line) = (path.to_string(), line);
    let staged = layers.get(UncommittedLayer::Staged);
    if let Some(position) = staged
        .iter()
        .position(|diff| diff.old_path.as_deref() == Some(path))
    {
        let diff = &staged[position];
        used.add(UncommittedLayer::Staged, position);
        match (old_to_new(diff, line), &diff.new_path) {
            (Some(moved), Some(new_path)) => {
                index_path = new_path.clone();
                index_line = moved;
            }
            _ => {
                return Some(LayerTrace {
                    layer: UncommittedLayer::Staged,
                    origin: LineOrigin::new(path, Side::Old, line),
                });
            }
        }
    }

    let unstaged = layers.get(UncommittedLayer::Unstaged);
    let position = unstaged
        .iter()
        .position(|diff| diff.old_path.as_deref() == Some(index_path.as_str()))?;
    used.add(UncommittedLayer::Unstaged, position);
    if old_to_new(&unstaged[position], index_line).is_none() {
        return Some(LayerTrace {
            layer: UncommittedLayer::Unstaged,
            origin: LineOrigin::new(index_path, Side::Old, index_line),
        });
    }
    None
}

fn add_line(ranges: &mut Vec<LineRange>, line: usize) {
    match ranges.last_mut() {
        Some(last) if last.end + 1 == line => last.end = line,
        _ => ranges.push(LineRange::new(line, line)),
    }
}
