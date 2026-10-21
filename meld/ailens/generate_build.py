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

"""Writing a build file and the folder's ``index.md``

The caller gathers the facts (see :mod:`meld.ailens.build_facts`) and asks
for the narrative (:func:`meld.ailens.build_narrative.write_narrative`);
this module only writes them. Builds cover commits only, so the caller must
not pass facts for an uncommitted target.
"""

import os
import re
import secrets
from dataclasses import dataclass
from datetime import datetime

from meld.ailens.build_facts import BuildFacts
from meld.ailens.build_file import build_file_name, render_build, render_index
from meld.ailens.build_index import (
    is_build_file_name,
    read_build_headers,
    resolve_build_folder,
)
from meld.ailens.build_narrative import BuildNarrative
from meld.ailens.models import AiLensError

INDEX_FILE_NAME = "index.md"


@dataclass(frozen=True)
class GeneratedBuild:
    path: str
    index_path: str
    facts: BuildFacts
    narrative: BuildNarrative


def write_build(
    repo_root: str,
    folder: str,
    facts: BuildFacts,
    narrative: BuildNarrative,
    generator: str,
    created: datetime,
) -> GeneratedBuild:
    """Writes ``<base>_<target>_build.md`` and rebuilds ``index.md``

    ``folder`` is the build folder relative to ``repo_root`` (see
    :func:`~meld.ailens.build_index.resolve_build_folder`); it is created if
    needed. ``generator`` names the writer, e.g. ``meld-ailens@3.23.0``. The
    same facts, narrative and ``created`` always give the same bytes; an
    existing build for the same pair is replaced.

    Each file is written to a temporary file and renamed into place, so a
    failure never leaves a half-written build. Raises :class:`AiLensError`
    when the folder can't be created or written.
    """
    folder_path = resolve_build_folder(repo_root, folder)
    name = build_file_name(facts)
    if not is_build_file_name(name):
        raise AiLensError(f"Cannot name a build file {name!r}.")

    try:
        os.makedirs(folder_path, exist_ok=True)
    except OSError as err:
        raise AiLensError(
            f"Could not create the build folder {folder_path}: {err.strerror or err}"
        ) from err

    path = os.path.join(folder_path, name)
    write_text_atomic(path, render_build(facts, narrative, created, generator))

    index_path = os.path.join(folder_path, INDEX_FILE_NAME)
    write_text_atomic(index_path, render_index(read_build_headers(folder_path)))

    return GeneratedBuild(path, index_path, facts, narrative)


_LONE_SURROGATE_RE = re.compile("[\ud800-\udfff]")


def _encode(text: str) -> bytes:
    """UTF-8, with bytes kept through ``surrogateescape`` written back as is

    Any other lone surrogate becomes U+FFFD, as Node writes it.
    """
    try:
        return text.encode("utf-8", "surrogateescape")
    except UnicodeEncodeError:
        return _LONE_SURROGATE_RE.sub("�", text).encode("utf-8")


def write_text_atomic(path: str, text: str) -> None:
    """Writes ``text`` as UTF-8 to ``path`` through a temporary file

    The temporary file sits next to ``path`` and is renamed over it, so
    readers see the old file or the new one, never a partial one. New files
    get the usual permissions (``0o666`` less the umask). Raises
    :class:`AiLensError` on failure.
    """
    directory, name = os.path.split(path)
    temp_path = os.path.join(
        directory, f".{name}.{os.getpid()}.{secrets.token_hex(4)}.tmp"
    )
    try:
        fd = os.open(temp_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o666)
        try:
            with os.fdopen(fd, "wb") as f:
                f.write(_encode(text))
                f.flush()
                os.fsync(f.fileno())
            os.replace(temp_path, path)
        except BaseException:
            try:
                os.unlink(temp_path)
            except OSError:
                pass
            raise
    except OSError as err:
        raise AiLensError(f"Could not write {path}: {err.strerror or err}") from err
