"""Ported from AI Lens src/build/__tests__/buildNarrative.test.ts, plus edge cases"""

import dataclasses
from types import SimpleNamespace

import pytest

from meld.ailens.build_facts import (
    BuildCommit,
    BuildFacts,
    BuildFile,
    BuildFileEntry,
    BuildRevision,
    BuildStats,
)
from meld.ailens.build_narrative import (
    BUILD_PROMPT_VERSION,
    MAX_COMMITS,
    MAX_FILES,
    MAX_WHY_CHARS,
    SCHEMA,
    SYSTEM_PROMPT,
    BuildDecision,
    BuildNarrative,
    build_narrative_prompt,
    one_line,
    parse_narrative,
    truncate,
    write_narrative,
)
from meld.ailens.models import AiLensError, LineRange
from meld.ailens.process import CancelToken


def make_facts() -> BuildFacts:
    return BuildFacts(
        base=BuildRevision("a" * 40, "aaaaaaa", "main"),
        target=BuildRevision("b" * 40, "bbbbbbb", None),
        merge_base="a" * 40,
        relation="ancestor",
        stats=BuildStats(files=1, insertions=1, deletions=0, commits=2),
        commits=[
            BuildCommit(
                sha="c" * 40,
                short_sha="ccccccc",
                patch_id=None,
                author="Jane",
                agent="claude-code",
                agent_model="claude-opus-5-5",
                agent_name="Claude Opus 5.5",
                what="Adds X",
                why="Needed\nfor Y.",
                why_source="commit",
            ),
            BuildCommit(
                sha="d" * 40,
                short_sha="ddddddd",
                patch_id=None,
                author="Jim",
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
                path="src/x.ts",
                original_path=None,
                status="A",
                additions=1,
                deletions=0,
                entries=[BuildFileEntry("c" * 40, [LineRange(1, 1)], [])],
            )
        ],
    )


# -- Ported --


def test_prompt_lists_commits_and_files():
    prompt = build_narrative_prompt(make_facts())

    assert "Range: main (aaaaaaa) → bbbbbbb" in prompt
    assert (
        "- ccccccc · Claude Opus 5.5 with Jane · What: Adds X · Why: Needed for Y."
        in prompt
    )
    assert "- ddddddd · Jim · What: tweak · Why: not recorded" in prompt
    assert "A src/x.ts · +1 \u22120 · by ccccccc" in prompt


def test_parse_keeps_known_commits_and_drops_empty_items():
    parsed = parse_narrative(
        {
            "title": " Adds X\n",
            "summary": "Summary.",
            "decisions": [
                {"text": "Use X.", "commits": ["ccccccc", "zzzzzzz"]},
                {"text": " ", "commits": []},
            ],
            "risks": ["ddddddd has no reason.", ""],
        },
        make_facts(),
    )
    assert parsed == BuildNarrative(
        title="Adds X",
        summary="Summary.",
        decisions=[BuildDecision("Use X.", ["ccccccc"])],
        risks=["ddddddd has no reason."],
        model="",
    )


def test_parse_rejects_missing_parts():
    with pytest.raises(AiLensError, match="missing parts"):
        parse_narrative({"title": "x"}, make_facts())


# -- Prompt --


def test_prompt_exact():
    facts = make_facts()
    facts.files.append(
        BuildFile(
            "new.ts",
            "old.ts",
            "R",
            2,
            3,
            entries=[
                BuildFileEntry("d" * 40),
                BuildFileEntry(None),
                BuildFileEntry("c" * 40),
                BuildFileEntry("d" * 40),
                BuildFileEntry("e" * 40),
            ],
        )
    )
    facts.files.append(BuildFile("img.png", None, "M", None, None, binary=True))
    facts.files.append(BuildFile("lock", None, "M", 9, 9, ignored=True))
    facts.files.append(BuildFile("n.ts", None, "M", None, None))
    assert build_narrative_prompt(facts) == "\n".join(
        [
            "Range: main (aaaaaaa) → bbbbbbb",
            "2 commits, 1 files, +1 \u22120 lines",
            "",
            "Commits, oldest first:",
            "- ccccccc · Claude Opus 5.5 with Jane · What: Adds X · Why: Needed for Y.",
            "- ddddddd · Jim · What: tweak · Why: not recorded",
            "",
            "Changed files:",
            "A src/x.ts · +1 \u22120 · by ccccccc",
            "R old.ts -> new.ts · +2 \u22123 · by ddddddd, ccccccc, " + "e" * 40,
            "M img.png · binary",
            "M lock · left out (generated or ignored)",
            "M n.ts · +0 \u22120",
        ]
    )


def test_prompt_diverged_inferred_and_unnamed_agent():
    facts = make_facts()
    facts.relation = "diverged"
    facts.commits[0] = dataclasses.replace(
        facts.commits[0],
        agent_name=None,
        why="Guess\n  wrapped.",
        why_source="inferred",
    )
    prompt = build_narrative_prompt(facts)
    assert prompt.startswith(
        "Range: main (aaaaaaa) → bbbbbbb (diverged branches; changes since their "
        "merge base)\n"
    )
    assert (
        "- ccccccc · Claude with Jane · What: Adds X · "
        "Why (inferred, not the author’s words): Guess wrapped." in prompt
    )


def test_prompt_cuts_long_ranges_and_says_so():
    facts = make_facts()
    commit = facts.commits[1]
    facts.commits = [
        dataclasses.replace(commit, sha=f"{i:040x}", short_sha=f"{i:07x}")
        for i in range(MAX_COMMITS + 3)
    ]
    facts.files = [BuildFile(f"f{i}", None, "M", 1, 1) for i in range(MAX_FILES + 2)]
    prompt = build_narrative_prompt(facts)
    assert prompt.count("\n- ") == MAX_COMMITS
    assert "\n(3 more commits left out)\n" in prompt
    assert prompt.endswith(f"M f{MAX_FILES - 1} · +1 \u22121\n(2 more files left out)")


def test_prompt_truncates_long_whys():
    facts = make_facts()
    facts.commits[0] = dataclasses.replace(facts.commits[0], why="x" * 1000)
    line = build_narrative_prompt(facts).split("\n")[4]
    assert line.endswith("Why: " + "x" * (MAX_WHY_CHARS - 1) + "…")


def test_truncate_counts_utf16_units():
    assert truncate("abc", 3) == "abc"
    assert truncate("abcd", 3) == "ab…"
    # An emoji is two units, as in JavaScript
    assert truncate("\U0001f600\U0001f600", 4) == "\U0001f600\U0001f600"
    assert truncate("\U0001f600\U0001f600", 3) == "\U0001f600…"
    # A pair the cut would split is dropped whole
    assert truncate("a\U0001f600bc", 3) == "a…"


def test_one_line_uses_javascript_whitespace():
    assert one_line("  a \n\t b \u00a0\n\u3000c  ") == "a b c"
    assert one_line("a\r\nb") == "a b"
    # \x1c is whitespace to Python, not to JavaScript
    assert one_line("\x1ca\x1c") == "\x1ca\x1c"


# -- Reply --


@pytest.mark.parametrize(
    "output",
    [
        None,
        [],
        "text",
        {"title": "t", "summary": "s", "decisions": [], "risks": "none"},
        {"title": 1, "summary": "s", "decisions": [], "risks": []},
        {"title": "t", "summary": None, "decisions": [], "risks": []},
        {"title": "t", "summary": "s", "decisions": {}, "risks": []},
    ],
)
def test_parse_rejects_malformed(output):
    with pytest.raises(AiLensError, match="missing parts"):
        parse_narrative(output, make_facts())


def test_parse_cleans_up():
    parsed = parse_narrative(
        {
            "title": " \n ",
            "summary": "  Two\nlines.  \n",
            "decisions": [
                "not an object",
                None,
                {"text": 5, "commits": ["ccccccc"]},
                {"text": "No list.", "commits": "ccccccc"},
                {"text": "Missing commits."},
                {
                    "text": "Many\n lines.",
                    "commits": [" ccccccc\n", 7, "", "ddddddd", "c" * 40],
                },
            ],
            "risks": [None, 3, "  ", "A\nrisk.", {"text": "x"}],
            "extra": "ignored",
        },
        make_facts(),
        "claude-x",
    )
    assert parsed == BuildNarrative(
        title="Changes",
        summary="Two\nlines.",
        decisions=[
            BuildDecision("No list.", []),
            BuildDecision("Missing commits.", []),
            # Full SHAs aren't short SHAs from the list
            BuildDecision("Many lines.", ["ccccccc", "ddddddd"]),
        ],
        risks=["A risk."],
        model="claude-x",
    )


def test_parse_drops_unknown_shas():
    parsed = parse_narrative(
        {
            "title": "t",
            "summary": "s",
            "decisions": [{"text": "d", "commits": ["eeeeeee", "CCCCCCC", "ccccccc"]}],
            "risks": [],
        },
        make_facts(),
    )
    assert parsed.decisions == [BuildDecision("d", ["ccccccc"])]


# -- write_narrative --


def test_write_narrative_sends_one_request_with_the_schema():
    calls = []
    cancel = CancelToken()

    def run(request, cancel=None):
        calls.append((request, cancel))
        return SimpleNamespace(
            output={
                "title": "Adds X",
                "summary": "S.",
                "decisions": [{"text": "D.", "commits": ["ccccccc"]}],
                "risks": [],
            },
            model="claude-opus-5-5-20261001",
            cost_usd=0,
        )

    facts = make_facts()
    narrative = write_narrative(
        facts, "claude-opus-5-5", claude_path="/opt/claude", cancel=cancel, run=run
    )

    assert narrative == BuildNarrative(
        title="Adds X",
        summary="S.",
        decisions=[BuildDecision("D.", ["ccccccc"])],
        risks=[],
        model="claude-opus-5-5-20261001",
    )
    [(request, passed_cancel)] = calls
    assert passed_cancel is cancel
    assert request.prompt == build_narrative_prompt(facts)
    assert request.system_prompt == SYSTEM_PROMPT
    assert request.model == "claude-opus-5-5"
    assert request.json_schema == SCHEMA
    assert request.claude_path == "/opt/claude"


def test_write_narrative_leaves_claude_path_default():
    from meld.ailens.claude import ClaudeRequest

    seen = []

    def run(request, cancel=None):
        seen.append(request)
        return SimpleNamespace(
            output={"title": "t", "summary": "s", "decisions": [], "risks": []},
            model="m",
        )

    write_narrative(make_facts(), "claude-opus-5-5", run=run)
    default = ClaudeRequest(prompt="", model="")
    assert seen[0].claude_path == default.claude_path


def test_write_narrative_rejects_bad_reply():
    def run(request, cancel=None):
        return SimpleNamespace(output={"title": "t"}, model="m")

    with pytest.raises(AiLensError, match="missing parts"):
        write_narrative(make_facts(), "m", run=run)


def test_write_narrative_propagates_run_errors():
    def run(request, cancel=None):
        raise AiLensError("Claude Code is not installed.")

    with pytest.raises(AiLensError, match="not installed"):
        write_narrative(make_facts(), "m", run=run)


def test_schema_and_version():
    assert BUILD_PROMPT_VERSION == 1
    assert SCHEMA["required"] == ["title", "summary", "decisions", "risks"]
    assert SCHEMA["additionalProperties"] is False
    assert SYSTEM_PROMPT.startswith("You write the summary of a range")
    assert SYSTEM_PROMPT.endswith("Plain text, no markdown.")
