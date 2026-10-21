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

"""Why a commit was made: from its message or its AI Lens note

The note format is shared with the AI Lens VS Code extension, so notes
written by either are read by the other; keep it byte-for-byte compatible.
"""

import re
from collections.abc import Sequence
from dataclasses import dataclass, field
from typing import Literal, TypeVar

from meld.ailens.models import (
    ChangeExplanation,
    CommitExplanation,
    CommitHunk,
    GitCommit,
    LineOrigin,
)

# JavaScript's ``\w`` is ASCII only, so the patterns below spell it out
_LABEL = re.compile(r"([A-Za-z][A-Za-z0-9_-]*):[ \t]*(.*)", re.DOTALL)
_HEAD_LINE = re.compile(r"([A-Za-z][A-Za-z0-9_-]*):[ \t]*(.*)")
_CHANGE_MODEL_LINE = re.compile(r"Change-Model:[ \t]*(.*)")
_CHANGE_LINE = re.compile(
    r"Change: -([0-9]+)(?:,([0-9]+))? \+([0-9]+)(?:,([0-9]+))? (.+)"
)
_CHANGE_FIELD = re.compile(r"(Change-From|What|Why):[ \t]*(.*)")
_TRAILER_LINE = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]*:\s")
_PARAGRAPH_BREAK = re.compile(r"\n\s*\n")
#: A Why line that would read as the next change's header
_CHANGE_HEADER_START = re.compile(r"^(?=Change: -[0-9])", re.MULTILINE)


@dataclass(frozen=True)
class MessageFormat:
    """Which labels mark What and Why in a commit message

    Either as trailers (``Why: …``) or as body paragraphs starting with them.
    """

    what_keys: tuple[str, ...]
    why_keys: tuple[str, ...]


DEFAULT_MESSAGE_FORMAT = MessageFormat(
    what_keys=("What", "Agent-What"), why_keys=("Why", "Agent-Why")
)


def _normalize(text: str) -> str:
    return text.replace("\r\n", "\n")


def explain_commit(
    commit: GitCommit, format: MessageFormat = DEFAULT_MESSAGE_FORMAT
) -> CommitExplanation:
    """The commit's explanation, best source first

    1. What/Why labeled in the commit message
    2. its note in the AI Lens notes ref (added later by hand, or inferred
       and saved)
    3. the plain message: subject as What, body (without trailers) as Why

    A result without a Why can be inferred.
    """
    what, why = _parse_labeled_message(commit, format)
    if why is not None:
        return CommitExplanation(what or commit.subject, why, "commit")

    if commit.note is not None:
        note = parse_note(commit.note)
        if note.why is not None:
            return CommitExplanation(
                note.what or commit.subject, note.why, note.source, note.model
            )

    body = _strip_trailer_block(_normalize(commit.body), bool(commit.trailers))
    plain_why = body.strip()
    return CommitExplanation(what or commit.subject, plain_why or None, "commit")


def _parse_labeled_message(
    commit: GitCommit, format: MessageFormat
) -> tuple[str | None, str | None]:
    """What/Why from trailers with the format's keys, or from body
    paragraphs that start with ``<key>:``; empty values count as missing
    """
    what_keys = {key.lower() for key in format.what_keys}
    why_keys = {key.lower() for key in format.why_keys}

    what: str | None = None
    why: str | None = None
    for trailer in commit.trailers:
        key = trailer.key.lower()
        if what is None and key in what_keys:
            what = trailer.value
        elif why is None and key in why_keys:
            why = trailer.value

    for paragraph in _PARAGRAPH_BREAK.split(_normalize(commit.body)):
        match = _LABEL.fullmatch(paragraph.strip())
        if match is None:
            continue
        key = match.group(1).lower()
        if what is None and key in what_keys:
            what = match.group(2).strip()
        elif why is None and key in why_keys:
            why = match.group(2).strip()

    return what or None, why or None


def _strip_trailer_block(body: str, has_trailers: bool) -> str:
    if not has_trailers:
        return body

    paragraphs = _PARAGRAPH_BREAK.split(body.rstrip())
    last = paragraphs[-1]
    if all(
        _TRAILER_LINE.match(line) or re.match(r"\s", line) for line in last.split("\n")
    ):
        paragraphs.pop()
    return "\n\n".join(paragraphs)


@dataclass
class ParsedNote:
    what: str | None
    why: str | None
    source: Literal["note", "inferred"]
    model: str | None
    #: Model that explained the commit's changes one by one; set once they
    #: were explained
    change_model: str | None = None
    #: In the order of the commit's diff
    changes: list[ChangeExplanation] = field(default_factory=list)
    #: The note's text before its changes: the commit's own explanation,
    #: as written
    head: str = ""


def parse_note(text: str) -> ParsedNote:
    """Parse an AI Lens note

    Notes are ``Key: value`` lines (``Source``, ``Model``, ``What``) and then
    ``Why:``, which runs to the end. A note without ``What:`` or ``Why:`` is
    all Why, so a plain ``git notes add -m "…"`` works too.

    Explanations of single changes come after, from a ``Change-Model:`` line
    on: each starts with ``Change: -<old> +<new> <path>`` (the hunk's ranges
    in the commit's own diff), then ``Change-From:`` for a rename,
    ``What:``, and ``Why:`` up to the next ``Change:``.
    """
    all_lines = _normalize(text).strip().split("\n")
    tail_start = next(
        (
            i
            for i, line in enumerate(all_lines)
            if _CHANGE_MODEL_LINE.fullmatch(line) or _CHANGE_LINE.fullmatch(line)
        ),
        len(all_lines),
    )
    head = "\n".join(all_lines[:tail_start]).strip()
    note = _parse_note_head(head)

    # The change being read: its header fields, What, and Why lines so far
    hunk: CommitHunk | None = None
    what = ""
    why: list[str] | None = None

    def finish() -> None:
        if hunk is not None:
            text = "\n".join(why).strip() if why is not None else ""
            if text:
                note.changes.append(ChangeExplanation(hunk, what, text))

    for line in all_lines[tail_start:]:
        model = _CHANGE_MODEL_LINE.fullmatch(line)
        if model is not None and hunk is None:
            note.change_model = model.group(1).strip() or None
            continue

        header = _CHANGE_LINE.fullmatch(line)
        if header is not None:
            finish()
            old_count, new_count = header.group(2), header.group(4)
            hunk = CommitHunk(
                path=header.group(5),
                old_start=int(header.group(1)),
                old_count=int(old_count) if old_count is not None else 1,
                new_start=int(header.group(3)),
                new_count=int(new_count) if new_count is not None else 1,
            )
            what = ""
            why = None
            continue
        if hunk is None:
            continue

        if why is not None:
            why.append(line)
            continue

        field_match = _CHANGE_FIELD.fullmatch(line)
        if field_match is None:
            continue
        key, value = field_match.groups()
        if key == "Change-From":
            hunk = CommitHunk(
                path=hunk.path,
                old_start=hunk.old_start,
                old_count=hunk.old_count,
                new_start=hunk.new_start,
                new_count=hunk.new_count,
                original_path=value,
            )
        elif key == "What":
            what = value.strip()
        else:
            why = [value]
    finish()

    return note


def _parse_note_head(text: str) -> ParsedNote:
    note = ParsedNote(what=None, why=None, source="note", model=None, head=text)
    lines = text.split("\n")

    i = 0
    while i < len(lines):
        match = _HEAD_LINE.fullmatch(lines[i])
        if match is None:
            break

        key = match.group(1).lower()
        value = match.group(2).strip()
        if key == "why":
            note.why = "\n".join([value, *lines[i + 1 :]]).strip() or None
            return note

        if key == "what":
            note.what = value or None
        elif key == "source":
            note.source = "inferred" if value.lower() == "inferred" else "note"
        elif key == "model":
            note.model = value or None
        else:
            break
        i += 1

    if note.what is None and i == 0:
        note.why = text.strip() or None
    return note


def format_note(
    what: str,
    why: str,
    source: Literal["note", "inferred"],
    model: str | None = None,
) -> str:
    """A note holding a commit's own explanation"""
    lines = [f"Source: {source}"]
    if model is not None:
        lines.append(f"Model: {model}")
    lines.append("What: " + what.replace("\n", " "))
    lines.append(f"Why: {why}")
    lines.append("")
    return "\n".join(lines)


def compose_note(head: str, model: str, changes: Sequence[ChangeExplanation]) -> str:
    """A note from its head (the commit's own explanation, may be empty) and
    the explanations of its changes
    """
    sections = []
    for change in changes:
        hunk = change.hunk
        lines = [
            (
                f"Change: -{hunk.old_start},{hunk.old_count} "
                f"+{hunk.new_start},{hunk.new_count} {hunk.path}"
            )
        ]
        if hunk.original_path is not None and hunk.original_path != hunk.path:
            lines.append(f"Change-From: {hunk.original_path}")
        lines.append("What: " + change.what.replace("\n", " "))
        # A line that looks like the next change's header would end this Why
        # early
        lines.append("Why: " + _CHANGE_HEADER_START.sub(" ", change.why.strip()))
        sections.append("\n".join(lines))

    parts = [head.strip()] if head.strip() else []
    parts.append(f"Change-Model: {model}")
    parts.extend(sections)
    return "\n\n".join(parts) + "\n"


_Change = TypeVar("_Change", bound=ChangeExplanation)


def find_change_explanations(
    origins: Sequence[LineOrigin], changes: Sequence[_Change]
) -> list[_Change]:
    """The explanations of the commit's own hunks that a change group's
    lines fall in, in the order the group's lines first reach them

    Empty when none do (the commit's changes were not explained, or the hunk
    was left out of the prompt).
    """
    found: list[_Change] = []
    for origin in origins:
        for change in changes:
            if not any(change is seen for seen in found) and _covers(
                change.hunk, origin
            ):
                found.append(change)
    return found


def _covers(hunk: CommitHunk, origin: LineOrigin) -> bool:
    if origin.side == "old":
        path = hunk.original_path or hunk.path
        start, count = hunk.old_start, hunk.old_count
    else:
        path = hunk.path
        start, count = hunk.new_start, hunk.new_count
    return path == origin.path and start <= origin.line < start + count
