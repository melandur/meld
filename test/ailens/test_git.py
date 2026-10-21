import os
import sys
import threading
import time

import pytest

from meld.ailens.git import GitError, GitNotFound, GitRepository
from meld.ailens.models import (
    AiLensError,
    Cancelled,
    GitCommit,
    GitDiffLine,
    GitFileChange,
    GitIdentity,
    GitTrailer,
)
from meld.ailens.parsers import GitFileStats
from meld.ailens.process import CancelToken
from test.ailens.fixture_repo import HUMAN, FixtureRepo, co_authored_by_claude


def open_repo(fixture: FixtureRepo, **kwargs) -> GitRepository:
    return GitRepository(str(fixture.path), env=fixture.env, **kwargs)


def build_forked(fixture: FixtureRepo) -> GitRepository:
    fixture.commit("Initial", {"a.ts": "1\n2\n3\n"}, label="base")
    fixture.tag("v1")
    fixture.branch("feature")
    fixture.commit(
        "Feature work",
        {"a.ts": "1\ntwo\n3\n"},
        label="feature",
        trailers=[co_authored_by_claude("Opus 5.5")],
    )
    fixture.checkout("main")
    fixture.commit("Main work", {"b.ts": "b\n"}, label="main")
    return open_repo(fixture)


def non_context(diff) -> list[GitDiffLine]:
    return [
        line for hunk in diff.hunks for line in hunk.lines if line.type != "context"
    ]


# -- revisions --


def test_resolves_branches_tags_and_short_shas(fixture_repo):
    repo = build_forked(fixture_repo)
    shas = fixture_repo.shas
    fixture_repo.tag("v2", shas["feature"], message="Release")

    assert repo.resolve_commit("feature") == shas["feature"]
    assert repo.resolve_commit("v1") == shas["base"]
    # Annotated tags are peeled to their commit
    assert repo.resolve_commit("v2") == shas["feature"]
    assert repo.resolve_commit(shas["main"][:7]) == shas["main"]
    assert repo.resolve_commit("HEAD") == shas["main"]
    assert repo.resolve_commit("main~1") == shas["base"]


def test_resolve_commit_returns_none_for_anything_else(fixture_repo):
    repo = build_forked(fixture_repo)
    tree = fixture_repo.git("rev-parse", "HEAD^{tree}").strip()

    assert repo.resolve_commit("missing") is None
    assert repo.resolve_commit("") is None
    assert repo.resolve_commit(tree) is None
    # Never taken for an option
    assert repo.resolve_commit("--all") is None


def test_short_sha(fixture_repo):
    repo = build_forked(fixture_repo)
    sha = fixture_repo.shas["main"]

    assert repo.short_sha(sha) == sha[:7]


def test_head_is_none_on_an_unborn_branch_and_follows_detached_heads(fixture_repo):
    repo = open_repo(fixture_repo)
    assert repo.head() is None

    build_forked(fixture_repo)
    assert repo.head() == fixture_repo.shas["main"]

    fixture_repo.git("checkout", "--quiet", "--detach", fixture_repo.shas["feature"])
    assert repo.head() == fixture_repo.shas["feature"]


def test_merge_base_and_ancestry(fixture_repo):
    repo = build_forked(fixture_repo)
    shas = fixture_repo.shas

    assert repo.merge_base(shas["main"], shas["feature"]) == shas["base"]
    assert repo.is_ancestor(shas["base"], shas["feature"])
    assert not repo.is_ancestor(shas["main"], shas["feature"])

    fixture_repo.git("checkout", "--quiet", "--orphan", "orphan")
    unrelated = fixture_repo.commit("Unrelated", {"o.txt": "o\n"})
    assert repo.merge_base(shas["main"], unrelated) is None
    assert not repo.is_ancestor(shas["main"], unrelated)


def test_empty_tree_and_git_dir(fixture_repo):
    repo = open_repo(fixture_repo)

    assert repo.empty_tree() == "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    assert repo.git_dir() == str(fixture_repo.path / ".git")
    assert repo.is_shallow() is False


# -- discovery and errors --


def test_discover_from_a_folder_or_file_inside_the_worktree(fixture_repo):
    fixture_repo.commit("Initial", {"src/deep/a.ts": "a\n"})
    env = fixture_repo.env

    assert GitRepository.discover(
        str(fixture_repo.path / "src" / "deep"), env=env
    ).root == str(fixture_repo.path)
    assert GitRepository.discover(
        str(fixture_repo.path / "src" / "deep" / "a.ts"), env=env
    ).root == str(fixture_repo.path)


def test_discover_in_a_linked_worktree_gives_that_worktree(fixture_repo, tmp_path):
    fixture_repo.commit("Initial", {"src/a.ts": "a\n"})
    other = fixture_repo.worktree(tmp_path / "other", "other")

    repo = GitRepository.discover(str(other / "src"), env=fixture_repo.env)

    assert repo.root == str(other)
    assert [worktree.path for worktree in repo.worktrees()] == [
        str(fixture_repo.path),
        str(other),
    ]


def test_discover_outside_a_repository(fixture_repo, tmp_path):
    outside = tmp_path / "outside"
    outside.mkdir()
    env = dict(fixture_repo.env, GIT_CEILING_DIRECTORIES=str(tmp_path))

    with pytest.raises(AiLensError, match="isn't in a Git repository"):
        GitRepository.discover(str(outside), env=env)
    with pytest.raises(AiLensError, match="doesn't exist"):
        GitRepository.discover(str(tmp_path / "missing"), env=env)


def test_discover_rejects_bare_repositories_and_git_folders(fixture_repo, tmp_path):
    fixture_repo.commit("Initial", {"a.ts": "a\n"})
    bare = tmp_path / "bare.git"
    fixture_repo.git("init", "--quiet", "--bare", str(bare))

    with pytest.raises(AiLensError, match="bare"):
        GitRepository.discover(str(bare), env=fixture_repo.env)
    with pytest.raises(AiLensError, match="inside a Git folder"):
        GitRepository.discover(str(fixture_repo.path / ".git"), env=fixture_repo.env)


def test_git_missing_is_a_clear_error(fixture_repo, tmp_path):
    repo = open_repo(fixture_repo, git_path=str(tmp_path / "no" / "git"))

    with pytest.raises(GitNotFound, match="Git wasn't found at"):
        repo.head()
    with pytest.raises(AiLensError, match="Git wasn't found at"):
        GitRepository.discover(
            str(fixture_repo.path),
            git_path=str(tmp_path / "no" / "git"),
            env=fixture_repo.env,
        )


def test_a_missing_folder_is_not_reported_as_missing_git(fixture_repo, tmp_path):
    repo = GitRepository(str(tmp_path / "gone"), env=fixture_repo.env)

    with pytest.raises(GitError, match="doesn't exist") as info:
        repo.head()
    assert not isinstance(info.value, GitNotFound)


def test_git_errors_carry_stderr_and_exit_code(fixture_repo):
    repo = build_forked(fixture_repo)

    with pytest.raises(GitError) as info:
        repo.run(["rev-parse", "--verify", "no-such-ref"])

    assert info.value.returncode == 128
    assert "Needed a single revision" in info.value.stderr
    assert "Needed a single revision" in str(info.value)


def test_variables_that_point_git_elsewhere_are_dropped(fixture_repo, tmp_path):
    build_forked(fixture_repo)
    env = dict(
        fixture_repo.env,
        GIT_DIR=str(tmp_path / "nowhere"),
        GIT_WORK_TREE=str(tmp_path),
        GIT_INDEX_FILE=str(tmp_path / "index"),
    )

    repo = GitRepository(str(fixture_repo.path), env=env)

    assert repo.head() == fixture_repo.shas["main"]


def test_output_does_not_depend_on_the_users_config(fixture_repo):
    build_forked(fixture_repo)
    for key, value in [
        ("diff.noprefix", "true"),
        ("diff.mnemonicPrefix", "true"),
        ("diff.suppressBlankEmpty", "true"),
        ("core.quotePath", "true"),
        ("color.ui", "always"),
        ("log.showSignature", "true"),
        ("diff.external", "false"),
    ]:
        fixture_repo.git("config", key, value)
    fixture_repo.commit("Unicode", {"ü dir/a.ts": "x\n\ny\n"}, label="u")
    fixture_repo.commit("Edit", {"ü dir/a.ts": "x\n\nz\n"}, label="edit")
    repo = open_repo(fixture_repo)

    [(change, diff)] = repo.commit_diffs(fixture_repo.shas["edit"])

    assert change == GitFileChange("M", "ü dir/a.ts")
    assert (diff.old_path, diff.new_path) == ("ü dir/a.ts", "ü dir/a.ts")
    assert [line.content for line in diff.hunks[0].lines] == ["x", "", "y", "z"]


# -- cancellation --


def test_cancelled_before_a_call(fixture_repo):
    repo = build_forked(fixture_repo)
    cancel = CancelToken()
    cancel.cancel()

    with pytest.raises(Cancelled):
        repo.head(cancel=cancel)
    with pytest.raises(Cancelled):
        repo.read_worktree_file("a.ts", cancel=cancel)


def test_cancelled_during_a_call_kills_git(fixture_repo, tmp_path):
    slow = tmp_path / "slow-git"
    slow.write_text(f"#!{sys.executable}\nimport time\ntime.sleep(30)\n")
    slow.chmod(0o755)
    repo = open_repo(fixture_repo, git_path=str(slow))
    cancel = CancelToken()
    timer = threading.Timer(0.2, cancel.cancel)

    start = time.monotonic()
    timer.start()
    try:
        with pytest.raises(Cancelled):
            repo.log([], cancel=cancel)
    finally:
        timer.cancel()
    assert time.monotonic() - start < 10


# -- refs and worktrees --


def test_lists_branches_remote_branches_and_tags(fixture_repo, tmp_path):
    repo = build_forked(fixture_repo)
    shas = fixture_repo.shas
    fixture_repo.git("update-ref", "refs/remotes/origin/main", shas["base"])
    fixture_repo.git(
        "symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main"
    )
    fixture_repo.tag("v2", shas["feature"], message="Release")
    other = fixture_repo.worktree(tmp_path / "wt", "wt-branch", "feature")

    refs = repo.refs()

    assert [
        (ref.type, ref.name, ref.sha, ref.current, ref.worktree) for ref in refs
    ] == [
        ("branch", "feature", shas["feature"], False, None),
        ("branch", "main", shas["main"], True, str(fixture_repo.path)),
        ("branch", "wt-branch", shas["feature"], False, str(other)),
        ("remote", "origin/main", shas["base"], False, None),
        ("tag", "v1", shas["base"], False, None),
        ("tag", "v2", shas["feature"], False, None),
    ]
    v2 = next(ref for ref in refs if ref.name == "v2")
    assert v2.subject == "Feature work"
    assert v2.full_name == "refs/tags/v2"
    assert v2.date == "2023-11-14T22:14:20Z"


def test_refs_with_a_detached_head_have_no_current_branch(fixture_repo):
    repo = build_forked(fixture_repo)
    fixture_repo.git("checkout", "--quiet", "--detach", fixture_repo.shas["feature"])

    assert not any(ref.current for ref in repo.refs())


def test_refs_and_worktrees_of_an_unborn_repository(fixture_repo):
    repo = open_repo(fixture_repo)

    assert repo.refs() == []
    [worktree] = repo.worktrees()
    assert (worktree.path, worktree.head, worktree.branch, worktree.main) == (
        str(fixture_repo.path),
        None,
        "main",
        True,
    )


def test_worktrees(fixture_repo, tmp_path):
    repo = build_forked(fixture_repo)
    other = fixture_repo.worktree(tmp_path / "other wt", "other", "feature")
    fixture_repo.git(
        "worktree", "add", "--quiet", "--detach", str(tmp_path / "detached"), "v1"
    )

    worktrees = repo.worktrees()

    # The main worktree first, the others in git's order
    assert [(w.path, w.head, w.branch, w.detached, w.main) for w in worktrees] == [
        (str(fixture_repo.path), fixture_repo.shas["main"], "main", False, True),
        (
            os.path.realpath(tmp_path / "detached"),
            fixture_repo.shas["base"],
            None,
            True,
            False,
        ),
        (str(other), fixture_repo.shas["feature"], "other", False, False),
    ]


# -- commits --


def test_reads_commits_with_parents_authors_and_trailers(fixture_repo):
    repo = build_forked(fixture_repo)
    shas = fixture_repo.shas
    identity = GitIdentity(HUMAN[0], HUMAN[1], "2023-11-14T22:14:20Z")

    assert repo.log([f"{shas['base']}..feature"]) == [
        GitCommit(
            sha=shas["feature"],
            parents=(shas["base"],),
            author=identity,
            committer=identity,
            subject="Feature work",
            body="Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>",
            trailers=(
                GitTrailer("Co-Authored-By", "Claude Opus 5.5 <noreply@anthropic.com>"),
            ),
        )
    ]


def test_pages_through_commits_across_all_refs(fixture_repo):
    repo = build_forked(fixture_repo)

    every = repo.log([], all_refs=True)
    page = repo.log([], all_refs=True, max_count=1, skip=1)

    assert {commit.sha for commit in every} == set(fixture_repo.shas.values())
    assert len(every) == 3
    assert [commit.sha for commit in page] == [every[1].sha]


def test_all_refs_include_a_detached_head(fixture_repo):
    repo = build_forked(fixture_repo)
    fixture_repo.git("checkout", "--quiet", "--detach")
    loose = fixture_repo.commit("Detached work", {"d.txt": "d\n"})

    assert loose in {commit.sha for commit in repo.log([], all_refs=True)}


def test_log_searches_message_and_author(fixture_repo):
    fixture_repo.commit(
        "Fix bug", {"a": "1\n"}, author=("Alice Smith", "alice@example.com")
    )
    fixture_repo.commit("Alice's idea", {"a": "2\n"}, author=("Bob", "bob@example.com"))
    fixture_repo.commit(
        "Other [work]", {"a": "3\n"}, author=("Carol", "carol@example.com")
    )
    repo = open_repo(fixture_repo)

    def subjects(**kwargs):
        return [commit.subject for commit in repo.log([], all_refs=True, **kwargs)]

    assert subjects(grep="alice") == ["Alice's idea", "Fix bug"]
    assert subjects(grep="BOB@EXAMPLE") == ["Alice's idea"]
    # A fixed string, not a pattern
    assert subjects(grep="[work]") == ["Other [work]"]
    assert subjects(grep="alice", max_count=1, skip=1) == ["Fix bug"]
    assert subjects(grep="nobody") == []


def test_log_on_an_unborn_branch_is_empty(fixture_repo):
    repo = open_repo(fixture_repo)

    assert repo.log([]) == []
    assert repo.log([], all_refs=True) == []


def test_commits_in_the_order_given(fixture_repo):
    repo = build_forked(fixture_repo)
    shas = fixture_repo.shas

    commits = repo.commits([shas["base"], shas["main"], shas["feature"], shas["base"]])

    assert [commit.sha for commit in commits] == [
        shas["base"],
        shas["main"],
        shas["feature"],
    ]
    assert repo.commits([]) == []
    assert repo.commit("feature").subject == "Feature work"
    with pytest.raises(AiLensError, match="isn't a commit"):
        repo.commit("missing")


def test_parents_and_commits_touching(fixture_repo):
    repo = build_forked(fixture_repo)
    shas = fixture_repo.shas
    merge = fixture_repo.merge("feature", "Merges feature", label="merge")

    parents = repo.parents([f"{shas['base']}..{merge}"])
    assert parents == {
        merge: [shas["main"], shas["feature"]],
        shas["main"]: [shas["base"]],
        shas["feature"]: [shas["base"]],
    }
    assert repo.commits_touching([f"{shas['base']}..{merge}"], ["a.ts"]) == [
        shas["feature"]
    ]
    assert repo.commits_touching([merge], ["b.ts"]) == [shas["main"]]


# -- notes --


def test_writes_notes_and_reads_them_back_only_from_the_requested_ref(fixture_repo):
    repo = build_forked(fixture_repo)
    shas = fixture_repo.shas
    repo.write_note(shas["feature"], "Why: first")
    repo.write_note(shas["feature"], "Why: replaced\n\n")
    fixture_repo.git("notes", "add", "-m", "default ref", shas["main"])

    assert repo.read_note(shas["feature"]) == "Why: replaced"
    assert repo.read_note(shas["feature"], "refs/notes/ai-lens") == "Why: replaced"
    assert repo.read_note(shas["main"]) is None
    assert repo.read_note(shas["main"], "commits") == "default ref"

    with_notes = {c.sha: c for c in repo.log([], all_refs=True, notes_ref="ai-lens")}
    assert with_notes[shas["feature"]].note == "Why: replaced"
    assert with_notes[shas["main"]].note is None

    full = {
        c.sha: c for c in repo.log([], all_refs=True, notes_ref="refs/notes/ai-lens")
    }
    assert full[shas["feature"]].note == "Why: replaced"

    assert all(commit.note is None for commit in repo.log([], all_refs=True))
    assert repo.commit(shas["feature"], notes_ref="ai-lens").note == "Why: replaced"
    assert (
        repo.commits([shas["feature"]], notes_ref="ai-lens")[0].note == "Why: replaced"
    )


def test_notes_keep_unicode(fixture_repo):
    repo = build_forked(fixture_repo)
    repo.write_note(fixture_repo.shas["main"], "Warum: ü → 日本", notes_ref="other")

    assert (
        repo.read_note(fixture_repo.shas["main"], notes_ref="other")
        == "Warum: ü → 日本"
    )
    assert repo.read_note(fixture_repo.shas["main"]) is None


# -- patch IDs --


def test_gives_the_same_patch_id_to_the_same_change_on_different_branches(fixture_repo):
    fixture_repo.commit("Initial", {"a.ts": "1\n2\n3\n"}, label="base")
    fixture_repo.branch("side")
    fixture_repo.commit("Change", {"a.ts": "1\n2\nthree\n"}, label="side")
    fixture_repo.checkout("main")
    fixture_repo.commit("Other", {"b.ts": "b\n"}, label="other")
    fixture_repo.commit("Same change", {"a.ts": "1\n2\nthree\n"}, label="main")
    fixture_repo.merge("side", "Merges side", label="merge")
    empty = fixture_repo.commit("Empty")
    repo = open_repo(fixture_repo)
    shas = fixture_repo.shas

    ids = repo.patch_ids(
        [shas["side"], shas["main"], shas["other"], shas["merge"], empty]
    )

    assert len(ids[shas["side"]]) == 40
    assert ids[shas["main"]] == ids[shas["side"]]
    assert ids[shas["other"]] != ids[shas["side"]]
    assert shas["merge"] not in ids
    assert empty not in ids
    assert repo.patch_ids([]) == {}


# -- commit diffs --


def test_commit_diffs_of_a_root_commit(fixture_repo):
    root = fixture_repo.commit("Initial", {"a.txt": "a\n", "e.txt": ""})
    repo = open_repo(fixture_repo)

    diffs = repo.commit_diffs(root)

    assert [change for change, _ in diffs] == [
        GitFileChange("A", "a.txt"),
        GitFileChange("A", "e.txt"),
    ]
    assert non_context(diffs[0][1]) == [GitDiffLine("add", "a", None, 1)]
    assert (diffs[1][1].old_path, diffs[1][1].new_path, diffs[1][1].hunks) == (
        None,
        "e.txt",
        (),
    )


def test_commit_diffs_with_renames_deletes_binary_files_and_mode_changes(fixture_repo):
    fixture_repo.commit(
        "Initial",
        {
            "old name.txt": "1\n2\n3\n4\n5\n",
            "gone.txt": "g\n",
            "img.bin": b"a\0b",
            "run.sh": "echo\n",
        },
    )
    (fixture_repo.path / "run.sh").chmod(0o755)
    fixture_repo.write_files({"img.bin": b"a\0c"})
    sha = fixture_repo.commit(
        "Changes",
        {"new “name”.txt": "1\n2\n3\n4\n5\n6\n", "gone.txt": None},
        renames={"old name.txt": "new “name”.txt"},
    )
    repo = open_repo(fixture_repo)

    diffs = {change.path: (change, diff) for change, diff in repo.commit_diffs(sha)}

    assert sorted(diffs) == ["gone.txt", "img.bin", "new “name”.txt", "run.sh"]
    change, diff = diffs["new “name”.txt"]
    assert (change.status, change.original_path) == ("R", "old name.txt")
    assert (diff.old_path, diff.new_path) == ("old name.txt", "new “name”.txt")
    assert non_context(diff) == [GitDiffLine("add", "6", None, 6)]
    change, diff = diffs["gone.txt"]
    assert (change.status, diff.old_path, diff.new_path) == ("D", "gone.txt", None)
    assert diffs["img.bin"][1].binary
    change, diff = diffs["run.sh"]
    assert (change.status, diff.new_path, diff.hunks) == ("M", "run.sh", ())


def test_commit_diffs_of_a_merge_are_against_its_first_parent(fixture_repo):
    repo = build_forked(fixture_repo)
    merge = fixture_repo.merge("feature", "Merges feature")

    assert [change.path for change, _ in repo.commit_diffs(merge)] == ["a.ts"]


# -- uncommitted changes and file contents --


def test_lists_counts_and_reads_staged_unstaged_and_untracked_changes(fixture_repo):
    fixture_repo.commit(
        "Initial", {"a.ts": "1\n2\n", ".gitignore": "out/\n"}, label="a"
    )
    fixture_repo.write_files({"a.ts": "1\nstaged\n"})
    fixture_repo.stage()
    fixture_repo.write_files(
        {
            "a.ts": "1\nstaged\nunstaged\n",
            "new/u.ts": "u\nlast",
            "bin.dat": b"a\0b",
            "out/ignored.js": "x\n",
        }
    )
    repo = open_repo(fixture_repo)
    base = fixture_repo.shas["a"]

    assert repo.name_status([base], cached=True) == [GitFileChange("M", "a.ts")]
    assert repo.name_status([base]) == [GitFileChange("M", "a.ts")]
    assert repo.untracked_files() == ["bin.dat", "new/u.ts"]
    assert repo.numstat([base]) == [GitFileStats("a.ts", 2, 1)]
    assert repo.untracked_stats("bin.dat") == GitFileStats("bin.dat", None, None)
    assert repo.untracked_stats("new/u.ts") == GitFileStats("new/u.ts", 2, 0)

    assert repo.read_index_file("a.ts") == b"1\nstaged\n"
    assert repo.read_worktree_file("a.ts") == b"1\nstaged\nunstaged\n"
    assert repo.read_blob(base, "a.ts") == b"1\n2\n"
    assert repo.read_blob(base, "new/u.ts") is None
    assert repo.read_index_file("new/u.ts") is None

    untracked = repo.untracked_diff("new/u.ts")
    assert (untracked.old_path, untracked.new_path, untracked.binary) == (
        None,
        "new/u.ts",
        False,
    )
    assert untracked.hunks[0].lines == (
        GitDiffLine("add", "u", None, 1),
        GitDiffLine("add", "last", None, 2, no_newline_at_end=True),
    )
    assert repo.untracked_diff("bin.dat").binary

    [staged] = repo.uncommitted_diffs("staged")
    assert non_context(staged) == [
        GitDiffLine("delete", "2", 2, None),
        GitDiffLine("add", "staged", None, 2),
    ]
    [unstaged] = repo.uncommitted_diffs("unstaged")
    assert non_context(unstaged) == [GitDiffLine("add", "unstaged", None, 3)]

    assert repo.ignored(["a.ts", "out/ignored.js", "out/other.js"]) == {
        "out/ignored.js",
        "out/other.js",
    }
    assert repo.ignored(["a.ts"]) == set()
    assert repo.ignored([]) == set()


def test_counts_untracked_lines_like_git(fixture_repo):
    fixture_repo.commit("Initial", {"a.ts": "1\n"})
    many = {f"many/f{index:03}.txt": "x\n" * index for index in range(40)}
    fixture_repo.write_files(
        {
            **many,
            # NUL bytes only after the first 8000 bytes don't make a file binary
            "late-nul.txt": "line\n" * 2000 + "\0\n",
            "empty.txt": "",
            "crlf.txt": "a\r\nb\r\n",
        }
    )
    os.symlink("a.ts", fixture_repo.path / "link")
    repo = open_repo(fixture_repo)

    stats = {path: repo.untracked_stats(path) for path in repo.untracked_files()}

    assert len(stats) == 44
    assert stats["many/f000.txt"] == GitFileStats("many/f000.txt", 0, 0)
    assert stats["many/f039.txt"] == GitFileStats("many/f039.txt", 39, 0)
    assert stats["late-nul.txt"] == GitFileStats("late-nul.txt", 2001, 0)
    assert stats["empty.txt"] == GitFileStats("empty.txt", 0, 0)
    assert stats["crlf.txt"] == GitFileStats("crlf.txt", 2, 0)
    assert stats["link"] == GitFileStats("link", 1, 0)
    # Gone since it was listed
    assert repo.untracked_stats("vanished.txt") == GitFileStats("vanished.txt", 0, 0)


def test_untracked_diff_of_an_empty_file_and_a_symlink(fixture_repo):
    fixture_repo.commit("Initial", {"a.ts": "1\n"})
    fixture_repo.write_files({"empty.txt": ""})
    os.symlink("/etc/passwd", fixture_repo.path / "link")
    repo = open_repo(fixture_repo)

    empty = repo.untracked_diff("empty.txt")
    assert (empty.old_path, empty.new_path, empty.hunks) == (None, "empty.txt", ())
    link = repo.untracked_diff("link")
    assert non_context(link) == [
        GitDiffLine("add", "/etc/passwd", None, 1, no_newline_at_end=True)
    ]
    assert repo.untracked_diff("missing.txt") is None


def test_reads_worktree_files_without_following_links_out(fixture_repo, tmp_path):
    fixture_repo.commit("Initial", {"a.ts": "a\n", "dir/b.ts": "b\n"})
    outside = tmp_path / "outside"
    outside.mkdir()
    (outside / "secret.txt").write_text("secret\n")
    os.symlink(str(outside / "secret.txt"), fixture_repo.path / "file-link")
    os.symlink(str(outside), fixture_repo.path / "dir-link")
    os.symlink("a.ts", fixture_repo.path / "inner-link")
    repo = open_repo(fixture_repo)

    assert repo.read_worktree_file("dir/b.ts") == b"b\n"
    # Links read as their target, as git stores them
    assert repo.read_worktree_file("file-link") == str(outside / "secret.txt").encode()
    assert repo.read_worktree_file("inner-link") == b"a.ts"
    assert repo.read_worktree_file("dir-link/secret.txt") is None
    assert repo.read_worktree_file("dir") is None
    assert repo.read_worktree_file("missing.txt") is None
    assert repo.read_worktree_file("a.ts/x") is None
    for bad in ["../outside/secret.txt", "/etc/passwd", "dir/../a.ts", ""]:
        with pytest.raises(AiLensError):
            repo.read_worktree_file(bad)


def test_reads_files_of_another_worktree(fixture_repo, tmp_path):
    fixture_repo.commit("Initial", {"a.ts": "main\n"})
    other = fixture_repo.worktree(tmp_path / "other", "other")
    fixture_repo.write_files({"a.ts": "staged there\n"}, root=other)
    fixture_repo.git("add", "a.ts", cwd=other)
    fixture_repo.write_files({"a.ts": "on disk there\n"}, root=other)
    repo = open_repo(fixture_repo)

    assert repo.read_index_file("a.ts") == b"main\n"
    assert repo.read_index_file("a.ts", str(other)) == b"staged there\n"
    assert repo.read_worktree_file("a.ts", str(other)) == b"on disk there\n"
    assert repo.head(str(other)) == repo.head()


def test_reads_paths_with_newlines_quotes_and_unicode(fixture_repo):
    names = ["new\nline.txt", 'quo"te.txt', "tab\there.txt", "日本/ü.txt", "-dash.txt"]
    first = fixture_repo.commit("Initial", {"x": "x\n"})
    sha = fixture_repo.commit(
        "Odd names", {name: f"{index}\n" for index, name in enumerate(names)}
    )
    repo = open_repo(fixture_repo)

    assert sorted(change.path for change, _ in repo.commit_diffs(sha)) == sorted(names)
    for index, name in enumerate(names):
        assert repo.read_blob(sha, name) == f"{index}\n".encode()
        assert repo.read_worktree_file(name) == f"{index}\n".encode()
    [(change, diff)] = [
        item for item in repo.commit_diffs(sha) if item[0].path == "new\nline.txt"
    ]
    assert diff.new_path == "new\nline.txt"
    [line] = repo.blame(first, sha, "new\nline.txt")
    assert (line.sha, line.filename, line.boundary) == (sha, "new\nline.txt", False)


def test_blame_of_a_path_the_target_lacks_is_empty(fixture_repo):
    repo = build_forked(fixture_repo)
    shas = fixture_repo.shas

    assert repo.blame(shas["base"], shas["feature"], "nope.ts") == []


def test_meld_environment_git_variables_do_not_leak(
    fixture_repo, monkeypatch, tmp_path
):
    """Meld started by `git difftool` or a hook has GIT_DIR etc. set"""
    from meld.ailens.git import GitRepository

    fixture_repo.commit("Adds a", {"a.txt": "a\n"})
    other = tmp_path / "elsewhere"
    other.mkdir()
    monkeypatch.setenv("GIT_DIR", str(other))
    monkeypatch.setenv("GIT_WORK_TREE", str(other))
    repo = GitRepository(str(fixture_repo.path), env=fixture_repo.env)
    assert repo.head() == fixture_repo.git("rev-parse", "HEAD").strip()
