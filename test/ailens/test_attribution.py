import os
import threading

import pytest

from meld.ailens.attribution import ComparisonAttributor
from meld.ailens.comparison import resolve_comparison
from meld.ailens.git import GitRepository
from meld.ailens.models import (
    Cancelled,
    ChangeGroup,
    FileAttribution,
    GitFileChange,
    LineOrigin,
    Revision,
)
from meld.ailens.process import CancelToken
from test.ailens.fixture_repo import HUMAN, FixtureRepo, co_authored_by_claude


def lines(*values: str) -> str:
    return "".join(f"{value}\n" for value in values)


class Setup:
    def __init__(
        self,
        fixture: FixtureRepo,
        base: Revision | str,
        target: Revision | str,
        **kwargs,
    ) -> None:
        self.fixture = fixture
        self.repo = GitRepository(str(fixture.path), env=fixture.env, **kwargs)
        self.comparison = resolve_comparison(
            self.repo, self._revision(base), self._revision(target)
        )
        self.attributor = ComparisonAttributor(self.repo, self.comparison)
        self.files: dict[str, FileAttribution] = {
            change.path: self.attributor.attribute(change)
            for change in self.attributor.changed_files()
        }

    def _revision(self, value: Revision | str) -> Revision:
        if isinstance(value, Revision):
            return value
        return Revision.commit(self.fixture.shas.get(value, value))

    def label(self, sha: str | None) -> str:
        if sha is None:
            return "?"
        labels = {sha: label for label, sha in self.fixture.shas.items()}
        return labels.get(sha, sha)

    def describe(self, path: str) -> list[dict]:
        """Groups with SHAs replaced by fixture labels, ranges as start-end"""

        def ranges(group: ChangeGroup, side: str) -> list[str]:
            return [f"{r.start}-{r.end}" for r in getattr(group, side)]

        return [
            {
                "by": group.uncommitted or self.label(group.sha),
                "hunk": group.hunk_index,
                "added": ranges(group, "added"),
                "deleted": ranges(group, "deleted"),
            }
            for group in self.files[path].groups
        ]


def group(by: str, added=(), deleted=(), hunk: int = 0) -> dict:
    return {"by": by, "hunk": hunk, "added": list(added), "deleted": list(deleted)}


def merge_by_hand(fixture: FixtureRepo, ref: str, files: dict[str, str]) -> str:
    """Merge ``ref`` without committing (conflicts allowed), write
    ``files`` as the resolution and commit it
    """
    env = fixture._commit_env(HUMAN)
    try:
        fixture.git("merge", "--quiet", "--no-ff", "--no-commit", ref, env=env)
    except RuntimeError:
        pass  # Conflicts
    fixture.write_files(files)
    fixture.git("add", "--all")
    fixture.git("commit", "--quiet", "--no-edit", env=env)
    return fixture.git("rev-parse", "HEAD").strip()


# -- committed changes --


def test_credits_each_added_and_deleted_line_to_the_commit_that_made_it(fixture_repo):
    f = fixture_repo
    f.commit(
        "Initial", {"a.ts": lines("1", "2", "3", "4", "5", "6", "7")}, label="base"
    )
    f.commit(
        "Agent work",
        {"a.ts": lines("1", "2a", "3", "new", "4", "5", "6", "7a")},
        label="agent",
        trailers=[co_authored_by_claude("Opus 5.5")],
    )
    f.commit(
        "Human work",
        {"a.ts": lines("1", "2a", "3", "new", "4", "6", "7b")},
        label="human",
    )

    setup = Setup(f, "base", "human")

    assert setup.describe("a.ts") == [
        # 2 -> 2a, all by agent
        group("agent", ["2-2"], ["2-2"]),
        # new line
        group("agent", ["4-4"]),
        # 5 deleted by human; 6 survives
        group("human", [], ["5-5"]),
        # 7 -> 7a by agent, then 7a -> 7b by human: the base line went in
        # agent, the final text came from human
        group("agent", [], ["7-7"]),
        group("human", ["7-7"]),
    ]
    assert setup.files["a.ts"].touched_by == [f.shas["human"], f.shas["agent"]]
    assert setup.files["a.ts"].layers is None


def test_follows_renames_crediting_edits_before_and_after_the_rename(fixture_repo):
    f = fixture_repo
    original = ("l1", "l2", "l3", "l4", "l5", "l6", "l7", "l8")
    f.commit("Initial", {"old.txt": lines(*original)}, label="base")
    f.commit("Edits l1", {"old.txt": lines("L1", *original[1:])}, label="edit")
    f.commit("Renames", label="rename", renames={"old.txt": "new.txt"})
    f.commit(
        "Deletes l4",
        {"new.txt": lines("L1", "l2", "l3", "l5", "l6", "l7", "l8")},
        label="delete",
    )
    f.commit(
        "Replaces l8",
        {"new.txt": lines("L1", "l2", "l3", "l5", "l6", "l7", "X")},
        label="replace",
    )

    setup = Setup(f, "base", "replace")

    change = setup.files["new.txt"].change
    assert (change.status, change.original_path) == ("R", "old.txt")
    assert setup.describe("new.txt") == [
        group("edit", ["1-1"], ["1-1"]),
        # The 3-line contexts overlap, so git puts all three in one hunk
        group("delete", [], ["4-4"]),
        group("replace", ["7-7"], ["8-8"]),
    ]


def test_diverged_branches_credit_only_the_target_side(fixture_repo):
    f = fixture_repo
    f.commit("Initial", {"a.ts": lines(*"123456789")}, label="base")
    f.branch("feature")
    f.commit("Feature", {"a.ts": lines("1", "two", *"3456789")}, label="feature")
    f.checkout("main")
    f.commit("Main", {"a.ts": lines(*"12345678", "nine"), "m.ts": "m\n"}, label="main")

    setup = Setup(f, "main", "feature")

    assert (setup.comparison.relation, setup.comparison.from_sha) == (
        "diverged",
        f.shas["base"],
    )
    assert list(setup.files) == ["a.ts"]
    assert setup.describe("a.ts") == [group("feature", ["2-2"], ["2-2"])]


def test_deletions_through_a_merge_go_to_the_commit_on_the_merged_side(fixture_repo):
    f = fixture_repo
    f.commit("Initial", {"f.txt": lines(*"abcdefghij")}, label="base")
    f.branch("side")
    f.commit("Side deletes b", {"f.txt": lines(*"acdefghij")}, label="side")
    f.checkout("main")
    f.commit("Main deletes i", {"f.txt": lines(*"abcdefghj")}, label="main")
    f.merge("side", "Merges side", label="merge")

    setup = Setup(f, "base", "merge")

    assert setup.describe("f.txt") == [
        group("side", [], ["2-2"]),
        # The two hunks' contexts touch, so git joins them
        group("main", [], ["9-9"]),
    ]


def test_a_deletion_made_by_the_merge_itself_goes_to_the_merge(fixture_repo):
    f = fixture_repo
    f.commit("Initial", {"f.txt": lines(*"abcdefghij")}, label="base")
    f.branch("side")
    f.commit("Side deletes b", {"f.txt": lines(*"acdefghij")}, label="side")
    f.checkout("main")
    f.commit("Main adds k", {"f.txt": lines(*"abcdefghijk")}, label="main")
    # A merge that also deletes f, which neither parent did
    merge = merge_by_hand(f, "side", {"f.txt": lines(*"acdeghijk")})

    setup = Setup(f, "base", merge)

    assert setup.describe("f.txt") == [
        group("side", [], ["2-2"]),
        group(merge, [], ["6-6"]),
        group("main", ["9-9"]),
    ]


def test_a_line_dropped_while_resolving_a_conflict_goes_to_the_merge(fixture_repo):
    f = fixture_repo
    f.commit("Initial", {"f.txt": lines(*"abcdefghij")}, label="base")
    f.branch("side")
    f.commit("Side", {"f.txt": lines(*"abcdefghi", "J-side")}, label="side")
    f.checkout("main")
    f.commit("Main", {"f.txt": lines(*"abcdefghi", "J-main")}, label="main")
    # Takes main's side of the conflict and drops b as well
    merge = merge_by_hand(f, "side", {"f.txt": lines(*"acdefghi", "J-main")})

    setup = Setup(f, "base", merge)

    assert setup.describe("f.txt") == [
        group(merge, [], ["2-2"], hunk=0),
        # Both parents deleted j; the first of them in git's order is
        # credited
        group("main", ["9-9"], ["10-10"], hunk=1),
    ]


def test_credits_added_and_deleted_files(fixture_repo):
    f = fixture_repo
    f.commit("Initial", {"gone.ts": lines("x", "y")}, label="base")
    f.commit("Adds new.ts", {"new.ts": lines("1", "2", "3")}, label="add")
    f.commit("Edits new.ts", {"new.ts": lines("1", "two", "3", "4")}, label="edit")
    f.commit("Deletes gone.ts", {"gone.ts": None}, label="remove")

    setup = Setup(f, "base", "remove")

    assert setup.describe("new.ts") == [
        group("add", ["1-1", "3-3"]),
        group("edit", ["2-2", "4-4"]),
    ]
    assert setup.describe("gone.ts") == [group("remove", [], ["1-2"])]


def test_binary_files_get_only_the_commits_that_touched_them(fixture_repo):
    f = fixture_repo
    f.commit("Initial", {"img.bin": b"a\0b"}, label="base")
    f.commit("Changes image", {"img.bin": b"a\0c"}, label="change")
    f.commit("Unrelated", {"other.txt": "o\n"}, label="other")

    setup = Setup(f, "base", "other")

    attribution = setup.files["img.bin"]
    assert attribution.diff.binary
    assert attribution.groups == []
    assert attribution.touched_by == [f.shas["change"]]


def test_mode_changes_have_no_groups(fixture_repo):
    f = fixture_repo
    f.commit("Initial", {"run.sh": "echo\n"}, label="base")
    (f.path / "run.sh").chmod(0o755)
    f.commit("Makes it executable", label="chmod")

    setup = Setup(f, "base", "chmod")

    attribution = setup.files["run.sh"]
    assert attribution.diff.hunks == ()
    assert attribution.groups == []
    assert attribution.touched_by == [f.shas["chmod"]]


def test_credits_nothing_when_the_target_is_an_ancestor_of_the_base(fixture_repo):
    f = fixture_repo
    f.commit("Initial", {"a.ts": lines("1", "2")}, label="old")
    f.commit("Change", {"a.ts": lines("1", "two")}, label="new")

    setup = Setup(f, "new", "old")

    assert setup.comparison.relation == "reversed"
    assert setup.describe("a.ts") == [group("?", ["2-2"], ["2-2"])]
    assert setup.files["a.ts"].touched_by == []


def test_unrelated_histories_credit_the_targets_commits_root_included(fixture_repo):
    f = fixture_repo
    f.commit("Initial", {"a.ts": lines("1", "2", "3")}, label="base")
    f.git("checkout", "--quiet", "--orphan", "other")
    f.git("rm", "--quiet", "-rf", ".")
    f.commit("Other root", {"a.ts": lines("1", "x", "3")}, label="root")
    f.commit("Other edit", {"a.ts": lines("1", "x", "3", "4")}, label="edit")

    setup = Setup(f, "base", "edit")

    assert setup.comparison.relation == "unrelated"
    # Deleted lines can't be traced without common history
    assert setup.describe("a.ts") == [
        group("?", [], ["2-2"]),
        group("root", ["2-2"]),
        group("edit", ["4-4"]),
    ]
    assert setup.files["a.ts"].touched_by == [f.shas["edit"], f.shas["root"]]


def test_places_each_line_in_the_crediting_commits_own_diff_across_a_rename(
    fixture_repo,
):
    f = fixture_repo
    f.commit("Initial", {"a.ts": "l1\nl2\nl3\nl4\nl5\n"}, label="base")
    f.commit("Insert", {"a.ts": "l1\nx\nl2\nl3\nl4\nl5\n"}, label="insert")
    f.commit("Remove", {"a.ts": "l1\nx\nl2\nl3\nl5\n"}, label="remove")
    f.commit(
        "Rename",
        {"b.ts": "l1\nx\nl2\nl3\nl5\nend\n"},
        label="rename",
        renames={"a.ts": "b.ts"},
    )

    setup = Setup(f, "base", "rename")

    assert [(setup.label(g.sha), g.origins) for g in setup.files["b.ts"].groups] == [
        ("insert", [LineOrigin("a.ts", "new", 2)]),
        # Line 5 of the file as remove's parent had it
        ("remove", [LineOrigin("a.ts", "old", 5)]),
        ("rename", [LineOrigin("b.ts", "new", 6)]),
    ]


def test_crlf_lines_and_missing_final_newlines(fixture_repo):
    f = fixture_repo
    f.commit("Initial", {"w.txt": "a\r\nb\r\nc"}, label="base")
    f.commit("Edits b", {"w.txt": "a\r\nB\r\nc"}, label="b")
    f.commit("Edits c", {"w.txt": "a\r\nB\r\nC"}, label="c")

    setup = Setup(f, "base", "c")

    assert setup.describe("w.txt") == [
        group("b", ["2-2"], ["2-2"]),
        group("c", ["3-3"], ["3-3"]),
    ]


def test_repeated_lines(fixture_repo):
    f = fixture_repo
    f.commit("Initial", {"r.txt": lines("x", "x", "x", "y")}, label="base")
    f.commit("Adds an x", {"r.txt": lines("x", "x", "x", "x", "y")}, label="more")
    f.commit("Removes y", {"r.txt": lines("x", "x", "x", "x")}, label="less")

    setup = Setup(f, "base", "less")

    attribution = setup.files["r.txt"]
    added = sum(r.end - r.start + 1 for g in attribution.groups for r in g.added)
    deleted = sum(r.end - r.start + 1 for g in attribution.groups for r in g.deleted)
    assert (added, deleted) == (1, 1)
    assert {setup.label(g.sha) for g in attribution.groups} <= {"more", "less", "?"}
    assert any(g.sha == f.shas["less"] and g.deleted for g in attribution.groups)


def test_odd_paths(fixture_repo):
    f = fixture_repo
    names = ["a b.txt", 'quo"te.txt', "日本/ü.txt", "new\nline.txt", "glob[1]*.txt"]
    f.commit("Initial", {name: lines("1", "2") for name in names}, label="base")
    f.commit("Edits", {name: lines("1", "two") for name in names}, label="edit")

    setup = Setup(f, "base", "edit")

    for name in names:
        assert setup.describe(name) == [group("edit", ["2-2"], ["2-2"])]
        assert setup.files[name].touched_by == [f.shas["edit"]]


def test_cancelled_attribution(fixture_repo):
    f = fixture_repo
    f.commit("Initial", {"a.ts": lines("1", "2")}, label="base")
    f.commit("Change", {"a.ts": lines("1", "two")}, label="change")
    repo = GitRepository(str(f.path), env=f.env)
    comparison = resolve_comparison(
        repo, Revision.commit(f.shas["base"]), Revision.commit(f.shas["change"])
    )
    attributor = ComparisonAttributor(repo, comparison)
    [change] = attributor.changed_files()
    cancel = CancelToken()
    cancel.cancel()

    with pytest.raises(Cancelled):
        attributor.attribute(change, cancel)


def test_cancelled_midway_leaves_the_attributor_usable(fixture_repo):
    f = fixture_repo
    f.commit("Initial", {"a.ts": lines("1", "2", "3")}, label="base")
    f.commit("Change", {"a.ts": lines("1", "two")}, label="change")

    class CancellingRepository(GitRepository):
        """Cancels on its third git call"""

        calls = 0
        token: CancelToken | None = None

        def run(self, args, **kwargs):
            self.calls += 1
            if self.token is not None and self.calls == 3:
                self.token.cancel()
            return super().run(args, **kwargs)

    repo = CancellingRepository(str(f.path), env=f.env)
    comparison = resolve_comparison(
        repo, Revision.commit(f.shas["base"]), Revision.commit(f.shas["change"])
    )
    attributor = ComparisonAttributor(repo, comparison)
    [change] = attributor.changed_files()

    repo.calls = 0
    repo.token = CancelToken()
    with pytest.raises(Cancelled):
        attributor.attribute(change, repo.token)

    repo.token = None
    attribution = attributor.attribute(change)
    assert [(g.sha, len(g.added), len(g.deleted)) for g in attribution.groups] == [
        (f.shas["change"], 1, 1)
    ]


def test_one_attributor_from_several_threads(fixture_repo):
    f = fixture_repo
    f.commit("Initial", {f"{n}.txt": lines("1", "2") for n in range(6)}, label="base")
    f.commit("Change", {f"{n}.txt": lines("1", "two") for n in range(6)}, label="c")
    repo = GitRepository(str(f.path), env=f.env)
    comparison = resolve_comparison(
        repo, Revision.commit(f.shas["base"]), Revision.commit(f.shas["c"])
    )
    attributor = ComparisonAttributor(repo, comparison)
    results: dict[str, list] = {}

    def work(change: GitFileChange) -> None:
        results[change.path] = attributor.attribute(change).groups

    threads = [
        threading.Thread(target=work, args=(change,))
        for change in attributor.changed_files()
    ]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()

    assert len(results) == 6
    assert all([g.sha for g in groups] == [f.shas["c"]] for groups in results.values())


# -- uncommitted changes --


def build_layered(f: FixtureRepo) -> None:
    """A committed edit, then staged and unstaged edits, and an untracked file"""
    f.commit("Initial", {"a.ts": lines(*map(str, range(1, 13)))}, label="base")
    f.commit(
        "Agent work", {"a.ts": lines("1", "2a", *map(str, range(3, 13)))}, label="agent"
    )
    # Staged: 3 -> 3s; 6 deleted
    f.write_files(
        {"a.ts": lines("1", "2a", "3s", "4", "5", "7", "8", "9", "10", "11", "12")}
    )
    f.stage()
    # Unstaged: 10 -> 10u; a line added at the end; an untracked file
    f.write_files(
        {
            "a.ts": lines(
                "1", "2a", "3s", "4", "5", "7", "8", "9", "10u", "11", "12", "end"
            ),
            "u.ts": lines("x", "y"),
        }
    )


def test_credits_working_tree_lines_to_their_layer_or_the_commit_below(fixture_repo):
    build_layered(fixture_repo)

    setup = Setup(fixture_repo, "base", Revision.working_tree())

    assert list(setup.files) == ["a.ts", "u.ts"]
    assert setup.describe("a.ts") == [
        group("agent", ["2-2"], ["2-2"]),
        group("staged", ["3-3"], ["3-3"]),
        group("staged", [], ["6-6"]),
        group("unstaged", ["9-9"], ["10-10"]),
        group("unstaged", ["12-12"]),
    ]
    assert setup.describe("u.ts") == [group("unstaged", ["1-2"])]
    assert setup.files["u.ts"].change == GitFileChange("A", "u.ts", untracked=True)
    assert setup.files["a.ts"].touched_by == [fixture_repo.shas["agent"]]


def test_places_uncommitted_lines_in_their_layers_own_diff(fixture_repo):
    build_layered(fixture_repo)

    setup = Setup(fixture_repo, "base", Revision.working_tree())
    file = setup.files["a.ts"]

    assert [g.origins for g in file.groups] == [
        # The commit's own diff
        [LineOrigin("a.ts", "old", 2), LineOrigin("a.ts", "new", 2)],
        # The staged diff: HEAD line 3 and index line 3
        [LineOrigin("a.ts", "old", 3), LineOrigin("a.ts", "new", 3)],
        [LineOrigin("a.ts", "old", 6)],
        # The unstaged diff: index line 9 and working tree line 9
        [LineOrigin("a.ts", "old", 9), LineOrigin("a.ts", "new", 9)],
        [LineOrigin("a.ts", "new", 12)],
    ]
    assert [diff.new_path for diff in file.layers["staged"]] == ["a.ts"]
    assert [diff.new_path for diff in file.layers["unstaged"]] == ["a.ts"]
    untracked = setup.files["u.ts"]
    assert untracked.layers == {"staged": [], "unstaged": [untracked.diff]}


def test_compares_with_the_index_without_unstaged_or_untracked_changes(fixture_repo):
    build_layered(fixture_repo)

    setup = Setup(fixture_repo, "base", Revision.index())

    assert list(setup.files) == ["a.ts"]
    assert setup.describe("a.ts") == [
        group("agent", ["2-2"], ["2-2"]),
        group("staged", ["3-3"], ["3-3"]),
        group("staged", [], ["6-6"]),
    ]
    assert list(setup.files["a.ts"].layers) == ["staged", "unstaged"]
    assert setup.files["a.ts"].layers["unstaged"] == []


def test_compares_head_with_the_working_tree(fixture_repo):
    build_layered(fixture_repo)

    setup = Setup(fixture_repo, "agent", Revision.working_tree())

    assert setup.comparison.relation == "same"
    assert setup.describe("a.ts") == [
        group("staged", ["3-3"], ["3-3"]),
        group("staged", [], ["6-6"]),
        group("unstaged", ["9-9"], ["10-10"]),
        group("unstaged", ["12-12"]),
    ]
    assert setup.files["a.ts"].touched_by == []


def test_index_to_working_tree_is_all_unstaged(fixture_repo):
    build_layered(fixture_repo)

    setup = Setup(fixture_repo, Revision.index(), Revision.working_tree())

    assert list(setup.files) == ["a.ts", "u.ts"]
    assert setup.describe("a.ts") == [
        group("unstaged", ["9-9"], ["9-9"]),
        group("unstaged", ["12-12"]),
    ]
    assert setup.describe("u.ts") == [group("unstaged", ["1-2"])]
    file = setup.files["a.ts"]
    assert [g.origins for g in file.groups] == [
        [LineOrigin("a.ts", "old", 9), LineOrigin("a.ts", "new", 9)],
        [LineOrigin("a.ts", "new", 12)],
    ]
    assert file.layers == {"staged": [], "unstaged": [file.diff]}
    assert file.touched_by == []


def test_index_to_working_tree_with_unstaged_deletions(fixture_repo):
    f = fixture_repo
    f.commit("Initial", {"a.ts": lines("1", "2", "3"), "gone.ts": "g\n"}, label="base")
    f.write_files({"a.ts": lines("1", "3"), "gone.ts": None})

    setup = Setup(f, Revision.index(), Revision.working_tree())

    assert setup.describe("a.ts") == [group("unstaged", [], ["2-2"])]
    assert setup.describe("gone.ts") == [group("unstaged", [], ["1-1"])]


def test_index_to_working_tree_on_an_unborn_branch(fixture_repo):
    f = fixture_repo
    f.write_files({"a.txt": lines("staged")})
    f.stage()
    f.write_files({"a.txt": lines("staged", "unstaged"), "u.txt": "u\n"})

    setup = Setup(f, Revision.index(), Revision.working_tree())

    assert setup.describe("a.txt") == [group("unstaged", ["2-2"])]
    assert setup.describe("u.txt") == [group("unstaged", ["1-1"])]


def test_follows_a_staged_rename_back_to_the_commits_before_it(fixture_repo):
    f = fixture_repo
    f.commit("Initial", {"a.ts": lines("1", "2", "3", "4", "5")}, label="base")
    f.commit("Edit", {"a.ts": lines("1", "2e", "3", "4", "5")}, label="edit")
    f.git("mv", "a.ts", "b.ts")
    f.write_files({"b.ts": lines("1", "2e", "3", "4", "5u")})

    setup = Setup(f, "base", Revision.working_tree())

    change = setup.files["b.ts"].change
    assert (change.status, change.original_path) == ("R", "a.ts")
    assert setup.describe("b.ts") == [
        group("edit", ["2-2"], ["2-2"]),
        group("unstaged", ["5-5"], ["5-5"]),
    ]
    assert setup.files["b.ts"].groups[0].origins == [
        LineOrigin("a.ts", "old", 2),
        LineOrigin("a.ts", "new", 2),
    ]


def test_credits_deleted_files_to_the_layer_that_deleted_them(fixture_repo):
    f = fixture_repo
    f.commit("Initial", {"s.ts": "s\n", "u.ts": "u\n"}, label="base")
    f.write_files({"s.ts": None, "u.ts": None})
    f.stage("s.ts")

    setup = Setup(f, "base", Revision.working_tree())

    assert setup.describe("s.ts") == [group("staged", [], ["1-1"])]
    assert setup.describe("u.ts") == [group("unstaged", [], ["1-1"])]


def test_uncommitted_binary_and_empty_files(fixture_repo):
    f = fixture_repo
    f.commit("Initial", {"img.bin": b"a\0b"}, label="base")
    f.write_files({"img.bin": b"a\0c", "new.bin": b"\0\0", "empty.txt": ""})

    setup = Setup(f, "base", Revision.working_tree())

    for path in ("img.bin", "new.bin"):
        assert setup.files[path].diff.binary
        assert setup.files[path].groups == []
    assert setup.files["empty.txt"].diff.hunks == ()
    assert setup.files["empty.txt"].groups == []


def test_uncommitted_changes_in_another_worktree(fixture_repo, tmp_path):
    f = fixture_repo
    f.commit(
        "Initial", {"a.ts": lines("1", "2", "3", "4", "5", "6", "7", "8")}, label="base"
    )
    other = f.worktree(tmp_path / "other", "other")
    f.commit(
        "There",
        {"a.ts": lines("1", "2t", "3", "4", "5", "6", "7", "8")},
        label="there",
        cwd=other,
    )
    f.write_files({"a.ts": lines("1", "2t", "3", "4s", "5", "6", "7", "8")}, root=other)
    f.git("add", "a.ts", cwd=other)
    f.write_files(
        {"a.ts": lines("1", "2t", "3", "4s", "5", "6", "7", "8u")}, root=other
    )
    # The repository's own worktree has other uncommitted changes
    f.write_files({"a.ts": lines("mine")})
    f.stage()

    setup = Setup(f, "base", Revision.working_tree(str(other)))

    assert setup.comparison.worktree_path == str(other)
    assert setup.describe("a.ts") == [
        group("there", ["2-2"], ["2-2"]),
        group("staged", ["4-4"], ["4-4"]),
        group("unstaged", ["8-8"], ["8-8"]),
    ]

    staged = Setup(f, "base", Revision.index(str(other)))
    assert staged.describe("a.ts") == [
        group("there", ["2-2"], ["2-2"]),
        group("staged", ["4-4"], ["4-4"]),
    ]
    unstaged = Setup(f, Revision.index(str(other)), Revision.working_tree(str(other)))
    assert unstaged.describe("a.ts") == [group("unstaged", ["8-8"], ["8-8"])]


def test_uncommitted_target_with_a_detached_head(fixture_repo):
    f = fixture_repo
    f.commit("Initial", {"a.ts": lines("1", "2")}, label="base")
    f.commit("Edit", {"a.ts": lines("1", "two")}, label="edit")
    f.git("checkout", "--quiet", "--detach")
    f.write_files({"a.ts": lines("1", "two", "three")})

    setup = Setup(f, "base", Revision.working_tree())

    assert setup.describe("a.ts") == [
        group("edit", ["2-2"], ["2-2"]),
        group("unstaged", ["3-3"]),
    ]


def test_uncommitted_target_after_a_merge(fixture_repo):
    f = fixture_repo
    f.commit("Initial", {"f.txt": lines(*"abcdefghij")}, label="base")
    f.branch("side")
    f.commit("Side deletes b", {"f.txt": lines(*"acdefghij")}, label="side")
    f.checkout("main")
    f.commit("Main deletes i", {"f.txt": lines(*"abcdefghj")}, label="main")
    f.merge("side", "Merges side", label="merge")
    f.write_files({"f.txt": lines(*"acdefgh", "J")})

    setup = Setup(f, "base", Revision.working_tree())

    assert setup.describe("f.txt") == [
        group("side", [], ["2-2"]),
        group("main", [], ["9-9"]),
        group("unstaged", ["8-8"], ["10-10"]),
    ]


def test_symlinks_in_the_working_tree(fixture_repo):
    f = fixture_repo
    f.commit("Initial", {"a.ts": "a\n"}, label="base")
    os.symlink("a.ts", f.path / "link")

    setup = Setup(f, "base", Revision.working_tree())

    assert setup.describe("link") == [group("unstaged", ["1-1"])]
