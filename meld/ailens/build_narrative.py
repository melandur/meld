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

"""The written part of a build: the one ``claude -p`` call

The model sees each commit's own explanation and the list of changed files,
never the code.
"""

import dataclasses
import re
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any

from meld.ailens.build_facts import BuildCommit, BuildFacts, BuildRevision
from meld.ailens.claude import ClaudeRequest, ClaudeResponse, run_claude
from meld.ailens.models import AiLensError
from meld.ailens.process import CancelToken

#: :func:`run_claude`, or a stand-in for tests
RunClaude = Callable[..., ClaudeResponse]

#: Bumped whenever the prompt or schema changes, so older builds can be told
#: apart
BUILD_PROMPT_VERSION = 1

# Keep the prompt a reasonable size on long ranges; what is cut is said in
# the prompt
MAX_COMMITS = 400
MAX_FILES = 400
MAX_WHY_CHARS = 600

SYSTEM_PROMPT = """\
You write the summary of a range of code changes, for engineers and AI coding agents who later
need to know why the code is the way it is. You get each commit's own explanation and the changed files; you
never see the code. Reply with JSON matching the schema:
- "title": the range's main change in at most 10 words.
- "summary": 2 to 5 plain sentences: what the range does as a whole and why.
- "decisions": design choices the commits made, each with its reason and the short SHAs (from the list) of the
  commits it came from. Only choices the explanations support; an empty list is fine.
- "risks": anything unfinished, risky or unexplained, such as commits whose reason is only inferred or missing.
Use only what you are given. Plain text, no markdown."""

SCHEMA: dict[str, Any] = {
    "type": "object",
    "properties": {
        "title": {"type": "string"},
        "summary": {"type": "string"},
        "decisions": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "text": {"type": "string"},
                    "commits": {"type": "array", "items": {"type": "string"}},
                },
                "required": ["text", "commits"],
                "additionalProperties": False,
            },
        },
        "risks": {"type": "array", "items": {"type": "string"}},
    },
    "required": ["title", "summary", "decisions", "risks"],
    "additionalProperties": False,
}


@dataclass(frozen=True)
class BuildDecision:
    text: str
    #: Short SHAs of the commits it came from; only commits in the range
    commits: list[str] = field(default_factory=list)


@dataclass(frozen=True)
class BuildNarrative:
    #: At most a short line; also the build's row in ``index.md``
    title: str
    summary: str
    decisions: list[BuildDecision]
    risks: list[str]
    #: The model that wrote it
    model: str


# -- Text helpers --
#
# Build files must come out byte-identical to the ones AI Lens writes, so
# these follow JavaScript's rules: its ``\s`` and ``trim()`` know a different
# set of whitespace than Python's ``str.isspace()``.

#: JavaScript's WhiteSpace and LineTerminator characters
JS_SPACE = "\t\n\v\f\r \u00a0\u1680\u2000\u2001\u2002\u2003\u2004\u2005\u2006\u2007\u2008\u2009\u200a\u2028\u2029\u202f\u205f\u3000\ufeff"
#: As a regex character class
JS_SPACE_CLASS = "[" + re.escape(JS_SPACE) + "]"

_ONE_LINE_RE = re.compile(f"{JS_SPACE_CLASS}*\n{JS_SPACE_CLASS}*")


def js_trim(text: str) -> str:
    """``String.prototype.trim``"""
    return text.strip(JS_SPACE)


def js_trim_end(text: str) -> str:
    """``String.prototype.trimEnd``"""
    return text.rstrip(JS_SPACE)


def one_line(text: str) -> str:
    """Joins hard-wrapped lines into one"""
    return js_trim(_ONE_LINE_RE.sub(" ", text))


def truncate(text: str, max_length: int) -> str:
    """Cuts ``text`` to ``max_length`` UTF-16 code units, ending in ``…``

    Lengths count UTF-16 code units, as JavaScript's do, so prompts match AI
    Lens'. A surrogate pair the cut would split is dropped whole.
    """
    units = text.encode("utf-16-le", "surrogatepass")
    if len(units) // 2 <= max_length:
        return text
    kept = units[: (max_length - 1) * 2]
    # Drop a trailing high surrogate left without its low half
    if len(kept) >= 2 and 0xD800 <= int.from_bytes(kept[-2:], "little") <= 0xDBFF:
        kept = kept[:-2]
    return kept.decode("utf-16-le", "surrogatepass") + "…"


# -- Prompt --


def build_narrative_prompt(facts: BuildFacts) -> str:
    commits = facts.commits[:MAX_COMMITS]
    files = facts.files[:MAX_FILES]
    short_shas = {commit.sha: commit.short_sha for commit in facts.commits}

    range_line = (
        f"Range: {_describe_revision(facts.base)} → {_describe_revision(facts.target)}"
    )
    if facts.relation == "diverged":
        range_line += " (diverged branches; changes since their merge base)"
    stats = facts.stats
    lines = [
        range_line,
        (
            f"{stats.commits} commits, {stats.files} files, "
            f"+{stats.insertions} \u2212{stats.deletions} lines"
        ),
        "",
        "Commits, oldest first:",
        *(_describe_commit(commit) for commit in commits),
    ]
    if len(facts.commits) > len(commits):
        lines.append(f"({len(facts.commits) - len(commits)} more commits left out)")
    lines += ["", "Changed files:"]
    for file in files:
        # Unique, in order of first appearance
        by = list(
            dict.fromkeys(
                short_shas.get(entry.sha, entry.sha)
                for entry in file.entries
                if entry.sha is not None
            )
        )
        original = f"{file.original_path} -> " if file.original_path is not None else ""
        if file.ignored:
            size = "left out (generated or ignored)"
        elif file.binary:
            size = "binary"
        else:
            size = f"+{file.additions or 0} \u2212{file.deletions or 0}"
        parts = [f"{file.status} {original}{file.path}", size]
        if by:
            parts.append(f"by {', '.join(by)}")
        lines.append(" · ".join(parts))
    if len(facts.files) > len(files):
        lines.append(f"({len(facts.files) - len(files)} more files left out)")
    return "\n".join(lines)


def _describe_revision(revision: BuildRevision) -> str:
    if revision.ref is not None:
        return f"{revision.ref} ({revision.short_sha})"
    return revision.short_sha


def _describe_commit(commit: BuildCommit) -> str:
    if commit.agent == "claude-code":
        name = commit.agent_name if commit.agent_name is not None else "Claude"
        who = f"{name} with {commit.author}"
    else:
        who = commit.author
    if commit.why is None:
        why = "Why: not recorded"
    else:
        label = (
            "Why (inferred, not the author’s words)"
            if commit.why_source == "inferred"
            else "Why"
        )
        why = f"{label}: {truncate(one_line(commit.why), MAX_WHY_CHARS)}"
    return f"- {commit.short_sha} · {who} · What: {one_line(commit.what)} · {why}"


# -- Reply --


def parse_narrative(
    output: object, facts: BuildFacts, model: str = ""
) -> BuildNarrative:
    """Validates the model's answer; decisions may only cite commits in the range

    ``model`` is copied into the result as is.
    """

    def get(value: object, name: str) -> object:
        return value.get(name) if isinstance(value, dict) else None

    title = get(output, "title")
    summary = get(output, "summary")
    decisions = get(output, "decisions")
    risks = get(output, "risks")
    if (
        not isinstance(title, str)
        or not isinstance(summary, str)
        or not isinstance(decisions, list)
        or not isinstance(risks, list)
    ):
        raise AiLensError("Claude Code's build summary was missing parts.")

    known = {commit.short_sha for commit in facts.commits}

    def strings(values: list[object]) -> list[str]:
        return [
            one_line(value)
            for value in values
            if isinstance(value, str) and js_trim(value) != ""
        ]

    parsed_decisions: list[BuildDecision] = []
    for decision in decisions:
        text = get(decision, "text")
        cited = get(decision, "commits")
        if not isinstance(text, str) or js_trim(text) == "":
            continue
        commits = (
            [sha for sha in strings(cited) if sha in known]
            if isinstance(cited, list)
            else []
        )
        parsed_decisions.append(BuildDecision(one_line(text), commits))

    return BuildNarrative(
        title=one_line(title) or "Changes",
        summary=js_trim(summary),
        decisions=parsed_decisions,
        risks=strings(risks),
        model=model,
    )


def write_narrative(
    facts: BuildFacts,
    model: str,
    claude_path: str | None = None,
    cancel: CancelToken | None = None,
    run: RunClaude | None = None,
) -> BuildNarrative:
    """Asks Claude Code for the build's title, summary, decisions and risks

    ``run`` replaces :func:`meld.ailens.claude.run_claude`, for tests; it is
    called as ``run(request, cancel=cancel)``. Blocks until Claude answers,
    so call it off the main thread. Raises :class:`AiLensError` (a
    ``ClaudeError`` when the call itself fails).
    """
    request = ClaudeRequest(
        prompt=build_narrative_prompt(facts),
        model=model,
        system_prompt=SYSTEM_PROMPT,
        json_schema=SCHEMA,
    )
    # Leave the client's default (``claude`` on PATH) alone otherwise
    if claude_path is not None:
        request = dataclasses.replace(request, claude_path=claude_path)
    response = (run or run_claude)(request, cancel=cancel)
    return parse_narrative(response.output, facts, response.model)
