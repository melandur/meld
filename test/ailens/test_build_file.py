"""Ported from AI Lens src/build/__tests__/buildFile.test.ts, plus edge cases"""

import dataclasses
from datetime import datetime, timedelta, timezone

import pytest

from meld.ailens.build_facts import (
    BuildChange,
    BuildCommit,
    BuildFacts,
    BuildFile,
    BuildFileEntry,
    BuildRevision,
    BuildStats,
    merge_ranges,
)
from meld.ailens.build_file import (
    BUILD_FORMAT_VERSION,
    BuildHeader,
    HeaderCommit,
    HeaderRevision,
    build_file_name,
    format_created,
    locale_compare,
    parse_build_header,
    render_build,
    render_index,
    yaml_string,
)
from meld.ailens.build_narrative import BuildDecision, BuildNarrative
from meld.ailens.models import LineRange


def sha(char: str) -> str:
    return char * 40


def make_facts() -> BuildFacts:
    return BuildFacts(
        base=BuildRevision(sha("a"), "aaaaaaa", "main"),
        target=BuildRevision(sha("b"), "bbbbbbb", "feature/x"),
        merge_base=sha("a"),
        relation="ancestor",
        stats=BuildStats(files=3, insertions=5, deletions=2, commits=2),
        commits=[
            BuildCommit(
                sha=sha("c"),
                short_sha="ccccccc",
                patch_id=sha("9"),
                author='Jane "J" Doe',
                agent="claude-code",
                agent_model="claude-opus-5-5",
                agent_name="Claude Opus 5.5",
                what="Makes load async",
                why="The cache reads\nfrom disk.",
                why_source="commit",
            ),
            BuildCommit(
                sha=sha("d"),
                short_sha="ddddddd",
                patch_id=None,
                author="Jane Doe",
                agent="human",
                agent_model=None,
                agent_name=None,
                what="Backs off",
                why="- Rate limits.\n- Fewer retries.",
                why_source="inferred",
                inferred_by="claude-sonnet-5-5",
                changes_inferred_by="claude-sonnet-5-5",
            ),
        ],
        files=[
            BuildFile(
                path="src/cache.ts",
                original_path="src/old_cache.ts",
                status="R",
                additions=5,
                deletions=2,
                entries=[
                    BuildFileEntry(sha("c"), [LineRange(2, 3)], [LineRange(2, 2)]),
                    BuildFileEntry(None, [LineRange(9, 9)], []),
                    BuildFileEntry(
                        sha("d"),
                        [],
                        [LineRange(7, 8)],
                        [
                            BuildChange(
                                "Drops the fixed retry",
                                "A fixed retry\nhit the limit again.",
                            )
                        ],
                    ),
                ],
                touched_by=[sha("d"), sha("c")],
            ),
            BuildFile(
                path="img.png",
                original_path=None,
                status="M",
                additions=None,
                deletions=None,
                binary=True,
                touched_by=[sha("d")],
            ),
            BuildFile(
                path="pnpm-lock.yaml",
                original_path=None,
                status="M",
                additions=0,
                deletions=0,
                ignored=True,
            ),
        ],
    )


def make_narrative(**changes) -> BuildNarrative:
    narrative = BuildNarrative(
        title="Async cache | backoff",
        summary="Makes loading async and backs off.",
        decisions=[BuildDecision("Read asynchronously.", ["ccccccc"])],
        risks=[],
        model="claude-opus-5-5",
    )
    return dataclasses.replace(narrative, **changes)


CREATED = datetime(2026, 10, 8, 14, 32, 0, 123000, tzinfo=timezone.utc)

EXPECTED = "\n".join(
    [
        "---",
        "ai_lens: 2",
        'title: "Async cache | backoff"',
        "base:",
        f"  sha: {sha('a')}",
        '  ref: "main"',
        "target:",
        f"  sha: {sha('b')}",
        '  ref: "feature/x"',
        f"merge_base: {sha('a')}",
        "relation: ancestor",
        "created: 2026-10-08T14:32:00Z",
        'build_model: "claude-opus-5-5"',
        'generator: "ai-lens@0.1.0"',
        "prompt_version: 1",
        "stats: { files: 3, insertions: 5, deletions: 2, commits: 2 }",
        "commits:",
        f"  - sha: {sha('c')}",
        f"    patch_id: {sha('9')}",
        '    author: "Jane \\"J\\" Doe"',
        "    agent: claude-code",
        '    agent_model: "claude-opus-5-5"',
        "    why_source: commit",
        f"  - sha: {sha('d')}",
        '    author: "Jane Doe"',
        "    agent: human",
        "    why_source: inferred",
        '    inferred_by: "claude-sonnet-5-5"',
        "---",
        "",
        "# Async cache | backoff",
        "",
        "`aaaaaaa` (main) → `bbbbbbb` (feature/x)",
        "",
        "## Summary",
        "",
        "Makes loading async and backs off.",
        "",
        "## Decisions",
        "",
        "- Read asynchronously. (`ccccccc`)",
        "",
        "## Commits",
        "",
        "### `ccccccc` Makes load async",
        "",
        "- claude-code (claude-opus-5-5) · source: commit",
        "- Why: The cache reads from disk.",
        "",
        "### `ddddddd` Backs off",
        "",
        "- human (Jane Doe) · source: inferred",
        "- Why (inferred by claude-sonnet-5-5, not the author’s words):",
        "  - Rate limits.",
        "  - Fewer retries.",
        "",
        "## Changes",
        "",
        "### src/cache.ts (from src/old\\_cache.ts) · R · +5 \u22122",
        "",
        "- **L2\u20133**, base **L2** · `ccccccc` · Makes load async",
        "- **L9** · commit unknown",
        "  - Why: git can’t tell which commit made this change.",
        "- base **L7\u20138** · `ddddddd` · Backs off",
        "  - What: Drops the fixed retry",
        (
            "  - Why (inferred by claude-sonnet-5-5, not the author’s words): "
            "A fixed retry hit the limit again."
        ),
        "",
        "### img.png · M · binary",
        "",
        "- Binary file; changed by `ddddddd`.",
        "",
        "### pnpm-lock.yaml · M · +0 \u22120",
        "",
        "- Left out: generated, or matched by `.ailensignore`.",
        "",
        "## Risks / open questions",
        "",
        "- None recorded.",
        "",
    ]
)


def render(facts=None, narrative=None, generator="ai-lens@0.1.0", created=CREATED):
    return render_build(
        facts or make_facts(), narrative or make_narrative(), created, generator
    )


def frontmatter(text: str) -> str:
    return text.split("\n---\n", 1)[0].removeprefix("---\n")


# -- Ported --


def test_render_build_documented_format():
    assert BUILD_FORMAT_VERSION == 2
    assert build_file_name(make_facts()) == "aaaaaaa_bbbbbbb_build.md"
    assert render() == EXPECTED


def test_header_round_trip_and_index_newest_first():
    header = parse_build_header("aaaaaaa_bbbbbbb_build.md", render(generator="g"))

    assert header == BuildHeader(
        file_name="aaaaaaa_bbbbbbb_build.md",
        title="Async cache | backoff",
        created="2026-10-08T14:32:00Z",
        build_model="claude-opus-5-5",
        base=HeaderRevision(sha("a"), "main"),
        target=HeaderRevision(sha("b"), "feature/x"),
        commits=[HeaderCommit(sha("c"), sha("9")), HeaderCommit(sha("d"), None)],
    )

    older = dataclasses.replace(
        header,
        file_name="eeeeeee_fffffff_build.md",
        created="2026-01-01T00:00:00Z",
        title="Older",
        base=HeaderRevision(sha("e"), None),
    )
    assert render_index([older, header]).split("\n")[5:] == [
        "| Build | Base → Target | Created | Model | Summary |",
        "| ----- | ------------- | ------- | ----- | ------- |",
        (
            "| [aaaaaaa_bbbbbbb](aaaaaaa_bbbbbbb_build.md) | main → feature/x "
            "| 2026-10-08 | claude-opus-5-5 | Async cache \\| backoff |"
        ),
        (
            "| [eeeeeee_fffffff](eeeeeee_fffffff_build.md) | eeeeeee → feature/x "
            "| 2026-01-01 | claude-opus-5-5 | Older |"
        ),
        "",
    ]


def test_header_without_frontmatter():
    header = parse_build_header("x_build.md", "# hand-written")
    assert header.file_name == "x_build.md"
    assert header.title is None


# -- Frontmatter edge cases --


def test_render_is_deterministic():
    assert render() == render()


def test_created_is_utc_to_the_second():
    plus_two = timezone(timedelta(hours=2))
    assert format_created(datetime(2026, 10, 8, 16, 32, 0, 999999, plus_two)) == (
        "2026-10-08T14:32:00Z"
    )
    # Naive datetimes are taken as UTC
    naive = datetime(2026, 1, 2, 3, 4, 5)  # noqa: DTZ001
    assert format_created(naive) == "2026-01-02T03:04:05Z"
    early = datetime(5, 1, 2, 3, 4, 5, tzinfo=timezone.utc)
    assert format_created(early) == "0005-01-02T03:04:05Z"


@pytest.mark.parametrize(
    ("value", "expected"),
    [
        ("plain", '"plain"'),
        ('say "hi"', '"say \\"hi\\""'),
        ("back\\slash", '"back\\\\slash"'),
        ("line\nbreak\r\ttab\b\f", '"line\\nbreak\\r\\ttab\\b\\f"'),
        ("\x00\x01\x1f\x7f", '"\\u0000\\u0001\\u001f\x7f"'),
        ("caf\u00e9 \u2192 \U0001f600 \u2028", '"caf\u00e9 \u2192 \U0001f600 \u2028"'),
        ("lone \ud800 surrogate", '"lone \\ud800 surrogate"'),
    ],
)
def test_yaml_string_matches_json_stringify(value, expected):
    assert yaml_string(value) == expected


YAML_HOSTILE = [
    "null",
    "yes",
    "~",
    "- item",
    "key: value",
    "# comment",
    "*alias",
    "&anchor",
    "!tag",
    "| block",
    "> folded",
    "'single'",
    '"double"',
    "@at `tick`",
    "{flow: map}",
    "[flow, seq]",
    "%directive",
    "  padded  ",
    "trailing:",
    "123",
    "1e3",
    "0x1F",
    "2026-10-08",
    "tab\there",
    "multi\nline\ntitle",
    "\u00fcnicode \u2192 \u65e5\u672c \U0001f600",
    "back\\slash",
    "---",
    "...",
]


@pytest.mark.parametrize("value", YAML_HOSTILE)
def test_frontmatter_strings_survive_yaml(value):
    yaml = pytest.importorskip("yaml")
    facts = make_facts()
    facts.base = BuildRevision(sha("a"), "aaaaaaa", value)
    facts.commits[0] = dataclasses.replace(
        facts.commits[0], author=value, agent_model=value
    )
    text = render(facts, make_narrative(title=value, model=value), generator=value)

    data = yaml.safe_load(frontmatter(text))
    assert data["title"] == value
    assert data["base"]["ref"] == value
    assert data["build_model"] == value
    assert data["generator"] == value
    assert data["commits"][0]["author"] == value
    assert data["commits"][0]["agent_model"] == value
    assert data["target"]["ref"] == "feature/x"
    assert data["ai_lens"] == 2
    assert data["stats"] == {"files": 3, "insertions": 5, "deletions": 2, "commits": 2}

    # And our own reader agrees
    header = parse_build_header("x_build.md", text)
    assert header.title == value
    assert header.base.ref == value
    assert header.build_model == value


def test_frontmatter_is_valid_yaml_with_nulls():
    yaml = pytest.importorskip("yaml")
    facts = make_facts()
    facts.base = BuildRevision(sha("a"), "aaaaaaa", None)
    facts.merge_base = None
    facts.relation = "unrelated"
    facts.commits = []
    facts.stats = BuildStats(0, 0, 0, 0)
    text = render(facts, make_narrative(decisions=[]))

    data = yaml.safe_load(frontmatter(text))
    assert data["base"] == {"sha": sha("a"), "ref": None}
    assert data["merge_base"] is None
    assert data["commits"] == []
    assert "commits: []" in text
    assert "## Commits\n\n- None.\n" in text
    assert "## Decisions\n\n- None recorded.\n" in text

    header = parse_build_header("x_build.md", text)
    assert header.base == HeaderRevision(sha("a"), None)
    assert header.commits == []


def test_unicode_and_quotes_in_title_and_refs():
    facts = make_facts()
    facts.target = BuildRevision(sha("b"), "bbbbbbb", 'feat/"quoted"_\u00e9|<x>')
    narrative = make_narrative(title='\u00c9t\u00e9 "quoted" \U0001f600 | `x`')
    text = render(facts, narrative)

    assert 'title: "\u00c9t\u00e9 \\"quoted\\" \U0001f600 | `x`"' in text
    assert '  ref: "feat/\\"quoted\\"_\u00e9|<x>"' in text
    # The heading takes the title as is; the revision line escapes the ref
    assert '# \u00c9t\u00e9 "quoted" \U0001f600 | `x`\n' in text
    assert '(feat/"quoted"\\_\u00e9\\|\\<x\\>)' in text

    header = parse_build_header("x_build.md", text)
    assert header.title == narrative.title
    assert header.target.ref == facts.target.ref


def test_diverged_names_the_merge_base():
    facts = make_facts()
    facts.relation = "diverged"
    facts.merge_base = sha("e")
    text = render(facts)
    assert (
        "`aaaaaaa` (main) → `bbbbbbb` (feature/x) · changes since the merge base "
        "`eeeeeee`\n" in text
    )
    assert "relation: diverged\n" in text


def test_diverged_without_merge_base():
    facts = make_facts()
    facts.relation = "diverged"
    facts.merge_base = None
    text = render(facts)
    assert "`aaaaaaa` (main) → `bbbbbbb` (feature/x)\n" in text
    assert "merge_base: null\n" in text


# -- Body edge cases --


def test_newlines_in_commit_fields_stay_on_one_line():
    facts = make_facts()
    facts.commits[0] = dataclasses.replace(
        facts.commits[0], what="Makes\n  load\r\n async", author="J\nD"
    )
    text = render(facts)
    assert "### `ccccccc` Makes load async\n" in text
    assert '    author: "J\\nD"' in text


def test_why_with_paragraphs_keeps_them_indented():
    facts = make_facts()
    facts.commits[0] = dataclasses.replace(
        facts.commits[0], why="  First para\nwrapped.\n\n  Second para.  \n"
    )
    text = render(facts)
    assert "- Why:\n  First para\n  wrapped.\n\n  Second para.\n\n### `ddddddd`" in text


def test_why_star_list_and_crlf():
    facts = make_facts()
    facts.commits[0] = dataclasses.replace(
        facts.commits[0], why="Reasons:\r\n* one\r\n* two"
    )
    text = render(facts)
    assert "- Why:\n  Reasons:\n  * one\n  * two\n" in text


def test_hyphen_inside_prose_is_not_a_list():
    facts = make_facts()
    facts.commits[0] = dataclasses.replace(
        facts.commits[0], why="Uses a - b\nfor the range."
    )
    assert "- Why: Uses a - b for the range.\n" in render(facts)


def test_missing_why_and_unknown_inferred_model():
    facts = make_facts()
    facts.commits[0] = dataclasses.replace(facts.commits[0], why=None)
    facts.commits[1] = dataclasses.replace(
        facts.commits[1], why="Guess.", inferred_by=None, changes_inferred_by=None
    )
    text = render(facts)
    assert "- Why: not recorded.\n" in text
    assert "- Why (inferred by a model, not the author’s words): Guess.\n" in text
    assert "  - Why (inferred by a model, not the author’s words): A fixed" in text
    assert "inferred_by:" not in text


def test_agent_without_model():
    facts = make_facts()
    facts.commits[0] = dataclasses.replace(facts.commits[0], agent_model=None)
    text = render(facts)
    assert "- claude-code · source: commit\n" in text
    assert "agent_model:" not in text


def test_markdown_in_paths_and_authors_is_escaped():
    facts = make_facts()
    facts.files = [
        BuildFile(
            path="a_b/*c*/[d]|e<f>`g`\\h.md",
            original_path=None,
            status="A",
            additions=1,
            deletions=0,
            entries=[BuildFileEntry(sha("d"), [LineRange(1, 1)], [])],
        )
    ]
    facts.commits[1] = dataclasses.replace(
        facts.commits[1], author="_Jane_ *D*", what="Fix `x` | y"
    )
    text = render(facts)
    assert (
        "### a\\_b/\\*c\\*/\\[d\\]\\|e\\<f\\>\\`g\\`\\\\h.md · A · +1 \u22120\n" in text
    )
    assert "- human (\\_Jane\\_ \\*D\\*) · source: inferred\n" in text
    assert "- **L1** · `ddddddd` · Fix \\`x\\` \\| y\n" in text


def test_empty_ranges_and_no_line_changes():
    facts = make_facts()
    facts.files = [
        # An entry without ranges still names its commit
        BuildFile(
            path="empty.txt",
            original_path=None,
            status="M",
            additions=0,
            deletions=0,
            entries=[BuildFileEntry(sha("c"), [], [])],
        ),
        # A mode-only change: no entries, touched by a commit outside the range
        BuildFile(
            path="script.sh",
            original_path=None,
            status="M",
            additions=0,
            deletions=None,
            touched_by=[sha("f")],
        ),
        BuildFile(
            path="gone.txt",
            original_path=None,
            status="D",
            additions=0,
            deletions=0,
        ),
    ]
    text = render(facts)
    assert (
        "### empty.txt · M · +0 \u22120\n\n-  · `ccccccc` · Makes load async\n" in text
    )
    assert (
        "### script.sh · M · +0 \u22120\n\n- No line changes; changed by `fffffff`.\n"
        in text
    )
    assert "### gone.txt · D · +0 \u22120\n\n- No line changes.\n" in text


def test_binary_without_commits_and_ignored_binary():
    facts = make_facts()
    facts.files = [
        BuildFile("a.bin", None, "A", None, None, binary=True),
        BuildFile("b.bin", None, "A", None, None, binary=True, ignored=True),
    ]
    text = render(facts)
    assert "### a.bin · A · binary\n\n- Binary file.\n" in text
    assert "### b.bin · A · binary\n\n- Left out: generated" in text


def test_risks_and_decisions_without_commits():
    narrative = make_narrative(
        decisions=[BuildDecision("No commits.", []), BuildDecision("Two.", ["a", "b"])],
        risks=["One.", "Two."],
    )
    text = render(narrative=narrative)
    assert "- No commits.\n- Two. (`a`, `b`)\n" in text
    assert text.endswith("## Risks / open questions\n\n- One.\n- Two.\n")


def test_no_files():
    facts = make_facts()
    facts.files = []
    assert "## Changes\n\n## Risks" in render(facts)


# -- Header edge cases --


def test_header_tolerates_junk():
    lines = [
        "---",
        "title: unquoted title",
        'created: "not json',
        "build_model: 42",
        "base:",
        "  sha: abc",
        "  ref:",
        "  - sha: NOTHEX",
        "    patch_id: 123",
        "target:",
        "  ref: null",
        "other: x",
        "  sha: ignored",
        "commits:",
        "    patch_id: 456",
        "  - sha: 0123abc",
        "    patch_id: 789def",
        "---",
        "title: after the frontmatter",
    ]
    header = parse_build_header("x_build.md", "\n".join(lines))
    assert header.title == "unquoted title"
    assert header.created is None
    assert header.build_model == "42"
    assert header.base == HeaderRevision("abc", None)
    assert header.target == HeaderRevision(None, None)
    assert header.commits == [HeaderCommit("0123abc", "789def")]


def test_header_with_crlf_is_not_read():
    text = render().replace("\n", "\r\n")
    assert parse_build_header("x_build.md", text) == BuildHeader("x_build.md")


def test_header_of_empty_file():
    assert parse_build_header("x_build.md", "") == BuildHeader("x_build.md")


def test_header_title_with_escapes():
    header = parse_build_header("x_build.md", '---\ntitle: "a\\u00e9\\n\\"b\\""\n---\n')
    assert header.title == 'a\u00e9\n"b"'


# -- Index edge cases --


def test_index_with_no_builds():
    expected = [
        "# AI Lens builds",
        "",
        (
            "Each build explains why the code changed between two commits: what every "
            "commit did, why, and which lines"
        ),
        (
            "it touched. Read the build that covers the code you are about to change. "
            "Newest first."
        ),
        "",
        "| Build | Base → Target | Created | Model | Summary |",
        "| ----- | ------------- | ------- | ----- | ------- |",
        "",
    ]
    assert render_index([]) == "\n".join(expected)


def test_index_cells_escape_pipes_and_newlines():
    header = BuildHeader(
        "x_build.md",
        title="a | b\n  c",
        created=None,
        build_model=None,
        base=HeaderRevision(None, "fe|at"),
        target=HeaderRevision(None, None),
    )
    assert render_index([header]).split("\n")[7] == (
        "| [x](x_build.md) | fe\\|at → ? |  |  | a \\| b c |"
    )


def test_index_orders_undated_last_then_by_name():
    def header(name, created):
        return BuildHeader(name, created=created)

    headers = [
        header("b_build.md", None),
        header("a_build.md", None),
        header("c_build.md", "2026-01-01T00:00:00Z"),
        header("d_build.md", "2026-02-01T00:00:00Z"),
    ]
    rows = render_index(headers).split("\n")[7:11]
    assert [row.split("](")[1].split(")")[0] for row in rows] == [
        "d_build.md",
        "c_build.md",
        "a_build.md",
        "b_build.md",
    ]


def test_locale_compare_approximates_icu():
    assert locale_compare("a", "b") < 0
    assert locale_compare("b", "a") > 0
    assert locale_compare("a", "a") == 0
    assert locale_compare("", "a") < 0
    # Punctuation before digits before letters, case-insensitive first
    assert locale_compare("abcdef1_x", "abcdef12_x") < 0
    assert locale_compare("9", "a") < 0
    assert locale_compare("a", "B") < 0
    assert locale_compare("a", "A") < 0


# -- merge_ranges --


def test_merge_ranges():
    assert merge_ranges([]) == []
    assert merge_ranges([LineRange(5, 6), LineRange(1, 2), LineRange(3, 3)]) == [
        LineRange(1, 3),
        LineRange(5, 6),
    ]
    assert merge_ranges([LineRange(1, 10), LineRange(2, 3), LineRange(11, 12)]) == [
        LineRange(1, 12)
    ]
    assert merge_ranges([LineRange(4, 4), LineRange(6, 6)]) == [
        LineRange(4, 4),
        LineRange(6, 6),
    ]
