//! Port of test/ailens/test_build_file.py (itself ported from AI Lens
//! src/build/__tests__/buildFile.test.ts, plus edge cases)

use meld_ailens::build_facts::{
    BuildAgent, BuildChange, BuildCommit, BuildFacts, BuildFile, BuildFileEntry, BuildRevision,
    BuildStats, merge_ranges,
};
use meld_ailens::build_file::{
    BUILD_FORMAT_VERSION, BuildHeader, HeaderCommit, HeaderRevision, build_file_name,
    format_created, locale_compare, parse_build_header, render_build, render_index, yaml_string,
};
use meld_ailens::build_narrative::{BuildDecision, BuildNarrative};
use meld_ailens::time::timestamp_utc;
use meld_ailens::{ComparisonRelation, ExplanationSource, GitFileStatus, LineRange};

fn sha(c: char) -> String {
    c.to_string().repeat(40)
}

fn range(start: usize, end: usize) -> LineRange {
    LineRange::new(start, end)
}

fn entry(sha: Option<String>, added: Vec<LineRange>, deleted: Vec<LineRange>) -> BuildFileEntry {
    BuildFileEntry {
        sha,
        added,
        deleted,
        changes: None,
    }
}

fn make_facts() -> BuildFacts {
    BuildFacts {
        base: BuildRevision {
            sha: sha('a'),
            short_sha: "aaaaaaa".into(),
            ref_name: Some("main".into()),
        },
        target: BuildRevision {
            sha: sha('b'),
            short_sha: "bbbbbbb".into(),
            ref_name: Some("feature/x".into()),
        },
        merge_base: Some(sha('a')),
        relation: ComparisonRelation::Ancestor,
        stats: BuildStats {
            files: 3,
            insertions: 5,
            deletions: 2,
            commits: 2,
        },
        commits: vec![
            BuildCommit {
                sha: sha('c'),
                short_sha: "ccccccc".into(),
                patch_id: Some(sha('9')),
                author: "Jane \"J\" Doe".into(),
                agent: BuildAgent::ClaudeCode,
                agent_model: Some("claude-opus-5-5".into()),
                agent_name: Some("Claude Opus 5.5".into()),
                what: "Makes load async".into(),
                why: Some("The cache reads\nfrom disk.".into()),
                justification: None,
                why_source: ExplanationSource::Commit,
                inferred_by: None,
                changes_inferred_by: None,
            },
            BuildCommit {
                sha: sha('d'),
                short_sha: "ddddddd".into(),
                patch_id: None,
                author: "Jane Doe".into(),
                agent: BuildAgent::Human,
                agent_model: None,
                agent_name: None,
                what: "Backs off".into(),
                why: Some("- Rate limits.\n- Fewer retries.".into()),
                justification: None,
                why_source: ExplanationSource::Inferred,
                inferred_by: Some("claude-sonnet-5-5".into()),
                changes_inferred_by: Some("claude-sonnet-5-5".into()),
            },
        ],
        files: vec![
            BuildFile {
                original_path: Some("src/old_cache.ts".into()),
                entries: vec![
                    entry(Some(sha('c')), vec![range(2, 3)], vec![range(2, 2)]),
                    entry(None, vec![range(9, 9)], vec![]),
                    BuildFileEntry {
                        sha: Some(sha('d')),
                        added: vec![],
                        deleted: vec![range(7, 8)],
                        changes: Some(vec![BuildChange {
                            what: "Drops the fixed retry".into(),
                            why: "A fixed retry\nhit the limit again.".into(),
                            justification: None,
                        }]),
                    },
                ],
                touched_by: vec![sha('d'), sha('c')],
                ..BuildFile::new("src/cache.ts", GitFileStatus::Renamed, Some(5), Some(2))
            },
            BuildFile {
                binary: true,
                touched_by: vec![sha('d')],
                ..BuildFile::new("img.png", GitFileStatus::Modified, None, None)
            },
            BuildFile {
                ignored: true,
                ..BuildFile::new("pnpm-lock.yaml", GitFileStatus::Modified, Some(0), Some(0))
            },
        ],
    }
}

fn make_narrative() -> BuildNarrative {
    BuildNarrative {
        title: "Async cache | backoff".into(),
        summary: "Makes loading async and backs off.".into(),
        decisions: vec![BuildDecision {
            text: "Read asynchronously.".into(),
            commits: vec!["ccccccc".into()],
        }],
        risks: vec![],
        model: "claude-opus-5-5".into(),
    }
}

/// 2026-10-08T14:32:00.123Z
fn created() -> f64 {
    timestamp_utc(2026, 10, 8, 14, 32, 0.123)
}

fn expected() -> String {
    let a = sha('a');
    let b = sha('b');
    let c = sha('c');
    let d = sha('d');
    let nine = sha('9');
    [
        "---".to_string(),
        "ai_lens: 2".into(),
        "title: \"Async cache | backoff\"".into(),
        "base:".into(),
        format!("  sha: {a}"),
        "  ref: \"main\"".into(),
        "target:".into(),
        format!("  sha: {b}"),
        "  ref: \"feature/x\"".into(),
        format!("merge_base: {a}"),
        "relation: ancestor".into(),
        "created: 2026-10-08T14:32:00Z".into(),
        "build_model: \"claude-opus-5-5\"".into(),
        "generator: \"ai-lens@0.1.0\"".into(),
        "prompt_version: 1".into(),
        "stats: { files: 3, insertions: 5, deletions: 2, commits: 2 }".into(),
        "commits:".into(),
        format!("  - sha: {c}"),
        format!("    patch_id: {nine}"),
        "    author: \"Jane \\\"J\\\" Doe\"".into(),
        "    agent: claude-code".into(),
        "    agent_model: \"claude-opus-5-5\"".into(),
        "    why_source: commit".into(),
        format!("  - sha: {d}"),
        "    author: \"Jane Doe\"".into(),
        "    agent: human".into(),
        "    why_source: inferred".into(),
        "    inferred_by: \"claude-sonnet-5-5\"".into(),
        "---".into(),
        "".into(),
        "# Async cache | backoff".into(),
        "".into(),
        "`aaaaaaa` (main) → `bbbbbbb` (feature/x)".into(),
        "".into(),
        "## Summary".into(),
        "".into(),
        "Makes loading async and backs off.".into(),
        "".into(),
        "## Decisions".into(),
        "".into(),
        "- Read asynchronously. (`ccccccc`)".into(),
        "".into(),
        "## Commits".into(),
        "".into(),
        "### `ccccccc` Makes load async".into(),
        "".into(),
        "- claude-code (claude-opus-5-5) · source: commit".into(),
        "- Why: The cache reads from disk.".into(),
        "".into(),
        "### `ddddddd` Backs off".into(),
        "".into(),
        "- human (Jane Doe) · source: inferred".into(),
        "- Why (inferred by claude-sonnet-5-5, not the author’s words):".into(),
        "  - Rate limits.".into(),
        "  - Fewer retries.".into(),
        "".into(),
        "## Changes".into(),
        "".into(),
        "### src/cache.ts (from src/old\\_cache.ts) · R · +5 \u{2212}2".into(),
        "".into(),
        "- **L2\u{2013}3**, base **L2** · `ccccccc` · Makes load async".into(),
        "- **L9** · commit unknown".into(),
        "  - Why: git can’t tell which commit made this change.".into(),
        "- base **L7\u{2013}8** · `ddddddd` · Backs off".into(),
        "  - What: Drops the fixed retry".into(),
        "  - Why (inferred by claude-sonnet-5-5, not the author’s words): A fixed retry hit the limit again.".into(),
        "".into(),
        "### img.png · M · binary".into(),
        "".into(),
        "- Binary file; changed by `ddddddd`.".into(),
        "".into(),
        "### pnpm-lock.yaml · M · +0 \u{2212}0".into(),
        "".into(),
        "- Left out: generated, or matched by `.ailensignore`.".into(),
        "".into(),
        "## Risks / open questions".into(),
        "".into(),
        "- None recorded.".into(),
        "".into(),
    ]
    .join("\n")
}

fn render_with(facts: &BuildFacts, narrative: &BuildNarrative, generator: &str) -> String {
    render_build(facts, narrative, created(), generator)
}

fn render_facts(facts: &BuildFacts) -> String {
    render_with(facts, &make_narrative(), "ai-lens@0.1.0")
}

fn render() -> String {
    render_facts(&make_facts())
}

// -- Ported --

#[test]
fn render_build_documented_format() {
    assert_eq!(BUILD_FORMAT_VERSION, 2);
    assert_eq!(build_file_name(&make_facts()), "aaaaaaa_bbbbbbb_build.md");
    assert_eq!(render(), expected());
}

#[test]
fn header_round_trip_and_index_newest_first() {
    let header = parse_build_header(
        "aaaaaaa_bbbbbbb_build.md",
        &render_with(&make_facts(), &make_narrative(), "g"),
    );
    assert_eq!(
        header,
        BuildHeader {
            file_name: "aaaaaaa_bbbbbbb_build.md".into(),
            title: Some("Async cache | backoff".into()),
            created: Some("2026-10-08T14:32:00Z".into()),
            build_model: Some("claude-opus-5-5".into()),
            base: HeaderRevision {
                sha: Some(sha('a')),
                ref_name: Some("main".into())
            },
            target: HeaderRevision {
                sha: Some(sha('b')),
                ref_name: Some("feature/x".into())
            },
            commits: vec![
                HeaderCommit::new(sha('c'), Some(&sha('9'))),
                HeaderCommit::new(sha('d'), None)
            ],
        }
    );

    let older = BuildHeader {
        file_name: "eeeeeee_fffffff_build.md".into(),
        created: Some("2026-01-01T00:00:00Z".into()),
        title: Some("Older".into()),
        base: HeaderRevision {
            sha: Some(sha('e')),
            ref_name: None,
        },
        ..header.clone()
    };
    let index = render_index(&[older, header]);
    let rows: Vec<&str> = index.split('\n').skip(5).collect();
    assert_eq!(
        rows,
        [
            "| Build | Base → Target | Created | Model | Summary |",
            "| ----- | ------------- | ------- | ----- | ------- |",
            "| [aaaaaaa_bbbbbbb](aaaaaaa_bbbbbbb_build.md) | main → feature/x | 2026-10-08 | claude-opus-5-5 | Async cache \\| backoff |",
            "| [eeeeeee_fffffff](eeeeeee_fffffff_build.md) | eeeeeee → feature/x | 2026-01-01 | claude-opus-5-5 | Older |",
            "",
        ]
    );
}

#[test]
fn header_without_frontmatter() {
    let header = parse_build_header("x_build.md", "# hand-written");
    assert_eq!(header.file_name, "x_build.md");
    assert_eq!(header.title, None);
}

// -- Frontmatter edge cases --

#[test]
fn render_is_deterministic() {
    assert_eq!(render(), render());
}

#[test]
fn created_is_utc_to_the_second() {
    // 16:32:00.999999 at +02:00
    let plus_two = timestamp_utc(2026, 10, 8, 16, 32, 0.999_999) - 2.0 * 3600.0;
    assert_eq!(format_created(plus_two), "2026-10-08T14:32:00Z");
    // Naive datetimes are taken as UTC
    assert_eq!(
        format_created(timestamp_utc(2026, 1, 2, 3, 4, 5.0)),
        "2026-01-02T03:04:05Z"
    );
    assert_eq!(
        format_created(timestamp_utc(5, 1, 2, 3, 4, 5.0)),
        "0005-01-02T03:04:05Z"
    );
}

#[test]
fn yaml_string_matches_json_stringify() {
    let cases = [
        ("plain", "\"plain\""),
        ("say \"hi\"", "\"say \\\"hi\\\"\""),
        ("back\\slash", "\"back\\\\slash\""),
        (
            "line\nbreak\r\ttab\u{8}\u{c}",
            "\"line\\nbreak\\r\\ttab\\b\\f\"",
        ),
        ("\u{0}\u{1}\u{1f}\u{7f}", "\"\\u0000\\u0001\\u001f\u{7f}\""),
        (
            "caf\u{e9} \u{2192} \u{1f600} \u{2028}",
            "\"caf\u{e9} \u{2192} \u{1f600} \u{2028}\"",
        ),
        // A lone surrogate can't be in a Rust String; Python's
        // `"lone \ud800 surrogate"` case has no equivalent
    ];
    for (value, expected) in cases {
        assert_eq!(yaml_string(value), expected, "{value:?}");
    }
}

const YAML_HOSTILE: [&str; 29] = [
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
    "\"double\"",
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
    "\u{fc}nicode \u{2192} \u{65e5}\u{672c} \u{1f600}",
    "back\\slash",
    "---",
    "...",
];

/// The Python test also loads the frontmatter with PyYAML; there is no YAML
/// parser here, so each value is checked to be a JSON string (a valid YAML
/// double-quoted scalar) and read back by our own reader
#[test]
fn frontmatter_strings_survive_yaml() {
    for value in YAML_HOSTILE {
        let mut facts = make_facts();
        facts.base = BuildRevision {
            sha: sha('a'),
            short_sha: "aaaaaaa".into(),
            ref_name: Some(value.into()),
        };
        facts.commits[0].author = value.into();
        facts.commits[0].agent_model = Some(value.into());
        let narrative = BuildNarrative {
            title: value.into(),
            model: value.into(),
            ..make_narrative()
        };
        let text = render_with(&facts, &narrative, value);

        let quoted = yaml_string(value);
        assert_eq!(serde_json::from_str::<String>(&quoted).unwrap(), value);
        assert!(text.contains(&format!("\ntitle: {quoted}\n")));
        assert!(text.contains(&format!("\n  ref: {quoted}\n")));
        assert!(text.contains(&format!("\nbuild_model: {quoted}\n")));
        assert!(text.contains(&format!("\ngenerator: {quoted}\n")));
        assert!(text.contains(&format!("\n    author: {quoted}\n")));
        assert!(text.contains(&format!("\n    agent_model: {quoted}\n")));

        // And our own reader agrees
        let header = parse_build_header("x_build.md", &text);
        assert_eq!(header.title.as_deref(), Some(value));
        assert_eq!(header.base.ref_name.as_deref(), Some(value));
        assert_eq!(header.build_model.as_deref(), Some(value));
        assert_eq!(header.target.ref_name.as_deref(), Some("feature/x"));
    }
}

#[test]
fn frontmatter_is_valid_yaml_with_nulls() {
    let mut facts = make_facts();
    facts.base = BuildRevision {
        sha: sha('a'),
        short_sha: "aaaaaaa".into(),
        ref_name: None,
    };
    facts.merge_base = None;
    facts.relation = ComparisonRelation::Unrelated;
    facts.commits = vec![];
    facts.stats = BuildStats::default();
    let narrative = BuildNarrative {
        decisions: vec![],
        ..make_narrative()
    };
    let text = render_with(&facts, &narrative, "ai-lens@0.1.0");

    assert!(text.contains(&format!("base:\n  sha: {}\n  ref: null\n", sha('a'))));
    assert!(text.contains("merge_base: null\n"));
    assert!(text.contains("commits: []"));
    assert!(text.contains("## Commits\n\n- None.\n"));
    assert!(text.contains("## Decisions\n\n- None recorded.\n"));

    let header = parse_build_header("x_build.md", &text);
    assert_eq!(
        header.base,
        HeaderRevision {
            sha: Some(sha('a')),
            ref_name: None
        }
    );
    assert!(header.commits.is_empty());
}

#[test]
fn unicode_and_quotes_in_title_and_refs() {
    let mut facts = make_facts();
    facts.target = BuildRevision {
        sha: sha('b'),
        short_sha: "bbbbbbb".into(),
        ref_name: Some("feat/\"quoted\"_\u{e9}|<x>".into()),
    };
    let narrative = BuildNarrative {
        title: "\u{c9}t\u{e9} \"quoted\" \u{1f600} | `x`".into(),
        ..make_narrative()
    };
    let text = render_with(&facts, &narrative, "ai-lens@0.1.0");

    assert!(text.contains("title: \"\u{c9}t\u{e9} \\\"quoted\\\" \u{1f600} | `x`\""));
    assert!(text.contains("  ref: \"feat/\\\"quoted\\\"_\u{e9}|<x>\""));
    // The heading takes the title as is; the revision line escapes the ref
    assert!(text.contains("# \u{c9}t\u{e9} \"quoted\" \u{1f600} | `x`\n"));
    assert!(text.contains("(feat/\"quoted\"\\_\u{e9}\\|\\<x\\>)"));

    let header = parse_build_header("x_build.md", &text);
    assert_eq!(header.title.as_deref(), Some(narrative.title.as_str()));
    assert_eq!(header.target.ref_name, facts.target.ref_name);
}

#[test]
fn diverged_names_the_merge_base() {
    let mut facts = make_facts();
    facts.relation = ComparisonRelation::Diverged;
    facts.merge_base = Some(sha('e'));
    let text = render_facts(&facts);
    assert!(text.contains(
        "`aaaaaaa` (main) → `bbbbbbb` (feature/x) · changes since the merge base `eeeeeee`\n"
    ));
    assert!(text.contains("relation: diverged\n"));
}

#[test]
fn diverged_without_merge_base() {
    let mut facts = make_facts();
    facts.relation = ComparisonRelation::Diverged;
    facts.merge_base = None;
    let text = render_facts(&facts);
    assert!(text.contains("`aaaaaaa` (main) → `bbbbbbb` (feature/x)\n"));
    assert!(text.contains("merge_base: null\n"));
}

// -- Body edge cases --

#[test]
fn newlines_in_commit_fields_stay_on_one_line() {
    let mut facts = make_facts();
    facts.commits[0].what = "Makes\n  load\r\n async".into();
    facts.commits[0].author = "J\nD".into();
    let text = render_facts(&facts);
    assert!(text.contains("### `ccccccc` Makes load async\n"));
    assert!(text.contains("    author: \"J\\nD\""));
}

#[test]
fn why_with_paragraphs_keeps_them_indented() {
    let mut facts = make_facts();
    facts.commits[0].why = Some("  First para\nwrapped.\n\n  Second para.  \n".into());
    let text = render_facts(&facts);
    assert!(text.contains("- Why:\n  First para\n  wrapped.\n\n  Second para.\n\n### `ddddddd`"));
}

#[test]
fn why_star_list_and_crlf() {
    let mut facts = make_facts();
    facts.commits[0].why = Some("Reasons:\r\n* one\r\n* two".into());
    let text = render_facts(&facts);
    assert!(text.contains("- Why:\n  Reasons:\n  * one\n  * two\n"));
}

#[test]
fn hyphen_inside_prose_is_not_a_list() {
    let mut facts = make_facts();
    facts.commits[0].why = Some("Uses a - b\nfor the range.".into());
    assert!(render_facts(&facts).contains("- Why: Uses a - b for the range.\n"));
}

#[test]
fn missing_why_and_unknown_inferred_model() {
    let mut facts = make_facts();
    facts.commits[0].why = None;
    facts.commits[1].why = Some("Guess.".into());
    facts.commits[1].inferred_by = None;
    facts.commits[1].changes_inferred_by = None;
    let text = render_facts(&facts);
    assert!(text.contains("- Why: not recorded.\n"));
    assert!(text.contains("- Why (inferred by a model, not the author’s words): Guess.\n"));
    assert!(text.contains("  - Why (inferred by a model, not the author’s words): A fixed"));
    assert!(!text.contains("inferred_by:"));
}

#[test]
fn agent_without_model() {
    let mut facts = make_facts();
    facts.commits[0].agent_model = None;
    let text = render_facts(&facts);
    assert!(text.contains("- claude-code · source: commit\n"));
    assert!(!text.contains("agent_model:"));
}

#[test]
fn markdown_in_paths_and_authors_is_escaped() {
    let mut facts = make_facts();
    facts.files = vec![BuildFile {
        entries: vec![entry(Some(sha('d')), vec![range(1, 1)], vec![])],
        ..BuildFile::new(
            "a_b/*c*/[d]|e<f>`g`\\h.md",
            GitFileStatus::Added,
            Some(1),
            Some(0),
        )
    }];
    facts.commits[1].author = "_Jane_ *D*".into();
    facts.commits[1].what = "Fix `x` | y".into();
    let text = render_facts(&facts);
    assert!(
        text.contains("### a\\_b/\\*c\\*/\\[d\\]\\|e\\<f\\>\\`g\\`\\\\h.md · A · +1 \u{2212}0\n")
    );
    assert!(text.contains("- human (\\_Jane\\_ \\*D\\*) · source: inferred\n"));
    assert!(text.contains("- **L1** · `ddddddd` · Fix \\`x\\` \\| y\n"));
}

#[test]
fn empty_ranges_and_no_line_changes() {
    let mut facts = make_facts();
    facts.files = vec![
        // An entry without ranges still names its commit
        BuildFile {
            entries: vec![entry(Some(sha('c')), vec![], vec![])],
            ..BuildFile::new("empty.txt", GitFileStatus::Modified, Some(0), Some(0))
        },
        // A mode-only change: no entries, touched by a commit outside the range
        BuildFile {
            touched_by: vec![sha('f')],
            ..BuildFile::new("script.sh", GitFileStatus::Modified, Some(0), None)
        },
        BuildFile::new("gone.txt", GitFileStatus::Deleted, Some(0), Some(0)),
    ];
    let text = render_facts(&facts);
    assert!(
        text.contains("### empty.txt · M · +0 \u{2212}0\n\n-  · `ccccccc` · Makes load async\n")
    );
    assert!(text.contains(
        "### script.sh · M · +0 \u{2212}0\n\n- No line changes; changed by `fffffff`.\n"
    ));
    assert!(text.contains("### gone.txt · D · +0 \u{2212}0\n\n- No line changes.\n"));
}

#[test]
fn binary_without_commits_and_ignored_binary() {
    let mut facts = make_facts();
    facts.files = vec![
        BuildFile {
            binary: true,
            ..BuildFile::new("a.bin", GitFileStatus::Added, None, None)
        },
        BuildFile {
            binary: true,
            ignored: true,
            ..BuildFile::new("b.bin", GitFileStatus::Added, None, None)
        },
    ];
    let text = render_facts(&facts);
    assert!(text.contains("### a.bin · A · binary\n\n- Binary file.\n"));
    assert!(text.contains("### b.bin · A · binary\n\n- Left out: generated"));
}

#[test]
fn risks_and_decisions_without_commits() {
    let narrative = BuildNarrative {
        decisions: vec![
            BuildDecision {
                text: "No commits.".into(),
                commits: vec![],
            },
            BuildDecision {
                text: "Two.".into(),
                commits: vec!["a".into(), "b".into()],
            },
        ],
        risks: vec!["One.".into(), "Two.".into()],
        ..make_narrative()
    };
    let text = render_with(&make_facts(), &narrative, "ai-lens@0.1.0");
    assert!(text.contains("- No commits.\n- Two. (`a`, `b`)\n"));
    assert!(text.ends_with("## Risks / open questions\n\n- One.\n- Two.\n"));
}

#[test]
fn no_files() {
    let mut facts = make_facts();
    facts.files = vec![];
    assert!(render_facts(&facts).contains("## Changes\n\n## Risks"));
}

// -- Header edge cases --

#[test]
fn header_tolerates_junk() {
    let lines = [
        "---",
        "title: unquoted title",
        "created: \"not json",
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
    ];
    let header = parse_build_header("x_build.md", &lines.join("\n"));
    assert_eq!(header.title.as_deref(), Some("unquoted title"));
    assert_eq!(header.created, None);
    assert_eq!(header.build_model.as_deref(), Some("42"));
    assert_eq!(
        header.base,
        HeaderRevision {
            sha: Some("abc".into()),
            ref_name: None
        }
    );
    assert_eq!(header.target, HeaderRevision::default());
    assert_eq!(
        header.commits,
        [HeaderCommit::new("0123abc", Some("789def"))]
    );
}

#[test]
fn header_with_crlf_is_not_read() {
    let text = render().replace('\n', "\r\n");
    assert_eq!(
        parse_build_header("x_build.md", &text),
        BuildHeader::new("x_build.md")
    );
}

#[test]
fn header_of_empty_file() {
    assert_eq!(
        parse_build_header("x_build.md", ""),
        BuildHeader::new("x_build.md")
    );
}

#[test]
fn header_title_with_escapes() {
    let header = parse_build_header("x_build.md", "---\ntitle: \"a\\u00e9\\n\\\"b\\\"\"\n---\n");
    assert_eq!(header.title.as_deref(), Some("a\u{e9}\n\"b\""));
}

// -- Index edge cases --

#[test]
fn index_with_no_builds() {
    let expected = [
        "# AI Lens builds",
        "",
        "Each build explains why the code changed between two commits: what every commit did, why, and which lines",
        "it touched. Read the build that covers the code you are about to change. Newest first.",
        "",
        "| Build | Base → Target | Created | Model | Summary |",
        "| ----- | ------------- | ------- | ----- | ------- |",
        "",
    ];
    assert_eq!(render_index(&[]), expected.join("\n"));
}

#[test]
fn index_cells_escape_pipes_and_newlines() {
    let header = BuildHeader {
        title: Some("a | b\n  c".into()),
        base: HeaderRevision {
            sha: None,
            ref_name: Some("fe|at".into()),
        },
        ..BuildHeader::new("x_build.md")
    };
    assert_eq!(
        render_index(&[header]).split('\n').nth(7).unwrap(),
        "| [x](x_build.md) | fe\\|at → ? |  |  | a \\| b c |"
    );
}

#[test]
fn index_orders_undated_last_then_by_name() {
    let header = |name: &str, created: Option<&str>| BuildHeader {
        created: created.map(str::to_string),
        ..BuildHeader::new(name)
    };
    let headers = [
        header("b_build.md", None),
        header("a_build.md", None),
        header("c_build.md", Some("2026-01-01T00:00:00Z")),
        header("d_build.md", Some("2026-02-01T00:00:00Z")),
    ];
    let index = render_index(&headers);
    let names: Vec<&str> = index
        .split('\n')
        .skip(7)
        .take(4)
        .map(|row| row.split("](").nth(1).unwrap().split(')').next().unwrap())
        .collect();
    assert_eq!(
        names,
        ["d_build.md", "c_build.md", "a_build.md", "b_build.md"]
    );
}

#[test]
fn locale_compare_approximates_icu() {
    use std::cmp::Ordering::*;
    assert_eq!(locale_compare("a", "b"), Less);
    assert_eq!(locale_compare("b", "a"), Greater);
    assert_eq!(locale_compare("a", "a"), Equal);
    assert_eq!(locale_compare("", "a"), Less);
    // Punctuation before digits before letters, case-insensitive first
    assert_eq!(locale_compare("abcdef1_x", "abcdef12_x"), Less);
    assert_eq!(locale_compare("9", "a"), Less);
    assert_eq!(locale_compare("a", "B"), Less);
    assert_eq!(locale_compare("a", "A"), Less);
}

// -- merge_ranges --

#[test]
fn merge_ranges_joins_overlaps_and_neighbours() {
    assert_eq!(merge_ranges(&[]), []);
    assert_eq!(
        merge_ranges(&[range(5, 6), range(1, 2), range(3, 3)]),
        [range(1, 3), range(5, 6)]
    );
    assert_eq!(
        merge_ranges(&[range(1, 10), range(2, 3), range(11, 12)]),
        [range(1, 12)]
    );
    assert_eq!(
        merge_ranges(&[range(4, 4), range(6, 6)]),
        [range(4, 4), range(6, 6)]
    );
}

// -- justifications --

#[test]
fn justifications_follow_their_whys() {
    let mut facts = make_facts();
    facts.commits[0].justification = Some("The profile shows\nthe stall.".into());
    facts.commits[1].justification = Some("- Logs show 429s.\n- Retries doubled.".into());
    if let Some(changes) = facts.files[0].entries[2].changes.as_mut() {
        changes[0].justification = Some("Only a backoff\nclears the limit.".into());
    }
    let text = render_facts(&facts);
    assert!(text.contains(
        "- Why: The cache reads from disk.\n- Justification: The profile shows the stall.\n\n### `ddddddd`"
    ));
    assert!(text.contains(
        "  - Fewer retries.\n- Justification:\n  - Logs show 429s.\n  - Retries doubled.\n\n## Changes"
    ));
    assert!(text.contains(
        "  - Why (inferred by claude-sonnet-5-5, not the author’s words): A fixed retry hit the limit again.\n  - Justification: Only a backoff clears the limit.\n"
    ));
    // Nothing else changes
    let without: Vec<&str> = text
        .lines()
        .filter(|line| {
            !line.contains("Justification")
                && !line.contains("Logs show")
                && !line.contains("Retries doubled")
        })
        .collect();
    assert_eq!(without.join("\n") + "\n", render());
}

#[test]
fn empty_justifications_are_left_out() {
    let mut facts = make_facts();
    facts.commits[0].justification = Some("  ".into());
    assert_eq!(render_facts(&facts), render());
}
