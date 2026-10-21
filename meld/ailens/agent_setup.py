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

"""Agent instructions: a ``CLAUDE.md`` section and the ``/why`` skill

They point coding agents at the build folder, so they look up why code is
the way it is before changing it. The text is the same as AI Lens writes.
"""

import os
from dataclasses import dataclass

from meld.ailens.build_narrative import js_trim, js_trim_end
from meld.ailens.generate_build import write_text_atomic
from meld.ailens.models import AiLensError

SECTION_START = "<!-- ai-lens:start -->"
SECTION_END = "<!-- ai-lens:end -->"


@dataclass(frozen=True)
class AgentSetupOptions:
    #: Build folder relative to the repository root, e.g. ``agent``
    build_folder: str
    #: Notes ref name, e.g. ``ai-lens``
    notes_ref: str


@dataclass(frozen=True)
class AgentSetupFile:
    path: str
    content: str
    existed: bool
    changed: bool


def render_claude_md_section(options: AgentSetupOptions) -> str:
    """The section added to a repository's ``CLAUDE.md``

    It sits between markers so it can be updated in place.
    """
    folder = options.build_folder
    return "\n".join(
        [
            SECTION_START,
            "## Why the code is the way it is (AI Lens)",
            "",
            (
                f"`{folder}/` holds AI Lens builds: one file per compared commit range, "
                "saying what each commit changed, why,"
            ),
            f"and which lines it touched. `{folder}/index.md` lists them, newest first.",
            "",
            (
                "- Before changing code whose purpose isn't clear, find out why it is the "
                "way it is: run `/why <file>:<line>`,"
            ),
            f"  or read the build in `{folder}/index.md` that covers it.",
            (
                '- A "Why (inferred …)" is a model\'s reading of the diff, not the '
                "author's reason. Treat it as a guess."
            ),
            SECTION_END,
        ]
    )


def merge_claude_md(existing: str | None, section: str) -> str:
    """Adds the section to ``CLAUDE.md``'s text, or replaces the one there"""
    if existing is None or js_trim(existing) == "":
        return f"{section}\n"

    start = existing.find(SECTION_START)
    end = existing.find(SECTION_END, max(start, 0))
    if start >= 0 and end > start:
        return f"{existing[:start]}{section}{existing[end + len(SECTION_END) :]}"
    return f"{js_trim_end(existing)}\n\n{section}\n"


_WHY_SKILL = """\
---
name: why
description: Explain why a line or block of code is the way it is, from the AI Lens builds in {folder}/ and the git history. Use when asked why code is written a certain way, before changing code whose purpose is unclear, or as /why <file>:<line> or /why <file>:<start>-<end>.
---

# Why is this code the way it is?

Input: `<file>:<line>` or `<file>:<start>-<end>`. Ask for it if it is missing.

1. Find the commits that last changed those lines:
   `git blame --porcelain -L <start>,<end> -- <file>`
   Each line's header starts with the full commit SHA. Skip `0000000000000000000000000000000000000000`: that
   line isn't committed yet.
2. Find the builds that cover each commit:
   `grep -l "sha: <SHA>" {folder}/*_build.md`
   None found? The commit may have been rebased or cherry-picked since. Find it by its change instead:
   `git show <SHA> | git patch-id --stable` (the first word is the patch ID), then
   `grep -l "patch_id: <PATCH_ID>" {folder}/*_build.md`
3. In each build found, newest `created:` first, read:
   - under `## Changes`, the `### <file>` section and the entries for the commit (its short SHA in backticks)
     whose line ranges cover the code: when its changes were explained one by one, each entry has the What and
     Why of that change. Line numbers there are as of the build's target commit, so they may have moved.
   - under `## Commits`, the commit's own What and Why.
   - `## Summary`, `## Decisions` and `## Risks / open questions`, for the wider reason.
4. No build covers it? Read the commit itself: `git log -1 --format=%B <SHA>`, and its AI Lens note if it has
   one: `git notes --ref={notes_ref} show <SHA>`. A note's `Change: -<old> +<new> <path>` sections
   explain single hunks of the commit's own diff (`git show <SHA>`); use the one whose `+<start>,<count>`
   range holds the line's number in that commit (the second number in its blame header).
5. Answer in a few sentences: what the code does, why it was written this way, and where that reason comes
   from (build, commit message or note, with the commit's short SHA and the build's file name). A Why marked
   "inferred" is a model's guess, so say so. If no reason is recorded anywhere, say that plainly. Don't make
   one up.
"""


def render_why_skill(options: AgentSetupOptions) -> str:
    """A Claude Code project skill

    ``/why <file>:<line>`` explains code from the builds and the history.
    """
    # Not str.format(): the folder and ref are inserted verbatim, braces too
    return _WHY_SKILL.replace("{folder}", options.build_folder).replace(
        "{notes_ref}", options.notes_ref
    )


def plan_agent_setup(
    repo_root: str, options: AgentSetupOptions
) -> list[AgentSetupFile]:
    """What setup would write; nothing is written yet

    ``CLAUDE.md`` with the section, and the ``/why`` skill.
    """
    claude_md_path = os.path.join(repo_root, "CLAUDE.md")
    skill_path = os.path.join(repo_root, ".claude", "skills", "why", "SKILL.md")
    claude_md = _read_if_exists(claude_md_path)
    skill = _read_if_exists(skill_path)

    planned = [
        (
            claude_md_path,
            claude_md,
            merge_claude_md(claude_md, render_claude_md_section(options)),
        ),
        (skill_path, skill, render_why_skill(options)),
    ]
    return [
        AgentSetupFile(
            path=path,
            content=content,
            existed=existing is not None,
            changed=existing != content,
        )
        for path, existing, content in planned
    ]


def write_agent_setup(files: list[AgentSetupFile]) -> None:
    """Writes the planned files that change, each atomically"""
    for file in files:
        if not file.changed:
            continue
        directory = os.path.dirname(file.path)
        try:
            os.makedirs(directory, exist_ok=True)
        except OSError as err:
            raise AiLensError(
                f"Could not create {directory}: {err.strerror or err}"
            ) from err
        write_text_atomic(file.path, file.content)


def _read_if_exists(path: str) -> str | None:
    # surrogateescape keeps bytes that aren't UTF-8, so writing the file
    # back leaves the rest of it untouched
    try:
        with open(path, encoding="utf-8", errors="surrogateescape", newline="") as f:
            return f.read()
    except (FileNotFoundError, NotADirectoryError):
        # A file where a folder on the path should be: writing will say so
        return None
    except OSError as err:
        raise AiLensError(f"Could not read {path}: {err.strerror or err}") from err
