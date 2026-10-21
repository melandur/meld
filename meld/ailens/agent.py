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

"""Which commits an agent wrote, and with which model"""

import functools
import logging
import re

from meld.ailens.models import CommitAgent, GitCommit

log = logging.getLogger(__name__)

_ANTHROPIC_EMAIL = re.compile(r"<noreply@anthropic\.com>\Z", re.IGNORECASE)
_CLAUDE_NAME = re.compile(r"(Claude(?:\s+[^<]*?)?)\s*<[^>]*>", re.IGNORECASE)
_CLAUDE_WORD = re.compile(r"claude\b", re.IGNORECASE | re.ASCII)
#: A trailer value's name, before its ``<email>``
_NAME = re.compile(r"\s*([^<]*?)\s*(?:<[^>]*>)?\s*")


def detect_agent(
    commit: GitCommit, author_pattern: str | None = None
) -> CommitAgent | None:
    """The Claude Code agent behind a commit

    Found from its ``Co-Authored-By: Claude …`` trailer, or a Claude author.
    ``author_pattern`` is an extra regular expression (the user's setting)
    searched in the author's and co-authors' ``Name <email>``; a pattern
    that doesn't compile is ignored.
    """
    co_authors = [
        trailer.value
        for trailer in commit.trailers
        if trailer.key.lower() == "co-authored-by"
    ]
    for value in co_authors:
        match = _CLAUDE_NAME.fullmatch(value)
        if match is None or not _ANTHROPIC_EMAIL.search(value):
            continue
        return _to_agent(match.group(1).strip())

    author = commit.author
    if _CLAUDE_WORD.match(author.name) and _ANTHROPIC_EMAIL.search(f"<{author.email}>"):
        return _to_agent(author.name.strip())

    pattern = _compile(author_pattern) if author_pattern else None
    if pattern is None:
        return None

    identity = f"{author.name} <{author.email}>"
    if pattern.search(identity):
        return _to_agent(author.name.strip() or identity)
    for value in co_authors:
        if pattern.search(value):
            name = _NAME.fullmatch(value)
            return _to_agent((name.group(1) if name else "") or value.strip())
    return None


@functools.lru_cache(maxsize=16)
def _compile(pattern: str) -> re.Pattern[str] | None:
    try:
        return re.compile(pattern)
    except re.error as err:
        log.warning("Ignoring invalid agent author pattern %r: %s", pattern, err)
        return None


def _to_agent(display_name: str) -> CommitAgent:
    # Only Claude's own names spell a model: "Claude Opus 5.5" → claude-opus-5-5
    words = display_name.split()[1:] if _CLAUDE_WORD.match(display_name) else []
    model = "-".join(["claude", *words]).lower().replace(".", "-") if words else None
    return CommitAgent(kind="claude-code", display_name=display_name, model=model)
