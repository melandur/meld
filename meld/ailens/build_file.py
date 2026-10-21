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

"""Build files: rendering, reading their header back, and ``index.md``

The output is byte-identical to what AI Lens writes for the same facts, so
a repository can mix builds from both. See ``docs/build-format.md`` in AI
Lens for the format.
"""

import functools
import json
import re
from dataclasses import dataclass, field
from datetime import datetime, timezone

from meld.ailens.build_facts import (
    BuildCommit,
    BuildFacts,
    BuildFile,
    BuildFileEntry,
    BuildRevision,
)
from meld.ailens.build_narrative import (
    BUILD_PROMPT_VERSION,
    JS_SPACE_CLASS,
    BuildNarrative,
    js_trim,
    one_line,
)
from meld.ailens.models import LineRange

#: Format version written as ``ai_lens:`` in the frontmatter
BUILD_FORMAT_VERSION = 2

_MINUS = "\u2212"
_EN_DASH = "\u2013"


def _inferred_label(model: str | None) -> str:
    model = model if model is not None else "a model"
    return f"Why (inferred by {model}, not the author’s words)"


def build_file_name(facts: BuildFacts) -> str:
    return f"{facts.base.short_sha}_{facts.target.short_sha}_build.md"


def format_created(created: datetime) -> str:
    """``created:`` as AI Lens writes it: UTC, to the second

    A naive ``created`` is taken to be UTC.
    """
    if created.tzinfo is None:
        created = created.replace(tzinfo=timezone.utc)
    c = created.astimezone(timezone.utc)
    return (
        f"{c.year:04d}-{c.month:02d}-{c.day:02d}"
        f"T{c.hour:02d}:{c.minute:02d}:{c.second:02d}Z"
    )


def render_build(
    facts: BuildFacts,
    narrative: BuildNarrative,
    created: datetime,
    generator: str,
) -> str:
    """The build file

    YAML frontmatter, then Summary, Decisions, Commits, Changes and Risks.
    Everything except the narrative and ``created`` comes from ``facts``, in
    a fixed order, so the same comparison renders the same way. It holds no
    diff text: each commit's explanation is given once under Commits, and
    changes are line ranges, with what each one does and why when the
    commit's changes were explained one by one.
    """
    commits = {commit.sha: commit for commit in facts.commits}
    stats = facts.stats

    lines = [
        "---",
        f"ai_lens: {BUILD_FORMAT_VERSION}",
        f"title: {yaml_string(narrative.title)}",
        *_yaml_revision("base", facts.base),
        *_yaml_revision("target", facts.target),
        f"merge_base: {facts.merge_base if facts.merge_base is not None else 'null'}",
        f"relation: {facts.relation}",
        f"created: {format_created(created)}",
        f"build_model: {yaml_string(narrative.model)}",
        f"generator: {yaml_string(generator)}",
        f"prompt_version: {BUILD_PROMPT_VERSION}",
        (
            f"stats: {{ files: {stats.files}, insertions: {stats.insertions}, "
            f"deletions: {stats.deletions}, commits: {stats.commits} }}"
        ),
        "commits:" if facts.commits else "commits: []",
    ]
    for commit in facts.commits:
        lines += _yaml_commit(commit)

    revisions = f"{_describe_revision(facts.base)} → {_describe_revision(facts.target)}"
    if facts.relation == "diverged" and facts.merge_base is not None:
        merge_base = facts.merge_base[: len(facts.base.short_sha)]
        revisions += f" · changes since the merge base `{merge_base}`"

    lines += ["---", "", f"# {narrative.title}", "", revisions, ""]
    lines += ["## Summary", "", narrative.summary, ""]

    lines += ["## Decisions", ""]
    if not narrative.decisions:
        lines.append("- None recorded.")
    for decision in narrative.decisions:
        cited = ""
        if decision.commits:
            cited = " (" + ", ".join(f"`{sha}`" for sha in decision.commits) + ")"
        lines.append(f"- {decision.text}{cited}")
    lines.append("")

    lines.append("## Commits")
    if not facts.commits:
        lines += ["", "- None."]
    for commit in facts.commits:
        lines += ["", *_render_commit(commit)]
    lines.append("")

    lines.append("## Changes")
    for file in facts.files:
        lines += ["", *_render_file(file, commits)]
    lines.append("")

    lines += ["## Risks / open questions", ""]
    if not narrative.risks:
        lines.append("- None recorded.")
    lines += [f"- {risk}" for risk in narrative.risks]
    lines.append("")

    return "\n".join(lines)


def _render_file(file: BuildFile, commits: dict[str, BuildCommit]) -> list[str]:
    if file.additions is None:
        counts = "binary"
    else:
        deletions = file.deletions if file.deletions is not None else 0
        counts = f"+{file.additions} {_MINUS}{deletions}"
    original = (
        f" (from {_escape_inline(file.original_path)})"
        if file.original_path is not None
        else ""
    )
    heading = f"### {_escape_inline(file.path)}{original} · {file.status} · {counts}"

    if file.ignored:
        return [heading, "", "- Left out: generated, or matched by `.ailensignore`."]
    if file.binary or not file.entries:
        by = [
            f"`{commits[sha].short_sha if sha in commits else sha[:7]}`"
            for sha in file.touched_by
        ]
        kind = "Binary file" if file.binary else "No line changes"
        changed_by = f"; changed by {', '.join(by)}" if by else ""
        return [heading, "", f"- {kind}{changed_by}."]

    lines = [heading, ""]
    for entry in file.entries:
        lines += _render_entry(entry, commits)
    return lines


def _render_commit(commit: BuildCommit) -> list[str]:
    if commit.why_source == "inferred":
        label = _inferred_label(commit.inferred_by)
    else:
        label = "Why"
    lines = [
        f"### `{commit.short_sha}` {_escape_inline(one_line(commit.what))}",
        "",
        f"- {_describe_author(commit)} · source: {commit.why_source}",
    ]
    if commit.why is None:
        lines.append("- Why: not recorded.")
    else:
        lines += _render_why(label, commit.why, "")
    return lines


def _render_entry(entry: BuildFileEntry, commits: dict[str, BuildCommit]) -> list[str]:
    ranges = ", ".join(
        [f"**{_format_range(r)}**" for r in entry.added]
        + [f"base **{_format_range(r)}**" for r in entry.deleted]
    )
    commit = commits.get(entry.sha) if entry.sha is not None else None
    if commit is None:
        return [
            f"- {ranges} · commit unknown",
            "  - Why: git can’t tell which commit made this change.",
        ]

    label = _inferred_label(commit.changes_inferred_by)
    lines = [
        f"- {ranges} · `{commit.short_sha}` · {_escape_inline(one_line(commit.what))}"
    ]
    for change in entry.changes or []:
        lines.append(f"  - What: {one_line(change.what)}")
        lines += _render_why(label, change.why, "  ")
    return lines


def _describe_author(commit: BuildCommit) -> str:
    if commit.agent == "claude-code":
        model = f" ({commit.agent_model})" if commit.agent_model is not None else ""
        return f"claude-code{model}"
    return f"human ({_escape_inline(commit.author)})"


# A list item or a blank line; ``^`` as JavaScript's multiline ``^``, which
# also matches after ``\r``, U+2028 and U+2029
_STRUCTURED_RE = re.compile(
    f"(?:^|(?<=[\n\r\u2028\u2029])){JS_SPACE_CLASS}*[-*] |\n{JS_SPACE_CLASS}*\n"
)


def _render_why(label: str, why: str, indent: str) -> list[str]:
    """The Why as list items

    Hard-wrapped prose joins into one line; a Why with its own lists or
    paragraphs keeps them, indented under the label so it stays part of the
    entry.
    """
    text = js_trim(why)
    if not _STRUCTURED_RE.search(text):
        return [f"{indent}- {label}: {one_line(text)}"]

    return [f"{indent}- {label}:"] + [
        "" if js_trim(line) == "" else f"{indent}  {js_trim(line)}"
        for line in text.split("\n")
    ]


def _yaml_revision(key: str, revision: BuildRevision) -> list[str]:
    ref = yaml_string(revision.ref) if revision.ref is not None else "null"
    return [f"{key}:", f"  sha: {revision.sha}", f"  ref: {ref}"]


def _yaml_commit(commit: BuildCommit) -> list[str]:
    lines = [f"  - sha: {commit.sha}"]
    if commit.patch_id is not None:
        lines.append(f"    patch_id: {commit.patch_id}")
    lines += [
        f"    author: {yaml_string(commit.author)}",
        f"    agent: {commit.agent}",
    ]
    if commit.agent_model is not None:
        lines.append(f"    agent_model: {yaml_string(commit.agent_model)}")
    lines.append(f"    why_source: {commit.why_source}")
    if commit.inferred_by is not None:
        lines.append(f"    inferred_by: {yaml_string(commit.inferred_by)}")
    return lines


_JSON_ESCAPES = {
    '"': '\\"',
    "\\": "\\\\",
    "\b": "\\b",
    "\f": "\\f",
    "\n": "\\n",
    "\r": "\\r",
    "\t": "\\t",
}
_JSON_ESCAPE_RE = re.compile('[\x00-\x1f"\\\\\ud800-\udfff]')


def yaml_string(value: str) -> str:
    """``JSON.stringify(value)``, which is a valid YAML double-quoted scalar

    Non-ASCII stays as is; control characters and lone surrogates are
    escaped, exactly as JavaScript does.
    """
    return (
        '"'
        + _JSON_ESCAPE_RE.sub(
            lambda m: _JSON_ESCAPES.get(m.group(), f"\\u{ord(m.group()):04x}"), value
        )
        + '"'
    )


def _describe_revision(revision: BuildRevision) -> str:
    ref = f" ({_escape_inline(revision.ref)})" if revision.ref is not None else ""
    return f"`{revision.short_sha}`{ref}"


def _format_range(line_range: LineRange) -> str:
    if line_range.start == line_range.end:
        return f"L{line_range.start}"
    return f"L{line_range.start}{_EN_DASH}{line_range.end}"


_INLINE_RE = re.compile(r"([\\`*_\[\]|<>])")


def _escape_inline(text: str) -> str:
    """Keeps paths and names from being read as Markdown"""
    return _INLINE_RE.sub(r"\\\1", text)


# -- Header --


@dataclass
class HeaderRevision:
    sha: str | None = None
    ref: str | None = None


@dataclass
class HeaderCommit:
    sha: str
    patch_id: str | None = None


@dataclass
class BuildHeader:
    file_name: str
    title: str | None = None
    created: str | None = None
    build_model: str | None = None
    base: HeaderRevision = field(default_factory=HeaderRevision)
    target: HeaderRevision = field(default_factory=HeaderRevision)
    #: The commits the build covers, to find it again from a commit, also
    #: after a rebase (by patch ID)
    commits: list[HeaderCommit] = field(default_factory=list)


# ``.`` and ``$`` as JavaScript has them without the ``m`` flag: no line
# terminator matches ``.``, and ``$`` is the very end
_REST = "([^\n\r\u2028\u2029]*)"
_TOP_RE = re.compile(f"([a-z_]+):{JS_SPACE_CLASS}*{_REST}")
_COMMIT_SHA_RE = re.compile(
    f" {{2}}- sha:{JS_SPACE_CLASS}*([0-9a-f]+){JS_SPACE_CLASS}*"
)
_PATCH_ID_RE = re.compile(
    f" {{4}}patch_id:{JS_SPACE_CLASS}*([0-9a-f]+){JS_SPACE_CLASS}*"
)
_NESTED_RE = re.compile(f" {{2}}(sha|ref):{JS_SPACE_CLASS}*{_REST}")


def parse_build_header(file_name: str, text: str) -> BuildHeader:
    """Reads the index fields back from a build file's frontmatter

    Reads what :func:`render_build` writes; anything else is skipped, and a
    file without frontmatter yields a header with only its name.
    """
    header = BuildHeader(file_name)

    lines = text.split("\n")
    if lines[0] != "---":
        return header

    block: HeaderRevision | None = None
    for line in lines[1:]:
        if line == "---":
            break

        top = _TOP_RE.fullmatch(line)
        if top:
            key, value = top.groups()
            block = {"base": header.base, "target": header.target}.get(key)
            if key == "title":
                header.title = _yaml_value(value)
            elif key == "created":
                header.created = _yaml_value(value)
            elif key == "build_model":
                header.build_model = _yaml_value(value)
            continue

        commit_sha = _COMMIT_SHA_RE.fullmatch(line)
        if commit_sha:
            header.commits.append(HeaderCommit(commit_sha.group(1)))
            continue

        patch_id = _PATCH_ID_RE.fullmatch(line)
        if patch_id and header.commits:
            header.commits[-1].patch_id = patch_id.group(1)
            continue

        nested = _NESTED_RE.fullmatch(line)
        if nested and block is not None:
            setattr(block, nested.group(1), _yaml_value(nested.group(2)))
    return header


def _yaml_value(value: str) -> str | None:
    trimmed = js_trim(value)
    if trimmed in ("", "null"):
        return None
    if not trimmed.startswith('"'):
        return trimmed

    try:
        parsed = json.loads(trimmed)
    except ValueError:
        return None
    return parsed if isinstance(parsed, str) else None


# -- Index --

# Root collation order of the ASCII punctuation and symbols, which all sort
# before digits, and digits before letters
_PUNCTUATION_ORDER = " _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$"


def _collation_key(text: str) -> tuple[object, ...]:
    def primary(char: str) -> tuple[int, int, str]:
        if char.isalpha():
            return (3, 0, char.casefold())
        if char.isdigit():
            return (2, 0, char)
        position = _PUNCTUATION_ORDER.find(char)
        if position >= 0:
            return (0, position, char)
        return (1, ord(char), char)

    # Lowercase before uppercase when nothing else differs
    return (
        tuple(primary(char) for char in text),
        tuple(char.isupper() for char in text),
        text,
    )


def locale_compare(a: str, b: str) -> int:
    """An approximation of JavaScript's ``a.localeCompare(b)``

    Close enough to ICU's root collation for the ISO dates and file names it
    sorts: punctuation before digits before letters, case-insensitive first.
    """
    key_a, key_b = _collation_key(a), _collation_key(b)
    return (key_a > key_b) - (key_a < key_b)


def _newest_first(a: BuildHeader, b: BuildHeader) -> int:
    return locale_compare(b.created or "", a.created or "") or locale_compare(
        a.file_name, b.file_name
    )


#: Sort key for headers, newest first, then by file name
newest_first = functools.cmp_to_key(_newest_first)

_CELL_RE = re.compile(f"{JS_SPACE_CLASS}*\n{JS_SPACE_CLASS}*")
_BUILD_SUFFIX_RE = re.compile(r"_build\.md\Z")


def _cell(text: str) -> str:
    return _CELL_RE.sub(" ", text.replace("|", "\\|"))


def _side(revision: HeaderRevision) -> str:
    if revision.ref is not None:
        return revision.ref
    if revision.sha is not None:
        return revision.sha[:7]
    return "?"


def render_index(headers: list[BuildHeader]) -> str:
    """``index.md``: one row per build, newest first"""
    rows = []
    for header in sorted(headers, key=newest_first):
        name = _cell(_BUILD_SUFFIX_RE.sub("", header.file_name))
        sides = _cell(f"{_side(header.base)} → {_side(header.target)}")
        created = header.created[:10] if header.created is not None else ""
        rows.append(
            f"| [{name}]({header.file_name}) | {sides} | {created} "
            f"| {_cell(header.build_model or '')} | {_cell(header.title or '')} |"
        )

    return "\n".join(
        [
            "# AI Lens builds",
            "",
            (
                "Each build explains why the code changed between two commits: what every "
                "commit did, why, and which lines"
            ),
            (
                "it touched. Read the build that covers the code you are about to change. "
                "Newest first."
            ),
            "",
            "| Build | Base → Target | Created | Model | Summary |",
            "| ----- | ------------- | ------- | ----- | ------- |",
            *rows,
            "",
        ]
    )
