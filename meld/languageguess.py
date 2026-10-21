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

"""Guess syntax highlighting languages for compared files

GtkSourceView guesses a language from a file name and content type,
which fails for files without a useful name (e.g., blank comparisons,
version control temporary files or scripts without an extension). The
helpers here fill that gap from the text itself, by looking for editor
modelines, shebang lines and finally GIO content sniffing, and keep the
languages of the panes in a comparison consistent.

The parsing functions work on language IDs (strings), and the only
GtkSourceView dependency is the language manager passed in, so they
can be tested without a display.
"""

import os.path
import re
from collections.abc import Callable, Collection, Sequence
from typing import TypeVar

from gi.repository import Gio, GtkSource

#: Number of lines at the start and end of a file searched for
#: modelines; this matches Vim's default 'modelines' setting.
MODELINE_LINES = 5

#: Maximum number of bytes passed to GIO content type sniffing
CONTENT_SAMPLE_SIZE = 4096

# Editor mode and interpreter names that don't match GtkSourceView's
# language IDs. Anything not listed here is tried as an ID directly.
LANGUAGE_ALIASES = {
    "ash": "sh",
    "bash": "sh",
    "bats": "sh",
    "bun": "js",
    "c++": "cpp",
    "cperl": "perl",
    "cs": "c-sharp",
    "csharp": "c-sharp",
    "cxx": "cpp",
    "dash": "sh",
    "deno": "js",
    "dockerfile": "docker",
    "dosini": "ini",
    "emacs-lisp": "commonlisp",
    "escript": "erlang",
    "gawk": "awk",
    "gfm": "markdown",
    "gmake": "makefile",
    "golang": "go",
    "javascript": "js",
    "jsonc": "json",
    "ksh": "sh",
    "lisp": "commonlisp",
    "make": "makefile",
    "mawk": "awk",
    "md": "markdown",
    "mksh": "sh",
    "nawk": "awk",
    "node": "js",
    "nodejs": "js",
    "objective-c": "objc",
    "patch": "diff",
    "pwsh": "powershell",
    "ps1": "powershell",
    "py": "python3",
    "python": "python3",
    "python2": "python",
    "rb": "ruby",
    "rs": "rust",
    "rscript": "r",
    "runghc": "haskell",
    "runhaskell": "haskell",
    "shell": "sh",
    "shell-script": "sh",
    "tclsh": "tcl",
    "tex": "latex",
    "ts": "typescript",
    "wish": "tcl",
    "yml": "yaml",
    "zsh": "sh",
}

# Vim modelines look like "vim: set ft=python :" or "vi:syntax=c ts=4";
# we only care about the file type.
_VIM_MODELINE_RE = re.compile(
    r"(?:^|\s)(?:vi|vim|ex)(?:[<=>]?\d+)?:.*?"
    r"(?<![\w])(?:ft|filetype|syn|syntax)=(?P<name>[\w+.#-]+)"
)
# Emacs modelines look like "-*- mode: python; coding: utf-8 -*-" or
# simply "-*- python -*-"
_EMACS_MODELINE_RE = re.compile(r"-\*-(?P<vars>.*?)-\*-")
_EMACS_MODE_RE = re.compile(r"(?:^|;)\s*mode\s*:\s*(?P<name>[\w+.#-]+)", re.IGNORECASE)
_EMACS_LOCAL_MODE_RE = re.compile(r"\bmode\s*:\s*(?P<name>[\w+.#-]+)", re.IGNORECASE)
_SHEBANG_RE = re.compile(r"^#!\s*(?P<command>.*)$")
_VERSION_SUFFIX_RE = re.compile(r"[.-]?\d+(?:\.\d+)*$")
_MINOR_VERSION_SUFFIX_RE = re.compile(r"(?:\.\d+)+$")


def normalise_language_name(name: str, known_ids: Collection[str]) -> str | None:
    """Map an editor mode or interpreter name to a known language ID

    Names are matched case-insensitively, with Emacs' "-mode" suffix
    and version suffixes (e.g., "python3.12", "lua5.4") removed if the
    full name isn't recognised.
    """

    name = name.strip().lower()
    if name.endswith("-mode"):
        name = name.removesuffix("-mode")
    if not name:
        return None

    candidates = [name]
    without_minor = _MINOR_VERSION_SUFFIX_RE.sub("", name)
    without_version = _VERSION_SUFFIX_RE.sub("", name)
    for candidate in (without_minor, without_version):
        if candidate and candidate not in candidates:
            candidates.append(candidate)

    for candidate in candidates:
        lang_id = LANGUAGE_ALIASES.get(candidate, candidate)
        if lang_id in known_ids:
            return lang_id
    return None


def language_name_from_modeline(text: str) -> str | None:
    """Find a Vim or Emacs mode name in the text's modelines, if any"""

    lines = text.splitlines()
    head = lines[:MODELINE_LINES]
    tail = lines[-MODELINE_LINES:] if len(lines) > MODELINE_LINES else []

    for line in head + tail:
        match = _VIM_MODELINE_RE.search(line)
        if match:
            return match.group("name")

    # Emacs only reads its modeline from the first line, or from the
    # second if the first is a shebang.
    for line in head[:2]:
        match = _EMACS_MODELINE_RE.search(line)
        if not match:
            continue
        variables = match.group("vars")
        if ":" not in variables:
            return variables.strip() or None
        mode = _EMACS_MODE_RE.search(variables)
        if mode:
            return mode.group("name")

    # Emacs also supports a "Local Variables:" block near the end of
    # the file.
    in_local_variables = False
    for line in lines[-20:]:
        if "Local Variables:" in line:
            in_local_variables = True
        elif "End:" in line:
            in_local_variables = False
        elif in_local_variables:
            mode = _EMACS_LOCAL_MODE_RE.search(line)
            if mode:
                return mode.group("name")

    return None


def interpreter_from_shebang(line: str) -> str | None:
    """Get the interpreter name from a shebang line

    This handles plain paths ("#!/bin/bash"), arguments
    ("#!/usr/bin/perl -w") and env ("#!/usr/bin/env -S python3 -u").
    """

    match = _SHEBANG_RE.match(line.strip())
    if not match:
        return None

    words = match.group("command").split()
    if not words:
        return None

    interpreter = os.path.basename(words[0])
    if interpreter != "env":
        return interpreter or None

    # Skip env's options and environment assignments to find the
    # actual command
    args = iter(words[1:])
    for arg in args:
        if arg in ("-u", "--unset", "-C", "--chdir"):
            next(args, None)
        elif arg.startswith("-") or "=" in arg:
            continue
        else:
            return os.path.basename(arg) or None
    return None


def language_id_from_text(text: str, known_ids: Collection[str]) -> str | None:
    """Guess a language ID from modelines and shebangs in the text

    Modelines take precedence, since they're an explicit statement of
    the file type by the user.
    """

    mode = language_name_from_modeline(text)
    if mode:
        lang_id = normalise_language_name(mode, known_ids)
        if lang_id:
            return lang_id

    first_line = text.split("\n", 1)[0]
    interpreter = interpreter_from_shebang(first_line)
    if interpreter:
        return normalise_language_name(interpreter, known_ids)

    return None


def guess_language_from_text(
    text: str,
    manager: GtkSource.LanguageManager,
) -> GtkSource.Language | None:
    """Guess a language for text whose file name gave no answer"""

    if not text:
        return None

    known_ids = set(manager.get_language_ids() or ())
    lang_id = language_id_from_text(text, known_ids)
    if lang_id:
        return manager.get_language(lang_id)

    # Fall back to GIO's content sniffing (e.g., XML declarations)
    sample = text.encode("utf-8", errors="replace")[:CONTENT_SAMPLE_SIZE]
    content_type, _uncertain = Gio.content_type_guess(None, sample)
    if not content_type or Gio.content_type_equals(content_type, "text/plain"):
        return None
    return manager.guess_language(None, content_type)


T = TypeVar("T")


def choose_pane_languages(
    file_languages: Sequence[T | None],
    guess_from_text: Callable[[int], T | None],
) -> list[T | None]:
    """Decide on the language of each pane in a comparison

    :param file_languages: languages guessed from each pane's file
    :param guess_from_text: callable taking a pane index and guessing
        its language from the pane's text; it is only called for panes
        whose file gave no language, and only when needed

    If every pane whose file gave a language agrees on it, all panes
    use that language. Otherwise, panes without a file language are
    guessed from their text, and if all known languages then agree,
    that language is used for every pane. Panes with conflicting
    languages (e.g., a C file compared with a C header) keep their own.
    """

    def consensus(langs):
        known = [lang for lang in langs if lang]
        if known and all(lang == known[0] for lang in known):
            return known[0]
        return None

    langs = list(file_languages)
    agreed = consensus(langs)
    if agreed:
        return [agreed] * len(langs)

    langs = [lang or guess_from_text(i) for i, lang in enumerate(langs)]
    agreed = consensus(langs)
    if agreed:
        return [agreed] * len(langs)
    return langs
