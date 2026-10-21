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

"""Data shared by the AI Lens modules.

Nothing in :mod:`meld.ailens` imports GTK, so all of it is unit-testable.
Line numbers are 1-based and ranges inclusive, as git prints them.
"""

import enum
from dataclasses import dataclass, field
from typing import Literal


@dataclass(frozen=True)
class GitIdentity:
    name: str
    email: str
    #: Strict ISO 8601
    date: str


@dataclass(frozen=True)
class GitTrailer:
    key: str
    value: str


@dataclass(frozen=True)
class GitCommit:
    sha: str
    parents: tuple[str, ...]
    author: GitIdentity
    committer: GitIdentity
    subject: str
    #: Message without the subject line; includes the trailer block
    body: str
    trailers: tuple[GitTrailer, ...] = ()
    #: The commit's note in the notes ref the log was read with, if any
    note: str | None = None


#: `git diff --name-status` letters: added, copied, deleted, modified,
#: renamed, type changed, unmerged, unknown
GitFileStatus = Literal["A", "C", "D", "M", "R", "T", "U", "X"]


@dataclass(frozen=True)
class GitFileChange:
    status: GitFileStatus
    #: Path in the target version (for deletions: the deleted path)
    path: str
    #: Path in the base version, for renames and copies
    original_path: str | None = None
    #: Rename/copy similarity, 0-100
    similarity: int | None = None
    #: Not tracked by git: only in the working tree (status ``A``)
    untracked: bool = False

    @property
    def paths(self) -> tuple[str, ...]:
        """Every path the change touches, target path first"""
        if self.original_path and self.original_path != self.path:
            return (self.path, self.original_path)
        return (self.path,)


@dataclass(frozen=True)
class GitDiffLine:
    type: Literal["context", "add", "delete"]
    content: str
    #: Line in the old version; None for added lines
    old_line: int | None
    #: Line in the new version; None for deleted lines
    new_line: int | None
    no_newline_at_end: bool = False


@dataclass(frozen=True)
class GitDiffHunk:
    old_start: int
    old_count: int
    new_start: int
    new_count: int
    #: Text after the closing ``@@``, usually the enclosing function
    section: str
    lines: tuple[GitDiffLine, ...]


@dataclass(frozen=True)
class GitFileDiff:
    #: None for added files
    old_path: str | None
    #: None for deleted files
    new_path: str | None
    binary: bool
    hunks: tuple[GitDiffHunk, ...]


@dataclass(frozen=True)
class GitBlameLine:
    #: For a normal blame, the commit that last changed the line; for
    #: ``--reverse``, the last commit it existed in
    sha: str
    #: Line number in the version of the file at :attr:`sha`
    original_line: int
    #: Line number in the blamed version of the file
    final_line: int
    #: The line predates the blamed range
    boundary: bool
    #: Path of the file at :attr:`sha`
    filename: str


GitRefType = Literal["branch", "remote", "tag"]


@dataclass(frozen=True)
class GitRef:
    type: GitRefType
    #: Short name, e.g. ``main``, ``origin/main``, ``v1.0``
    name: str
    #: Full name, e.g. ``refs/heads/main``
    full_name: str
    #: Commit the ref points to (annotated tags are peeled)
    sha: str
    subject: str
    #: Committer date of :attr:`sha`, strict ISO 8601
    date: str
    #: The branch checked out in the repository's own worktree
    current: bool = False
    #: Path of the worktree that has this branch checked out, if any
    worktree: str | None = None


@dataclass(frozen=True)
class GitWorktree:
    path: str
    #: None for a worktree on an unborn branch
    head: str | None
    #: Short branch name; None when detached
    branch: str | None
    bare: bool = False
    detached: bool = False
    locked: bool = False
    prunable: bool = False
    #: The main worktree (the first one ``git worktree list`` prints)
    main: bool = False


class RevisionKind(enum.Enum):
    #: Any revision git can resolve to a commit
    COMMIT = "commit"
    #: The index of a worktree: its staged changes
    INDEX = "index"
    #: The files on disk in a worktree: staged, unstaged and untracked
    WORKING_TREE = "working-tree"


@dataclass(frozen=True)
class Revision:
    """One side of a comparison, as the user picked it"""

    kind: RevisionKind
    #: For COMMIT: the ref or SHA as typed or picked
    ref: str | None = None
    #: For INDEX and WORKING_TREE: the worktree; None means the repository's
    #: own worktree (the one the comparison was opened in)
    worktree: str | None = None

    @classmethod
    def commit(cls, ref: str) -> "Revision":
        return cls(RevisionKind.COMMIT, ref=ref)

    @classmethod
    def index(cls, worktree: str | None = None) -> "Revision":
        return cls(RevisionKind.INDEX, worktree=worktree)

    @classmethod
    def working_tree(cls, worktree: str | None = None) -> "Revision":
        return cls(RevisionKind.WORKING_TREE, worktree=worktree)

    @property
    def uncommitted(self) -> bool:
        return self.kind is not RevisionKind.COMMIT


#: Where an uncommitted change is: staged in the index, or only in the
#: working tree (untracked files too)
UncommittedLayer = Literal["staged", "unstaged"]

#: - ``same``: base and target are the same commit
#: - ``ancestor``: base is an ancestor of target (the usual case)
#: - ``diverged``: different branches; changes are shown from their merge base
#: - ``reversed``: target is an ancestor of base
#: - ``unrelated``: no common history
ComparisonRelation = Literal["same", "ancestor", "diverged", "reversed", "unrelated"]


@dataclass(frozen=True)
class Comparison:
    """A resolved base/target pair"""

    base: Revision
    target: Revision
    #: Full SHA of the base commit; for an INDEX base, the worktree's HEAD
    base_sha: str
    #: Full SHA of the target commit; for an uncommitted target, the
    #: worktree's HEAD
    target_sha: str
    merge_base: str | None
    relation: ComparisonRelation
    #: Where the diff and attribution start: :attr:`base_sha`, or
    #: :attr:`merge_base` when the branches diverged
    from_sha: str
    #: Directory git runs in for the uncommitted side (index / working tree);
    #: the repository's own worktree otherwise
    worktree_path: str

    @property
    def uncommitted(self) -> bool:
        return self.target.uncommitted


@dataclass(frozen=True)
class LineRange:
    """1-based, inclusive"""

    start: int
    end: int


@dataclass(frozen=True)
class LineOrigin:
    """A line as the commit that changed it has it

    ``new`` in the commit's version, ``old`` in its first parent's.
    """

    path: str
    side: Literal["old", "new"]
    line: int


@dataclass
class ChangeGroup:
    """The lines of one change block that one commit is credited with

    A block changed by several commits yields one group per commit.
    """

    #: The commit in the range that made the change; None when git cannot
    #: tell or it is uncommitted
    sha: str | None
    #: Index into the file diff's hunks
    hunk_index: int
    #: Lines this group added, numbered in the target version
    added: list[LineRange] = field(default_factory=list)
    #: Lines this group removed, numbered in the base version
    deleted: list[LineRange] = field(default_factory=list)
    #: Where the lines were in the commit's own diff (against its first
    #: parent), or, when uncommitted, in its layer's diff
    origins: list[LineOrigin] = field(default_factory=list)
    #: Set for a change not committed yet
    uncommitted: UncommittedLayer | None = None


@dataclass
class FileAttribution:
    change: GitFileChange
    #: None when git shows no textual difference (e.g. a mode-only change)
    diff: GitFileDiff | None
    #: In diff order
    groups: list[ChangeGroup]
    #: Commits in the range that touched the file, newest first; the only
    #: attribution binary files get
    touched_by: list[str]
    #: For uncommitted targets: the file's diffs in each layer that its
    #: uncommitted groups' origins point into
    layers: dict[UncommittedLayer, list[GitFileDiff]] | None = None


ExplanationSource = Literal["commit", "note", "inferred"]


@dataclass(frozen=True)
class CommitExplanation:
    what: str
    why: str | None
    source: ExplanationSource
    #: Model that inferred it, for ``inferred``
    model: str | None = None


@dataclass(frozen=True)
class CommitHunk:
    """A hunk of a commit's own diff (against its first parent)"""

    #: Path in the commit; for a deleted file, the path it had
    path: str
    old_start: int
    old_count: int
    new_start: int
    new_count: int
    #: Path in the first parent, when the commit renamed the file
    original_path: str | None = None


@dataclass(frozen=True)
class ChangeExplanation:
    """What one hunk changed and why; always inferred by a model"""

    hunk: CommitHunk
    what: str
    why: str


@dataclass(frozen=True)
class CommitAgent:
    kind: Literal["claude-code"]
    #: As written in the trailer, e.g. ``Claude Opus 5.5``
    display_name: str
    #: Model ID derived from the trailer, e.g. ``claude-opus-5-5``
    model: str | None


class AiLensError(Exception):
    """An error with a message fit to show the user"""


class Cancelled(AiLensError):
    """The operation was cancelled"""
