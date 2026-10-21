# Copyright (C) 2026 Meld contributors
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 2 of the License, or (at
# your option) any later version.
#
# This program is distributed in the hope that it will be useful, but
# WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
# General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program.  If not, see <http://www.gnu.org/licenses/>.

"""Resolving a base/target pair, and the files that differ between them

Supported pairs:

- commit -> commit
- commit -> INDEX: the commit against a worktree's staged changes
- commit -> WORKING_TREE: against the files on disk, untracked ones too
- INDEX -> WORKING_TREE, in one worktree: its unstaged changes

INDEX and WORKING_TREE may name any worktree of the repository.
"""

import os

from meld.ailens.git import GitRepository, path_sort_key
from meld.ailens.models import (
    AiLensError,
    Comparison,
    ComparisonRelation,
    GitFileChange,
    GitFileDiff,
    Revision,
    RevisionKind,
)
from meld.ailens.parsers import GitFileStats
from meld.ailens.process import CancelToken


def resolve_comparison(
    repo: GitRepository,
    base: Revision,
    target: Revision,
    cancel: CancelToken | None = None,
) -> Comparison:
    """Resolve both sides to SHAs and work out how they relate

    An uncommitted target's commits end at its worktree's HEAD, so the
    relation is the base's with that HEAD. Raises :class:`AiLensError`
    with a message for the user for pairs that can't be compared.

    On an unborn branch only INDEX -> WORKING_TREE works; its SHAs are
    then the empty tree's ID and :attr:`Comparison.merge_base` is None.
    """
    unstaged_only = base.kind is RevisionKind.INDEX and (
        target.kind is RevisionKind.WORKING_TREE
    )
    if base.uncommitted and not unstaged_only:
        if base.kind is RevisionKind.WORKING_TREE:
            raise AiLensError("The working tree can only be compared as the target.")
        raise AiLensError(
            "Staged changes can only be the base when the target is the working tree."
        )

    worktree = repo.root
    if target.uncommitted:
        worktree = _worktree_path(repo, target.worktree, cancel)
    if unstaged_only:
        base_worktree = _worktree_path(repo, base.worktree, cancel)
        if os.path.realpath(base_worktree) != os.path.realpath(worktree):
            raise AiLensError(
                "Staged changes and the working tree must be in the same worktree."
            )
        head = repo.head(worktree, cancel=cancel)
        sha = head if head is not None else repo.empty_tree(cancel=cancel)
        return Comparison(
            base=base,
            target=target,
            base_sha=sha,
            target_sha=sha,
            merge_base=head,
            relation="same",
            from_sha=sha,
            worktree_path=worktree,
        )

    base_sha = _resolve(repo, base, cancel)
    if target.uncommitted:
        target_sha = repo.head(worktree, cancel=cancel)
        if target_sha is None:
            raise AiLensError(
                f"The branch checked out in “{worktree}” has no commits yet, "
                "so its changes can only be compared with its staged changes."
            )
    else:
        target_sha = _resolve(repo, target, cancel)

    if base_sha == target_sha:
        merge_base: str | None = base_sha
    else:
        merge_base = repo.merge_base(base_sha, target_sha, cancel=cancel)

    relation: ComparisonRelation
    if base_sha == target_sha:
        relation = "same"
    elif merge_base is None:
        relation = "unrelated"
    elif merge_base == base_sha:
        relation = "ancestor"
    elif merge_base == target_sha:
        relation = "reversed"
    else:
        relation = "diverged"

    return Comparison(
        base=base,
        target=target,
        base_sha=base_sha,
        target_sha=target_sha,
        merge_base=merge_base,
        relation=relation,
        from_sha=merge_base if relation == "diverged" and merge_base else base_sha,
        worktree_path=worktree,
    )


def _resolve(
    repo: GitRepository, revision: Revision, cancel: CancelToken | None
) -> str:
    if not revision.ref:
        raise AiLensError("Pick a branch, tag or commit to compare.")
    sha = repo.resolve_commit(revision.ref, cancel=cancel)
    if sha is None:
        raise AiLensError(f"“{revision.ref}” isn't a commit in this repository.")
    return sha


def _worktree_path(
    repo: GitRepository, path: str | None, cancel: CancelToken | None
) -> str:
    """The root of worktree ``path`` (None: the repository's own)"""
    if path is None:
        return repo.root
    wanted = os.path.realpath(path)
    if wanted == os.path.realpath(repo.root):
        return repo.root
    for worktree in repo.worktrees(cancel=cancel):
        if worktree.bare or os.path.realpath(worktree.path) != wanted:
            continue
        if worktree.prunable or not os.path.isdir(worktree.path):
            raise AiLensError(f"The worktree “{path}” is missing.")
        return worktree.path
    raise AiLensError(f"“{path}” isn't a worktree of this repository.")


def _diff_args(comparison: Comparison) -> tuple[list[str], bool]:
    """Revisions and ``--cached`` for the comparison's ``git diff``"""
    target = comparison.target.kind
    if target is RevisionKind.COMMIT:
        return [comparison.from_sha, comparison.target_sha], False
    if comparison.base.kind is RevisionKind.INDEX:
        return [], False
    return [comparison.from_sha], target is RevisionKind.INDEX


def changed_files(
    repo: GitRepository,
    comparison: Comparison,
    cancel: CancelToken | None = None,
) -> list[GitFileChange]:
    """Files that differ, with renames, in git's path order

    For a WORKING_TREE target, untracked files that aren't ignored are
    added files with :attr:`GitFileChange.untracked` set. Submodules are
    not recursed into: a changed submodule is one ``M`` (or ``T``) entry.
    """
    return _changed_files(
        repo, comparison, _untracked(repo, comparison, cancel), cancel
    )


def file_stats(
    repo: GitRepository,
    comparison: Comparison,
    cancel: CancelToken | None = None,
) -> list[GitFileStats]:
    """Lines added and deleted per changed file, untracked files too"""
    return _file_stats(repo, comparison, _untracked(repo, comparison, cancel), cancel)


def changed_files_with_stats(
    repo: GitRepository,
    comparison: Comparison,
    cancel: CancelToken | None = None,
) -> tuple[list[GitFileChange], list[GitFileStats]]:
    """:func:`changed_files` and :func:`file_stats`, listing untracked
    files once
    """
    untracked = _untracked(repo, comparison, cancel)
    return (
        _changed_files(repo, comparison, untracked, cancel),
        _file_stats(repo, comparison, untracked, cancel),
    )


def file_diff(
    repo: GitRepository,
    comparison: Comparison,
    change: GitFileChange,
    cancel: CancelToken | None = None,
    *,
    context: int = 3,
) -> GitFileDiff | None:
    """The diff of one changed file; None when git shows no difference

    An untracked file is diffed against nothing, so every line is added.
    """
    revs, cached = _diff_args(comparison)
    return repo.file_diff(
        revs,
        change,
        cached=cached,
        context=context,
        worktree=comparison.worktree_path,
        cancel=cancel,
    )


def _untracked(
    repo: GitRepository, comparison: Comparison, cancel: CancelToken | None
) -> list[str]:
    if comparison.target.kind is not RevisionKind.WORKING_TREE:
        return []
    return repo.untracked_files(comparison.worktree_path, cancel=cancel)


def _changed_files(
    repo: GitRepository,
    comparison: Comparison,
    untracked: list[str],
    cancel: CancelToken | None,
) -> list[GitFileChange]:
    revs, cached = _diff_args(comparison)
    changes = repo.name_status(
        revs, cached=cached, worktree=comparison.worktree_path, cancel=cancel
    )
    changes += [GitFileChange("A", path, untracked=True) for path in untracked]
    return sorted(changes, key=lambda change: path_sort_key(change.path))


def _file_stats(
    repo: GitRepository,
    comparison: Comparison,
    untracked: list[str],
    cancel: CancelToken | None,
) -> list[GitFileStats]:
    revs, cached = _diff_args(comparison)
    stats = repo.numstat(
        revs, cached=cached, worktree=comparison.worktree_path, cancel=cancel
    )
    for path in untracked:
        stats.append(
            repo.untracked_stats(path, worktree=comparison.worktree_path, cancel=cancel)
        )
    return stats
