import shutil

import pytest

from meld.ailens.comparison import (
    changed_files,
    changed_files_with_stats,
    file_diff,
    file_stats,
    resolve_comparison,
)
from meld.ailens.git import GitRepository
from meld.ailens.models import (
    AiLensError,
    Cancelled,
    Comparison,
    GitDiffLine,
    GitFileChange,
    Revision,
    RevisionKind,
)
from meld.ailens.parsers import GitFileStats
from meld.ailens.process import CancelToken
from test.ailens.fixture_repo import FixtureRepo

commit = Revision.commit
INDEX = Revision.index()
WORKING_TREE = Revision.working_tree()


def open_repo(fixture: FixtureRepo) -> GitRepository:
    return GitRepository(str(fixture.path), env=fixture.env)


def build_forked(fixture: FixtureRepo) -> GitRepository:
    fixture.commit("Initial", {"a.ts": "1\n2\n3\n"}, label="base")
    fixture.tag("v1")
    fixture.branch("feature")
    fixture.commit("Feature work", {"a.ts": "1\ntwo\n3\n"}, label="feature")
    fixture.checkout("main")
    fixture.commit("Main work", {"b.ts": "b\n"}, label="main")
    return open_repo(fixture)


def non_context(diff) -> list[GitDiffLine]:
    return [
        line for hunk in diff.hunks for line in hunk.lines if line.type != "context"
    ]


# -- resolve_comparison --


def test_uses_the_base_when_it_is_an_ancestor_of_the_target(fixture_repo):
    repo = build_forked(fixture_repo)
    shas = fixture_repo.shas

    assert resolve_comparison(repo, commit("v1"), commit("feature")) == Comparison(
        base=commit("v1"),
        target=commit("feature"),
        base_sha=shas["base"],
        target_sha=shas["feature"],
        merge_base=shas["base"],
        relation="ancestor",
        from_sha=shas["base"],
        worktree_path=str(fixture_repo.path),
    )


def test_starts_from_the_merge_base_when_the_branches_diverged(fixture_repo):
    repo = build_forked(fixture_repo)
    shas = fixture_repo.shas

    comparison = resolve_comparison(repo, commit("main"), commit("feature"))

    assert (comparison.base_sha, comparison.relation, comparison.from_sha) == (
        shas["main"],
        "diverged",
        shas["base"],
    )


def test_compares_from_a_detached_head(fixture_repo):
    repo = build_forked(fixture_repo)
    fixture_repo.git("checkout", "--quiet", "--detach", fixture_repo.shas["feature"])

    comparison = resolve_comparison(repo, commit("v1"), commit("HEAD"))
    assert (comparison.target_sha, comparison.relation) == (
        fixture_repo.shas["feature"],
        "ancestor",
    )
    uncommitted = resolve_comparison(repo, commit("v1"), WORKING_TREE)
    assert (uncommitted.target_sha, uncommitted.relation) == (
        fixture_repo.shas["feature"],
        "ancestor",
    )


def test_annotated_tags_are_peeled(fixture_repo):
    repo = build_forked(fixture_repo)
    fixture_repo.tag("v2", "feature", message="Release")

    comparison = resolve_comparison(repo, commit("v1"), commit("v2"))

    assert comparison.target_sha == fixture_repo.shas["feature"]


def test_flags_reversed_same_and_unrelated_comparisons(fixture_repo):
    repo = build_forked(fixture_repo)
    shas = fixture_repo.shas

    reversed_ = resolve_comparison(repo, commit("feature"), commit("v1"))
    assert (reversed_.relation, reversed_.from_sha) == ("reversed", shas["feature"])
    same = resolve_comparison(repo, commit("main"), commit(shas["main"]))
    assert (same.relation, same.merge_base) == ("same", shas["main"])

    fixture_repo.git("checkout", "--quiet", "--orphan", "orphan")
    fixture_repo.commit("Unrelated", {"o.txt": "o\n"})
    unrelated = resolve_comparison(repo, commit("main"), commit("orphan"))
    assert (unrelated.relation, unrelated.merge_base, unrelated.from_sha) == (
        "unrelated",
        None,
        shas["main"],
    )


def test_uncommitted_targets_end_at_head(fixture_repo):
    repo = build_forked(fixture_repo)
    shas = fixture_repo.shas

    staged = resolve_comparison(repo, commit("v1"), INDEX)
    assert (staged.target_sha, staged.relation, staged.uncommitted) == (
        shas["main"],
        "ancestor",
        True,
    )
    on_head = resolve_comparison(repo, commit("main"), WORKING_TREE)
    assert (on_head.relation, on_head.from_sha) == ("same", shas["main"])
    diverged = resolve_comparison(repo, commit("feature"), WORKING_TREE)
    assert (diverged.relation, diverged.from_sha) == ("diverged", shas["base"])


def test_index_to_working_tree(fixture_repo):
    repo = build_forked(fixture_repo)

    comparison = resolve_comparison(repo, INDEX, WORKING_TREE)

    assert comparison == Comparison(
        base=INDEX,
        target=WORKING_TREE,
        base_sha=fixture_repo.shas["main"],
        target_sha=fixture_repo.shas["main"],
        merge_base=fixture_repo.shas["main"],
        relation="same",
        from_sha=fixture_repo.shas["main"],
        worktree_path=str(fixture_repo.path),
    )


def test_rejects_unsupported_pairs(fixture_repo, tmp_path):
    repo = build_forked(fixture_repo)
    other = fixture_repo.worktree(tmp_path / "other", "other")

    with pytest.raises(
        AiLensError, match="working tree can only be compared as the target"
    ):
        resolve_comparison(repo, WORKING_TREE, commit("main"))
    with pytest.raises(
        AiLensError, match="working tree can only be compared as the target"
    ):
        resolve_comparison(repo, WORKING_TREE, INDEX)
    with pytest.raises(AiLensError, match="Staged changes can only be the base"):
        resolve_comparison(repo, INDEX, commit("main"))
    with pytest.raises(AiLensError, match="Staged changes can only be the base"):
        resolve_comparison(repo, INDEX, INDEX)
    with pytest.raises(AiLensError, match="same worktree"):
        resolve_comparison(repo, INDEX, Revision.working_tree(str(other)))
    with pytest.raises(AiLensError, match="“missing” isn't a commit"):
        resolve_comparison(repo, commit("missing"), commit("main"))
    with pytest.raises(AiLensError, match="isn't a commit"):
        resolve_comparison(repo, commit("main"), commit("missing"))
    with pytest.raises(AiLensError, match="Pick a branch"):
        resolve_comparison(repo, Revision(RevisionKind.COMMIT), commit("main"))


def test_rejects_worktrees_of_other_repositories(fixture_repo, tmp_path):
    repo = build_forked(fixture_repo)
    stranger = FixtureRepo(tmp_path / "stranger")
    stranger.commit("Initial", {"s": "s\n"})

    with pytest.raises(AiLensError, match="isn't a worktree of this repository"):
        resolve_comparison(
            repo, commit("main"), Revision.working_tree(str(stranger.path))
        )
    with pytest.raises(AiLensError, match="isn't a worktree of this repository"):
        resolve_comparison(
            repo, commit("main"), Revision.index(str(fixture_repo.path / "x"))
        )


def test_rejects_a_missing_worktree(fixture_repo, tmp_path):
    repo = build_forked(fixture_repo)
    other = fixture_repo.worktree(tmp_path / "other", "other")
    shutil.rmtree(other)

    with pytest.raises(AiLensError, match="missing"):
        resolve_comparison(repo, commit("main"), Revision.working_tree(str(other)))


def test_unborn_branches(fixture_repo):
    repo = open_repo(fixture_repo)
    fixture_repo.write_files({"a.txt": "staged\n"})
    fixture_repo.stage()
    fixture_repo.write_files({"a.txt": "staged\nunstaged\n", "u.txt": "u\n"})

    comparison = resolve_comparison(repo, INDEX, WORKING_TREE)
    empty = repo.empty_tree()
    assert (comparison.base_sha, comparison.target_sha, comparison.from_sha) == (
        empty,
        empty,
        empty,
    )
    assert (comparison.relation, comparison.merge_base) == ("same", None)
    assert changed_files(repo, comparison) == [
        GitFileChange("M", "a.txt"),
        GitFileChange("A", "u.txt", untracked=True),
    ]

    fixture_repo.git("checkout", "--quiet", "--orphan", "x")
    with pytest.raises(AiLensError, match="isn't a commit"):
        resolve_comparison(repo, commit("HEAD"), WORKING_TREE)


def test_unborn_worktree_target_with_a_commit_base(fixture_repo, tmp_path):
    repo = build_forked(fixture_repo)
    orphan = tmp_path / "orphan"
    fixture_repo.git(
        "worktree", "add", "--quiet", "--orphan", "-b", "orphan", str(orphan)
    )

    with pytest.raises(AiLensError, match="has no commits yet"):
        resolve_comparison(repo, commit("main"), Revision.index(str(orphan)))


def test_cancelled_resolution(fixture_repo):
    repo = build_forked(fixture_repo)
    cancel = CancelToken()
    cancel.cancel()

    with pytest.raises(Cancelled):
        resolve_comparison(repo, commit("v1"), commit("feature"), cancel)


# -- changed files --


def test_detects_renames_deletes_and_adds(fixture_repo):
    fixture_repo.commit(
        "Initial", {"old.ts": "one\ntwo\nthree\nfour\n", "x.ts": "x\n"}, label="a"
    )
    fixture_repo.commit(
        "Moves",
        {"src/new.ts": "one\ntwo\nthree\nfour\nfive\n", "x.ts": None, "y.ts": "y\n"},
        label="b",
        renames={"old.ts": "src/new.ts"},
    )
    repo = open_repo(fixture_repo)
    comparison = resolve_comparison(
        repo, commit(fixture_repo.shas["a"]), commit(fixture_repo.shas["b"])
    )

    changes = changed_files(repo, comparison)

    assert changes == [
        GitFileChange("R", "src/new.ts", "old.ts", changes[0].similarity),
        GitFileChange("D", "x.ts"),
        GitFileChange("A", "y.ts"),
    ]
    diff = file_diff(repo, comparison, changes[0])
    assert (diff.old_path, diff.new_path, diff.binary) == (
        "old.ts",
        "src/new.ts",
        False,
    )
    assert non_context(diff) == [GitDiffLine("add", "five", None, 5)]
    deleted = file_diff(repo, comparison, changes[1])
    assert (deleted.old_path, deleted.new_path) == ("x.ts", None)


def build_layered(fixture: FixtureRepo) -> GitRepository:
    fixture.commit(
        "Initial", {"a.ts": "1\n2\n", "s.ts": "s\n", ".gitignore": "out/\n"}, label="a"
    )
    fixture.write_files({"a.ts": "1\nstaged\n", "s.ts": None})
    fixture.stage()
    fixture.write_files(
        {
            "a.ts": "1\nstaged\nunstaged\n",
            "new/u.ts": "u\nlast",
            "bin.dat": b"a\0b",
            "out/ignored.js": "x\n",
        }
    )
    return open_repo(fixture)


def test_lists_staged_unstaged_and_untracked_changes(fixture_repo):
    repo = build_layered(fixture_repo)
    base = commit(fixture_repo.shas["a"])

    staged = resolve_comparison(repo, base, INDEX)
    assert changed_files(repo, staged) == [
        GitFileChange("M", "a.ts"),
        GitFileChange("D", "s.ts"),
    ]
    working_tree = resolve_comparison(repo, base, WORKING_TREE)
    assert changed_files(repo, working_tree) == [
        GitFileChange("M", "a.ts"),
        GitFileChange("A", "bin.dat", untracked=True),
        GitFileChange("A", "new/u.ts", untracked=True),
        GitFileChange("D", "s.ts"),
    ]
    unstaged = resolve_comparison(repo, INDEX, WORKING_TREE)
    assert changed_files(repo, unstaged) == [
        GitFileChange("M", "a.ts"),
        GitFileChange("A", "bin.dat", untracked=True),
        GitFileChange("A", "new/u.ts", untracked=True),
    ]


def test_counts_lines_per_file_untracked_ones_too(fixture_repo):
    repo = build_layered(fixture_repo)
    comparison = resolve_comparison(repo, commit(fixture_repo.shas["a"]), WORKING_TREE)

    stats = file_stats(repo, comparison)

    assert sorted(stats, key=lambda stat: stat.path) == [
        GitFileStats("a.ts", 2, 1),
        GitFileStats("bin.dat", None, None),
        GitFileStats("new/u.ts", 2, 0),
        GitFileStats("s.ts", 0, 1),
    ]
    assert changed_files_with_stats(repo, comparison) == (
        changed_files(repo, comparison),
        stats,
    )
    staged = resolve_comparison(repo, commit(fixture_repo.shas["a"]), INDEX)
    assert sorted(file_stats(repo, staged), key=lambda stat: stat.path) == [
        GitFileStats("a.ts", 1, 1),
        GitFileStats("s.ts", 0, 1),
    ]


def test_file_diffs_of_each_layer(fixture_repo):
    repo = build_layered(fixture_repo)
    base = commit(fixture_repo.shas["a"])
    modified = GitFileChange("M", "a.ts")

    staged = file_diff(repo, resolve_comparison(repo, base, INDEX), modified)
    assert non_context(staged) == [
        GitDiffLine("delete", "2", 2, None),
        GitDiffLine("add", "staged", None, 2),
    ]
    working_tree = file_diff(
        repo, resolve_comparison(repo, base, WORKING_TREE), modified
    )
    assert non_context(working_tree) == [
        GitDiffLine("delete", "2", 2, None),
        GitDiffLine("add", "staged", None, 2),
        GitDiffLine("add", "unstaged", None, 3),
    ]
    unstaged = file_diff(repo, resolve_comparison(repo, INDEX, WORKING_TREE), modified)
    assert non_context(unstaged) == [GitDiffLine("add", "unstaged", None, 3)]

    untracked = file_diff(
        repo,
        resolve_comparison(repo, base, WORKING_TREE),
        GitFileChange("A", "new/u.ts", untracked=True),
    )
    assert (untracked.old_path, untracked.new_path) == (None, "new/u.ts")
    assert non_context(untracked)[-1] == GitDiffLine(
        "add", "last", None, 2, no_newline_at_end=True
    )
    binary = file_diff(
        repo,
        resolve_comparison(repo, base, WORKING_TREE),
        GitFileChange("A", "bin.dat", untracked=True),
    )
    assert binary.binary


def test_a_file_without_textual_changes_has_no_diff(fixture_repo):
    fixture_repo.commit("Initial", {"run.sh": "echo\n"}, label="a")
    (fixture_repo.path / "run.sh").chmod(0o755)
    fixture_repo.commit("Executable", label="b")
    repo = open_repo(fixture_repo)
    comparison = resolve_comparison(
        repo, commit(fixture_repo.shas["a"]), commit(fixture_repo.shas["b"])
    )

    [change] = changed_files(repo, comparison)
    diff = file_diff(repo, comparison, change)

    assert change == GitFileChange("M", "run.sh")
    assert (diff.new_path, diff.hunks) == ("run.sh", ())
    assert file_diff(repo, comparison, GitFileChange("M", "unchanged.txt")) is None


def test_another_worktrees_staged_and_working_tree_changes(fixture_repo, tmp_path):
    fixture_repo.commit("Initial", {"a.ts": "1\n2\n"}, label="a")
    other = fixture_repo.worktree(tmp_path / "other", "other")
    fixture_repo.commit("There", {"a.ts": "1\n2\n3\n"}, label="there", cwd=other)
    fixture_repo.write_files({"a.ts": "1\n2\n3\nstaged\n"}, root=other)
    fixture_repo.git("add", "a.ts", cwd=other)
    fixture_repo.write_files(
        {"a.ts": "1\n2\n3\nstaged\nunstaged\n", "u.ts": "u\n"}, root=other
    )
    # Changes in the repository's own worktree must not show up
    fixture_repo.write_files({"a.ts": "mine\n", "mine.ts": "m\n"})
    repo = open_repo(fixture_repo)
    base = commit(fixture_repo.shas["a"])

    staged = resolve_comparison(repo, base, Revision.index(str(other)))
    assert (staged.target_sha, staged.relation, staged.worktree_path) == (
        fixture_repo.shas["there"],
        "ancestor",
        str(other),
    )
    assert changed_files(repo, staged) == [GitFileChange("M", "a.ts")]
    assert non_context(file_diff(repo, staged, GitFileChange("M", "a.ts"))) == [
        GitDiffLine("add", "3", None, 3),
        GitDiffLine("add", "staged", None, 4),
    ]

    working_tree = resolve_comparison(repo, base, Revision.working_tree(str(other)))
    assert changed_files(repo, working_tree) == [
        GitFileChange("M", "a.ts"),
        GitFileChange("A", "u.ts", untracked=True),
    ]
    unstaged = resolve_comparison(
        repo, Revision.index(str(other)), Revision.working_tree(str(other))
    )
    assert unstaged.base_sha == fixture_repo.shas["there"]
    assert non_context(file_diff(repo, unstaged, GitFileChange("M", "a.ts"))) == [
        GitDiffLine("add", "unstaged", None, 5)
    ]


def test_the_repositorys_own_worktree_by_path(fixture_repo):
    repo = build_forked(fixture_repo)
    fixture_repo.write_files({"b.ts": "changed\n"})

    comparison = resolve_comparison(
        repo, Revision.index(str(fixture_repo.path)), WORKING_TREE
    )

    assert comparison.worktree_path == str(fixture_repo.path)
    assert changed_files(repo, comparison) == [GitFileChange("M", "b.ts")]


def test_paths_with_spaces_quotes_unicode_and_newlines(fixture_repo):
    names = ["a b.txt", 'quo"te.txt', "日本/ü.txt", "new\nline.txt", "glob[1]*.txt"]
    fixture_repo.commit("Initial", {name: "1\n" for name in names}, label="a")
    fixture_repo.commit("Edit", {name: "1\n2\n" for name in names}, label="b")
    fixture_repo.write_files({name: "1\n2\n3\n" for name in names})
    fixture_repo.write_files({"untracked ü\n.txt": "u\n"})
    repo = open_repo(fixture_repo)

    committed = resolve_comparison(
        repo, commit(fixture_repo.shas["a"]), commit(fixture_repo.shas["b"])
    )
    changes = changed_files(repo, committed)
    assert sorted(change.path for change in changes) == sorted(names)
    for change in changes:
        diff = file_diff(repo, committed, change)
        assert (diff.old_path, diff.new_path) == (change.path, change.path)
        assert non_context(diff) == [GitDiffLine("add", "2", None, 2)]

    working_tree = resolve_comparison(
        repo, commit(fixture_repo.shas["b"]), WORKING_TREE
    )
    changes = changed_files(repo, working_tree)
    # git's order: by bytes
    assert [change.path for change in changes] == sorted(
        [*names, "untracked ü\n.txt"], key=lambda path: path.encode()
    )
    untracked = next(change for change in changes if change.untracked)
    assert non_context(file_diff(repo, working_tree, untracked)) == [
        GitDiffLine("add", "u", None, 1)
    ]
    stats = {stat.path: stat for stat in file_stats(repo, working_tree)}
    assert stats["new\nline.txt"] == GitFileStats("new\nline.txt", 1, 0)
    assert stats["untracked ü\n.txt"] == GitFileStats("untracked ü\n.txt", 1, 0)


def test_crlf_and_missing_final_newlines_are_kept(fixture_repo):
    fixture_repo.commit("Initial", {"w.txt": "a\r\nb\r\n", "n.txt": "x\ny"}, label="a")
    fixture_repo.commit("Edit", {"w.txt": "a\r\nB\r\n", "n.txt": "x\nz"}, label="b")
    repo = open_repo(fixture_repo)
    comparison = resolve_comparison(
        repo, commit(fixture_repo.shas["a"]), commit(fixture_repo.shas["b"])
    )

    crlf = file_diff(repo, comparison, GitFileChange("M", "w.txt"))
    no_newline = file_diff(repo, comparison, GitFileChange("M", "n.txt"))

    assert non_context(crlf) == [
        GitDiffLine("delete", "b\r", 2, None),
        GitDiffLine("add", "B\r", None, 2),
    ]
    assert non_context(no_newline) == [
        GitDiffLine("delete", "y", 2, None, no_newline_at_end=True),
        GitDiffLine("add", "z", None, 2, no_newline_at_end=True),
    ]


def test_submodules_are_single_entries(fixture_repo):
    fixture_repo.commit("Initial", {"a.ts": "a\n"}, label="a")
    fixture_repo.git("update-index", "--add", "--cacheinfo", f"160000,{'1' * 40},sub")
    fixture_repo.git(
        "commit",
        "--quiet",
        "-m",
        "Adds sub",
        env=fixture_repo._commit_env(("x", "x@x")),
    )
    first = fixture_repo.git("rev-parse", "HEAD").strip()
    fixture_repo.git("update-index", "--cacheinfo", f"160000,{'2' * 40},sub")
    fixture_repo.git(
        "commit",
        "--quiet",
        "-m",
        "Moves sub",
        env=fixture_repo._commit_env(("x", "x@x")),
    )
    repo = open_repo(fixture_repo)

    comparison = resolve_comparison(repo, commit(first), commit("HEAD"))

    assert changed_files(repo, comparison) == [GitFileChange("M", "sub")]
    working_tree = resolve_comparison(repo, commit(first), WORKING_TREE)
    assert [change.path for change in changed_files(repo, working_tree)] == ["sub"]


def test_cancelled_listing(fixture_repo):
    repo = build_layered(fixture_repo)
    comparison = resolve_comparison(repo, commit(fixture_repo.shas["a"]), WORKING_TREE)
    cancel = CancelToken()
    cancel.cancel()

    with pytest.raises(Cancelled):
        changed_files(repo, comparison, cancel)
    with pytest.raises(Cancelled):
        file_stats(repo, comparison, cancel)
    with pytest.raises(Cancelled):
        file_diff(repo, comparison, GitFileChange("M", "a.ts"), cancel)
