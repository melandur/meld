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

"""Parsers for the output of the git commands AI Lens runs

Every parser takes text that :mod:`meld.ailens.git` decoded from git's
output: paths and file contents as UTF-8 with ``surrogateescape`` (so that
any path survives the round trip back to git), messages with ``replace``.
Lines are split on ``\\n`` only; a ``\\r`` before it stays in the content.

Malformed output raises :class:`GitParseError`.
"""

import re
from dataclasses import dataclass

from meld.ailens.models import (
    AiLensError,
    GitBlameLine,
    GitCommit,
    GitDiffHunk,
    GitDiffLine,
    GitFileChange,
    GitFileDiff,
    GitIdentity,
    GitRef,
    GitRefType,
    GitTrailer,
    GitWorktree,
)


class GitParseError(AiLensError):
    """git printed something the parser doesn't understand"""


# -- paths --

_ESCAPES = {
    "a": 0x07,
    "b": 0x08,
    "f": 0x0C,
    "n": 0x0A,
    "r": 0x0D,
    "t": 0x09,
    "v": 0x0B,
    "\\": 0x5C,
    '"': 0x22,
}


def unquote_git_path(value: str) -> str:
    """Undo git's C-style quoting of a path with special characters

    Octal escapes are the bytes of the path's UTF-8 encoding. Unquoted
    paths are returned as they are.
    """
    if len(value) < 2 or not value.startswith('"') or not value.endswith('"'):
        return value

    out = bytearray()
    i = 1
    end = len(value) - 1
    while i < end:
        char = value[i]
        if char != "\\":
            out += char.encode("utf-8", "surrogateescape")
            i += 1
            continue
        following = value[i + 1] if i + 1 < end else ""
        if following and following in "01234567":
            out.append(int(value[i + 1 : i + 4], 8) & 0xFF)
            i += 4
        else:
            out.append(_ESCAPES.get(following, ord(following or "\\")))
            i += 2
    return out.decode("utf-8", "surrogateescape")


def _read_quoted(value: str, start: int) -> tuple[str, int] | None:
    """The quoted string at ``value[start]``, and the index after it"""
    i = start + 1
    while i < len(value):
        if value[i] == "\\":
            i += 2
            continue
        if value[i] == '"':
            return value[start : i + 1], i + 1
        i += 1
    return None


def _split_header_paths(rest: str) -> tuple[str, str] | None:
    """The two paths of a ``diff --git <a> <b>`` line, prefixes kept

    Unquoted paths may contain spaces, which makes the split ambiguous
    unless both sides name the same path; None when it can't be told.
    """
    if rest.startswith('"'):
        first = _read_quoted(rest, 0)
        if first is None or rest[first[1] : first[1] + 1] != " ":
            return None
        a, after = first
        b = rest[after + 1 :]
        return unquote_git_path(a), unquote_git_path(b)

    if rest.endswith('"'):
        index = rest.rfind(' "')
        if index == -1:
            return None
        return rest[:index], unquote_git_path(rest[index + 1 :])

    # a/<path> b/<path>: with the same path on both sides, the halves have
    # equal length
    if len(rest) % 2 == 1:
        half = len(rest) // 2
        a, b = rest[:half], rest[half + 1 :]
        if rest[half] == " " and a[2:] == b[2:]:
            return a, b
    return None


def _strip_prefix(path: str, prefix: str) -> str:
    return path.removeprefix(prefix)


def _header_path(value: str, prefix: str) -> str | None:
    """A ``---``/``+++`` path; None for ``/dev/null``"""
    # git appends a tab to paths that contain a space
    value = value.removesuffix("\t")
    path = unquote_git_path(value)
    if path == "/dev/null":
        return None
    return _strip_prefix(path, prefix)


# -- diff --

_HUNK_HEADER = re.compile(
    r"@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@ ?(.*)", re.DOTALL
)


@dataclass
class _FileDiffBuilder:
    old_path: str | None = None
    new_path: str | None = None
    binary: bool = False
    new_file: bool = False
    deleted_file: bool = False
    #: The paths came from ``---``/``+++`` or rename/copy lines
    explicit: bool = False
    header: tuple[str, str] | None = None

    def __post_init__(self) -> None:
        self.hunks: list[GitDiffHunk] = []

    def build(self) -> GitFileDiff:
        old_path, new_path = self.old_path, self.new_path
        if not self.explicit and self.header is not None:
            # No ---/+++ lines (binary, empty or mode-only changes): fall
            # back to the paths in the diff --git line
            old_path = _strip_prefix(self.header[0], "a/")
            new_path = _strip_prefix(self.header[1], "b/")
        if self.new_file:
            old_path = None
        if self.deleted_file:
            new_path = None
        return GitFileDiff(old_path, new_path, self.binary, tuple(self.hunks))


def parse_diff(output: str) -> list[GitFileDiff]:
    """Parse unified ``git diff`` output (one or more files)

    Hunk bodies are read by the line counts in their headers, so content
    that looks like a header (e.g. a line starting with ``diff --git``)
    is safe. Files without ``---``/``+++`` lines (binary, empty, mode-only)
    take their paths from the ``diff --git`` line when it is unambiguous.
    """
    lines = output.split("\n")
    files: list[_FileDiffBuilder] = []
    file: _FileDiffBuilder | None = None

    i = 0
    while i < len(lines):
        line = lines[i]
        i += 1

        if line.startswith("diff --git "):
            file = _FileDiffBuilder(header=_split_header_paths(line[11:]))
            files.append(file)
            continue
        if file is None:
            continue

        if line.startswith("@@ "):
            hunk, i = _parse_hunk(line, lines, i)
            file.hunks.append(hunk)
        elif line.startswith("--- "):
            file.old_path = _header_path(line[4:], "a/")
            file.explicit = True
        elif line.startswith("+++ "):
            file.new_path = _header_path(line[4:], "b/")
            file.explicit = True
        elif line.startswith(("rename from ", "copy from ")):
            file.old_path = unquote_git_path(line.split(" ", 2)[2])
            file.explicit = True
        elif line.startswith(("rename to ", "copy to ")):
            file.new_path = unquote_git_path(line.split(" ", 2)[2])
            file.explicit = True
        elif line.startswith("new file mode "):
            file.new_file = True
        elif line.startswith("deleted file mode "):
            file.deleted_file = True
        elif line.startswith("Binary files ") or line == "GIT binary patch":
            file.binary = True
    return [file.build() for file in files]


def _parse_hunk(header: str, lines: list[str], start: int) -> tuple[GitDiffHunk, int]:
    match = _HUNK_HEADER.fullmatch(header)
    if match is None:
        raise GitParseError(f"Unexpected git diff hunk header {header!r}")

    old_start = int(match.group(1))
    old_count = 1 if match.group(2) is None else int(match.group(2))
    new_start = int(match.group(3))
    new_count = 1 if match.group(4) is None else int(match.group(4))

    body: list[GitDiffLine] = []
    old_line, new_line = old_start, new_start
    old_left, new_left = old_count, new_count
    i = start
    while old_left > 0 or new_left > 0 or (i < len(lines) and lines[i][:1] == "\\"):
        if i >= len(lines) or (i == len(lines) - 1 and lines[i] == ""):
            raise GitParseError(f"Truncated git diff hunk {header!r}")
        line = lines[i]
        i += 1

        kind = line[:1]
        if kind == " ":
            body.append(GitDiffLine("context", line[1:], old_line, new_line))
            old_line += 1
            new_line += 1
            old_left -= 1
            new_left -= 1
        elif kind == "-":
            body.append(GitDiffLine("delete", line[1:], old_line, None))
            old_line += 1
            old_left -= 1
        elif kind == "+":
            body.append(GitDiffLine("add", line[1:], None, new_line))
            new_line += 1
            new_left -= 1
        elif kind == "\\":
            # "\ No newline at end of file" applies to the line before it
            if body:
                last = body[-1]
                body[-1] = GitDiffLine(
                    last.type, last.content, last.old_line, last.new_line, True
                )
        else:
            raise GitParseError(f"Unexpected git diff line {line!r} in hunk {header!r}")

    if old_left < 0 or new_left < 0:
        raise GitParseError(
            f"git diff hunk {header!r} has more lines than its header says"
        )

    hunk = GitDiffHunk(
        old_start, old_count, new_start, new_count, match.group(5), tuple(body)
    )
    return hunk, i


# -- blame --

_BLAME_HEADER = re.compile(r"([0-9a-f]{40,64}) (\d+) (\d+)(?: \d+)?")


def parse_blame(output: str) -> list[GitBlameLine]:
    """Parse ``git blame --porcelain`` (also with ``--reverse``)

    Commit details, ``boundary`` and ``filename`` among them, only appear
    the first time a commit does; they apply to all of its lines. Sorted
    by :attr:`GitBlameLine.final_line`.
    """
    result: list[GitBlameLine] = []
    #: SHA -> [boundary, filename]
    commits: dict[str, list] = {}
    current: tuple[str, int, int] | None = None

    for line in output.split("\n"):
        if current is None:
            if line == "":
                continue
            match = _BLAME_HEADER.fullmatch(line)
            if match is None:
                raise GitParseError(f"Unexpected git blame line {line!r}")
            sha = match.group(1)
            current = (sha, int(match.group(2)), int(match.group(3)))
            commits.setdefault(sha, [False, ""])
            continue

        info = commits[current[0]]
        if line.startswith("\t"):
            # The line's content ends the entry
            result.append(GitBlameLine(current[0], current[1], current[2], *info))
            current = None
        elif line == "boundary":
            info[0] = True
        elif line.startswith("filename "):
            info[1] = unquote_git_path(line[9:])

    if current is not None:
        raise GitParseError("Truncated git blame output")

    result.sort(key=lambda blamed: blamed.final_line)
    return result


# -- log --

_LOG_FIELDS = (
    "%H",
    "%P",
    "%an",
    "%ae",
    "%aI",
    "%cn",
    "%ce",
    "%cI",
    "%s",
    "%b",
    "%(trailers:only,unfold,separator=%x1f)",
)


def log_format(with_notes: bool = False) -> str:
    """The ``--format`` argument :func:`parse_log` reads; pass with ``-z``

    Fields and records are both NUL-separated, so records are read in
    groups of fields. ``with_notes`` adds the commit's note; git prints
    ``%N`` literally unless notes are on (``--notes=<ref>``).
    """
    fields = _LOG_FIELDS + (("%N",) if with_notes else ())
    return "--format=" + "%x00".join(fields)


def parse_log(output: str, with_notes: bool = False) -> list[GitCommit]:
    field_count = len(_LOG_FIELDS) + (1 if with_notes else 0)
    tokens = output.split("\0")
    # -z terminates the last record too, leaving one empty token
    if tokens and tokens[-1] == "":
        tokens.pop()
    if len(tokens) % field_count != 0:
        raise GitParseError(
            f"Unexpected git log output: {len(tokens)} fields is not a "
            f"multiple of {field_count}"
        )

    commits = []
    for i in range(0, len(tokens), field_count):
        fields = tokens[i : i + field_count]
        (
            sha,
            parents,
            author_name,
            author_email,
            author_date,
            committer_name,
            committer_email,
            committer_date,
            subject,
            body,
            trailers,
        ) = fields[: len(_LOG_FIELDS)]
        note = fields[-1].strip() if with_notes else ""
        commits.append(
            GitCommit(
                sha=sha,
                parents=tuple(parents.split()),
                author=GitIdentity(author_name, author_email, author_date),
                committer=GitIdentity(committer_name, committer_email, committer_date),
                subject=subject,
                body=body.rstrip(),
                trailers=_parse_trailers(trailers),
                note=note or None,
            )
        )
    return commits


def _parse_trailers(value: str) -> tuple[GitTrailer, ...]:
    trailers = []
    for line in value.split("\x1f"):
        key, sep, rest = line.partition(":")
        if not sep or not key.strip():
            continue
        trailers.append(GitTrailer(key.strip(), rest.strip()))
    return tuple(trailers)


# -- name-status, numstat --

_STATUSES = frozenset("ACDMRTUX")


def _nul_tokens(output: str) -> list[str]:
    tokens = output.split("\0")
    # Every entry ends with NUL, leaving one empty token
    if tokens and tokens[-1] == "":
        tokens.pop()
    return tokens


def parse_name_status(output: str) -> list[GitFileChange]:
    """Parse ``git diff --name-status -z`` (and ``diff-tree``)"""
    tokens = _nul_tokens(output)
    changes = []
    i = 0
    while i < len(tokens):
        token = tokens[i]
        i += 1
        status = token[:1]
        if status not in _STATUSES:
            raise GitParseError(f"Unexpected git name-status entry {token!r}")

        if status in ("R", "C"):
            if i + 1 >= len(tokens):
                raise GitParseError(f"Truncated git name-status entry {token!r}")
            original_path, path = tokens[i], tokens[i + 1]
            i += 2
            similarity = int(token[1:]) if token[1:].isdigit() else None
            changes.append(GitFileChange(status, path, original_path, similarity))
        else:
            if i >= len(tokens):
                raise GitParseError(f"Truncated git name-status entry {token!r}")
            changes.append(GitFileChange(status, tokens[i]))
            i += 1
    return changes


@dataclass(frozen=True)
class GitFileStats:
    """Lines added and deleted in one file; None for binary files"""

    #: For renames, the new path
    path: str
    additions: int | None
    deletions: int | None

    @property
    def binary(self) -> bool:
        return self.additions is None


_NUMSTAT = re.compile(r"(\d+|-)\t(\d+|-)\t(.*)", re.DOTALL)


def parse_numstat(output: str) -> list[GitFileStats]:
    """Parse ``git diff --numstat -z``; renames are keyed by their new path"""
    tokens = _nul_tokens(output)
    stats = []
    i = 0
    while i < len(tokens):
        token = tokens[i]
        i += 1
        match = _NUMSTAT.fullmatch(token)
        if match is None:
            raise GitParseError(f"Unexpected git numstat entry {token!r}")

        path = match.group(3)
        if path == "":
            # A rename: the old and new paths follow as their own tokens
            if i + 1 >= len(tokens) or tokens[i + 1] == "":
                raise GitParseError(f"Truncated git numstat rename entry {token!r}")
            path = tokens[i + 1]
            i += 2

        def count(value: str) -> int | None:
            return None if value == "-" else int(value)

        stats.append(GitFileStats(path, count(match.group(1)), count(match.group(2))))
    return stats


# -- small ones --


def parse_patch_ids(output: str) -> dict[str, str]:
    """Parse ``git patch-id`` output into commit SHA -> patch ID"""
    ids = {}
    for line in output.split("\n"):
        if not line:
            continue
        parts = line.split(" ")
        if len(parts) != 2:
            raise GitParseError(f"Unexpected git patch-id line {line!r}")
        patch_id, sha = parts
        ids[sha] = patch_id
    return ids


def parse_rev_list_parents(output: str) -> dict[str, list[str]]:
    """Parse ``git rev-list --parents`` into commit SHA -> parent SHAs"""
    parents = {}
    for line in output.split("\n"):
        if not line:
            continue
        sha, *rest = line.split(" ")
        parents[sha] = rest
    return parents


# -- refs --

_REF_FIELDS = (
    "%(refname)",
    "%(refname:short)",
    "%(symref)",
    "%(HEAD)",
    "%(objecttype)",
    "%(objectname)",
    "%(contents:subject)",
    "%(committerdate:iso-strict)",
    # The same, peeled: set for annotated tags only
    "%(*objecttype)",
    "%(*objectname)",
    "%(*contents:subject)",
    "%(*committerdate:iso-strict)",
)

#: Fields are NUL-separated, refs newline-separated (neither can appear in
#: a ref name or a subject)
REF_FORMAT = "--format=" + "%00".join(_REF_FIELDS)

_REF_TYPES: tuple[tuple[str, GitRefType], ...] = (
    ("refs/heads/", "branch"),
    ("refs/remotes/", "remote"),
    ("refs/tags/", "tag"),
)


def parse_refs(output: str) -> list[GitRef]:
    """Parse ``git for-each-ref`` with :data:`REF_FORMAT`

    Skips symbolic refs (e.g. ``origin/HEAD``) and refs to non-commits.
    :attr:`GitRef.current` comes from ``%(HEAD)``; :attr:`GitRef.worktree`
    is left for the caller to fill in.
    """
    refs = []
    for line in output.split("\n"):
        if not line:
            continue
        fields = line.split("\0")
        if len(fields) != len(_REF_FIELDS):
            raise GitParseError(f"Unexpected git for-each-ref line {line!r}")
        (
            full_name,
            name,
            symref,
            head,
            object_type,
            sha,
            subject,
            date,
            peeled_type,
            peeled_sha,
            peeled_subject,
            peeled_date,
        ) = fields
        if symref:
            continue

        ref_type = next(
            (kind for prefix, kind in _REF_TYPES if full_name.startswith(prefix)),
            None,
        )
        if ref_type is None:
            continue

        peeled = peeled_type != ""
        if (peeled_type if peeled else object_type) != "commit":
            continue

        refs.append(
            GitRef(
                type=ref_type,
                name=name,
                full_name=full_name,
                sha=peeled_sha if peeled else sha,
                subject=peeled_subject if peeled else subject,
                date=peeled_date if peeled else date,
                current=head == "*",
            )
        )
    return refs


# -- worktrees --


def parse_worktrees(output: str) -> list[GitWorktree]:
    """Parse ``git worktree list --porcelain -z``

    Attributes are NUL-terminated and records end with an empty one. The
    first worktree is the main one. An all-zero ``HEAD`` (unborn branch)
    becomes None.
    """
    worktrees: list[GitWorktree] = []
    record: dict[str, str] = {}

    def finish() -> None:
        if not record:
            return
        if "worktree" not in record:
            raise GitParseError(f"Unexpected git worktree record {record!r}")
        head = record.get("HEAD") or None
        if head is not None and head.strip("0") == "":
            head = None
        branch = record.get("branch")
        worktrees.append(
            GitWorktree(
                path=record["worktree"],
                head=head,
                branch=_strip_prefix(branch, "refs/heads/") if branch else None,
                bare="bare" in record,
                detached="detached" in record,
                locked="locked" in record,
                prunable="prunable" in record,
                main=not worktrees,
            )
        )
        record.clear()

    for token in output.split("\0"):
        if token == "":
            finish()
            continue
        key, _, value = token.partition(" ")
        record[key] = value
    finish()
    return worktrees
