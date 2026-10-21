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

"""What a build file states as fact

Everything here is read from git the same way every time, so the same
comparison always yields the same facts. See ``docs/build-format.md`` in
AI Lens for how :mod:`meld.ailens.build_file` renders them.

Gathering the facts from an open comparison is not here yet. It belongs in
this module as::

    def gather_build_facts(
        session,
        ignore_rules: str | None = None,
        cancel: CancelToken | None = None,
        on_progress: Callable[[int, int], None] | None = None,
    ) -> BuildFacts

a port of ``gatherBuildFacts`` in AI Lens' ``src/build/buildFacts.ts``:
one :class:`BuildFile` per changed file (ignored files get no entries),
its change cards folded into :class:`BuildFileEntry` items with
:func:`merge_ranges`, the range's commits with their patch IDs, and the
refs the user picked each side by (``None`` when picked by SHA).
"""

from dataclasses import dataclass, field
from typing import Literal

from meld.ailens.models import (
    ComparisonRelation,
    ExplanationSource,
    GitFileStatus,
    LineRange,
)


@dataclass(frozen=True)
class BuildRevision:
    sha: str
    #: ``git rev-parse --short``; longer than 7 when 7 would be ambiguous
    short_sha: str
    #: How the user picked it (branch, tag, ``HEAD``); None when by SHA
    ref: str | None


@dataclass(frozen=True)
class BuildCommit:
    sha: str
    short_sha: str
    #: ``git patch-id --stable``; None for merges
    patch_id: str | None
    author: str
    agent: Literal["claude-code", "human"]
    #: From the Co-Authored-By trailer, when it names one
    agent_model: str | None
    #: e.g. ``Claude Opus 5.5``
    agent_name: str | None
    what: str
    why: str | None
    why_source: ExplanationSource
    #: Model that inferred the Why, for ``inferred``
    inferred_by: str | None = None
    #: Model that explained the commit's changes one by one
    changes_inferred_by: str | None = None


@dataclass(frozen=True)
class BuildChange:
    """What one change does and why; always inferred"""

    what: str
    why: str


@dataclass
class BuildFileEntry:
    """The lines of a file one commit is credited with

    One entry per explained change of the commit, or one for all its lines
    when its changes weren't explained one by one.
    """

    #: None when git can't tell which commit made the change
    sha: str | None
    #: Numbered in the target version
    added: list[LineRange] = field(default_factory=list)
    #: Numbered in the base version
    deleted: list[LineRange] = field(default_factory=list)
    #: What the change does and why, inferred; None when not explained one
    #: by one
    changes: list[BuildChange] | None = None


@dataclass
class BuildFile:
    path: str
    original_path: str | None
    status: GitFileStatus
    #: None for binary files
    additions: int | None
    deletions: int | None
    binary: bool = False
    #: Matched by ``.ailensignore`` or the generated-file rules: listed by
    #: path only
    ignored: bool = False
    #: In the order each commit first appears in the file's diff
    entries: list[BuildFileEntry] = field(default_factory=list)
    #: Commits that touched the file, newest first; what binary files get
    #: instead of entries
    touched_by: list[str] = field(default_factory=list)


@dataclass(frozen=True)
class BuildStats:
    files: int
    insertions: int
    deletions: int
    commits: int


@dataclass
class BuildFacts:
    """Everything a build file states as fact"""

    base: BuildRevision
    target: BuildRevision
    merge_base: str | None
    relation: ComparisonRelation
    stats: BuildStats
    #: Oldest first
    commits: list[BuildCommit]
    files: list[BuildFile]


def merge_ranges(ranges: list[LineRange]) -> list[LineRange]:
    """Sorted, with overlapping and adjacent ranges joined"""
    merged: list[LineRange] = []
    for line_range in sorted(ranges, key=lambda r: r.start):
        if merged and line_range.start <= merged[-1].end + 1:
            last = merged[-1]
            merged[-1] = LineRange(last.start, max(last.end, line_range.end))
        else:
            merged.append(line_range)
    return merged
