"""Ported from AI Lens src/agents/__tests__/agentSetup.test.ts, plus edge cases"""

import os
import re
import sys

import pytest

from meld.ailens.agent_setup import (
    SECTION_END,
    SECTION_START,
    AgentSetupOptions,
    merge_claude_md,
    plan_agent_setup,
    render_claude_md_section,
    render_why_skill,
    write_agent_setup,
)
from meld.ailens.models import AiLensError

OPTIONS = AgentSetupOptions(build_folder="docs/agent", notes_ref="ai-lens")
SECTION = render_claude_md_section(OPTIONS)


# -- Ported --


def test_section_points_at_build_folder_and_why_between_markers():
    assert SECTION.startswith("<!-- ai-lens:start -->\n")
    assert SECTION.endswith("\n<!-- ai-lens:end -->")
    assert "`docs/agent/index.md`" in SECTION
    assert "/why <file>:<line>" in SECTION


def test_merge_creates_appends_and_replaces_in_place():
    assert merge_claude_md(None, SECTION) == f"{SECTION}\n"

    appended = merge_claude_md("# Project\n\nRules.\n", SECTION)
    assert appended == f"# Project\n\nRules.\n\n{SECTION}\n"

    updated = merge_claude_md(
        appended.replace("## Why the code", "## Old heading"), SECTION
    )
    assert updated == appended
    assert merge_claude_md(updated, SECTION) == updated


def test_why_skill_finds_builds_by_sha_then_patch_id():
    skill = render_why_skill(OPTIONS)

    assert re.match(r"---\nname: why\ndescription: .+\n---\n", skill)
    assert "git blame --porcelain -L <start>,<end> -- <file>" in skill
    assert 'grep -l "sha: <SHA>" docs/agent/*_build.md' in skill
    assert 'grep -l "patch_id: <PATCH_ID>" docs/agent/*_build.md' in skill
    assert "git notes --ref=ai-lens show <SHA>" in skill


def test_plan_only_changed_files_and_write_them(tmp_path):
    repo = str(tmp_path)
    (tmp_path / "CLAUDE.md").write_text("# Project\n")

    first = plan_agent_setup(repo, OPTIONS)
    assert [(f.path[len(repo) :], f.existed, f.changed) for f in first] == [
        ("/CLAUDE.md", True, True),
        ("/.claude/skills/why/SKILL.md", False, True),
    ]

    write_agent_setup(first)
    assert (tmp_path / "CLAUDE.md").read_text() == f"# Project\n\n{SECTION}\n"
    assert (tmp_path / ".claude/skills/why/SKILL.md").read_text() == render_why_skill(
        OPTIONS
    )

    assert not any(f.changed for f in plan_agent_setup(repo, OPTIONS))


# -- Exact text --


def test_section_exact():
    expected = [
        "<!-- ai-lens:start -->",
        "## Why the code is the way it is (AI Lens)",
        "",
        (
            "`agent/` holds AI Lens builds: one file per compared commit range, saying "
            "what each commit changed, why,"
        ),
        "and which lines it touched. `agent/index.md` lists them, newest first.",
        "",
        (
            "- Before changing code whose purpose isn't clear, find out why it is the "
            "way it is: run `/why <file>:<line>`,"
        ),
        "  or read the build in `agent/index.md` that covers it.",
        (
            '- A "Why (inferred …)" is a model\'s reading of the diff, not the '
            "author's reason. Treat it as a guess."
        ),
        "<!-- ai-lens:end -->",
    ]
    section = render_claude_md_section(AgentSetupOptions("agent", "ai-lens"))
    assert section == "\n".join(expected)


def test_why_skill_inserts_values_verbatim():
    skill = render_why_skill(AgentSetupOptions("we{ir}d {folder}", "re{f}"))
    assert "AI Lens builds in we{ir}d {folder}/ and" in skill
    assert "git notes --ref=re{f} show <SHA>" in skill
    assert "{notes_ref}" not in skill
    assert skill.endswith("one up.\n")


# -- merge_claude_md --


def test_merge_into_blank_file():
    assert merge_claude_md("", SECTION) == f"{SECTION}\n"
    assert merge_claude_md(" \n\t\n", SECTION) == f"{SECTION}\n"


def test_merge_keeps_text_around_the_section():
    existing = f"# Top\n\n{SECTION_START}\nold\n{SECTION_END}\n\n## Bottom\n"
    assert merge_claude_md(existing, SECTION) == f"# Top\n\n{SECTION}\n\n## Bottom\n"


def test_merge_appends_when_markers_are_broken():
    # End before start, or start only: append a fresh section
    for existing in (
        f"{SECTION_END}\nx\n{SECTION_START}\n",
        f"# P\n{SECTION_START}\nold",
    ):
        assert merge_claude_md(existing, SECTION) == (
            f"{existing.rstrip()}\n\n{SECTION}\n"
        )


def test_merge_replaces_only_the_first_section():
    existing = f"{SECTION_START}\na\n{SECTION_END}\n{SECTION_START}\nb\n{SECTION_END}\n"
    assert merge_claude_md(existing, SECTION) == (
        f"{SECTION}\n{SECTION_START}\nb\n{SECTION_END}\n"
    )


def test_merge_keeps_crlf_and_unicode_elsewhere():
    existing = "# Pröject\r\n\r\nRules \U0001f600.\r\n\r\n"
    assert merge_claude_md(existing, SECTION) == (
        f"# Pröject\r\n\r\nRules \U0001f600.\n\n{SECTION}\n"
    )


# -- plan / write --


def test_plan_in_empty_repo(tmp_path):
    files = plan_agent_setup(str(tmp_path), OPTIONS)
    assert [(f.existed, f.changed) for f in files] == [(False, True), (False, True)]
    assert files[0].content == f"{SECTION}\n"


def test_write_skips_unchanged_files(tmp_path):
    files = plan_agent_setup(str(tmp_path), OPTIONS)
    write_agent_setup(files)
    skill = tmp_path / ".claude/skills/why/SKILL.md"
    skill.chmod(0o444)
    try:
        # Nothing changed, so the read-only skill isn't touched
        write_agent_setup(plan_agent_setup(str(tmp_path), OPTIONS))
    finally:
        skill.chmod(0o644)


def test_rewrites_an_edited_skill(tmp_path):
    write_agent_setup(plan_agent_setup(str(tmp_path), OPTIONS))
    skill = tmp_path / ".claude/skills/why/SKILL.md"
    skill.write_text("edited")
    files = plan_agent_setup(str(tmp_path), OPTIONS)
    assert [(f.existed, f.changed) for f in files] == [(True, False), (True, True)]
    write_agent_setup(files)
    assert skill.read_text() == render_why_skill(OPTIONS)


def test_keeps_bytes_that_are_not_utf8(tmp_path):
    (tmp_path / "CLAUDE.md").write_bytes(b"# Caf\xe9\n")
    write_agent_setup(plan_agent_setup(str(tmp_path), OPTIONS))
    assert (tmp_path / "CLAUDE.md").read_bytes() == (
        b"# Caf\xe9\n\n" + SECTION.encode() + b"\n"
    )


def test_claude_md_that_is_a_folder(tmp_path):
    (tmp_path / "CLAUDE.md").mkdir()
    with pytest.raises(AiLensError, match="Could not read"):
        plan_agent_setup(str(tmp_path), OPTIONS)


def test_claude_folder_that_is_a_file(tmp_path):
    (tmp_path / ".claude").write_text("x")
    files = plan_agent_setup(str(tmp_path), OPTIONS)
    with pytest.raises(AiLensError, match="Could not create"):
        write_agent_setup(files)


@pytest.mark.skipif(
    sys.platform == "win32" or os.geteuid() == 0, reason="needs file permissions"
)
def test_read_only_repo(tmp_path):
    files = plan_agent_setup(str(tmp_path), OPTIONS)
    tmp_path.chmod(0o555)
    try:
        with pytest.raises(AiLensError, match="Could not write"):
            write_agent_setup(files)
        assert os.listdir(tmp_path) == []
    finally:
        tmp_path.chmod(0o755)
