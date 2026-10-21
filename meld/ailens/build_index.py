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

"""Finding the build files in a repository's build folder"""

import logging
import os
import re

from meld.ailens.build_file import BuildHeader, newest_first, parse_build_header
from meld.ailens.build_narrative import js_trim
from meld.ailens.models import AiLensError

log = logging.getLogger(__name__)

#: Used when no build folder is configured
DEFAULT_BUILD_FOLDER = "agent"


def read_build_headers(folder: str) -> list[BuildHeader]:
    """Headers of every ``*_build.md`` in the folder, in file-name order

    None when the folder doesn't exist. A build file that can't be read is
    left out (and logged) rather than failing the whole list; invalid UTF-8
    is replaced, as AI Lens reads it.
    """
    try:
        names = sorted(
            name for name in os.listdir(folder) if name.endswith("_build.md")
        )
    except FileNotFoundError:
        return []
    except OSError as err:
        raise AiLensError(
            f"Could not read the build folder {folder}: {err.strerror or err}"
        ) from err

    headers = []
    for name in names:
        path = os.path.join(folder, name)
        if not os.path.isfile(path):
            continue
        try:
            with open(path, encoding="utf-8", errors="replace", newline="") as f:
                text = f.read()
        except OSError as err:
            log.warning("Skipping unreadable build file %s: %s", path, err)
            continue
        headers.append(parse_build_header(name, text))
    return headers


def find_builds(
    headers: list[BuildHeader], sha: str, patch_id: str | None
) -> list[str]:
    """File names of the builds that cover a commit, newest first

    By SHA, or by patch ID when the commit was rebased or cherry-picked since
    (same change, new SHA). Squashed commits have a new patch and aren't
    found.
    """
    found = [
        header
        for header in headers
        if any(
            commit.sha == sha or (patch_id is not None and commit.patch_id == patch_id)
            for commit in header.commits
        )
    ]
    return [header.file_name for header in sorted(found, key=newest_first)]


_BUILD_FILE_NAME_RE = re.compile(r"[^/\\]+_build\.md")


def is_build_file_name(name: str) -> bool:
    """Whether ``name`` is a plain build file name, safe to join to the folder"""
    return bool(_BUILD_FILE_NAME_RE.fullmatch(name)) and not name.startswith(".")


def resolve_build_folder(repo_root: str, folder: str) -> str:
    """The build folder inside the repository

    ``folder`` is relative to ``repo_root``; empty means
    :data:`DEFAULT_BUILD_FOLDER`. Refuses absolute paths, paths that climb
    out with ``..``, and paths that leave the repository through a symlink.
    """
    relative = os.path.normpath(js_trim(folder) or DEFAULT_BUILD_FOLDER)
    error = AiLensError(
        f'The build folder "{folder}" must be a folder inside the repository.'
    )
    if (
        os.path.isabs(relative)
        or relative == ".."
        or relative.startswith(("../", "..\\"))
    ):
        raise error

    path = os.path.normpath(os.path.join(repo_root, relative))
    # Symlinks in the existing part of the path must stay inside, too
    real_root = os.path.realpath(repo_root)
    real_path = os.path.realpath(path)
    if os.path.commonpath([real_root, real_path]) != real_root:
        raise error
    return path
