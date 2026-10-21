import pytest

from meld.ailens.linemap import new_to_old, old_to_new
from meld.ailens.models import (
    GitBlameLine,
    GitCommit,
    GitDiffHunk,
    GitDiffLine,
    GitFileChange,
    GitFileDiff,
    GitIdentity,
    GitRef,
    GitTrailer,
    GitWorktree,
)
from meld.ailens.parsers import (
    GitFileStats,
    GitParseError,
    log_format,
    parse_blame,
    parse_diff,
    parse_log,
    parse_name_status,
    parse_numstat,
    parse_patch_ids,
    parse_refs,
    parse_rev_list_parents,
    parse_worktrees,
    unquote_git_path,
)


def text(*lines: str) -> str:
    return "\n".join(lines)


def changed(diff: GitFileDiff) -> list[GitDiffLine]:
    return [
        line for hunk in diff.hunks for line in hunk.lines if line.type != "context"
    ]


# -- parse_diff --


def test_diff_numbers_lines_on_both_sides_across_hunks():
    [file] = parse_diff(
        text(
            "diff --git a/src/cache.ts b/src/cache.ts",
            "index 1111111..2222222 100644",
            "--- a/src/cache.ts",
            "+++ b/src/cache.ts",
            "@@ -1,3 +1,4 @@ export class Cache {",
            " a",
            "-b",
            "+B",
            "+B2",
            " c",
            "@@ -10 +11,0 @@",
            "-gone",
            "",
        )
    )

    assert file == GitFileDiff(
        old_path="src/cache.ts",
        new_path="src/cache.ts",
        binary=False,
        hunks=(
            GitDiffHunk(
                1,
                3,
                1,
                4,
                "export class Cache {",
                (
                    GitDiffLine("context", "a", 1, 1),
                    GitDiffLine("delete", "b", 2, None),
                    GitDiffLine("add", "B", None, 2),
                    GitDiffLine("add", "B2", None, 3),
                    GitDiffLine("context", "c", 3, 4),
                ),
            ),
            GitDiffHunk(10, 1, 11, 0, "", (GitDiffLine("delete", "gone", 10, None),)),
        ),
    )


def test_diff_marks_lines_without_a_trailing_newline():
    [file] = parse_diff(
        text(
            "diff --git a/x b/x",
            "--- a/x",
            "+++ b/x",
            "@@ -1 +1 @@",
            "-old",
            "\\ No newline at end of file",
            "+new",
            "\\ No newline at end of file",
            "",
        )
    )

    assert file.hunks[0].lines == (
        GitDiffLine("delete", "old", 1, None, no_newline_at_end=True),
        GitDiffLine("add", "new", None, 1, no_newline_at_end=True),
    )


def test_diff_no_newline_marker_on_a_context_line_mid_file():
    [file] = parse_diff(
        text(
            "diff --git a/x b/x",
            "--- a/x",
            "+++ b/x",
            "@@ -1,2 +1,3 @@",
            " a",
            "-b",
            "\\ No newline at end of file",
            "+b",
            "+c",
            "\\ No newline at end of file",
            "",
        )
    )

    assert [line.no_newline_at_end for line in file.hunks[0].lines] == [
        False,
        True,
        False,
        True,
    ]


def test_diff_reads_hunk_bodies_by_count_so_header_like_content_stays_content():
    files = parse_diff(
        text(
            "diff --git a/x b/x",
            "--- a/x",
            "+++ b/x",
            "@@ -0,0 +1,4 @@",
            "+diff --git a/y b/y",
            "+@@ -1 +1 @@",
            "+--- a/z",
            "+Binary files a and b differ",
            "",
        )
    )

    assert len(files) == 1
    assert files[0].binary is False
    assert [line.content for line in files[0].hunks[0].lines] == [
        "diff --git a/y b/y",
        "@@ -1 +1 @@",
        "--- a/z",
        "Binary files a and b differ",
    ]


def test_diff_handles_added_deleted_renamed_and_binary_files():
    files = parse_diff(
        text(
            "diff --git a/new.ts b/new.ts",
            "new file mode 100644",
            "--- /dev/null",
            "+++ b/new.ts",
            "@@ -0,0 +1 @@",
            "+x",
            "diff --git a/old.ts b/old.ts",
            "deleted file mode 100644",
            "--- a/old.ts",
            "+++ /dev/null",
            "@@ -1 +0,0 @@",
            "-x",
            "diff --git a/a.ts b/b.ts",
            "similarity index 100%",
            "rename from a.ts",
            "rename to b.ts",
            "diff --git a/img.png b/img.png",
            "index 1111111..2222222 100644",
            "Binary files a/img.png and b/img.png differ",
            "diff --git a/new.png b/new.png",
            "new file mode 100644",
            "index 0000000..2222222",
            "Binary files /dev/null and b/new.png differ",
            "",
        )
    )

    assert [(f.old_path, f.new_path, f.binary, len(f.hunks)) for f in files] == [
        (None, "new.ts", False, 1),
        ("old.ts", None, False, 1),
        ("a.ts", "b.ts", False, 0),
        # Paths from the diff --git line (ai_lens leaves them unset)
        ("img.png", "img.png", True, 0),
        (None, "new.png", True, 0),
    ]


def test_diff_empty_files_and_mode_changes_take_paths_from_the_header():
    files = parse_diff(
        text(
            "diff --git a/empty file b/empty file",
            "new file mode 100644",
            "index 0000000..e69de29",
            "diff --git a/gone b/gone",
            "deleted file mode 100644",
            "index e69de29..0000000",
            "diff --git a/run.sh b/run.sh",
            "old mode 100644",
            "new mode 100755",
            "",
        )
    )

    assert [(f.old_path, f.new_path, f.hunks) for f in files] == [
        (None, "empty file", ()),
        ("gone", None, ()),
        ("run.sh", "run.sh", ()),
    ]


def test_diff_copies():
    [file] = parse_diff(
        text(
            "diff --git a/a.txt b/c.txt",
            "similarity index 90%",
            "copy from a.txt",
            "copy to c.txt",
            "--- a/a.txt",
            "+++ b/c.txt",
            "@@ -1 +1 @@",
            "-a",
            "+c",
            "",
        )
    )

    assert (file.old_path, file.new_path) == ("a.txt", "c.txt")


def test_diff_unquotes_paths_and_drops_the_tab_after_paths_with_spaces():
    [file] = parse_diff(
        text(
            'diff --git "a/t\\303\\244b\\tx" "b/my file"',
            '--- "a/t\\303\\244b\\tx"',
            "+++ b/my file\t",
            "@@ -1 +1 @@",
            "-a",
            "+b",
            "",
        )
    )

    assert file.old_path == "täb\tx"
    assert file.new_path == "my file"


def test_diff_quoted_header_paths_for_binary_files():
    [file] = parse_diff(
        text(
            'diff --git "a/new\\nline.bin" "b/new\\nline.bin"',
            "index 1111111..2222222 100644",
            'Binary files "a/new\\nline.bin" and "b/new\\nline.bin" differ',
            "",
        )
    )

    assert (file.old_path, file.new_path, file.binary) == (
        "new\nline.bin",
        "new\nline.bin",
        True,
    )


def test_diff_keeps_carriage_returns_and_odd_whitespace_in_content():
    [file] = parse_diff(
        text(
            "diff --git a/w.txt b/w.txt",
            "--- a/w.txt",
            "+++ b/w.txt",
            "@@ -1,2 +1,2 @@",
            "-a\r",
            "-form\x0cfeed sep",
            "+a",
            "+form\x0cfeed sep\r",
            "",
        )
    )

    assert [line.content for line in changed(file)] == [
        "a\r",
        "form\x0cfeed sep",
        "a",
        "form\x0cfeed sep\r",
    ]


def test_diff_blank_context_lines():
    [file] = parse_diff(
        text(
            "diff --git a/x b/x",
            "--- a/x",
            "+++ b/x",
            "@@ -1,3 +1,3 @@",
            " ",
            "-a",
            "+b",
            " ",
            "",
        )
    )

    assert [line.content for line in file.hunks[0].lines] == ["", "a", "b", ""]


def test_diff_rejects_hunks_shorter_than_their_header_says():
    with pytest.raises(GitParseError, match="Truncated|Unexpected"):
        parse_diff(text("diff --git a/x b/x", "@@ -1,2 +1,2 @@", " a", ""))


def test_diff_rejects_bad_lines_and_headers():
    with pytest.raises(GitParseError, match="Unexpected git diff line"):
        parse_diff(text("diff --git a/x b/x", "@@ -1 +1 @@", "?a", "+b", ""))
    with pytest.raises(GitParseError, match="hunk header"):
        parse_diff(text("diff --git a/x b/x", "@@ nonsense @@", ""))


def test_diff_of_nothing():
    assert parse_diff("") == []


def test_unquote_leaves_unquoted_paths_alone():
    assert unquote_git_path("src/a b.ts") == "src/a b.ts"
    assert unquote_git_path('"') == '"'


def test_unquote_decodes_escapes_and_utf8_octal_bytes():
    assert unquote_git_path('"a\\"b\\\\c\\n\\346\\227\\245"') == 'a"b\\c\n日'
    assert unquote_git_path('"\\a\\b\\f\\r\\t\\v"') == "\a\b\f\r\t\v"


def test_unquote_keeps_invalid_utf8_as_surrogates():
    value = unquote_git_path('"\\377x"')
    assert value.encode("utf-8", "surrogateescape") == b"\xffx"


# -- parse_blame --

BASE = "1f15d8ef7070dc0f28bf266db22c37c813d56c4d"
CHANGE = "c5872d2919c20e8f16dedc70223500b70e1d4a94"


def commit_info(summary: str) -> list[str]:
    return [
        "author a",
        "author-mail <a@a>",
        "author-time 1791446524",
        "author-tz +0200",
        "committer a",
        "committer-mail <a@a>",
        "committer-time 1791446524",
        "committer-tz +0200",
        f"summary {summary}",
    ]


def test_blame_reads_boundary_and_filename_once_per_commit():
    lines = parse_blame(
        text(
            f"{BASE} 1 1 1",
            *commit_info("base"),
            "boundary",
            "filename old.txt",
            "\tl1",
            f"{CHANGE} 5 2 1",
            *commit_info("replace l6"),
            f"previous {BASE} old.txt",
            "filename new.txt",
            "\tX",
            f"{BASE} 3 3 2",
            "\tl3",
            f"{BASE} 4 4",
            "\tl4",
            "",
        )
    )

    assert lines == [
        GitBlameLine(BASE, 1, 1, True, "old.txt"),
        GitBlameLine(CHANGE, 5, 2, False, "new.txt"),
        GitBlameLine(BASE, 3, 3, True, "old.txt"),
        GitBlameLine(BASE, 4, 4, True, "old.txt"),
    ]


def test_blame_sorts_by_final_line():
    lines = parse_blame(
        text(
            f"{CHANGE} 2 2 1",
            "filename f",
            "\tb",
            f"{BASE} 1 1 1",
            "filename f",
            "\ta",
            "",
        )
    )

    assert [line.final_line for line in lines] == [1, 2]


def test_blame_keeps_content_that_looks_like_a_header():
    lines = parse_blame(
        text(f"{CHANGE} 1 1 1", *commit_info("x"), "filename f", f"\t{BASE} 1 1 1", "")
    )

    assert len(lines) == 1


def test_blame_unquotes_filenames():
    [line] = parse_blame(text(f"{CHANGE} 1 1 1", 'filename "a\\nb \\"q\\""', "\tx", ""))

    assert line.filename == 'a\nb "q"'


def test_blame_rejects_truncated_and_garbled_output():
    with pytest.raises(GitParseError, match="Truncated git blame output"):
        parse_blame(f"{CHANGE} 1 1 1\nfilename f\n")
    with pytest.raises(GitParseError, match="Unexpected git blame line"):
        parse_blame("nonsense\n")


def test_blame_of_nothing():
    assert parse_blame("") == []


# -- parse_log --


def record(
    sha="a" * 40,
    parents="",
    subject="Subject",
    body="",
    trailers="",
    note: str | None = None,
) -> str:
    fields = [
        sha,
        parents,
        "Jane Doe",
        "jane@example.com",
        "2023-11-14T22:13:20+00:00",
        "Jane Doe",
        "jane@example.com",
        "2023-11-14T22:14:20+00:00",
        subject,
        body,
        trailers,
    ]
    if note is not None:
        fields.append(note)
    return "\0".join(fields)


def test_log_parses_records_parents_and_trailers():
    output = (
        record(
            sha="b" * 40,
            parents=f"{'a' * 40} {'c' * 40}",
            subject="Makes load async",
            body=(
                "Why: the cache needs I/O\n\n"
                "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>\n"
            ),
            trailers=(
                "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
                "\x1fSigned-off-by: Jane Doe <jane@example.com>"
            ),
        )
        + "\0"
        + record()
        + "\0"
    )

    commits = parse_log(output)

    assert len(commits) == 2
    assert commits[0] == GitCommit(
        sha="b" * 40,
        parents=("a" * 40, "c" * 40),
        author=GitIdentity("Jane Doe", "jane@example.com", "2023-11-14T22:13:20+00:00"),
        committer=GitIdentity(
            "Jane Doe", "jane@example.com", "2023-11-14T22:14:20+00:00"
        ),
        subject="Makes load async",
        body="Why: the cache needs I/O\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>",
        trailers=(
            GitTrailer("Co-Authored-By", "Claude Opus 5.5 <noreply@anthropic.com>"),
            GitTrailer("Signed-off-by", "Jane Doe <jane@example.com>"),
        ),
    )
    assert commits[1].parents == ()
    assert commits[1].trailers == ()
    assert commits[1].body == ""
    assert commits[1].note is None


def test_log_reads_the_note_trimmed():
    [commit] = parse_log(record(note="Why: because\n\n") + "\0", with_notes=True)
    assert commit.note == "Why: because"

    [commit] = parse_log(record(note="") + "\0", with_notes=True)
    assert commit.note is None


def test_log_skips_trailer_lines_without_a_key():
    [commit] = parse_log(record(trailers=": x\x1fno colon\x1fKey: v") + "\0")

    assert commit.trailers == (GitTrailer("Key", "v"),)


def test_log_returns_nothing_for_empty_output():
    assert parse_log("") == []


def test_log_rejects_output_with_a_partial_record():
    with pytest.raises(GitParseError, match="not a multiple"):
        parse_log("abc\0def\0")


def test_log_format_adds_notes_only_when_asked():
    assert "%N" not in log_format()
    assert log_format(True).endswith("%x00%N")
    assert log_format().startswith("--format=%H%x00%P")


# -- parse_name_status, parse_numstat --


def test_name_status_parses_z_output_with_renames_and_copies():
    output = "M\0src/a.ts\0R087\0old name.ts\0src/new.ts\0A\0b.ts\0D\0c.ts\0C100\0x\0y\0T\0l\0"

    assert parse_name_status(output) == [
        GitFileChange("M", "src/a.ts"),
        GitFileChange("R", "src/new.ts", "old name.ts", 87),
        GitFileChange("A", "b.ts"),
        GitFileChange("D", "c.ts"),
        GitFileChange("C", "y", "x", 100),
        GitFileChange("T", "l"),
    ]


def test_name_status_keeps_newlines_and_tabs_in_paths():
    assert parse_name_status("M\0a\nb\tc\0") == [GitFileChange("M", "a\nb\tc")]


def test_name_status_rejects_unknown_statuses_and_truncated_entries():
    with pytest.raises(GitParseError, match="Unexpected"):
        parse_name_status("Q\0a\0")
    with pytest.raises(GitParseError, match="Truncated"):
        parse_name_status("R100\0a\0")
    with pytest.raises(GitParseError, match="Truncated"):
        parse_name_status("M\0")


def test_numstat_reads_counts_binary_files_and_renames():
    output = "3\t1\tsrc/a.ts\0-\t-\timg.png\x002\t0\t\0old.ts\0src/new.ts\0"

    assert parse_numstat(output) == [
        GitFileStats("src/a.ts", 3, 1),
        GitFileStats("img.png", None, None),
        GitFileStats("src/new.ts", 2, 0),
    ]
    assert parse_numstat(output)[1].binary


def test_numstat_keeps_newlines_in_paths():
    assert parse_numstat("1\t0\ta\nb\0") == [GitFileStats("a\nb", 1, 0)]


def test_numstat_rejects_malformed_entries():
    with pytest.raises(GitParseError, match="Unexpected"):
        parse_numstat("x\0")
    with pytest.raises(GitParseError, match="Truncated"):
        parse_numstat("1\t1\t\0old.ts\0")


# -- small parsers --


def test_patch_ids_map_commit_to_patch_id():
    assert parse_patch_ids("p1 c1\np2 c2\n") == {"c1": "p1", "c2": "p2"}
    with pytest.raises(GitParseError):
        parse_patch_ids("p1\n")


def test_rev_list_parents_map_commit_to_parents():
    assert parse_rev_list_parents("m a b\na r\nr\n") == {
        "m": ["a", "b"],
        "a": ["r"],
        "r": [],
    }


# -- parse_refs --


def sha(char: str) -> str:
    return char * 40


def ref_line(*fields: str) -> str:
    return "\0".join(fields) + "\n"


def test_refs_reads_branches_remotes_and_tags_peeling_annotated_tags():
    output = (
        ref_line(
            "refs/heads/main",
            "main",
            "",
            "*",
            "commit",
            sha("a"),
            "Main tip",
            "2024-01-02T00:00:00Z",
            "",
            "",
            "",
            "",
        )
        + ref_line(
            "refs/remotes/origin/main",
            "origin/main",
            "",
            " ",
            "commit",
            sha("b"),
            "Remote tip",
            "2024-01-01T00:00:00Z",
            "",
            "",
            "",
            "",
        )
        + ref_line(
            "refs/tags/v1",
            "v1",
            "",
            " ",
            "tag",
            sha("c"),
            "Release notes",
            "",
            "commit",
            sha("d"),
            "Tagged commit",
            "2023-12-01T00:00:00Z",
        )
    )

    assert parse_refs(output) == [
        GitRef(
            "branch",
            "main",
            "refs/heads/main",
            sha("a"),
            "Main tip",
            "2024-01-02T00:00:00Z",
            current=True,
        ),
        GitRef(
            "remote",
            "origin/main",
            "refs/remotes/origin/main",
            sha("b"),
            "Remote tip",
            "2024-01-01T00:00:00Z",
        ),
        GitRef(
            "tag",
            "v1",
            "refs/tags/v1",
            sha("d"),
            "Tagged commit",
            "2023-12-01T00:00:00Z",
        ),
    ]


def test_refs_skips_symbolic_refs_and_refs_that_do_not_point_at_commits():
    output = (
        ref_line(
            "refs/remotes/origin/HEAD",
            "origin/HEAD",
            "refs/remotes/origin/main",
            " ",
            "commit",
            sha("b"),
            "s",
            "d",
            "",
            "",
            "",
            "",
        )
        + ref_line(
            "refs/tags/tree", "tree", "", " ", "tree", sha("e"), "", "", "", "", "", ""
        )
        + ref_line(
            "refs/tags/blob-tag",
            "blob-tag",
            "",
            " ",
            "tag",
            sha("f"),
            "m",
            "",
            "blob",
            sha("g"),
            "",
            "",
        )
        + ref_line(
            "refs/stash", "stash", "", " ", "commit", sha("h"), "s", "d", "", "", "", ""
        )
    )

    assert parse_refs(output) == []


def test_refs_rejects_lines_with_the_wrong_field_count():
    with pytest.raises(GitParseError):
        parse_refs("refs/heads/main\0main\n")


# -- parse_worktrees --


def test_worktrees_reads_every_attribute():
    output = "\0".join(
        [
            "worktree /repo",
            f"HEAD {sha('a')}",
            "branch refs/heads/main",
            "",
            "worktree /wt/with space\nand newline",
            f"HEAD {sha('b')}",
            "branch refs/heads/feature/x",
            "locked on usb",
            "",
            "worktree /wt/unborn",
            f"HEAD {'0' * 40}",
            "branch refs/heads/orphan",
            "",
            "worktree /wt/gone",
            f"HEAD {sha('c')}",
            "detached",
            "prunable gitdir file points to non-existent location",
            "",
            "",
        ]
    )

    assert parse_worktrees(output) == [
        GitWorktree("/repo", sha("a"), "main", main=True),
        GitWorktree("/wt/with space\nand newline", sha("b"), "feature/x", locked=True),
        GitWorktree("/wt/unborn", None, "orphan"),
        GitWorktree("/wt/gone", sha("c"), None, detached=True, prunable=True),
    ]


def test_worktrees_of_a_bare_repository():
    assert parse_worktrees("worktree /r.git\0bare\0\0") == [
        GitWorktree("/r.git", None, None, bare=True, main=True)
    ]


def test_worktrees_rejects_records_without_a_path():
    with pytest.raises(GitParseError):
        parse_worktrees("HEAD x\0\0")


# -- line maps --

#: old: 1 2 3 4 5 6 7 8 9 -> new: 1 x 2 3 4 6 7 9 y
#: (x added, 5 and 8 deleted, y added at the end)
[LINE_MAP_DIFF] = parse_diff(
    text(
        "diff --git a/f b/f",
        "--- a/f",
        "+++ b/f",
        "@@ -1,2 +1,3 @@",
        " 1",
        "+x",
        " 2",
        "@@ -5 +5,0 @@",
        "-5",
        "@@ -8 +7,0 @@",
        "-8",
        "@@ -9,0 +9 @@",
        "+y",
        "",
    )
)


def test_new_to_old_with_none_for_added_lines():
    assert [new_to_old(LINE_MAP_DIFF, line) for line in range(1, 10)] == [
        1,
        None,
        2,
        3,
        4,
        6,
        7,
        9,
        None,
    ]


def test_old_to_new_with_none_for_deleted_lines():
    assert [old_to_new(LINE_MAP_DIFF, line) for line in range(1, 10)] == [
        1,
        3,
        4,
        5,
        None,
        6,
        7,
        None,
        8,
    ]


def test_line_maps_beyond_the_last_hunk_shift():
    [diff] = parse_diff(
        text(
            "diff --git a/f b/f", "--- a/f", "+++ b/f", "@@ -1 +1,2 @@", " a", "+b", ""
        )
    )

    assert new_to_old(diff, 10) == 9
    assert old_to_new(diff, 9) == 10


def test_line_maps_nothing_in_a_file_that_is_all_added_or_all_deleted():
    [added] = parse_diff(
        "diff --git a/f b/f\nnew file mode 100644\n--- /dev/null\n+++ b/f\n@@ -0,0 +1,2 @@\n+a\n+b\n"
    )
    assert new_to_old(added, 1) is None
    assert new_to_old(added, 2) is None

    [deleted] = parse_diff(
        "diff --git a/f b/f\ndeleted file mode 100644\n--- a/f\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-a\n-b\n"
    )
    assert old_to_new(deleted, 1) is None
    assert old_to_new(deleted, 2) is None


def test_line_maps_without_hunks_are_identity():
    diff = GitFileDiff("f", "f", False, ())

    assert new_to_old(diff, 3) == 3
    assert old_to_new(diff, 3) == 3
