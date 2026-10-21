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

"""Asks Claude Code why a commit, or an uncommitted change, was made

Nothing here runs git: callers pass the commit and its own diff (against
its first parent), already parsed. Ignored and generated files are never
sent, and the diff is capped to keep the prompt a reasonable size.
"""

import itertools
from collections.abc import Iterator, Sequence
from dataclasses import dataclass
from typing import Any, Protocol

from meld.ailens.claude import (
    DEFAULT_TIMEOUT,
    ClaudeError,
    ClaudeRequest,
    ClaudeResponse,
    run_claude,
)
from meld.ailens.explanation import (
    DEFAULT_MESSAGE_FORMAT,
    MessageFormat,
    compose_note,
    explain_commit,
    format_note,
    parse_note,
)
from meld.ailens.ignore import create_ignore_rules, is_ignored
from meld.ailens.models import (
    AiLensError,
    ChangeExplanation,
    CommitExplanation,
    CommitHunk,
    GitCommit,
    GitDiffHunk,
    GitFileChange,
    GitFileDiff,
    UncommittedLayer,
)
from meld.ailens.process import CancelToken

#: Caps that keep the prompt a reasonable size; whatever is cut is listed in
#: the prompt
MAX_PATCH_CHARS_PER_FILE = 12_000
MAX_PATCH_CHARS = 60_000

SYSTEM_PROMPT = """You explain git commits to engineers reviewing why code changed.
The diff's hunks are numbered: each hunk header starts with "[change N]".
Reply with JSON matching the schema:
- "what": one plain sentence saying what the commit changed.
- "why": one to three plain sentences on why: the problem it solves or the goal it serves.
- "changes": one entry for every numbered change in the diff, with its number as "change", and
  - "what": one short plain sentence saying what that change does.
  - "why": one or two plain sentences on why that change is needed: its part in the commit, specific to it.
    Don't repeat the commit's overall reason.
Base the answer only on the commit message and diff you are given. If a reason is not evident, give the most
likely one and say that it is a guess. No markdown, no headings, no lists."""

_CHANGES_SCHEMA: dict[str, Any] = {
    "type": "array",
    "items": {
        "type": "object",
        "properties": {
            "change": {"type": "integer"},
            "what": {"type": "string"},
            "why": {"type": "string"},
        },
        "required": ["change", "what", "why"],
        "additionalProperties": False,
    },
}

SCHEMA: dict[str, Any] = {
    "type": "object",
    "properties": {
        "what": {"type": "string"},
        "why": {"type": "string"},
        "changes": _CHANGES_SCHEMA,
    },
    "required": ["what", "why", "changes"],
    "additionalProperties": False,
}

UNCOMMITTED_SYSTEM_PROMPT = """You explain uncommitted changes to engineers reviewing why code is changing.
The changes are not committed yet, so there is no commit message: base the answer on the diff alone.
The diff's hunks are numbered: each hunk header starts with "[change N]".
Reply with JSON matching the schema: "changes" has one entry for every numbered change in the diff, with its
number as "change", and
- "what": one short plain sentence saying what that change does.
- "why": one or two plain sentences on why that change is likely needed, specific to it.
If a reason is not evident, give the most likely one and say that it is a guess. No markdown, no headings, no lists."""

UNCOMMITTED_SCHEMA: dict[str, Any] = {
    "type": "object",
    "properties": {"changes": _CHANGES_SCHEMA},
    "required": ["changes"],
    "additionalProperties": False,
}


class RunClaude(Protocol):
    """:func:`meld.ailens.claude.run_claude`, or a stand-in for tests"""

    def __call__(
        self, request: ClaudeRequest, cancel: CancelToken | None = None
    ) -> ClaudeResponse: ...


#: A changed file and its diff; None when git shows no textual difference
FileDiff = tuple[GitFileChange, GitFileDiff | None]


class IgnoredFile(AiLensError):
    """The file is generated or in ``.ailensignore``, so it isn't sent"""


class NothingToExplain(AiLensError):
    """There are no hunks to explain"""


@dataclass(frozen=True)
class InferredExplanation:
    #: The commit's own explanation; ``source`` is ``inferred``
    explanation: CommitExplanation
    #: The model that answered
    model: str
    #: One per hunk of the commit's diff the model explained, in diff order
    changes: tuple[ChangeExplanation, ...]


@dataclass(frozen=True)
class InferredChanges:
    """Explanations of a file's uncommitted hunks in one layer"""

    model: str
    #: In diff order; hunks the model didn't explain are left out
    changes: tuple[ChangeExplanation, ...]


@dataclass(frozen=True)
class Prompt:
    text: str
    #: The hunks in the order they are numbered in it: ``[change 1]`` is
    #: the first
    hunks: tuple[CommitHunk, ...]


def infer_explanation(
    commit: GitCommit,
    files: Sequence[FileDiff],
    *,
    model: str,
    ignore_rules: str | None = None,
    claude_path: str = "claude",
    timeout: float = DEFAULT_TIMEOUT,
    cancel: CancelToken | None = None,
    run: RunClaude = run_claude,
) -> InferredExplanation:
    """Ask Claude Code what a commit did and why, as a whole and hunk by hunk

    ``files`` is every file the commit changed with its diff against the
    first parent; ``ignore_rules`` is the text of the repo's
    ``.ailensignore``. Blocks until Claude Code answers.
    """
    prompt = build_prompt(commit, files, ignore_rules)
    request = ClaudeRequest(
        prompt=prompt.text,
        model=model,
        system_prompt=SYSTEM_PROMPT,
        json_schema=SCHEMA,
        claude_path=claude_path,
        timeout=timeout,
    )
    response = run(request, cancel=cancel)

    output = response.output if isinstance(response.output, dict) else {}
    what = output.get("what")
    why = output.get("why")
    if not isinstance(what, str) or not isinstance(why, str) or not why.strip():
        raise ClaudeError(
            "Claude Code's answer had no explanation in it.", "invalid-output"
        )

    return InferredExplanation(
        explanation=CommitExplanation(
            what=what.strip() or commit.subject,
            why=why.strip(),
            source="inferred",
            model=response.model,
        ),
        model=response.model,
        changes=_parse_changes(output.get("changes"), prompt.hunks),
    )


def compose_inferred_note(
    commit: GitCommit,
    inferred: InferredExplanation,
    format: MessageFormat = DEFAULT_MESSAGE_FORMAT,
) -> str:
    """The note to save on ``commit`` after inferring its explanation

    A commit that already has a Why (from its message or a hand-written
    note) keeps it, and only gets the explanations of its changes.
    """
    current = explain_commit(commit, format)
    note = parse_note(commit.note) if commit.note is not None else None
    if current.why is None or current.source == "inferred":
        why = inferred.explanation.why or ""
        head = format_note(inferred.explanation.what, why, "inferred", inferred.model)
    elif note is not None and note.source == "note":
        head = note.head
    else:
        head = ""
    return compose_note(head, inferred.model, inferred.changes)


def infer_uncommitted_explanation(
    layer: UncommittedLayer,
    diff: GitFileDiff,
    *,
    model: str,
    ignore_rules: str | None = None,
    claude_path: str = "claude",
    timeout: float = DEFAULT_TIMEOUT,
    cancel: CancelToken | None = None,
    run: RunClaude = run_claude,
) -> InferredChanges:
    """Ask Claude Code what each hunk of a file's staged or unstaged diff
    does and why, from the diff alone

    Raises :class:`IgnoredFile` for generated and ignored files, which are
    never sent, and :class:`NothingToExplain` for a diff without hunks.
    """
    path = diff.new_path or diff.old_path
    if path is None:
        raise NothingToExplain("The change has no path.")
    original_path = (
        diff.old_path if diff.old_path is not None and diff.old_path != path else None
    )
    if is_ignored(create_ignore_rules(ignore_rules), path, original_path):
        raise IgnoredFile(
            f"{path} is generated or in .ailensignore, so it isn't sent to Claude Code."
        )

    hunks = _hunks_of(diff)
    if not hunks:
        raise NothingToExplain(f"{path} has no {layer} changes to explain.")

    patch = _render_file(diff, None, itertools.count(1))
    shown, cut = _cap_patch([(patch, diff.old_path or path)])
    name = f"{original_path} -> {path}" if original_path is not None else path
    title = "Staged" if layer == "staged" else "Unstaged"
    lines = [f"{title} changes to {name}, not committed yet"]
    if cut:
        lines += ["", "The diff is cut short."]
    lines += ["", "Diff:", shown]

    request = ClaudeRequest(
        prompt="\n".join(lines),
        model=model,
        system_prompt=UNCOMMITTED_SYSTEM_PROMPT,
        json_schema=UNCOMMITTED_SCHEMA,
        claude_path=claude_path,
        timeout=timeout,
    )
    response = run(request, cancel=cancel)

    output = response.output if isinstance(response.output, dict) else {}
    changes = _parse_changes(output.get("changes"), hunks)
    if not changes:
        raise ClaudeError(
            "Claude Code's answer had no explanation in it.", "invalid-output"
        )
    return InferredChanges(model=response.model, changes=changes)


def _parse_changes(
    answer: object, hunks: Sequence[CommitHunk]
) -> tuple[ChangeExplanation, ...]:
    """The answer's explanations of numbered hunks, in diff order

    Unknown numbers, repeated numbers and empty explanations are dropped.
    """
    if not isinstance(answer, list):
        return ()

    by_hunk: dict[int, tuple[str, str]] = {}
    for item in answer:
        if not isinstance(item, dict):
            continue
        number = item.get("change")
        what = item.get("what")
        why = item.get("why")
        if (
            not isinstance(number, (int, float))
            or isinstance(number, bool)
            or not isinstance(what, str)
            or not isinstance(why, str)
        ):
            continue
        if isinstance(number, float):
            # JSON has no integers of its own; 2.0 is change 2
            if not number.is_integer():
                continue
            number = int(number)
        if not why.strip() or number in by_hunk:
            continue
        by_hunk[number] = (what.strip(), why.strip())

    return tuple(
        ChangeExplanation(hunk, *by_hunk[index])
        for index, hunk in enumerate(hunks, start=1)
        if index in by_hunk
    )


def build_prompt(
    commit: GitCommit,
    files: Sequence[FileDiff],
    ignore_rules: str | None = None,
) -> Prompt:
    """The prompt for :func:`infer_explanation`, and the hunks it numbers"""
    rules = create_ignore_rules(ignore_rules)
    included = [
        (change, diff)
        for change, diff in files
        if not is_ignored(rules, change.path, change.original_path)
    ]
    left_out = len(files) - len(included)

    numbering = itertools.count(1)
    hunks: list[CommitHunk] = []
    rendered: list[tuple[str, str]] = []
    for change, diff in included:
        if diff is None:
            continue
        hunks += _hunks_of(diff)
        name = diff.old_path or diff.new_path or change.path
        rendered.append((_render_file(diff, change, numbering), name))
    shown, cut = _cap_patch(rendered)

    message = "\n\n".join(
        part for part in (commit.subject, commit.body) if part.strip()
    )
    lines = [
        f"Commit {commit.sha} by {commit.author.name}",
        "",
        "Message:",
        message,
        "",
        "Changed files:",
    ]
    for change, _diff in files:
        name = (
            f"{change.original_path} -> {change.path}"
            if change.original_path is not None
            else change.path
        )
        lines.append(f"{change.status} {name}")
    if left_out:
        lines += [
            "",
            (
                f"{left_out} of these files are left out of the diff "
                "(ignored or generated)."
            ),
        ]
    if cut:
        lines += ["", f"The diff is cut short for: {', '.join(cut)}."]
    lines += ["", "Diff:", shown or "(no textual changes to show)"]
    return Prompt("\n".join(lines), tuple(hunks))


def _hunks_of(diff: GitFileDiff) -> list[CommitHunk]:
    path = diff.new_path or diff.old_path
    if path is None:
        return []
    original_path = (
        diff.old_path
        if diff.new_path is not None
        and diff.old_path is not None
        and diff.old_path != path
        else None
    )
    return [
        CommitHunk(
            path=path,
            old_start=hunk.old_start,
            old_count=hunk.old_count,
            new_start=hunk.new_start,
            new_count=hunk.new_count,
            original_path=original_path,
        )
        for hunk in diff.hunks
    ]


def _range(start: int, count: int) -> str:
    return str(start) if count == 1 else f"{start},{count}"


def _hunk_header(hunk: GitDiffHunk) -> str:
    header = (
        f"@@ -{_range(hunk.old_start, hunk.old_count)} "
        f"+{_range(hunk.new_start, hunk.new_count)} @@"
    )
    return f"{header} {hunk.section}" if hunk.section else header


_PREFIXES = {"context": " ", "add": "+", "delete": "-"}


def _render_file(
    diff: GitFileDiff, change: GitFileChange | None, numbering: Iterator[int]
) -> str:
    """One file's diff as ``git diff`` prints it, with each hunk header
    prefixed by ``[change N]`` (N from ``numbering``)
    """
    old = diff.old_path
    new = diff.new_path
    lines = [f"diff --git a/{old or new} b/{new or old}"]
    if old is not None and new is not None and old != new:
        if change is not None and change.status == "C":
            kind = "copy"
        else:
            kind = "rename"
        if change is not None and change.similarity is not None:
            lines.append(f"similarity index {change.similarity}%")
        lines += [f"{kind} from {old}", f"{kind} to {new}"]

    old_name = f"a/{old}" if old is not None else "/dev/null"
    new_name = f"b/{new}" if new is not None else "/dev/null"
    if diff.binary:
        lines.append(f"Binary files {old_name} and {new_name} differ")
    elif diff.hunks:
        lines += [f"--- {old_name}", f"+++ {new_name}"]
        for hunk in diff.hunks:
            lines.append(f"[change {next(numbering)}] {_hunk_header(hunk)}")
            for line in hunk.lines:
                lines.append(_PREFIXES[line.type] + line.content)
                if line.no_newline_at_end:
                    lines.append("\\ No newline at end of file")
    return "\n".join(lines) + "\n"


def _cap_patch(files: Sequence[tuple[str, str]]) -> tuple[str, list[str]]:
    """Keep each file's patch and the whole patch under the caps

    ``files`` holds each file's patch and name. Returns the patch and the
    names of the files that were cut.
    """
    cut: list[str] = []
    total = 0
    kept: list[str] = []
    for text, name in files:
        if len(text) > MAX_PATCH_CHARS_PER_FILE:
            text = f"{text[:MAX_PATCH_CHARS_PER_FILE]}\n[… rest of {name} left out]\n"
            cut.append(name)
        if total + len(text) > MAX_PATCH_CHARS:
            cut.append(name)
            continue
        kept.append(text)
        total += len(text)
    return "".join(kept), cut
