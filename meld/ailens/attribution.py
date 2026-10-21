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

"""Crediting each changed line of a comparison to the commit that made it

- Added lines: ``git blame from..target`` at the target.
- Deleted lines: ``git blame --reverse from..target`` gives the last commit
  the line existed in; the commit that removed it is that commit's child.
  When the child is a merge, the line went away because the merge took
  another parent's side, so the commit on that side that deleted the same
  content is credited, or else the merge itself (e.g. a line dropped while
  resolving a conflict).
- Uncommitted targets are attributed in layers: a changed line is traced
  back through the unstaged and staged diffs first, and only a line that
  reaches HEAD that way is credited to a commit.

Where git's blame and the diff disagree, a line is left uncredited rather
than guessed.
"""

import threading
from collections.abc import Callable, Sequence
from dataclasses import dataclass

from meld.ailens.comparison import changed_files, file_diff
from meld.ailens.git import GitRepository
from meld.ailens.linemap import new_to_old, old_to_new
from meld.ailens.models import (
    ChangeGroup,
    Comparison,
    FileAttribution,
    GitBlameLine,
    GitFileChange,
    GitFileDiff,
    LineOrigin,
    LineRange,
    RevisionKind,
    UncommittedLayer,
)
from meld.ailens.process import CancelToken


@dataclass(frozen=True)
class _LayerDiffs:
    """Every file's staged and unstaged diffs

    No unstaged ones when the comparison ends at the index.
    """

    staged: tuple[GitFileDiff, ...]
    unstaged: tuple[GitFileDiff, ...] | None


@dataclass(frozen=True)
class _LayerTrace:
    """A changed line that an uncommitted layer made"""

    layer: UncommittedLayer
    origin: LineOrigin


@dataclass(frozen=True)
class _HeadTrace:
    """A changed line traced back to where it is at HEAD, for blame"""

    path: str
    line: int


_Use = Callable[[UncommittedLayer, GitFileDiff], None]


class ComparisonAttributor:
    """Attributes the changed files of one comparison

    Caches what all files of the comparison share (the commit graph,
    commits' deletions, the uncommitted diffs), so use one instance for
    all of them. Calls are serialised by a lock: an attributor can be
    handed between threads, but only one call runs at a time.
    """

    def __init__(self, repo: GitRepository, comparison: Comparison) -> None:
        self.repo = repo
        self.comparison = comparison
        self._lock = threading.Lock()
        self._graph: tuple[dict[str, list[str]], dict[str, list[str]]] | None = None
        self._deleted_content: dict[tuple[str, tuple[str, ...]], frozenset[str]] = {}
        self._layer_diffs: _LayerDiffs | None = None

    @property
    def _attributable(self) -> bool:
        """False when there are no commits from ``from`` to the target"""
        return self.comparison.relation not in ("same", "reversed")

    def changed_files(self, cancel: CancelToken | None = None) -> list[GitFileChange]:
        return changed_files(self.repo, self.comparison, cancel)

    def attribute(
        self, change: GitFileChange, cancel: CancelToken | None = None
    ) -> FileAttribution:
        """Group the changed lines of ``change`` by the commit (or
        uncommitted layer) that made them
        """
        with self._lock:
            return self._attribute(change, cancel)

    def _attribute(
        self, change: GitFileChange, cancel: CancelToken | None
    ) -> FileAttribution:
        comparison = self.comparison
        from_sha, target = comparison.from_sha, comparison.target_sha
        paths = list(change.paths)

        diff = file_diff(self.repo, comparison, change, cancel)
        touched_by = (
            self.repo.commits_touching([f"{from_sha}..{target}"], paths, cancel=cancel)
            if self._attributable
            else []
        )

        layers: _LayerDiffs | None
        if comparison.base.kind is RevisionKind.INDEX:
            # Staged changes are the base, so every change is unstaged
            layers = _LayerDiffs((), (diff,) if diff is not None else ())
        elif comparison.uncommitted:
            layers = self._get_layer_diffs(cancel)
            if change.untracked and diff is not None:
                # An untracked file is its own unstaged diff
                layers = _LayerDiffs(layers.staged, (diff,))
        else:
            layers = None

        used: dict[UncommittedLayer, list[GitFileDiff]] = {"staged": [], "unstaged": []}

        def use(layer: UncommittedLayer, layer_diff: GitFileDiff) -> None:
            if not any(known is layer_diff for known in used[layer]):
                used[layer].append(layer_diff)

        def result(groups: list[ChangeGroup]) -> FileAttribution:
            return FileAttribution(
                change=change,
                diff=diff,
                groups=groups,
                touched_by=touched_by,
                layers=used if layers is not None else None,
            )

        if diff is None or diff.binary:
            return result([])

        lines = [line for hunk in diff.hunks for line in hunk.lines]
        base_path = change.original_path or change.path

        # Blame at the target, by path: with uncommitted changes, lines may
        # reach HEAD under another path
        blames: dict[str, dict[int, GitBlameLine]] = {}

        def added_by(path: str) -> dict[int, GitBlameLine]:
            if path not in blames:
                found = (
                    self.repo.blame(from_sha, target, path, cancel=cancel)
                    if self._attributable
                    else []
                )
                blames[path] = {
                    line.final_line: line for line in found if not line.boundary
                }
            return blames[path]

        # --reverse needs ``from`` to be an ancestor of the target; a base
        # at the target (uncommitted changes on top of HEAD) has no history
        # to blame
        reverse: list[GitBlameLine] = []
        if (
            from_sha != target
            and comparison.relation in ("ancestor", "diverged")
            and any(line.type == "delete" for line in lines)
        ):
            reverse = self.repo.blame(
                from_sha, target, base_path, reverse=True, cancel=cancel
            )
        last_seen_in = {line.final_line: line for line in reverse}

        groups: list[ChangeGroup] = []
        for hunk_index, hunk in enumerate(diff.hunks):
            block: dict[object, ChangeGroup] | None = None
            for line in hunk.lines:
                if line.type == "context":
                    block = None
                    continue

                sha: str | None = None
                layer: UncommittedLayer | None = None
                origin: LineOrigin | None = None
                if line.type == "add":
                    assert line.new_line is not None
                    trace = (
                        _trace_added(layers, change.path, line.new_line, use)
                        if layers is not None
                        else _HeadTrace(change.path, line.new_line)
                    )
                    if isinstance(trace, _LayerTrace):
                        layer, origin = trace.layer, trace.origin
                    else:
                        blamed = added_by(trace.path).get(trace.line)
                        if blamed is not None:
                            sha = blamed.sha
                            origin = LineOrigin(
                                blamed.filename, "new", blamed.original_line
                            )
                else:
                    assert line.old_line is not None
                    seen = (
                        GitBlameLine(
                            target, line.old_line, line.old_line, False, base_path
                        )
                        if from_sha == target
                        else last_seen_in.get(line.old_line)
                    )
                    if layers is not None and seen is not None and seen.sha == target:
                        # Still at HEAD, so an uncommitted change deleted it
                        traced = _trace_deleted(
                            layers, seen.filename, seen.original_line, use
                        )
                        if traced is not None:
                            layer, origin = traced.layer, traced.origin
                    else:
                        sha = self._find_deleting_commit(
                            seen.sha if seen is not None else None,
                            line.content,
                            paths,
                            cancel,
                        )
                        # The line's number in the commit's own diff is
                        # known when it was deleted from its first parent
                        if sha is not None and seen is not None:
                            parents = self._get_graph(cancel)[0].get(sha) or []
                            if parents and parents[0] == seen.sha:
                                origin = LineOrigin(
                                    seen.filename, "old", seen.original_line
                                )

                if block is None:
                    block = {}
                key: object = ("uncommitted", layer) if layer is not None else sha
                group = block.get(key)
                if group is None:
                    group = ChangeGroup(
                        sha=sha, hunk_index=hunk_index, uncommitted=layer
                    )
                    block[key] = group
                    groups.append(group)

                if line.type == "add":
                    _add_line(group.added, line.new_line)
                else:
                    _add_line(group.deleted, line.old_line)
                if origin is not None:
                    group.origins.append(origin)

        return result(groups)

    def _get_layer_diffs(self, cancel: CancelToken | None) -> _LayerDiffs:
        if self._layer_diffs is None:
            comparison = self.comparison
            staged = self.repo.uncommitted_diffs(
                "staged",
                head=comparison.target_sha,
                worktree=comparison.worktree_path,
                cancel=cancel,
            )
            unstaged = None
            if comparison.target.kind is RevisionKind.WORKING_TREE:
                unstaged = tuple(
                    self.repo.uncommitted_diffs(
                        "unstaged", worktree=comparison.worktree_path, cancel=cancel
                    )
                )
            self._layer_diffs = _LayerDiffs(tuple(staged), unstaged)
        return self._layer_diffs

    def _find_deleting_commit(
        self,
        last_seen_in: str | None,
        content: str,
        paths: Sequence[str],
        cancel: CancelToken | None,
    ) -> str | None:
        # Still present at the target means git's blame and our diff
        # disagree about this line
        if last_seen_in is None or last_seen_in == self.comparison.target_sha:
            return None

        parents, children = self._get_graph(cancel)
        kids = children.get(last_seen_in, [])
        if len(kids) == 1 and len(parents.get(kids[0], [])) == 1:
            return kids[0]

        # Several children (the history forked after the line's last
        # commit): the one that deleted it
        for kid in kids:
            kid_parents = parents.get(kid, [])
            if len(kid_parents) == 1 and self._deletes(
                kid, kid_parents[0], content, paths, cancel
            ):
                return kid

        for merge in kids:
            merge_parents = parents.get(merge, [])
            if len(merge_parents) < 2:
                continue

            other_side = self.repo.commits_touching(
                [
                    *(parent for parent in merge_parents if parent != last_seen_in),
                    f"^{last_seen_in}",
                    f"^{self.comparison.from_sha}",
                ],
                paths,
                full_history=True,
                cancel=cancel,
            )
            for sha in other_side:
                sha_parents = parents.get(sha, [])
                if len(sha_parents) == 1 and self._deletes(
                    sha, sha_parents[0], content, paths, cancel
                ):
                    return sha

            # Nothing on the other side deleted it, so the merge itself did
            # (e.g. while resolving a conflict)
            return merge

        return kids[0] if kids else None

    def _deletes(
        self,
        sha: str,
        parent: str,
        content: str,
        paths: Sequence[str],
        cancel: CancelToken | None,
    ) -> bool:
        """Whether ``sha`` (against ``parent``) deleted a line with exactly
        this content from the file
        """
        key = (sha, tuple(paths))
        deleted = self._deleted_content.get(key)
        if deleted is None:
            diffs = self.repo.diff([parent, sha], paths, context=0, cancel=cancel)
            deleted = frozenset(
                line.content
                for diff in diffs
                for hunk in diff.hunks
                for line in hunk.lines
                if line.type == "delete"
            )
            self._deleted_content[key] = deleted
        return content in deleted

    def _get_graph(
        self, cancel: CancelToken | None
    ) -> tuple[dict[str, list[str]], dict[str, list[str]]]:
        """Parents and children of the commits from ``from`` to the target"""
        if self._graph is None:
            comparison = self.comparison
            parents = self.repo.parents(
                [f"{comparison.from_sha}..{comparison.target_sha}"], cancel=cancel
            )
            children: dict[str, list[str]] = {}
            for sha, sha_parents in parents.items():
                for parent in sha_parents:
                    children.setdefault(parent, []).append(sha)
            self._graph = (parents, children)
        return self._graph


def _trace_added(
    layers: _LayerDiffs, path: str, line: int, use: _Use
) -> _LayerTrace | _HeadTrace:
    """Trace a line added in the target back

    Added by the unstaged diff (into the working tree), or else by the
    staged diff (into the index), or else to where it is at HEAD.
    """
    index_path, index_line = path, line
    unstaged = next(
        (diff for diff in layers.unstaged or () if diff.new_path == path), None
    )
    if unstaged is not None:
        use("unstaged", unstaged)
        old = new_to_old(unstaged, line)
        if old is None or unstaged.old_path is None:
            return _LayerTrace("unstaged", LineOrigin(path, "new", line))
        index_path, index_line = unstaged.old_path, old

    staged = next((diff for diff in layers.staged if diff.new_path == index_path), None)
    if staged is None:
        return _HeadTrace(index_path, index_line)

    use("staged", staged)
    old = new_to_old(staged, index_line)
    if old is None or staged.old_path is None:
        return _LayerTrace("staged", LineOrigin(index_path, "new", index_line))
    return _HeadTrace(staged.old_path, old)


def _trace_deleted(
    layers: _LayerDiffs, path: str, line: int, use: _Use
) -> _LayerTrace | None:
    """Trace a line that is at HEAD but not in the target forward

    Deleted by the staged diff, or else by the unstaged one. None when
    neither deleted it, i.e. git's blame and the diff disagree.
    """
    index_path, index_line = path, line
    staged = next((diff for diff in layers.staged if diff.old_path == path), None)
    if staged is not None:
        use("staged", staged)
        moved = old_to_new(staged, line)
        if moved is None or staged.new_path is None:
            return _LayerTrace("staged", LineOrigin(path, "old", line))
        index_path, index_line = staged.new_path, moved

    unstaged = next(
        (diff for diff in layers.unstaged or () if diff.old_path == index_path), None
    )
    if unstaged is None:
        return None

    use("unstaged", unstaged)
    if old_to_new(unstaged, index_line) is None:
        return _LayerTrace("unstaged", LineOrigin(index_path, "old", index_line))
    return None


def _add_line(ranges: list[LineRange], line: int | None) -> None:
    assert line is not None
    if ranges and ranges[-1].end == line - 1:
        ranges[-1] = LineRange(ranges[-1].start, line)
    else:
        ranges.append(LineRange(line, line))
