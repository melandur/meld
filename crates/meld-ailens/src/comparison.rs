//! Resolving a base/target pair, and the files that differ between them
//! (port of `comparison.py`)
//!
//! Supported pairs:
//!
//! - commit -> commit
//! - commit -> INDEX: the commit against a worktree's staged changes
//! - commit -> WORKING_TREE: against the files on disk, untracked ones too
//! - INDEX -> WORKING_TREE, in one worktree: its unstaged changes
//!
//! INDEX and WORKING_TREE may name any worktree of the repository.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::git::{DiffOptions, GitRepository, realpath};
use crate::models::{
    Comparison, ComparisonRelation, GitFileChange, GitFileDiff, Revision, RevisionKind,
};
use crate::parsers::GitFileStats;
use crate::process::CancelToken;

/// Resolve both sides to SHAs and work out how they relate
///
/// An uncommitted target's commits end at its worktree's HEAD, so the
/// relation is the base's with that HEAD. Fails with a message for the
/// user for pairs that can't be compared.
///
/// On an unborn branch only INDEX -> WORKING_TREE works; its SHAs are then
/// the empty tree's ID and `merge_base` is None.
pub fn resolve_comparison(
    repo: &GitRepository,
    base: &Revision,
    target: &Revision,
    cancel: &CancelToken,
) -> Result<Comparison> {
    let unstaged_only =
        base.kind == RevisionKind::Index && target.kind == RevisionKind::WorkingTree;
    if base.uncommitted() && !unstaged_only {
        if base.kind == RevisionKind::WorkingTree {
            return Err(Error::msg(
                "The working tree can only be compared as the target.",
            ));
        }
        return Err(Error::msg(
            "Staged changes can only be the base when the target is the working tree.",
        ));
    }

    let mut worktree = repo.root.clone();
    if target.uncommitted() {
        worktree = worktree_path(repo, target.worktree.as_deref(), cancel)?;
    }
    if unstaged_only {
        let base_worktree = worktree_path(repo, base.worktree.as_deref(), cancel)?;
        if realpath(&base_worktree) != realpath(&worktree) {
            return Err(Error::msg(
                "Staged changes and the working tree must be in the same worktree.",
            ));
        }
        let head = repo.head(Some(&worktree), cancel)?;
        let sha = match &head {
            Some(head) => head.clone(),
            None => repo.empty_tree(cancel)?,
        };
        return Ok(Comparison {
            base: base.clone(),
            target: target.clone(),
            base_sha: sha.clone(),
            target_sha: sha.clone(),
            merge_base: head,
            relation: ComparisonRelation::Same,
            from_sha: sha,
            worktree_path: worktree,
        });
    }

    let base_sha = resolve(repo, base, cancel)?;
    let target_sha = if target.uncommitted() {
        match repo.head(Some(&worktree), cancel)? {
            Some(sha) => sha,
            None => {
                return Err(Error::msg(format!(
                    "The branch checked out in “{}” has no commits yet, so its changes can \
                     only be compared with its staged changes.",
                    worktree.display()
                )));
            }
        }
    } else {
        resolve(repo, target, cancel)?
    };

    let merge_base = if base_sha == target_sha {
        Some(base_sha.clone())
    } else {
        repo.merge_base(&base_sha, &target_sha, cancel)?
    };

    let relation = if base_sha == target_sha {
        ComparisonRelation::Same
    } else {
        match &merge_base {
            None => ComparisonRelation::Unrelated,
            Some(merge_base) if *merge_base == base_sha => ComparisonRelation::Ancestor,
            Some(merge_base) if *merge_base == target_sha => ComparisonRelation::Reversed,
            Some(_) => ComparisonRelation::Diverged,
        }
    };

    let from_sha = match (&merge_base, relation) {
        (Some(merge_base), ComparisonRelation::Diverged) => merge_base.clone(),
        _ => base_sha.clone(),
    };
    Ok(Comparison {
        base: base.clone(),
        target: target.clone(),
        base_sha,
        target_sha,
        merge_base,
        relation,
        from_sha,
        worktree_path: worktree,
    })
}

fn resolve(repo: &GitRepository, revision: &Revision, cancel: &CancelToken) -> Result<String> {
    let ref_name = revision.ref_name.as_deref().unwrap_or("");
    if ref_name.is_empty() {
        return Err(Error::msg("Pick a branch, tag or commit to compare."));
    }
    repo.resolve_commit(ref_name, cancel)?
        .ok_or_else(|| Error::msg(format!("“{ref_name}” isn't a commit in this repository.")))
}

/// The root of worktree `path` (None: the repository's own)
fn worktree_path(
    repo: &GitRepository,
    path: Option<&Path>,
    cancel: &CancelToken,
) -> Result<PathBuf> {
    let Some(path) = path else {
        return Ok(repo.root.clone());
    };
    let wanted = realpath(path);
    if wanted == realpath(&repo.root) {
        return Ok(repo.root.clone());
    }
    for worktree in repo.worktrees(cancel)? {
        if worktree.bare || realpath(&worktree.path) != wanted {
            continue;
        }
        if worktree.prunable || !worktree.path.is_dir() {
            return Err(Error::msg(format!(
                "The worktree “{}” is missing.",
                path.display()
            )));
        }
        return Ok(worktree.path);
    }
    Err(Error::msg(format!(
        "“{}” isn't a worktree of this repository.",
        path.display()
    )))
}

/// Revisions and `--cached` for the comparison's `git diff`
pub(crate) fn diff_args(comparison: &Comparison) -> (Vec<String>, bool) {
    match comparison.target.kind {
        RevisionKind::Commit => (
            vec![comparison.from_sha.clone(), comparison.target_sha.clone()],
            false,
        ),
        _ if comparison.base.kind == RevisionKind::Index => (Vec::new(), false),
        kind => (
            vec![comparison.from_sha.clone()],
            kind == RevisionKind::Index,
        ),
    }
}

/// Files that differ, with renames, in git's path order
///
/// For a WORKING_TREE target, untracked files that aren't ignored are added
/// files with `untracked` set. Submodules are not recursed into: a changed
/// submodule is one `M` (or `T`) entry.
pub fn changed_files(
    repo: &GitRepository,
    comparison: &Comparison,
    cancel: &CancelToken,
) -> Result<Vec<GitFileChange>> {
    let untracked = untracked(repo, comparison, cancel)?;
    list_changed_files(repo, comparison, &untracked, cancel)
}

/// [`changed_files`] and the lines added and deleted per changed file
/// (untracked files too), listing untracked files once
pub fn changed_files_with_stats(
    repo: &GitRepository,
    comparison: &Comparison,
    cancel: &CancelToken,
) -> Result<(Vec<GitFileChange>, Vec<GitFileStats>)> {
    let untracked = untracked(repo, comparison, cancel)?;
    Ok((
        list_changed_files(repo, comparison, &untracked, cancel)?,
        list_file_stats(repo, comparison, &untracked, cancel)?,
    ))
}

/// The diff of one changed file; None when git shows no difference
///
/// An untracked file is diffed against nothing, so every line is added.
pub fn file_diff(
    repo: &GitRepository,
    comparison: &Comparison,
    change: &GitFileChange,
    context: usize,
    cancel: &CancelToken,
) -> Result<Option<GitFileDiff>> {
    let (revs, cached) = diff_args(comparison);
    repo.file_diff(
        &revs,
        change,
        DiffOptions {
            cached,
            context,
            worktree: Some(&comparison.worktree_path),
        },
        cancel,
    )
}

fn untracked(
    repo: &GitRepository,
    comparison: &Comparison,
    cancel: &CancelToken,
) -> Result<Vec<String>> {
    if comparison.target.kind != RevisionKind::WorkingTree {
        return Ok(Vec::new());
    }
    repo.untracked_files(Some(&comparison.worktree_path), cancel)
}

fn list_changed_files(
    repo: &GitRepository,
    comparison: &Comparison,
    untracked: &[String],
    cancel: &CancelToken,
) -> Result<Vec<GitFileChange>> {
    let (revs, cached) = diff_args(comparison);
    let mut changes = repo.name_status(&revs, cached, Some(&comparison.worktree_path), cancel)?;
    changes.extend(
        untracked
            .iter()
            .map(|path| GitFileChange::untracked(path.as_str())),
    );
    changes.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    Ok(changes)
}

fn list_file_stats(
    repo: &GitRepository,
    comparison: &Comparison,
    untracked: &[String],
    cancel: &CancelToken,
) -> Result<Vec<GitFileStats>> {
    let (revs, cached) = diff_args(comparison);
    let mut stats = repo.numstat(&revs, cached, Some(&comparison.worktree_path), cancel)?;
    for path in untracked {
        stats.push(repo.untracked_stats(path, Some(&comparison.worktree_path), cancel)?);
    }
    Ok(stats)
}
