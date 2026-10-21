"""Ported from AI Lens src/build/__tests__/generateBuild.test.ts (the writing
half), plus edge cases

Gathering the facts from a comparison isn't ported yet, so the facts here are
the ones the AI Lens test's fixture repository yields.
"""

import dataclasses
import os
import sys
from datetime import datetime, timezone
from pathlib import Path

import pytest

from meld.ailens.build_facts import (
    BuildChange,
    BuildCommit,
    BuildFacts,
    BuildFile,
    BuildFileEntry,
    BuildRevision,
    BuildStats,
)
from meld.ailens.build_file import render_index
from meld.ailens.build_index import read_build_headers
from meld.ailens.build_narrative import BuildDecision, BuildNarrative
from meld.ailens.generate_build import write_build, write_text_atomic
from meld.ailens.models import AiLensError, LineRange

BASE = "1" * 40
AGENT = "2" * 40
HUMAN = "3" * 40
GENERATOR = "meld-ailens@test"
CREATED = datetime(2026, 10, 8, 12, 0, 0, tzinfo=timezone.utc)


def make_facts() -> BuildFacts:
    return BuildFacts(
        base=BuildRevision(BASE, BASE[:7], None),
        target=BuildRevision(HUMAN, HUMAN[:7], "main"),
        merge_base=BASE,
        relation="ancestor",
        stats=BuildStats(files=5, insertions=5, deletions=2, commits=2),
        commits=[
            BuildCommit(
                sha=AGENT,
                short_sha=AGENT[:7],
                patch_id="4" * 40,
                author="Jane Doe",
                agent="claude-code",
                agent_model="claude-opus-5-5",
                agent_name="Claude Opus 5.5",
                what="Moves and extends the module",
                why="The module grew past its old name.",
                why_source="commit",
                changes_inferred_by="claude-sonnet-5-5",
            ),
            BuildCommit(
                sha=HUMAN,
                short_sha=HUMAN[:7],
                patch_id="5" * 40,
                author="Jane Doe",
                agent="human",
                agent_model=None,
                agent_name=None,
                what="tweak",
                why=None,
                why_source="commit",
            ),
        ],
        files=[
            BuildFile(
                "img.bin", None, "A", None, None, binary=True, touched_by=[AGENT]
            ),
            BuildFile("pnpm-lock.yaml", None, "A", 1, 0, ignored=True),
            BuildFile("secrets/key.txt", None, "M", 1, 1, ignored=True),
            BuildFile(
                "src/module.ts",
                "src/old.ts",
                "R",
                2,
                1,
                entries=[
                    BuildFileEntry(HUMAN, [LineRange(1, 1)], [LineRange(1, 1)]),
                    BuildFileEntry(
                        AGENT,
                        [LineRange(11, 11)],
                        [],
                        [
                            BuildChange(
                                "Adds a closing line",
                                "Marks where the agent work ends.",
                            )
                        ],
                    ),
                ],
                touched_by=[HUMAN, AGENT],
            ),
        ],
    )


NARRATIVE = BuildNarrative(
    title="Renames the module",
    summary="The module was renamed and extended.",
    decisions=[BuildDecision("Rename for clarity.", [])],
    risks=["One commit gives no reason."],
    model="claude-opus-5-5",
)


@pytest.fixture
def repo(tmp_path):
    path = tmp_path / "repo"
    path.mkdir()
    return path


def write(repo, folder="agent", facts=None, created=CREATED, narrative=NARRATIVE):
    return write_build(
        str(repo), folder, facts or make_facts(), narrative, GENERATOR, created
    )


def read(path) -> str:
    with open(path, encoding="utf-8", newline="") as f:
        return f.read()


# -- Ported --


def test_writes_build_and_index_the_same_way_every_time(repo):
    build = write(repo)
    base, target, agent = BASE[:7], HUMAN[:7], AGENT[:7]
    assert build.path == str(repo / "agent" / f"{base}_{target}_build.md")
    assert build.index_path == str(repo / "agent" / "index.md")
    assert build.narrative is NARRATIVE

    text = read(build.path)
    assert 'title: "Renames the module"' in text
    assert f"  sha: {BASE}\n  ref: null" in text
    assert f'  sha: {HUMAN}\n  ref: "main"' in text
    assert 'build_model: "claude-opus-5-5"' in text
    assert f'generator: "{GENERATOR}"' in text
    assert (
        'agent: claude-code\n    agent_model: "claude-opus-5-5"\n    why_source: commit'
        in text
    )
    assert "### src/module.ts (from src/old.ts) · R · +2 \u22121" in text
    assert (
        f"### `{agent}` Moves and extends the module\n\n"
        "- claude-code (claude-opus-5-5) · source: commit\n"
        "- Why: The module grew past its old name."
    ) in text
    assert (
        f"### `{target}` tweak\n\n- human (Jane Doe) · source: commit\n"
        "- Why: not recorded."
    ) in text
    assert (
        f"- **L11** · `{agent}` · Moves and extends the module\n"
        "  - What: Adds a closing line\n"
        "  - Why (inferred by claude-sonnet-5-5, not the author’s words): "
        "Marks where the agent work ends."
    ) in text
    assert f"- **L1**, base **L1** · `{target}` · tweak\n" in text
    assert "### img.bin · A · binary" in text
    assert "### pnpm-lock.yaml · A · +1 \u22120\n\n- Left out" in text
    assert "### secrets/key.txt · M · +1 \u22121\n\n- Left out" in text

    index = read(build.index_path)
    assert (
        f"| [{base}_{target}]({base}_{target}_build.md) | {base} → main | "
        "2026-10-08 | claude-opus-5-5 | Renames the module |"
    ) in index

    # Same facts, same answer, same clock: byte-identical; another clock
    # changes only `created`
    write(repo)
    assert read(build.path) == text
    write(repo, created=datetime(2026, 10, 9, 8, 0, 0, tzinfo=timezone.utc))
    assert read(build.path) == text.replace(
        "created: 2026-10-08T12:00:00Z", "created: 2026-10-09T08:00:00Z"
    )


# -- Edge cases --


def test_index_lists_every_build_and_tolerates_junk(repo):
    folder = repo / "docs" / "agent"
    folder.mkdir(parents=True)
    (folder / "0000000_1111111_build.md").write_text(
        '---\ntitle: "Older"\ncreated: 2026-01-01T00:00:00Z\n'
        "base:\n  sha: 0000000000\n  ref: null\n"
        'target:\n  sha: 1111111111\n  ref: "v1"\n---\n'
    )
    (folder / "junk_build.md").write_bytes(b"\xff\xfe not a build")
    (folder / "dir_build.md").mkdir()
    (folder / "index.md").write_text("stale")

    build = write(repo, "docs/agent")
    index = read(build.index_path)
    assert index == render_index(read_build_headers(str(folder)))
    rows = index.split("\n")[7:-1]
    assert [row.split("](")[0] for row in rows] == [
        f"| [{BASE[:7]}_{HUMAN[:7]}",
        "| [0000000_1111111",
        "| [junk",
    ]
    assert "| 0000000 → v1 | 2026-01-01 |" in rows[1]
    assert rows[2] == "| [junk](junk_build.md) | ? → ? |  |  |  |"


def test_creates_nested_folder(repo):
    build = write(repo, "a/b/c")
    assert os.path.isfile(build.path)
    assert os.path.dirname(build.path) == str(repo / "a" / "b" / "c")


def test_leaves_no_temp_files(repo):
    write(repo)
    write(repo)
    assert sorted(os.listdir(repo / "agent")) == [
        f"{BASE[:7]}_{HUMAN[:7]}_build.md",
        "index.md",
    ]


def test_writes_utf8_with_lf(repo):
    facts = make_facts()
    facts.target = BuildRevision(HUMAN, HUMAN[:7], "féat/\U0001f600")
    narrative = BuildNarrative(
        title='"Quoted" → tïtle\nwith a break',
        summary="S",
        decisions=[],
        risks=[],
        model="m",
    )
    build = write(repo, facts=facts, narrative=narrative)
    data = Path(build.path).read_bytes()
    assert b"\r" not in data
    assert '  ref: "féat/\U0001f600"'.encode() in data
    assert '"\\"Quoted\\" → tïtle\\nwith a break"'.encode() in data


def test_bytes_kept_through_surrogateescape_are_written_back(repo):
    # Git output that isn't UTF-8, decoded with surrogateescape
    facts = make_facts()
    facts.commits[1] = dataclasses.replace(facts.commits[1], author="Jos\udce9")
    data = Path(write(repo, facts=facts).path).read_bytes()
    assert b"- human (Jos\xe9) \xc2\xb7 source" in data
    # YAML escapes it, as JSON.stringify does
    assert b'author: "Jos\\udce9"' in data


def test_other_lone_surrogates_become_replacement_characters(repo):
    facts = make_facts()
    facts.commits[1] = dataclasses.replace(facts.commits[1], what="lone \ud800")
    data = Path(write(repo, facts=facts).path).read_bytes()
    assert "` lone \ufffd\n".encode() in data


def test_refuses_folder_outside_repo(repo):
    for folder in ("../out", "/tmp/agent", ".."):
        with pytest.raises(AiLensError, match="inside the repository"):
            write(repo, folder)
    assert not (repo.parent / "out").exists()


@pytest.mark.skipif(sys.platform == "win32", reason="symlinks")
def test_refuses_symlink_out_of_repo(repo, tmp_path):
    outside = tmp_path / "outside"
    outside.mkdir()
    (repo / "agent").symlink_to(outside)
    with pytest.raises(AiLensError, match="inside the repository"):
        write(repo)
    assert os.listdir(outside) == []


def test_refuses_unsafe_build_names(repo):
    facts = make_facts()
    facts.base = BuildRevision(BASE, "../../x", None)
    with pytest.raises(AiLensError, match="Cannot name a build file"):
        write(repo, facts=facts)
    assert not (repo / "agent").exists()


def test_build_folder_that_is_a_file(repo):
    (repo / "agent").write_text("a file")
    with pytest.raises(AiLensError, match="Could not create the build folder"):
        write(repo)
    assert (repo / "agent").read_text() == "a file"


def test_build_path_that_is_a_folder(repo):
    (repo / "agent" / f"{BASE[:7]}_{HUMAN[:7]}_build.md").mkdir(parents=True)
    with pytest.raises(AiLensError, match="Could not write"):
        write(repo)
    # The temporary file is cleaned up
    assert sorted(os.listdir(repo / "agent")) == [f"{BASE[:7]}_{HUMAN[:7]}_build.md"]


@pytest.mark.skipif(
    sys.platform == "win32" or os.geteuid() == 0, reason="needs file permissions"
)
def test_read_only_folder(repo):
    folder = repo / "agent"
    folder.mkdir()
    folder.chmod(0o555)
    try:
        with pytest.raises(AiLensError, match="Could not write"):
            write(repo)
        assert os.listdir(folder) == []
    finally:
        folder.chmod(0o755)


@pytest.mark.skipif(
    sys.platform == "win32" or os.geteuid() == 0, reason="needs file permissions"
)
def test_read_only_parent(repo):
    repo.chmod(0o555)
    try:
        with pytest.raises(AiLensError, match="Could not create the build folder"):
            write(repo)
    finally:
        repo.chmod(0o755)


def test_write_text_atomic_replaces_and_keeps_umask_mode(tmp_path):
    path = tmp_path / "f.md"
    path.write_text("old")
    write_text_atomic(str(path), "new é")
    assert path.read_bytes() == "new é".encode()

    umask = os.umask(0o022)
    os.umask(umask)
    fresh = tmp_path / "g.md"
    write_text_atomic(str(fresh), "x")
    assert fresh.stat().st_mode & 0o777 == 0o666 & ~umask


def test_write_text_atomic_missing_folder(tmp_path):
    with pytest.raises(AiLensError, match="Could not write"):
        write_text_atomic(str(tmp_path / "nope" / "f.md"), "x")
