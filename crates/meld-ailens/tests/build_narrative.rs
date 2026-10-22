//! Port of test/ailens/test_build_narrative.py (itself ported from AI Lens
//! src/build/__tests__/buildNarrative.test.ts, plus edge cases)

use std::sync::{Arc, Mutex};

use meld_ailens::build_facts::{
    BuildAgent, BuildCommit, BuildFacts, BuildFile, BuildFileEntry, BuildRevision, BuildStats,
};
use meld_ailens::build_narrative::{
    BUILD_PROMPT_VERSION, BuildDecision, BuildNarrative, MAX_COMMITS, MAX_FILES, MAX_WHY_CHARS,
    SCHEMA, SYSTEM_PROMPT, build_narrative_prompt, one_line, parse_narrative, truncate,
    write_narrative,
};
use meld_ailens::claude::{ClaudeRequest, ClaudeResponse, RunClaude};
use meld_ailens::{
    CancelToken, ComparisonRelation, Error, ExplanationSource, GitFileStatus, LineRange, infer,
};
use serde_json::{Value, json};

fn make_facts() -> BuildFacts {
    BuildFacts {
        base: BuildRevision {
            sha: "a".repeat(40),
            short_sha: "aaaaaaa".into(),
            ref_name: Some("main".into()),
        },
        target: BuildRevision {
            sha: "b".repeat(40),
            short_sha: "bbbbbbb".into(),
            ref_name: None,
        },
        merge_base: Some("a".repeat(40)),
        relation: ComparisonRelation::Ancestor,
        stats: BuildStats {
            files: 1,
            insertions: 1,
            deletions: 0,
            commits: 2,
        },
        commits: vec![
            BuildCommit {
                sha: "c".repeat(40),
                short_sha: "ccccccc".into(),
                patch_id: None,
                author: "Jane".into(),
                agent: BuildAgent::ClaudeCode,
                agent_model: Some("claude-opus-5-5".into()),
                agent_name: Some("Claude Opus 5.5".into()),
                what: "Adds X".into(),
                why: Some("Needed\nfor Y.".into()),
                why_source: ExplanationSource::Commit,
                inferred_by: None,
                changes_inferred_by: None,
            },
            BuildCommit {
                sha: "d".repeat(40),
                short_sha: "ddddddd".into(),
                patch_id: None,
                author: "Jim".into(),
                agent: BuildAgent::Human,
                agent_model: None,
                agent_name: None,
                what: "tweak".into(),
                why: None,
                why_source: ExplanationSource::Commit,
                inferred_by: None,
                changes_inferred_by: None,
            },
        ],
        files: vec![BuildFile {
            entries: vec![BuildFileEntry {
                sha: Some("c".repeat(40)),
                added: vec![LineRange::new(1, 1)],
                ..Default::default()
            }],
            ..BuildFile::new("src/x.ts", GitFileStatus::Added, Some(1), Some(0))
        }],
    }
}

fn by(sha: Option<String>) -> BuildFileEntry {
    BuildFileEntry {
        sha,
        ..Default::default()
    }
}

fn decision(text: &str, commits: &[&str]) -> BuildDecision {
    BuildDecision {
        text: text.into(),
        commits: commits.iter().map(|c| c.to_string()).collect(),
    }
}

fn missing_parts(result: meld_ailens::Result<BuildNarrative>) {
    let err = result.unwrap_err();
    assert!(err.to_string().contains("missing parts"), "{err}");
}

// -- Ported --

#[test]
fn prompt_lists_commits_and_files() {
    let prompt = build_narrative_prompt(&make_facts());
    assert!(prompt.contains("Range: main (aaaaaaa) → bbbbbbb"));
    assert!(
        prompt
            .contains("- ccccccc · Claude Opus 5.5 with Jane · What: Adds X · Why: Needed for Y.")
    );
    assert!(prompt.contains("- ddddddd · Jim · What: tweak · Why: not recorded"));
    assert!(prompt.contains("A src/x.ts · +1 \u{2212}0 · by ccccccc"));
}

#[test]
fn parse_keeps_known_commits_and_drops_empty_items() {
    let parsed = parse_narrative(
        &json!({
            "title": " Adds X\n",
            "summary": "Summary.",
            "decisions": [
                {"text": "Use X.", "commits": ["ccccccc", "zzzzzzz"]},
                {"text": " ", "commits": []},
            ],
            "risks": ["ddddddd has no reason.", ""],
        }),
        &make_facts(),
        "",
    )
    .unwrap();
    assert_eq!(
        parsed,
        BuildNarrative {
            title: "Adds X".into(),
            summary: "Summary.".into(),
            decisions: vec![decision("Use X.", &["ccccccc"])],
            risks: vec!["ddddddd has no reason.".into()],
            model: String::new(),
        }
    );
}

#[test]
fn parse_rejects_missing_parts() {
    missing_parts(parse_narrative(&json!({"title": "x"}), &make_facts(), ""));
}

// -- Prompt --

#[test]
fn prompt_exact() {
    let mut facts = make_facts();
    let (c, d, e) = ("c".repeat(40), "d".repeat(40), "e".repeat(40));
    facts.files.push(BuildFile {
        original_path: Some("old.ts".into()),
        entries: vec![
            by(Some(d.clone())),
            by(None),
            by(Some(c)),
            by(Some(d)),
            by(Some(e.clone())),
        ],
        ..BuildFile::new("new.ts", GitFileStatus::Renamed, Some(2), Some(3))
    });
    facts.files.push(BuildFile {
        binary: true,
        ..BuildFile::new("img.png", GitFileStatus::Modified, None, None)
    });
    facts.files.push(BuildFile {
        ignored: true,
        ..BuildFile::new("lock", GitFileStatus::Modified, Some(9), Some(9))
    });
    facts
        .files
        .push(BuildFile::new("n.ts", GitFileStatus::Modified, None, None));
    let expected = [
        "Range: main (aaaaaaa) → bbbbbbb".to_string(),
        "2 commits, 1 files, +1 \u{2212}0 lines".into(),
        "".into(),
        "Commits, oldest first:".into(),
        "- ccccccc · Claude Opus 5.5 with Jane · What: Adds X · Why: Needed for Y.".into(),
        "- ddddddd · Jim · What: tweak · Why: not recorded".into(),
        "".into(),
        "Changed files:".into(),
        "A src/x.ts · +1 \u{2212}0 · by ccccccc".into(),
        format!("R old.ts -> new.ts · +2 \u{2212}3 · by ddddddd, ccccccc, {e}"),
        "M img.png · binary".into(),
        "M lock · left out (generated or ignored)".into(),
        "M n.ts · +0 \u{2212}0".into(),
    ];
    assert_eq!(build_narrative_prompt(&facts), expected.join("\n"));
}

#[test]
fn prompt_diverged_inferred_and_unnamed_agent() {
    let mut facts = make_facts();
    facts.relation = ComparisonRelation::Diverged;
    facts.commits[0].agent_name = None;
    facts.commits[0].why = Some("Guess\n  wrapped.".into());
    facts.commits[0].why_source = ExplanationSource::Inferred;
    let prompt = build_narrative_prompt(&facts);
    assert!(prompt.starts_with(
        "Range: main (aaaaaaa) → bbbbbbb (diverged branches; changes since their merge base)\n"
    ));
    assert!(prompt.contains(
        "- ccccccc · Claude with Jane · What: Adds X · Why (inferred, not the author’s words): Guess wrapped."
    ));
}

#[test]
fn prompt_cuts_long_ranges_and_says_so() {
    let mut facts = make_facts();
    let commit = facts.commits[1].clone();
    facts.commits = (0..MAX_COMMITS + 3)
        .map(|i| BuildCommit {
            sha: format!("{i:040x}"),
            short_sha: format!("{i:07x}"),
            ..commit.clone()
        })
        .collect();
    facts.files = (0..MAX_FILES + 2)
        .map(|i| BuildFile::new(format!("f{i}"), GitFileStatus::Modified, Some(1), Some(1)))
        .collect();
    let prompt = build_narrative_prompt(&facts);
    assert_eq!(prompt.matches("\n- ").count(), MAX_COMMITS);
    assert!(prompt.contains("\n(3 more commits left out)\n"));
    assert!(prompt.ends_with(&format!(
        "M f{} · +1 \u{2212}1\n(2 more files left out)",
        MAX_FILES - 1
    )));
}

#[test]
fn prompt_truncates_long_whys() {
    let mut facts = make_facts();
    facts.commits[0].why = Some("x".repeat(1000));
    let prompt = build_narrative_prompt(&facts);
    let line = prompt.split('\n').nth(4).unwrap();
    assert!(line.ends_with(&format!("Why: {}…", "x".repeat(MAX_WHY_CHARS - 1))));
}

#[test]
fn truncate_counts_utf16_units() {
    assert_eq!(truncate("abc", 3), "abc");
    assert_eq!(truncate("abcd", 3), "ab…");
    // An emoji is two units, as in JavaScript
    assert_eq!(truncate("\u{1f600}\u{1f600}", 4), "\u{1f600}\u{1f600}");
    assert_eq!(truncate("\u{1f600}\u{1f600}", 3), "\u{1f600}…");
    // A pair the cut would split is dropped whole
    assert_eq!(truncate("a\u{1f600}bc", 3), "a…");
}

#[test]
fn one_line_uses_javascript_whitespace() {
    assert_eq!(one_line("  a \n\t b \u{a0}\n\u{3000}c  "), "a b c");
    assert_eq!(one_line("a\r\nb"), "a b");
    // \x1c is whitespace to Python, not to JavaScript
    assert_eq!(one_line("\x1ca\x1c"), "\x1ca\x1c");
}

// -- Reply --

#[test]
fn parse_rejects_malformed() {
    for output in [
        Value::Null,
        json!([]),
        json!("text"),
        json!({"title": "t", "summary": "s", "decisions": [], "risks": "none"}),
        json!({"title": 1, "summary": "s", "decisions": [], "risks": []}),
        json!({"title": "t", "summary": null, "decisions": [], "risks": []}),
        json!({"title": "t", "summary": "s", "decisions": {}, "risks": []}),
    ] {
        missing_parts(parse_narrative(&output, &make_facts(), ""));
    }
}

#[test]
fn parse_cleans_up() {
    let parsed = parse_narrative(
        &json!({
            "title": " \n ",
            "summary": "  Two\nlines.  \n",
            "decisions": [
                "not an object",
                null,
                {"text": 5, "commits": ["ccccccc"]},
                {"text": "No list.", "commits": "ccccccc"},
                {"text": "Missing commits."},
                {
                    "text": "Many\n lines.",
                    "commits": [" ccccccc\n", 7, "", "ddddddd", "c".repeat(40)],
                },
            ],
            "risks": [null, 3, "  ", "A\nrisk.", {"text": "x"}],
            "extra": "ignored",
        }),
        &make_facts(),
        "claude-x",
    )
    .unwrap();
    assert_eq!(
        parsed,
        BuildNarrative {
            title: "Changes".into(),
            summary: "Two\nlines.".into(),
            decisions: vec![
                decision("No list.", &[]),
                decision("Missing commits.", &[]),
                // Full SHAs aren't short SHAs from the list
                decision("Many lines.", &["ccccccc", "ddddddd"]),
            ],
            risks: vec!["A risk.".into()],
            model: "claude-x".into(),
        }
    );
}

#[test]
fn parse_drops_unknown_shas() {
    let parsed = parse_narrative(
        &json!({
            "title": "t",
            "summary": "s",
            "decisions": [{"text": "d", "commits": ["eeeeeee", "CCCCCCC", "ccccccc"]}],
            "risks": [],
        }),
        &make_facts(),
        "",
    )
    .unwrap();
    assert_eq!(parsed.decisions, [decision("d", &["ccccccc"])]);
}

// -- write_narrative --

fn runner(
    output: Value,
    model: &str,
    seen: Arc<Mutex<Vec<(ClaudeRequest, CancelToken)>>>,
) -> RunClaude {
    let model = model.to_string();
    Arc::new(move |request, cancel| {
        seen.lock().unwrap().push((request.clone(), cancel.clone()));
        Ok(ClaudeResponse::new(output.clone(), "", model.clone()))
    })
}

#[test]
fn write_narrative_sends_one_request_with_the_schema() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let run = runner(
        json!({
            "title": "Adds X",
            "summary": "S.",
            "decisions": [{"text": "D.", "commits": ["ccccccc"]}],
            "risks": [],
        }),
        "claude-opus-5-5-20261001",
        seen.clone(),
    );
    let cancel = CancelToken::new();
    let facts = make_facts();
    let narrative = write_narrative(
        &facts,
        "claude-opus-5-5",
        Some("/opt/claude"),
        Some(run),
        &cancel,
    )
    .unwrap();

    assert_eq!(
        narrative,
        BuildNarrative {
            title: "Adds X".into(),
            summary: "S.".into(),
            decisions: vec![decision("D.", &["ccccccc"])],
            risks: vec![],
            model: "claude-opus-5-5-20261001".into(),
        }
    );
    let calls = seen.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let (request, passed_cancel) = &calls[0];
    assert_eq!(*passed_cancel, cancel);
    assert_eq!(request.prompt, build_narrative_prompt(&facts));
    assert_eq!(request.system_prompt.as_deref(), Some(SYSTEM_PROMPT));
    assert_eq!(request.model, "claude-opus-5-5");
    assert_eq!(request.json_schema.as_ref(), Some(&*SCHEMA));
    assert_eq!(request.claude_path, "/opt/claude");
}

#[test]
fn write_narrative_leaves_claude_path_default() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let run = runner(
        json!({"title": "t", "summary": "s", "decisions": [], "risks": []}),
        "m",
        seen.clone(),
    );
    write_narrative(
        &make_facts(),
        "claude-opus-5-5",
        None,
        Some(run),
        &CancelToken::new(),
    )
    .unwrap();
    let default = ClaudeRequest::new("", "");
    assert_eq!(seen.lock().unwrap()[0].0.claude_path, default.claude_path);
}

#[test]
fn write_narrative_rejects_bad_reply() {
    let run = runner(json!({"title": "t"}), "m", Arc::default());
    missing_parts(write_narrative(
        &make_facts(),
        "m",
        None,
        Some(run),
        &CancelToken::new(),
    ));
}

#[test]
fn write_narrative_propagates_run_errors() {
    let run: RunClaude = Arc::new(|_, _| Err(Error::msg("Claude Code is not installed.")));
    let err =
        write_narrative(&make_facts(), "m", None, Some(run), &CancelToken::new()).unwrap_err();
    assert!(err.to_string().contains("not installed"));
}

#[test]
fn schema_and_version() {
    assert_eq!(BUILD_PROMPT_VERSION, 1);
    assert_eq!(
        SCHEMA["required"],
        json!(["title", "summary", "decisions", "risks"])
    );
    assert_eq!(SCHEMA["additionalProperties"], json!(false));
    assert!(SYSTEM_PROMPT.starts_with("You write the summary of a range"));
    assert!(SYSTEM_PROMPT.ends_with("Plain text, no markdown."));
}

// -- The prompts and schemas are byte-identical to the Python ones --

fn sha1(text: &str) -> String {
    sha1_smol::Sha1::from(text.as_bytes()).digest().to_string()
}

/// SHA-1s of the Python constants (`meld/ailens/infer.py`,
/// `build_narrative.py`), so a stray edit to a prompt is caught
#[test]
fn prompts_match_python() {
    assert_eq!(
        sha1(SYSTEM_PROMPT),
        "53c71508d0f53d3367a841e2558a1bbf1497d362"
    );
    assert_eq!(
        sha1(infer::SYSTEM_PROMPT),
        "f02fcc3240377c63ad9dc6e72112fc5f89f8619b"
    );
    assert_eq!(
        sha1(infer::UNCOMMITTED_SYSTEM_PROMPT),
        "c4badc9c354ad2b094a58e118a319ebe78c36eef"
    );
}

/// `json.dumps(schema, separators=(",", ":"))` of the Python schemas, as
/// passed to `claude --json-schema`
#[test]
fn schemas_match_python() {
    assert_eq!(
        SCHEMA.to_string(),
        r#"{"type":"object","properties":{"title":{"type":"string"},"summary":{"type":"string"},"decisions":{"type":"array","items":{"type":"object","properties":{"text":{"type":"string"},"commits":{"type":"array","items":{"type":"string"}}},"required":["text","commits"],"additionalProperties":false}},"risks":{"type":"array","items":{"type":"string"}}},"required":["title","summary","decisions","risks"],"additionalProperties":false}"#
    );
    assert_eq!(
        infer::SCHEMA.to_string(),
        r#"{"type":"object","properties":{"what":{"type":"string"},"why":{"type":"string"},"changes":{"type":"array","items":{"type":"object","properties":{"change":{"type":"integer"},"what":{"type":"string"},"why":{"type":"string"}},"required":["change","what","why"],"additionalProperties":false}}},"required":["what","why","changes"],"additionalProperties":false}"#
    );
    assert_eq!(
        infer::UNCOMMITTED_SCHEMA.to_string(),
        r#"{"type":"object","properties":{"changes":{"type":"array","items":{"type":"object","properties":{"change":{"type":"integer"},"what":{"type":"string"},"why":{"type":"string"}},"required":["change","what","why"],"additionalProperties":false}}},"required":["changes"],"additionalProperties":false}"#
    );
}
