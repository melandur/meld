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

"""Explanations of uncommitted hunks, kept in memory

There is no commit to save them on as a note, so they are keyed by the
hunk's layer, file and lines (not its position): an explanation lasts until
its hunk changes.
"""

import hashlib
import threading
from collections import OrderedDict
from collections.abc import Iterable, Sequence
from dataclasses import dataclass

from meld.ailens.models import (
    ChangeExplanation,
    CommitHunk,
    GitDiffHunk,
    GitFileDiff,
    UncommittedLayer,
)

#: Explanations kept at most; the oldest go first
MAX_ENTRIES = 2000


@dataclass(frozen=True)
class HunkExplanation:
    """What an uncommitted hunk does and why, and the model that inferred it"""

    what: str
    why: str
    model: str


@dataclass(frozen=True)
class PlacedExplanation(ChangeExplanation):
    """A hunk's explanation, placed where the hunk is now"""

    model: str


_LINE_PREFIXES = {"add": "+", "delete": "-", "context": " "}


def _hunk_key(layer: UncommittedLayer, diff: GitFileDiff, hunk: GitDiffHunk) -> str:
    digest = hashlib.sha1(usedforsecurity=False)
    digest.update(f"{layer}\0{diff.old_path or ''}\0{diff.new_path or ''}\0".encode())
    for line in hunk.lines:
        digest.update(f"{_LINE_PREFIXES[line.type]}{line.content}\n".encode())
    return digest.hexdigest()


class UncommittedExplanations:
    """Explanations of uncommitted hunks; safe to share between threads"""

    def __init__(self, max_entries: int = MAX_ENTRIES) -> None:
        self._by_hunk: OrderedDict[str, HunkExplanation] = OrderedDict()
        self._max_entries = max_entries
        self._lock = threading.Lock()

    def __len__(self) -> int:
        with self._lock:
            return len(self._by_hunk)

    def get(
        self, layer: UncommittedLayer, diff: GitFileDiff, hunk: GitDiffHunk
    ) -> HunkExplanation | None:
        key = _hunk_key(layer, diff, hunk)
        with self._lock:
            return self._by_hunk.get(key)

    def set(
        self,
        layer: UncommittedLayer,
        diff: GitFileDiff,
        hunk: GitDiffHunk,
        explanation: HunkExplanation,
    ) -> None:
        key = _hunk_key(layer, diff, hunk)
        with self._lock:
            self._by_hunk.pop(key, None)
            self._by_hunk[key] = explanation
            while len(self._by_hunk) > self._max_entries:
                self._by_hunk.popitem(last=False)

    def remember(
        self,
        layer: UncommittedLayer,
        diff: GitFileDiff,
        changes: Iterable[ChangeExplanation],
        model: str,
    ) -> int:
        """Keep the explanations a model gave of ``diff``'s hunks

        Each change is matched to the hunk at its position in ``diff``.
        Returns how many were kept.
        """
        kept = 0
        for change in changes:
            hunk = next(
                (
                    candidate
                    for candidate in diff.hunks
                    if candidate.old_start == change.hunk.old_start
                    and candidate.new_start == change.hunk.new_start
                ),
                None,
            )
            if hunk is not None:
                self.set(
                    layer, diff, hunk, HunkExplanation(change.what, change.why, model)
                )
                kept += 1
        return kept

    def is_explained(self, layer: UncommittedLayer, diff: GitFileDiff) -> bool:
        """Whether every hunk of ``diff`` has an explanation"""
        return all(self.get(layer, diff, hunk) is not None for hunk in diff.hunks)

    def place(
        self, layer: UncommittedLayer, diffs: Sequence[GitFileDiff]
    ) -> list[PlacedExplanation]:
        """The explained hunks of a layer's diffs, where they are now"""
        placed: list[PlacedExplanation] = []
        for diff in diffs:
            path = diff.new_path or diff.old_path
            if path is None:
                continue
            original_path = (
                diff.old_path
                if diff.old_path is not None and diff.old_path != path
                else None
            )
            for hunk in diff.hunks:
                found = self.get(layer, diff, hunk)
                if found is None:
                    continue
                placed.append(
                    PlacedExplanation(
                        hunk=CommitHunk(
                            path=path,
                            old_start=hunk.old_start,
                            old_count=hunk.old_count,
                            new_start=hunk.new_start,
                            new_count=hunk.new_count,
                            original_path=original_path,
                        ),
                        what=found.what,
                        why=found.why,
                        model=found.model,
                    )
                )
        return placed

    def clear(self) -> None:
        with self._lock:
            self._by_hunk.clear()
