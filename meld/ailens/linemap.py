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

"""Map line numbers between the two versions of a file diff

Lines outside the hunks move by the lines added and deleted above them.
"""

from meld.ailens.models import GitFileDiff


def new_to_old(diff: GitFileDiff, line: int) -> int | None:
    """Where line ``line`` of the new version was in the old one

    None when the diff added it.
    """
    shift = 0
    for hunk in diff.hunks:
        # A hunk without new lines sits after line new_start
        start = hunk.new_start + 1 if hunk.new_count == 0 else hunk.new_start
        if line < start:
            break
        if line < start + hunk.new_count:
            for candidate in hunk.lines:
                if candidate.type != "delete" and candidate.new_line == line:
                    return candidate.old_line if candidate.type == "context" else None
            return None
        shift += hunk.old_count - hunk.new_count
    return line + shift


def old_to_new(diff: GitFileDiff, line: int) -> int | None:
    """Where line ``line`` of the old version is in the new one

    None when the diff deleted it.
    """
    shift = 0
    for hunk in diff.hunks:
        # A hunk without old lines sits after line old_start
        start = hunk.old_start + 1 if hunk.old_count == 0 else hunk.old_start
        if line < start:
            break
        if line < start + hunk.old_count:
            for candidate in hunk.lines:
                if candidate.type != "add" and candidate.old_line == line:
                    return candidate.new_line if candidate.type == "context" else None
            return None
        shift += hunk.new_count - hunk.old_count
    return line + shift
