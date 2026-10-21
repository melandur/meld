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

"""Diff colour palettes that override the style scheme's chunk colours

Meld's chunk colours normally come from the ``meld:*`` styles of the
current GtkSourceView style scheme. The palettes here replace the
insert, delete and replace colours (plus the inline highlight) with
fixed sets so that comparisons can look like other diff tools.

Everything in this module is pure data and selection logic, so that it
can be tested without a display; :mod:`meld.style` converts the
results into ``Gdk.RGBA`` values.
"""

import enum
from dataclasses import dataclass


class DiffColours(enum.StrEnum):
    """Values of the ``diff-colors`` setting"""

    #: Use the style scheme's own ``meld:*`` colours
    scheme = "scheme"
    #: Red for removed and green for added text, like GitHub or GitLens
    red_green = "red-green"
    #: Orange for removed and blue for added text
    colorblind = "colorblind"


class DiffSide(enum.StrEnum):
    """Which side of a two-way comparison a pane shows"""

    old = "old"
    new = "new"


@dataclass(frozen=True)
class DiffTone:
    """Colours for one kind of change

    The fill is the chunk background, the line is the chunk outline
    (and the colour used for zero-height chunks) and the inline colour
    highlights changed words within a replaced chunk.
    """

    fill: str
    line: str
    inline: str


@dataclass(frozen=True)
class DiffPalette:
    """Chunk colours for a single pane or side-less widget"""

    insert: DiffTone
    delete: DiffTone
    replace: DiffTone

    @property
    def inline(self) -> str:
        # Inline highlighting only ever happens within replace chunks
        return self.replace.inline

    def tone(self, tag: str) -> DiffTone:
        return getattr(self, tag)


#: Chunk tags whose colours a palette overrides; conflict and error
#: colours always come from the style scheme.
PALETTE_TAGS = ("insert", "delete", "replace")


@dataclass(frozen=True)
class _ToneSet:
    removed: DiffTone
    added: DiffTone
    neutral: DiffTone


# Light colours follow GitHub's diff view; dark colours are opaque
# equivalents that keep enough contrast against dark editor
# backgrounds, since chunk fills are drawn underneath the text.
_TONES: dict[tuple[DiffColours, bool], _ToneSet] = {
    (DiffColours.red_green, False): _ToneSet(
        removed=DiffTone(fill="#ffebe9", line="#ff8182", inline="#ffc0bd"),
        added=DiffTone(fill="#e6ffec", line="#4ac26b", inline="#abf2bc"),
        neutral=DiffTone(fill="#ddf4ff", line="#54aeff", inline="#b6e3ff"),
    ),
    (DiffColours.red_green, True): _ToneSet(
        removed=DiffTone(fill="#4a1d1f", line="#f85149", inline="#7d2a2c"),
        added=DiffTone(fill="#173d22", line="#3fb950", inline="#1f6a33"),
        neutral=DiffTone(fill="#12304f", line="#388bfd", inline="#1f4f80"),
    ),
    # Colours from the Okabe-Ito palette, which stays distinguishable
    # for the common forms of colour vision deficiency. The neutral
    # tone is a low-saturation grey so that it can't be confused with
    # either side.
    (DiffColours.colorblind, False): _ToneSet(
        removed=DiffTone(fill="#fff1e0", line="#e69f00", inline="#ffd29e"),
        added=DiffTone(fill="#e3f0fb", line="#0072b2", inline="#b3d7f2"),
        neutral=DiffTone(fill="#ebe8f2", line="#8e86a8", inline="#d3cde3"),
    ),
    (DiffColours.colorblind, True): _ToneSet(
        removed=DiffTone(fill="#4a3110", line="#e69f00", inline="#7a4f12"),
        added=DiffTone(fill="#0f3550", line="#56b4e9", inline="#1a5a85"),
        neutral=DiffTone(fill="#34313d", line="#8e86a8", inline="#4d4860"),
    ),
}


def parse_diff_colours(value: object) -> DiffColours:
    """Convert a setting value to a palette, defaulting to the scheme"""

    try:
        return DiffColours(value)
    except (TypeError, ValueError):
        return DiffColours.scheme


def parse_diff_side(value: object) -> DiffSide | None:
    """Convert a side name to a side, with anything unknown as no side"""

    try:
        return DiffSide(value)
    except (TypeError, ValueError):
        return None


def get_diff_palette(
    palette: str,
    dark: bool,
    side: str | None = None,
) -> DiffPalette | None:
    """Get the chunk colours for a palette, or None for scheme colours

    :param palette: one of the :class:`DiffColours` values; anything
        unrecognised is treated as ``scheme``
    :param dark: whether the dark variant of the palette is wanted
    :param side: :class:`DiffSide` of the pane being coloured, or None
        for panes and widgets that don't belong to one side of a
        two-way comparison (e.g., three-way comparisons, link maps)

    Panes on the old side colour every change as removed text, and
    panes on the new side as added text. Side-less widgets get a
    neutral variant, where inserts are added text, deletes are removed
    text and replacements get a neutral colour. Note that Meld's insert
    and delete tags are relative to the pane a chunk is viewed from, so
    callers drawing chunks from the new side's perspective need to swap
    insert and delete.
    """

    tones = _TONES.get((parse_diff_colours(palette), bool(dark)))
    if tones is None:
        return None

    side = parse_diff_side(side)
    if side == DiffSide.old:
        tone = tones.removed
        return DiffPalette(insert=tone, delete=tone, replace=tone)
    elif side == DiffSide.new:
        tone = tones.added
        return DiffPalette(insert=tone, delete=tone, replace=tone)

    return DiffPalette(
        insert=tones.added,
        delete=tones.removed,
        replace=tones.neutral,
    )
