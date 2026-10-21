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

"""What the Why column shows: one card per change group, without GTK"""

import re
from dataclasses import dataclass, field
from typing import Literal

from meld.ailens.models import ExplanationSource, LineRange

CardKind = Literal["commit", "staged", "unstaged", "unknown"]

#: What a card's explain button does: infer the commit's Why (and its
#: changes), add per-change explanations to a commit that has a Why, or
#: explain an uncommitted layer's hunks
ExplainAction = Literal["commit", "changes", "uncommitted"]


@dataclass(frozen=True)
class Card:
    #: Stable across reloads of the same file, e.g. to keep focus
    key: str
    kind: CardKind
    #: Lines in the target version (1-based, inclusive)
    added: tuple[LineRange, ...]
    #: Lines in the base version (1-based, inclusive)
    deleted: tuple[LineRange, ...]
    sha: str | None = None
    short_sha: str = ""
    subject: str = ""
    #: The whole commit message
    message: str = ""
    author: str = ""
    #: Author date, strict ISO 8601
    date: str = ""
    #: e.g. "Claude Opus 5.5"; empty for people
    agent_name: str = ""
    agent_model: str = ""
    what: str = ""
    why: str = ""
    source: ExplanationSource | None = None
    #: Model that inferred :attr:`why`, for ``inferred``
    inferred_by: str = ""
    #: What this change (not the whole commit) does and why; always inferred
    change_what: str = ""
    change_why: str = ""
    change_model: str = ""
    #: The explain button, if any
    explain: ExplainAction | None = None
    #: Index into the comparison's commits, oldest first, for its color;
    #: -1 for uncommitted and unknown changes
    color_index: int = -1
    #: Build files (names in the build folder) covering this commit
    builds: tuple[str, ...] = field(default=())

    @property
    def anchor(self) -> tuple[Literal["new", "old"], int]:
        """Where the card is level with: its first added line, else its first
        deleted line (in the base version)
        """
        if self.added:
            return ("new", min(r.start for r in self.added))
        if self.deleted:
            return ("old", min(r.start for r in self.deleted))
        return ("new", 1)

    @property
    def agent(self) -> bool:
        return bool(self.agent_name)


def layout_cards(
    tops: list[float], heights: list[float], gap: float = 4.0
) -> list[float]:
    """Positions for cards wanting to be at ``tops``, in order

    Cards are margin notes: each stays level with its change, pushed down
    only as far as needed to clear the card above it.
    """
    placed: list[float] = []
    bottom = float("-inf")
    for top, height in zip(tops, heights, strict=True):
        y = max(top, bottom + gap) if placed else top
        placed.append(y)
        bottom = y + height
    return placed


_LIST_ITEM = re.compile(r"^\s*(?:[-*+•]|\d+[.)])\s+")


def reflow(text: str) -> str:
    """Join hard-wrapped lines so labels can wrap them to fit

    Paragraphs (blank lines) and list items stay on lines of their own.
    """
    paragraphs = re.split(r"\n\s*\n", text.replace("\r\n", "\n").strip())
    result = []
    for paragraph in paragraphs:
        lines: list[str] = []
        for line in paragraph.split("\n"):
            stripped = line.strip()
            if not stripped:
                continue
            if lines and not _LIST_ITEM.match(line):
                lines[-1] += " " + stripped
            else:
                lines.append(stripped if not _LIST_ITEM.match(line) else line.rstrip())
        result.append("\n".join(lines))
    return "\n\n".join(p for p in result if p)
