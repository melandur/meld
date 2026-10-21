"""Ported from AI Lens src/build/__tests__/buildIndex.test.ts, plus edge cases"""

import os
import sys

import pytest

from meld.ailens.build_file import BuildHeader, HeaderCommit
from meld.ailens.build_index import (
    find_builds,
    is_build_file_name,
    read_build_headers,
    resolve_build_folder,
)
from meld.ailens.models import AiLensError


def header(file_name: str, created: str | None, commits: list[HeaderCommit]):
    return BuildHeader(file_name, created=created, commits=commits)


HEADERS = [
    header("a_build.md", "2026-01-01T00:00:00Z", [HeaderCommit("x1", "p1")]),
    header(
        "b_build.md",
        "2026-03-01T00:00:00Z",
        [HeaderCommit("x1", "p1"), HeaderCommit("x2", None)],
    ),
]


# -- Ported --


def test_find_builds_by_sha_newest_first():
    assert find_builds(HEADERS, "x1", None) == ["b_build.md", "a_build.md"]
    assert find_builds(HEADERS, "x2", None) == ["b_build.md"]


def test_find_builds_by_patch_id_for_new_sha():
    assert find_builds(HEADERS, "rebased", "p1") == ["b_build.md", "a_build.md"]
    assert find_builds(HEADERS, "rebased", "other") == []


def test_is_build_file_name_accepts_only_plain_names():
    assert is_build_file_name("a1b2c3d_9f8e7d6_build.md")
    assert not is_build_file_name("../secrets_build.md")
    assert not is_build_file_name("sub/x_build.md")
    assert not is_build_file_name("index.md")


def test_resolve_build_folder_keeps_it_inside(tmp_path):
    # From generateBuild.test.ts
    assert resolve_build_folder("/repo", "agent") == "/repo/agent"
    assert resolve_build_folder("/repo", " docs/agent ") == "/repo/docs/agent"
    with pytest.raises(AiLensError, match="inside the repository"):
        resolve_build_folder("/repo", "../elsewhere")
    with pytest.raises(AiLensError, match="inside the repository"):
        resolve_build_folder("/repo", "/tmp/agent")


# -- find_builds --


def test_find_builds_none_patch_id_does_not_match_missing_ones():
    # A header commit without a patch ID must not match a lookup without one
    assert find_builds(HEADERS, "nope", None) == []


def test_find_builds_same_date_by_name_and_undated_last():
    headers = [
        header("z_build.md", "2026-01-01T00:00:00Z", [HeaderCommit("x")]),
        header("m_build.md", None, [HeaderCommit("x")]),
        header("a_build.md", "2026-01-01T00:00:00Z", [HeaderCommit("x")]),
    ]
    assert find_builds(headers, "x", None) == ["a_build.md", "z_build.md", "m_build.md"]


# -- is_build_file_name --


@pytest.mark.parametrize(
    "name",
    [
        "x_build.md",
        "a b_build.md",
        "été_build.md",
        "x..y_build.md",
    ],
)
def test_build_file_names(name):
    assert is_build_file_name(name)


@pytest.mark.parametrize(
    "name",
    [
        "_build.md",
        ".hidden_build.md",
        "..\\x_build.md",
        "sub\\x_build.md",
        "/abs_build.md",
        "x_build.md\n",
        "x_build.md.tmp",
        "x_build.mdx",
        "x_BUILD.md",
        "",
    ],
)
def test_not_build_file_names(name):
    assert not is_build_file_name(name)


# -- resolve_build_folder --


def test_resolve_build_folder_defaults_and_normalizes():
    assert resolve_build_folder("/repo", "") == "/repo/agent"
    assert resolve_build_folder("/repo", "   ") == "/repo/agent"
    assert resolve_build_folder("/repo", "./a//b/") == "/repo/a/b"
    assert resolve_build_folder("/repo", "a/../b") == "/repo/b"
    assert resolve_build_folder("/repo/", "agent") == "/repo/agent"
    assert resolve_build_folder("/repo", ".") == "/repo"
    assert resolve_build_folder("/repo", "..foo") == "/repo/..foo"


@pytest.mark.parametrize(
    "folder",
    [
        "..",
        "../",
        "../..",
        "a/../../b",
        "a/../..",
        "./../x",
        "/",
        "/etc",
        "//etc",
        " /etc ",
    ],
)
def test_resolve_build_folder_refuses_traversal(folder):
    with pytest.raises(AiLensError, match="inside the repository"):
        resolve_build_folder("/repo", folder)


@pytest.mark.skipif(sys.platform == "win32", reason="symlinks")
def test_resolve_build_folder_refuses_symlinks_out(tmp_path):
    repo = tmp_path / "repo"
    outside = tmp_path / "outside"
    repo.mkdir()
    outside.mkdir()
    (repo / "out").symlink_to(outside)
    (repo / "inner").mkdir()
    (repo / "in").symlink_to(repo / "inner")

    with pytest.raises(AiLensError, match="inside the repository"):
        resolve_build_folder(str(repo), "out")
    with pytest.raises(AiLensError, match="inside the repository"):
        resolve_build_folder(str(repo), "out/agent")
    # A symlink that stays inside is fine
    assert resolve_build_folder(str(repo), "in/agent") == str(repo / "in" / "agent")


@pytest.mark.skipif(sys.platform == "win32", reason="symlinks")
def test_resolve_build_folder_through_symlinked_repo(tmp_path):
    real = tmp_path / "real"
    real.mkdir()
    link = tmp_path / "link"
    link.symlink_to(real)
    assert resolve_build_folder(str(link), "agent") == str(link / "agent")


# -- read_build_headers --


def test_read_build_headers_missing_folder(tmp_path):
    assert read_build_headers(str(tmp_path / "nope")) == []


def test_read_build_headers_reads_builds_in_name_order(tmp_path):
    (tmp_path / "b_build.md").write_text('---\ntitle: "B"\n---\n')
    (tmp_path / "a_build.md").write_text('---\ntitle: "A"\n---\n')
    (tmp_path / "index.md").write_text('---\ntitle: "index"\n---\n')
    (tmp_path / "notes.md").write_text("x")
    # Not UTF-8, no frontmatter, a folder with a build's name: all tolerated
    (tmp_path / "c_build.md").write_bytes(b'---\ntitle: "\xff\xfe"\n---\n')
    (tmp_path / "d_build.md").write_text("# hand-written")
    (tmp_path / "e_build.md").mkdir()

    headers = read_build_headers(str(tmp_path))
    assert [(h.file_name, h.title) for h in headers] == [
        ("a_build.md", "A"),
        ("b_build.md", "B"),
        ("c_build.md", "��"),
        ("d_build.md", None),
    ]


def test_read_build_headers_folder_is_a_file(tmp_path):
    path = tmp_path / "agent"
    path.write_text("not a folder")
    with pytest.raises(AiLensError, match="Could not read the build folder"):
        read_build_headers(str(path))


@pytest.mark.skipif(
    sys.platform == "win32" or os.geteuid() == 0, reason="needs file permissions"
)
def test_read_build_headers_skips_unreadable_files(tmp_path):
    (tmp_path / "a_build.md").write_text('---\ntitle: "A"\n---\n')
    locked = tmp_path / "b_build.md"
    locked.write_text('---\ntitle: "B"\n---\n')
    locked.chmod(0)
    try:
        assert [h.file_name for h in read_build_headers(str(tmp_path))] == [
            "a_build.md"
        ]
    finally:
        locked.chmod(0o644)
