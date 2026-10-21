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

"""Which files are never sent to a model: ``.ailensignore`` and generated files

``.ailensignore`` uses gitignore syntax. Matching follows git (and the npm
``ignore`` package AI Lens uses): the last matching pattern wins, ``!``
re-includes, a trailing ``/`` matches only directories, a pattern with a
slash in it is anchored to the repository root, ``**`` spans directories,
and a file inside an ignored directory can't be re-included. Like
``ignore``, matching is case-insensitive.
"""

import re
from collections.abc import Iterable
from dataclasses import dataclass

#: Never sent to a model or detailed in a build: lockfiles and generated or
#: minified files, which explain nothing
GENERATED_PATTERNS: tuple[str, ...] = (
    "*.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "npm-shrinkwrap.json",
    "*.min.js",
    "*.min.css",
    "*.map",
    "node_modules/",
)

#: Classes git's wildmatch accepts in brackets, e.g. ``[[:digit:]]``
_POSIX_CLASSES = {
    "alnum": "a-zA-Z0-9",
    "alpha": "a-zA-Z",
    "blank": " \\t",
    "cntrl": "\\x00-\\x1f\\x7f",
    "digit": "0-9",
    "graph": "\\x21-\\x7e",
    "lower": "a-z",
    "print": "\\x20-\\x7e",
    "punct": "!-/:-@\\[-`{-~",
    "space": " \\t\\n\\r\\f\\v",
    "upper": "A-Z",
    "xdigit": "0-9a-fA-F",
}


@dataclass(frozen=True)
class _Rule:
    pattern: str
    negative: bool
    #: Matches only directories (the pattern ended with ``/``)
    directory: bool
    #: Matched against the whole path (without its trailing slash)
    regex: re.Pattern[str]


def _trim_end(body: str) -> str:
    """Drop trailing spaces, keeping one escaped by a backslash"""
    body = body.rstrip("\r\n")
    trimmed = body.rstrip(" ")
    if trimmed == body:
        return body
    backslashes = len(trimmed) - len(trimmed.rstrip("\\"))
    if backslashes % 2:
        return trimmed[:-1] + " "
    return trimmed


def _ends_in_escape(body: str) -> bool:
    """Whether ``body`` ends with an unescaped backslash, which git rejects"""
    return (len(body) - len(body.rstrip("\\"))) % 2 == 1


def _bracket(glob: str, start: int) -> tuple[str, int] | None:
    """The regex for the bracket expression at ``glob[start]``

    Returns it and the index after the closing ``]``, or None when the
    bracket isn't closed.
    """
    i = start + 1
    negated = i < len(glob) and glob[i] in "!^"
    if negated:
        i += 1

    members: list[str] = []
    first = True
    while i < len(glob):
        char = glob[i]
        if char == "]" and not first:
            break
        first = False

        if char == "[" and glob.startswith("[:", i):
            end = glob.find(":]", i + 2)
            name = glob[i + 2 : end] if end != -1 else ""
            if name in _POSIX_CLASSES:
                members.append(_POSIX_CLASSES[name])
                i = end + 2
                continue

        if char == "\\" and i + 1 < len(glob):
            i += 1
            char = glob[i]
        low = char
        i += 1

        # A range, unless the dash is the last member
        if i + 1 < len(glob) and glob[i] == "-" and glob[i + 1] != "]":
            high = glob[i + 1]
            i += 2
            if high == "\\" and i < len(glob):
                high = glob[i]
                i += 1
            # Of a reversed range, git's wildmatch matches only its start
            if low <= high:
                members.append(f"{re.escape(low)}-{re.escape(high)}")
            else:
                members.append(re.escape(low))
            continue
        members.append(re.escape(low))
    else:
        return None

    body = "".join(members)
    if negated:
        # Like ``*`` and ``?``, a bracket never matches a slash
        return f"[^/{body}]", i + 1
    if not body:
        return "(?!)", i + 1
    return f"(?!/)[{body}]", i + 1


def _glob_regex(glob: str) -> str | None:
    """Regex source for a gitignore glob, matched against a whole path

    None when the glob can never match: git gives up on a bracket that
    isn't closed.
    """
    out: list[str] = []
    i = 0
    while i < len(glob):
        char = glob[i]
        if char == "*":
            end = i
            while end < len(glob) and glob[end] == "*":
                end += 1
            after_slash = i == 0 or glob[i - 1] == "/"
            before_slash = end == len(glob) or glob[end] == "/"
            if end - i >= 2 and after_slash and before_slash:
                if end == len(glob):
                    # ``a/**`` is everything inside ``a``; ``**`` everything
                    out.append(".+" if i > 0 else ".*")
                    i = end
                else:
                    # ``**/`` is zero or more directories
                    out.append("(?:.*/)?")
                    i = end + 1
            else:
                out.append("[^/]*")
                i = end
        elif char == "?":
            out.append("[^/]")
            i += 1
        elif char == "[":
            bracket = _bracket(glob, i)
            if bracket is None:
                return None
            out.append(bracket[0])
            i = bracket[1]
        elif char == "\\" and i + 1 < len(glob):
            out.append(re.escape(glob[i + 1]))
            i += 2
        else:
            out.append(re.escape(char))
            i += 1
    return "".join(out)


def _parse_rule(pattern: str) -> _Rule | None:
    body = pattern.removeprefix("﻿")
    if not body or body.startswith("#") or _ends_in_escape(body):
        return None

    negative = body.startswith("!")
    if negative:
        body = body[1:]
    if body.endswith((" ", "\r", "\n")):
        body = _trim_end(body)
    if not body:
        return None

    # A leading "\!" or "\#" is a literal "!" or "#"
    if body.startswith(("\\!", "\\#")):
        body = body[1:]

    directory = body.endswith("/")
    if directory:
        body = body[:-1]
        if not body:
            return None
    # A slash at the start or in the middle anchors the pattern to the root;
    # otherwise it matches a name at any depth
    anchored = "/" in body
    body = body.removeprefix("/")
    source = _glob_regex(body)
    if source is None:
        return None
    if not anchored:
        source = f"(?:.*/)?{source}"

    try:
        regex = re.compile(source, re.IGNORECASE | re.DOTALL)
    except re.error:
        return None
    return _Rule(pattern, negative, directory, regex)


class IgnoreRules:
    """A list of gitignore patterns; later patterns override earlier ones"""

    def __init__(self, patterns: str | Iterable[str] = ()) -> None:
        self._rules: list[_Rule] = []
        self.add(patterns)

    def add(self, patterns: str | Iterable[str]) -> "IgnoreRules":
        """Add patterns: the text of an ignore file, or one per item"""
        if isinstance(patterns, str):
            patterns = re.split(r"\r?\n", patterns)
        for pattern in patterns:
            rule = _parse_rule(pattern)
            if rule is not None:
                self._rules.append(rule)
        return self

    def _matches(self, path: str) -> bool:
        directory = path.endswith("/")
        name = path.rstrip("/")
        ignored = False
        for rule in self._rules:
            # Only a rule that could change the outcome is worth testing
            if rule.negative != ignored:
                continue
            if rule.directory and not directory:
                continue
            if rule.regex.fullmatch(name):
                ignored = not rule.negative
        return ignored

    def ignores(self, path: str) -> bool:
        """Whether ``path`` (relative, ``/``-separated) is ignored

        A path ending with ``/`` is a directory. A path in an ignored
        directory is ignored whatever later patterns say.
        """
        if not path:
            return False
        parts = path.rstrip("/").split("/")
        for depth in range(1, len(parts)):
            if self._matches("/".join(parts[:depth]) + "/"):
                return True
        return self._matches(path)


def create_ignore_rules(ailensignore: str | None = None) -> IgnoreRules:
    """The built-in rules plus a repo's ``.ailensignore`` (gitignore syntax)"""
    rules = IgnoreRules(GENERATED_PATTERNS)
    if ailensignore is not None:
        rules.add(ailensignore)
    return rules


def is_ignored(rules: IgnoreRules, path: str, original_path: str | None = None) -> bool:
    """Whether a changed file is left out, by either of its paths"""
    return rules.ignores(path) or (
        original_path is not None and rules.ignores(original_path)
    )
