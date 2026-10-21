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

"""Entries of the Base and Target revision pickers, without GTK"""

import datetime
import os
import re
from collections.abc import Iterable, Sequence
from dataclasses import dataclass, field
from typing import Literal

import meld.conf
from meld.ailens.models import GitCommit, GitRef, GitWorktree, Revision, RevisionKind
from meld.conf import _

EntryKind = Literal["head", "uncommitted", "branch", "remote", "tag", "commit", "typed"]

#: How many commits the picker loads at a time
COMMIT_PAGE_SIZE = 200


@dataclass(frozen=True)
class PickerEntry:
    kind: EntryKind
    #: Section heading the entry is listed under
    section: str
    label: str
    description: str
    icon_name: str
    #: What picking the entry sets the picker to
    revision: Revision
    #: How the comparison names the revision once picked
    short_label: str
    #: Picking this also sets the other side (e.g. Unstaged sets the base to
    #: Staged); None when only this side changes
    paired_base: Revision | None = None
    #: Lower-cased text the filter matches against
    search_text: str = field(default="", compare=False)


def ngettext(singular: str, plural: str, n: int) -> str:
    # Uninstalled and in tests, meld.conf.ngettext is a one-argument no-op
    try:
        return meld.conf.ngettext(singular, plural, n)
    except TypeError:
        return singular if n == 1 else plural


def short_sha(sha: str) -> str:
    return sha[:7]


def relative_date(date: datetime.datetime, now: datetime.datetime) -> str:
    """e.g. "3 days ago", "in 2 hours", "now" """
    seconds = round((date - now).total_seconds())
    units = (
        (365 * 24 * 3600, lambda n: ngettext("%d year", "%d years", n)),
        (30 * 24 * 3600, lambda n: ngettext("%d month", "%d months", n)),
        (7 * 24 * 3600, lambda n: ngettext("%d week", "%d weeks", n)),
        (24 * 3600, lambda n: ngettext("%d day", "%d days", n)),
        (3600, lambda n: ngettext("%d hour", "%d hours", n)),
        (60, lambda n: ngettext("%d minute", "%d minutes", n)),
    )
    for size, unit in units:
        if abs(seconds) >= size:
            count = int(abs(seconds) / size)
            text = unit(count) % count
            if seconds < 0:
                return _("%s ago") % text
            return _("in %s") % text
    return _("now")


def parse_iso_date(value: str) -> datetime.datetime | None:
    try:
        date = datetime.datetime.fromisoformat(value)
    except (TypeError, ValueError):
        return None
    if date.tzinfo is None:
        date = date.replace(tzinfo=datetime.timezone.utc)
    return date


def _when(value: str, now: datetime.datetime) -> str:
    date = parse_iso_date(value)
    return relative_date(date, now) if date else ""


def _join(*parts: str | None) -> str:
    return " · ".join(p for p in parts if p)


def _entry(**kwargs) -> PickerEntry:
    search = " ".join(
        str(kwargs.get(key) or "") for key in ("label", "description", "short_label")
    )
    return PickerEntry(search_text=search.lower(), **kwargs)


def worktree_name(worktree: GitWorktree) -> str:
    if worktree.branch:
        return worktree.branch
    if worktree.head:
        return short_sha(worktree.head)
    return os.path.basename(worktree.path.rstrip(os.sep)) or worktree.path


def build_entries(
    *,
    refs: Sequence[GitRef],
    worktrees: Sequence[GitWorktree] = (),
    commits: Sequence[GitCommit] = (),
    current_worktree: str | None = None,
    head: GitCommit | None = None,
    head_branch: str | None = None,
    side: Literal["base", "target"] = "target",
    now: datetime.datetime | None = None,
) -> list[PickerEntry]:
    """Every entry a picker offers, in display order

    HEAD, the uncommitted versions (this worktree's first, then the other
    worktrees'), branches (current first), remote branches, tags (each newest
    first), then commits newest first.
    """
    now = now or datetime.datetime.now(datetime.timezone.utc)
    entries: list[PickerEntry] = []

    if head is not None:
        entries.append(
            _entry(
                kind="head",
                section=_("Current"),
                label="HEAD",
                description=_join(
                    head_branch or _("detached"), short_sha(head.sha), head.subject
                ),
                icon_name="go-home-symbolic",
                revision=Revision.commit("HEAD"),
                short_label="HEAD",
            )
        )

    current = os.path.realpath(current_worktree) if current_worktree else None
    ordered = sorted(
        (wt for wt in worktrees if not wt.bare),
        key=lambda wt: (os.path.realpath(wt.path) != current, not wt.main, wt.path),
    )
    for wt in ordered:
        is_current = current is not None and os.path.realpath(wt.path) == current
        # None names the repository's own worktree, so a comparison saved
        # with it survives the worktree being moved
        where = None if is_current else wt.path
        section = (
            _("Uncommitted") if is_current else _("Worktree %s") % (worktree_name(wt))
        )
        suffix = "" if is_current else " (%s)" % worktree_name(wt)
        location = _join(
            None if is_current else wt.path,
            _("unborn branch") if wt.head is None else None,
        )
        if side == "target":
            entries.append(
                _entry(
                    kind="uncommitted",
                    section=section,
                    label=_("Working Tree") + suffix,
                    description=_join(
                        _("staged, unstaged and untracked changes"), location
                    ),
                    icon_name="document-edit-symbolic",
                    revision=Revision.working_tree(where),
                    short_label=_("Working Tree") + suffix,
                )
            )
            entries.append(
                _entry(
                    kind="uncommitted",
                    section=section,
                    label=_("Unstaged") + suffix,
                    description=_join(
                        _("changes not staged yet (Staged → Working Tree)"), location
                    ),
                    icon_name="document-edit-symbolic",
                    revision=Revision.working_tree(where),
                    short_label=_("Working Tree") + suffix,
                    paired_base=Revision.index(where),
                )
            )
        entries.append(
            _entry(
                kind="uncommitted",
                section=section,
                label=_("Staged") + suffix,
                description=_join(_("staged changes only (the index)"), location),
                icon_name="list-add-symbolic",
                revision=Revision.index(where),
                short_label=_("Staged") + suffix,
            )
        )

    def newest_first(items: Iterable[GitRef]) -> list[GitRef]:
        return sorted(items, key=lambda r: parse_iso_date(r.date) or now, reverse=True)

    branches = sorted(
        newest_first(r for r in refs if r.type == "branch"),
        key=lambda r: not r.current,
    )
    sections = (
        ("branch", _("Branches"), "media-playlist-consecutive-symbolic", branches),
        (
            "remote",
            _("Remote branches"),
            "network-server-symbolic",
            newest_first(r for r in refs if r.type == "remote"),
        ),
        (
            "tag",
            _("Tags"),
            "bookmark-new-symbolic",
            newest_first(r for r in refs if r.type == "tag"),
        ),
    )
    for kind, section, icon, items in sections:
        for ref in items:
            worktree = None
            if ref.worktree and (
                current is None or os.path.realpath(ref.worktree) != current
            ):
                worktree = _("in worktree %s") % ref.worktree
            entries.append(
                _entry(
                    kind=kind,
                    section=section,
                    label=ref.name,
                    description=_join(
                        _("current") if ref.current else None,
                        worktree,
                        short_sha(ref.sha),
                        ref.subject,
                    ),
                    icon_name="object-select-symbolic" if ref.current else icon,
                    revision=Revision.commit(ref.full_name),
                    short_label=ref.name,
                )
            )

    for commit in commits:
        entries.append(
            _entry(
                kind="commit",
                section=_("Commits"),
                label=commit.subject or _("(no message)"),
                description=_join(
                    short_sha(commit.sha),
                    commit.author.name,
                    _when(commit.author.date, now),
                ),
                icon_name="emblem-documents-symbolic",
                revision=Revision.commit(commit.sha),
                short_label=short_sha(commit.sha),
            )
        )
    return entries


_REF_CHARS = re.compile(r"^[^\s~^:?*\[\\]+(?:[~^]\d*)*$|^[0-9a-fA-F]{4,64}$")


def typed_entry(text: str) -> PickerEntry | None:
    """The entry for a ref or SHA typed into the filter

    None when the text can't be one. Whether it names a commit is only known
    once git resolves it.
    """
    ref = text.strip()
    if not ref or not _REF_CHARS.match(ref) or ref.startswith("-"):
        return None
    return _entry(
        kind="typed",
        section=_("Typed"),
        label=_("Use “%s”") % ref,
        description=_("a branch, tag or SHA"),
        icon_name="system-search-symbolic",
        revision=Revision.commit(ref),
        short_label=ref,
    )


def filter_entries(entries: Sequence[PickerEntry], query: str) -> list[PickerEntry]:
    """Entries whose text contains every word of ``query``

    A typed-ref entry comes first when the query could name a revision that
    is not already listed by that exact name.
    """
    words = query.lower().split()
    if not words:
        return list(entries)
    matched = [e for e in entries if all(w in e.search_text for w in words)]
    typed = typed_entry(query)
    if typed is not None and not any(
        e.short_label == query.strip() or e.revision == typed.revision for e in matched
    ):
        matched.insert(0, typed)
    return matched


#: Names the command line accepts for the uncommitted versions
_WORKING_TREE_NAMES = {"working-tree", "worktree", "working", "wt"}
_INDEX_NAMES = {"staged", "index", "cached"}
_UNSTAGED_NAMES = {"unstaged"}


def revisions_from_args(
    base: str | None, target: str | None
) -> tuple[Revision, Revision]:
    """Base and target from command-line text

    Besides any ref or SHA, ``working-tree``, ``staged`` and (as the target
    only) ``unstaged``, which also makes the base ``staged``. Defaults:
    HEAD → working tree. Raises ValueError for text that can't be a revision.
    """

    def parse(text: str) -> Revision:
        name = text.strip().lower()
        if name in _WORKING_TREE_NAMES:
            return Revision.working_tree()
        if name in _INDEX_NAMES:
            return Revision.index()
        if typed_entry(text) is None:
            raise ValueError(_("“%s” is not a revision") % text)
        return Revision.commit(text.strip())

    if target is not None and target.strip().lower() in _UNSTAGED_NAMES:
        if base is not None and parse(base) != Revision.index():
            raise ValueError(_("“unstaged” compares Staged with the working tree"))
        return Revision.index(), Revision.working_tree()
    if base is not None and base.strip().lower() in _UNSTAGED_NAMES:
        raise ValueError(_("“unstaged” can only be the target"))
    return (
        parse(base) if base is not None else Revision.commit("HEAD"),
        parse(target) if target is not None else Revision.working_tree(),
    )


def apply_pick(
    base: Revision,
    target: Revision,
    entry: PickerEntry,
    side: Literal["base", "target"],
    worktree_heads: dict[str | None, str] | None = None,
) -> tuple[Revision, Revision]:
    """Base and target after picking ``entry`` for ``side``

    Keeps the pair valid: the index can only be a base for the working tree
    of the same worktree, so picking one side adjusts the other when needed.
    ``worktree_heads`` maps worktree paths (None: this one) to their HEAD.
    """
    heads = worktree_heads or {}
    if side == "base":
        base = entry.revision
        if base.kind is RevisionKind.INDEX:
            target = Revision.working_tree(base.worktree)
        elif base.kind is RevisionKind.WORKING_TREE:
            # Only a target can be the working tree; swap it there
            base, target = Revision.commit(heads.get(base.worktree, "HEAD")), base
        return base, target

    target = entry.revision
    if entry.paired_base is not None:
        return entry.paired_base, target
    if base.kind is not RevisionKind.COMMIT and target != Revision.working_tree(
        base.worktree
    ):
        base = Revision.commit(heads.get(target.worktree, "HEAD"))
    return base, target
