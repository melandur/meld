import datetime

import pytest

from meld.ailens.models import (
    GitCommit,
    GitIdentity,
    GitRef,
    GitWorktree,
    Revision,
    RevisionKind,
)
from meld.ailens.picker import (
    build_entries,
    filter_entries,
    relative_date,
    revisions_from_args,
    typed_entry,
)

NOW = datetime.datetime(2026, 10, 21, 12, 0, tzinfo=datetime.timezone.utc)


def ref(name, type_="branch", date="2026-10-20T12:00:00+00:00", **kw):
    full = {
        "branch": f"refs/heads/{name}",
        "remote": f"refs/remotes/{name}",
        "tag": f"refs/tags/{name}",
    }[type_]
    return GitRef(
        type=type_,
        name=name,
        full_name=full,
        sha="a" * 40,
        subject="Subj",
        date=date,
        **kw,
    )


def commit(sha, subject, date="2026-10-19T12:00:00+00:00"):
    who = GitIdentity("Jane Doe", "jane@example.com", date)
    return GitCommit(
        sha=sha, parents=(), author=who, committer=who, subject=subject, body=""
    )


@pytest.mark.parametrize(
    ("delta", "expected"),
    [
        (datetime.timedelta(seconds=-10), "now"),
        (datetime.timedelta(minutes=-1), "1 minute ago"),
        (datetime.timedelta(hours=-5), "5 hours ago"),
        (datetime.timedelta(days=-1), "1 day ago"),
        (datetime.timedelta(days=-14), "2 weeks ago"),
        (datetime.timedelta(days=-400), "1 year ago"),
        (datetime.timedelta(hours=2), "in 2 hours"),
    ],
)
def test_relative_date(delta, expected):
    assert relative_date(NOW + delta, NOW) == expected


def test_entries_order_and_sections():
    entries = build_entries(
        refs=[
            ref("old", date="2026-01-01T00:00:00+00:00"),
            ref("main", current=True, date="2025-01-01T00:00:00+00:00"),
            ref("new"),
            ref("origin/main", "remote"),
            ref("v1", "tag"),
        ],
        worktrees=[
            GitWorktree(path="/repo", head="a" * 40, branch="main", main=True),
            GitWorktree(path="/wt/feature", head="b" * 40, branch="feature"),
            GitWorktree(path="/bare", head=None, branch=None, bare=True),
        ],
        commits=[commit("c" * 40, "Fixes it")],
        current_worktree="/repo",
        head=commit("a" * 40, "Head subject"),
        head_branch="main",
        side="target",
        now=NOW,
    )
    labels = [e.label for e in entries]
    assert labels[0] == "HEAD"
    # This worktree's uncommitted versions, then the other worktree's
    assert labels[1:4] == ["Working Tree", "Unstaged", "Staged"]
    assert labels[4:7] == [
        "Working Tree (feature)",
        "Unstaged (feature)",
        "Staged (feature)",
    ]
    # Current branch first, then newest first
    assert labels[7:10] == ["main", "new", "old"]
    assert labels[10:] == ["origin/main", "v1", "Fixes it"]

    working = entries[1]
    assert working.revision == Revision.working_tree()
    unstaged = entries[2]
    assert unstaged.paired_base == Revision.index()
    other = entries[4]
    assert other.revision == Revision.working_tree("/wt/feature")
    assert entries[-1].revision == Revision.commit("c" * 40)
    assert entries[-1].short_label == "ccccccc"
    assert "2 days ago" in entries[-1].description


def test_base_side_offers_no_working_tree():
    entries = build_entries(
        refs=[],
        worktrees=[GitWorktree(path="/repo", head="a" * 40, branch="main", main=True)],
        current_worktree="/repo",
        side="base",
        now=NOW,
    )
    assert [e.label for e in entries] == ["Staged"]
    assert entries[0].revision.kind is RevisionKind.INDEX


def test_branch_in_other_worktree_is_marked():
    entries = build_entries(
        refs=[ref("feature", worktree="/wt/feature")],
        current_worktree="/repo",
        now=NOW,
    )
    assert "in worktree /wt/feature" in entries[0].description


def test_unborn_worktree():
    entries = build_entries(
        refs=[],
        worktrees=[GitWorktree(path="/repo", head=None, branch="main", main=True)],
        current_worktree="/repo",
        now=NOW,
    )
    assert all("unborn branch" in e.description for e in entries)


@pytest.mark.parametrize(
    "text", ["main", "origin/main", "HEAD~3", "HEAD^2", "abc1234", "v1.0", "a/b-c_d"]
)
def test_typed_entry_accepts_refs(text):
    entry = typed_entry(text)
    assert entry is not None
    assert entry.revision == Revision.commit(text)


@pytest.mark.parametrize(
    "text", ["", "   ", "two words", "-rf", "a:b", "x*", "a[1]", "back\\slash"]
)
def test_typed_entry_rejects_non_refs(text):
    assert typed_entry(text) is None


def test_filter_entries():
    entries = build_entries(
        refs=[ref("main", current=True), ref("feature/login")],
        commits=[commit("d" * 40, "Adds login form")],
        now=NOW,
    )
    assert filter_entries(entries, "") == entries
    matched = filter_entries(entries, "login")
    # "login" could be a ref too, so it's offered first
    assert [e.kind for e in matched] == ["typed", "branch", "commit"]
    assert [e.label for e in filter_entries(entries, "ADDS form")] == [
        "Adds login form"
    ]
    # An exact name match isn't offered again as typed text
    assert [e.kind for e in filter_entries(entries, "main")] == ["branch"]


@pytest.mark.parametrize(
    ("base", "target", "expected"),
    [
        (None, None, (Revision.commit("HEAD"), Revision.working_tree())),
        ("main", "staged", (Revision.commit("main"), Revision.index())),
        ("v1", "WORKING-TREE", (Revision.commit("v1"), Revision.working_tree())),
        (None, "unstaged", (Revision.index(), Revision.working_tree())),
        ("staged", "unstaged", (Revision.index(), Revision.working_tree())),
        ("a1b2c3d", "main", (Revision.commit("a1b2c3d"), Revision.commit("main"))),
    ],
)
def test_revisions_from_args(base, target, expected):
    assert revisions_from_args(base, target) == expected


@pytest.mark.parametrize(
    ("base", "target"),
    [("unstaged", None), ("main", "unstaged"), ("two words", None), (None, "-x")],
)
def test_revisions_from_args_errors(base, target):
    with pytest.raises(ValueError):
        revisions_from_args(base, target)


def _pick(revision, paired_base=None):
    from meld.ailens.picker import PickerEntry

    return PickerEntry(
        kind="uncommitted",
        section="",
        label="",
        description="",
        icon_name="",
        revision=revision,
        short_label="",
        paired_base=paired_base,
    )


@pytest.mark.parametrize(
    ("base", "target", "picked", "side", "expected"),
    [
        # Plain picks
        (
            Revision.commit("a"),
            Revision.commit("b"),
            Revision.commit("c"),
            "target",
            (Revision.commit("a"), Revision.commit("c")),
        ),
        (
            Revision.commit("a"),
            Revision.working_tree(),
            Revision.commit("c"),
            "base",
            (Revision.commit("c"), Revision.working_tree()),
        ),
        # Staged as base makes the target this worktree's files
        (
            Revision.commit("a"),
            Revision.commit("b"),
            Revision.index(),
            "base",
            (Revision.index(), Revision.working_tree()),
        ),
        (
            Revision.commit("a"),
            Revision.commit("b"),
            Revision.index("/wt"),
            "base",
            (Revision.index("/wt"), Revision.working_tree("/wt")),
        ),
        # Leaving Staged → Working Tree for another target resets the base
        (
            Revision.index(),
            Revision.working_tree(),
            Revision.working_tree("/wt"),
            "target",
            (Revision.commit("wthead"), Revision.working_tree("/wt")),
        ),
        (
            Revision.index(),
            Revision.working_tree(),
            Revision.commit("main"),
            "target",
            (Revision.commit("head"), Revision.commit("main")),
        ),
        (
            Revision.index(),
            Revision.working_tree(),
            Revision.working_tree(),
            "target",
            (Revision.index(), Revision.working_tree()),
        ),
        # A working tree picked as base moves to the target
        (
            Revision.commit("a"),
            Revision.commit("b"),
            Revision.working_tree(),
            "base",
            (Revision.commit("head"), Revision.working_tree()),
        ),
    ],
)
def test_apply_pick(base, target, picked, side, expected):
    from meld.ailens.picker import apply_pick

    heads = {None: "head", "/wt": "wthead"}
    assert apply_pick(base, target, _pick(picked), side, heads) == expected


def test_apply_pick_paired_base():
    from meld.ailens.picker import apply_pick

    entry = _pick(Revision.working_tree("/wt"), paired_base=Revision.index("/wt"))
    assert apply_pick(Revision.commit("a"), Revision.commit("b"), entry, "target") == (
        Revision.index("/wt"),
        Revision.working_tree("/wt"),
    )
