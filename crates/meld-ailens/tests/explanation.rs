//! Port of test/ailens/test_explanation.py

use meld_ailens::explanation::{
    MessageFormat, ParsedNote, compose_note, explain_commit, find_change_explanations, format_note,
    parse_note,
};
use meld_ailens::{
    ChangeExplanation, ChangeGuide, ChangeKind, ChangeStep, CommitExplanation, CommitHunk,
    ExplanationSource, GitCommit, GitIdentity, GitTrailer, LineOrigin, Side, StepLines,
};

use ExplanationSource::{Commit, Inferred, Note};

#[derive(Default)]
struct C<'a> {
    subject: Option<&'a str>,
    body: &'a str,
    note: Option<&'a str>,
    trailers: Vec<GitTrailer>,
}

fn commit(c: C<'_>) -> GitCommit {
    let identity = GitIdentity {
        name: "Jane Doe".into(),
        email: "jane@example.com".into(),
        date: "2024-01-01T00:00:00Z".into(),
    };
    GitCommit {
        sha: "a".repeat(40),
        parents: Vec::new(),
        author: identity.clone(),
        committer: identity,
        subject: c.subject.unwrap_or("Subject").into(),
        body: c.body.into(),
        trailers: c.trailers,
        note: c.note.map(str::to_string),
    }
}

fn explain(c: C<'_>) -> CommitExplanation {
    explain_commit(&commit(c), &MessageFormat::default())
}

fn claude(value: &str) -> GitTrailer {
    GitTrailer::new("Co-Authored-By", value)
}

fn expl(
    what: &str,
    why: Option<&str>,
    source: ExplanationSource,
    model: Option<&str>,
) -> CommitExplanation {
    CommitExplanation {
        what: what.into(),
        why: why.map(str::to_string),
        justification: None,
        source,
        model: model.map(str::to_string),
    }
}

struct Ch<'a> {
    path: &'a str,
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
    what: &'a str,
    why: &'a str,
    original_path: Option<&'a str>,
}

impl Default for Ch<'_> {
    fn default() -> Self {
        Self {
            path: "a.ts",
            old_start: 1,
            old_count: 0,
            new_start: 1,
            new_count: 0,
            what: "w",
            why: "y",
            original_path: None,
        }
    }
}

fn hunk(
    path: &str,
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
    original_path: Option<&str>,
) -> CommitHunk {
    CommitHunk {
        path: path.into(),
        old_start,
        old_count,
        new_start,
        new_count,
        original_path: original_path.map(str::to_string),
    }
}

fn change(c: Ch<'_>) -> ChangeExplanation {
    ChangeExplanation {
        hunk: hunk(
            c.path,
            c.old_start,
            c.old_count,
            c.new_start,
            c.new_count,
            c.original_path,
        ),
        what: c.what.into(),
        why: c.why.into(),
        justification: None,
        guide: Default::default(),
    }
}

fn note(
    what: Option<&str>,
    why: Option<&str>,
    source: ExplanationSource,
    model: Option<&str>,
    head: &str,
) -> ParsedNote {
    ParsedNote {
        what: what.map(str::to_string),
        why: why.map(str::to_string),
        justification: None,
        source,
        model: model.map(str::to_string),
        change_model: None,
        changes: Vec::new(),
        changes_by_block: false,
        head: head.into(),
    }
}

fn origin(path: &str, side: Side, line: usize) -> LineOrigin {
    LineOrigin::new(path, side, line)
}

// -- explain_commit --

#[test]
fn uses_the_subject_as_what_and_the_body_without_trailers_as_why() {
    let explanation = explain(C {
        subject: Some("Makes load async"),
        body: "The cache now reads from disk.\n\nThat needs I/O.\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>",
        trailers: vec![claude("Claude Opus 5.5 <noreply@anthropic.com>")],
        ..Default::default()
    });
    assert_eq!(
        explanation,
        expl(
            "Makes load async",
            Some("The cache now reads from disk.\n\nThat needs I/O."),
            Commit,
            None
        )
    );
}

#[test]
fn has_no_why_when_the_body_is_only_trailers_or_empty() {
    assert_eq!(
        explain(C {
            body: "Signed-off-by: Jane <j@x>",
            trailers: vec![GitTrailer::new("Signed-off-by", "Jane <j@x>")],
            ..Default::default()
        })
        .why,
        None
    );
    assert_eq!(explain(C::default()).why, None);
}

#[test]
fn keeps_a_last_paragraph_that_only_looks_like_trailers_when_git_found_none() {
    assert_eq!(
        explain(C {
            body: "Note: this is prose.",
            ..Default::default()
        })
        .why
        .as_deref(),
        Some("Note: this is prose.")
    );
}

#[test]
fn trailer_block_with_continuation_lines_is_stripped() {
    let explanation = explain(C {
        body: "Reason.\n\nSigned-off-by: Jane\n  continued\nAcked-by: Bob",
        trailers: vec![
            GitTrailer::new("Signed-off-by", "Jane continued"),
            GitTrailer::new("Acked-by", "Bob"),
        ],
        ..Default::default()
    });
    assert_eq!(explanation.why.as_deref(), Some("Reason."));
}

#[test]
fn a_last_paragraph_with_prose_is_kept_even_with_trailers() {
    let explanation = explain(C {
        body: "Reason.\n\nSigned-off-by: Jane\nthis is prose",
        trailers: vec![GitTrailer::new("Signed-off-by", "Jane")],
        ..Default::default()
    });
    assert_eq!(
        explanation.why.as_deref(),
        Some("Reason.\n\nSigned-off-by: Jane\nthis is prose")
    );
}

#[test]
fn crlf_messages() {
    let explanation = explain(C {
        body: "First line.\r\nSecond line.\r\n\r\nCo-Authored-By: Claude <noreply@anthropic.com>\r\n",
        trailers: vec![claude("Claude <noreply@anthropic.com>")],
        ..Default::default()
    });
    assert_eq!(
        explanation.why.as_deref(),
        Some("First line.\nSecond line.")
    );

    let labeled = explain(C {
        body: "What: Adds X\r\n\r\nWhy: Because\r\nof Y.\r\n",
        ..Default::default()
    });
    assert_eq!(
        labeled,
        expl("Adds X", Some("Because\nof Y."), Commit, None)
    );
}

#[test]
fn whitespace_only_body_has_no_why() {
    assert_eq!(
        explain(C {
            body: "  \n\n \t\n",
            ..Default::default()
        })
        .why,
        None
    );
}

#[test]
fn unicode_messages() {
    let explanation = explain(C {
        subject: Some("Ändert ☃"),
        body: "Weil 雪 fällt.",
        ..Default::default()
    });
    assert_eq!(
        explanation,
        expl("Ändert ☃", Some("Weil 雪 fällt."), Commit, None)
    );
}

// -- explanation sources --

#[test]
fn prefers_what_why_labeled_in_the_message_as_trailers_or_body_paragraphs() {
    assert_eq!(
        explain(C {
            trailers: vec![GitTrailer::new("Agent-Why", "Users asked for it")],
            note: Some("Why: from a note"),
            ..Default::default()
        }),
        expl("Subject", Some("Users asked for it"), Commit, None)
    );

    assert_eq!(
        explain(C {
            body: "What: Adds backoff\n\nWhy: Retries hit the\nrate limit.\n\nOther text.",
            ..Default::default()
        }),
        expl(
            "Adds backoff",
            Some("Retries hit the\nrate limit."),
            Commit,
            None
        )
    );
}

#[test]
fn uses_labels_from_the_configured_format() {
    let format = MessageFormat {
        what_keys: vec!["Change".into()],
        why_keys: vec!["Reason".into()],
        justification_keys: vec!["Evidence".into()],
    };
    let reason = commit(C {
        trailers: vec![GitTrailer::new("reason", "Because")],
        ..Default::default()
    });
    assert_eq!(
        explain_commit(&reason, &format).why.as_deref(),
        Some("Because")
    );
    let why = commit(C {
        trailers: vec![GitTrailer::new("Why", "Ignored")],
        ..Default::default()
    });
    assert_eq!(explain_commit(&why, &format).why, None);
}

#[test]
fn default_format() {
    assert_eq!(
        MessageFormat::default(),
        MessageFormat {
            what_keys: vec!["What".into(), "Agent-What".into()],
            why_keys: vec!["Why".into(), "Agent-Why".into()],
            justification_keys: vec!["Justification".into(), "Agent-Justification".into()],
        }
    );
}

#[test]
fn the_first_label_wins_and_trailers_beat_paragraphs() {
    let explanation = explain(C {
        body: "Why: from the body\n\nWhy: second\n\nWhy: from a trailer",
        trailers: vec![GitTrailer::new("Why", "from a trailer")],
        ..Default::default()
    });
    assert_eq!(explanation.why.as_deref(), Some("from a trailer"));
    assert_eq!(
        explain(C {
            body: "Why: first\n\nWhy: second",
            ..Default::default()
        })
        .why
        .as_deref(),
        Some("first")
    );
}

#[test]
fn a_labeled_what_without_a_why_falls_through_to_the_body() {
    let explanation = explain(C {
        body: "What: Adds X\n\nPlain reason.",
        ..Default::default()
    });
    assert_eq!(
        explanation,
        expl(
            "Adds X",
            Some("What: Adds X\n\nPlain reason."),
            Commit,
            None
        )
    );
}

#[test]
fn an_empty_labeled_why_counts_as_missing() {
    let explanation = explain(C {
        body: "Why:\n\nPlain.",
        ..Default::default()
    });
    assert_eq!(explanation.why.as_deref(), Some("Why:\n\nPlain."));
    assert_eq!(explanation.source, Commit);
}

#[test]
fn uses_the_note_before_the_plain_body() {
    assert_eq!(
        explain(C {
            body: "Plain body.",
            note: Some("Source: inferred\nModel: claude-sonnet-5-5\nWhat: Adds X\nWhy: Because Y."),
            ..Default::default()
        }),
        expl(
            "Adds X",
            Some("Because Y."),
            Inferred,
            Some("claude-sonnet-5-5")
        )
    );

    assert_eq!(
        explain(C {
            subject: Some("S"),
            note: Some("Written by hand\nover two lines"),
            ..Default::default()
        }),
        expl("S", Some("Written by hand\nover two lines"), Note, None)
    );
}

#[test]
fn falls_back_to_the_body_and_has_no_why_when_there_is_none() {
    assert_eq!(
        explain(C {
            body: "Plain body.",
            ..Default::default()
        }),
        expl("Subject", Some("Plain body."), Commit, None)
    );
    assert_eq!(
        explain(C {
            note: Some("Source: inferred\nModel: m"),
            ..Default::default()
        }),
        expl("Subject", None, Commit, None)
    );
}

#[test]
fn an_empty_note_is_ignored() {
    for empty in ["", "\n  \n"] {
        assert_eq!(
            explain(C {
                body: "Body.",
                note: Some(empty),
                ..Default::default()
            })
            .why
            .as_deref(),
            Some("Body.")
        );
    }
}

// -- notes --

#[test]
fn round_trips_through_format_note_and_parse_note() {
    let text = format_note(
        "Adds X\nsecond line",
        "Because Y.\n\nAnd Z.",
        None,
        Inferred,
        Some("claude-sonnet-5-5"),
    );

    assert_eq!(
        text,
        "Source: inferred\nModel: claude-sonnet-5-5\nWhat: Adds X second line\nWhy: Because Y.\n\nAnd Z.\n"
    );
    assert_eq!(
        parse_note(&text),
        note(
            Some("Adds X second line"),
            Some("Because Y.\n\nAnd Z."),
            Inferred,
            Some("claude-sonnet-5-5"),
            text.trim()
        )
    );
}

#[test]
fn format_note_without_a_model() {
    assert_eq!(
        format_note("W", "Y", None, Note, None),
        "Source: note\nWhat: W\nWhy: Y\n"
    );
}

#[test]
fn keeps_the_explanations_of_single_changes_after_the_commits_own() {
    let changes = vec![
        change(Ch {
            path: "src/a b.ts",
            old_start: 3,
            old_count: 2,
            new_start: 3,
            new_count: 4,
            what: "Awaits the load",
            why: "The cache reads from disk.\n\nChange: -1,1 +1,1 x looks like a header.",
            ..Default::default()
        }),
        change(Ch {
            path: "src/new.ts",
            original_path: Some("src/old.ts"),
            old_start: 0,
            old_count: 0,
            new_start: 1,
            new_count: 2,
            what: "Adds the file",
            why: "Room for the helper.",
        }),
    ];
    let text = compose_note("Plain note by hand.", "claude-sonnet-5-5", &changes);

    let expected = [
        "Plain note by hand.",
        "",
        "Change-Model: claude-sonnet-5-5",
        "Change-Unit: block",
        "",
        "Change: -3,2 +3,4 src/a b.ts",
        "What: Awaits the load",
        "Why: The cache reads from disk.",
        "",
        " Change: -1,1 +1,1 x looks like a header.",
        "",
        "Change: -0,0 +1,2 src/new.ts",
        "Change-From: src/old.ts",
        "What: Adds the file",
        "Why: Room for the helper.",
        "",
    ];
    assert_eq!(text, expected.join("\n"));
    assert_eq!(
        parse_note(&text),
        ParsedNote {
            change_model: Some("claude-sonnet-5-5".into()),
            changes_by_block: true,
            changes: vec![
                ChangeExplanation {
                    hunk: changes[0].hunk.clone(),
                    what: "Awaits the load".into(),
                    justification: None,
                    why: "The cache reads from disk.\n\n Change: -1,1 +1,1 x looks like a header."
                        .into(),
                    guide: Default::default(),
                },
                changes[1].clone(),
            ],
            ..note(
                None,
                Some("Plain note by hand."),
                Note,
                None,
                "Plain note by hand."
            )
        }
    );

    // Only changes: the commit's own Why stays with its message
    let only_changes = parse_note(&compose_note("", "m", &[]));
    assert_eq!(only_changes.why, None);
    assert_eq!(only_changes.change_model.as_deref(), Some("m"));
    assert!(only_changes.changes.is_empty());
    assert_eq!(only_changes.head, "");
    assert_eq!(
        explain(C {
            body: "Body why.",
            note: Some("Change-Model: m\n"),
            ..Default::default()
        }),
        expl("Subject", Some("Body why."), Commit, None)
    );
}

#[test]
fn compose_note_flattens_what_and_skips_a_same_path_change_from() {
    let text = compose_note(
        "",
        "m",
        &[change(Ch {
            path: "a",
            original_path: Some("a"),
            what: "two\nlines",
            why: "  padded  ",
            ..Default::default()
        })],
    );
    assert_eq!(
        text,
        "Change-Model: m\nChange-Unit: block\n\nChange: -1,0 +1,0 a\nWhat: two lines\nWhy: padded\n"
    );
}

#[test]
fn round_trips_an_inferred_head_with_changes() {
    let head = format_note(
        "Adds X",
        "Because.",
        None,
        Inferred,
        Some("claude-opus-5-5"),
    );
    let changes = vec![change(Ch {
        old_start: 10,
        old_count: 3,
        new_start: 11,
        new_count: 5,
        why: "Why A.",
        ..Default::default()
    })];
    let parsed = parse_note(&compose_note(&head, "claude-haiku-4-5", &changes));
    assert_eq!(parsed.what.as_deref(), Some("Adds X"));
    assert_eq!(parsed.why.as_deref(), Some("Because."));
    assert_eq!(parsed.source, Inferred);
    assert_eq!(parsed.model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(parsed.change_model.as_deref(), Some("claude-haiku-4-5"));
    assert_eq!(parsed.changes, changes);
    assert_eq!(parsed.head, head.trim());
}

#[test]
fn parse_note_with_only_why() {
    assert_eq!(
        parse_note("Why: Because.\nSecond line."),
        note(
            None,
            Some("Because.\nSecond line."),
            Note,
            None,
            "Why: Because.\nSecond line."
        )
    );
}

#[test]
fn parse_note_head_edge_cases() {
    // Only What: no Why, so the commit's own body is used
    assert_eq!(parse_note("What: Adds X").why, None);
    // Labels stop at the first unknown line; that is not Why either
    assert_eq!(parse_note("What: Adds X\nfree text").why, None);
    assert_eq!(parse_note("Source: inferred\nfree text").why, None);
    // An unknown first label makes the whole note Why
    assert_eq!(
        parse_note("Note: by hand").why.as_deref(),
        Some("Note: by hand")
    );
    // Labels are case-insensitive; an unknown Source is a note
    let parsed = parse_note("SOURCE: Inferred\nmodel: m\nwhat: W\nWHY: Y");
    assert_eq!(
        (
            parsed.source,
            parsed.model.as_deref(),
            parsed.what.as_deref(),
            parsed.why.as_deref()
        ),
        (Inferred, Some("m"), Some("W"), Some("Y"))
    );
    assert_eq!(parse_note("Source: robot\nWhy: Y").source, Note);
    // Empty values count as missing
    let parsed = parse_note("Model:\nWhat:\nWhy:   ");
    assert_eq!((parsed.model, parsed.what, parsed.why), (None, None, None));
}

#[test]
fn parse_note_crlf() {
    let text = "Source: inferred\r\nModel: m\r\nWhat: W\r\nWhy: Y\r\n\r\nChange-Model: c\r\n\r\nChange: -1,2 +1,3 a.py\r\nWhat: A\r\nWhy: B\r\n";
    let parsed = parse_note(text);
    assert_eq!(
        (
            parsed.what.as_deref(),
            parsed.why.as_deref(),
            parsed.model.as_deref(),
            parsed.change_model.as_deref()
        ),
        (Some("W"), Some("Y"), Some("m"), Some("c"))
    );
    assert_eq!(
        parsed.changes,
        vec![change(Ch {
            path: "a.py",
            old_count: 2,
            new_count: 3,
            what: "A",
            why: "B",
            ..Default::default()
        })]
    );
}

#[test]
fn parse_note_unicode() {
    let parsed = parse_note(
        "Why: Für ☃.\n\nChange-Model: m\n\nChange: -1 +1 données/é.py\nWhat: ✓\nWhy: 雪",
    );
    assert_eq!(parsed.why.as_deref(), Some("Für ☃."));
    assert_eq!(
        parsed.changes,
        vec![change(Ch {
            path: "données/é.py",
            old_count: 1,
            new_count: 1,
            what: "✓",
            why: "雪",
            ..Default::default()
        })]
    );
}

#[test]
fn parse_note_change_ranges_default_to_one_line() {
    let parsed = parse_note("Change: -5 +7,0 a.py\nWhat: W\nWhy: Y");
    assert_eq!(parsed.changes[0].hunk, hunk("a.py", 5, 1, 7, 0, None));
    // Changes without a Change-Model line before them still count
    assert_eq!(parsed.change_model, None);
    assert_eq!(parsed.head, "");
}

#[test]
fn parse_note_drops_changes_without_a_why() {
    let parsed = parse_note(
        "Change-Model: m\n\n\
         Change: -1,1 +1,1 a.py\nWhat: no reason\n\n\
         Change: -2,1 +2,1 b.py\nWhat: empty reason\nWhy:   \n\n\
         Change: -3,1 +3,1 c.py\nWhy: kept without What",
    );
    assert_eq!(
        parsed.changes,
        vec![change(Ch {
            path: "c.py",
            old_start: 3,
            old_count: 1,
            new_start: 3,
            new_count: 1,
            what: "",
            why: "kept without What",
            ..Default::default()
        })]
    );
}

#[test]
fn parse_note_skips_malformed_change_lines() {
    let parsed = parse_note(
        "Change-Model: m\n\n\
         Change: 1,1 +1,1 missing-dash.py\nWhat: W\nWhy: lost\n\n\
         Change: -a,1 +1,1 letters.py\nWhy: lost\n\n\
         Change: -1,1 +1,1\nWhy: no path\n\n\
         Change: -4,1 +4,1 good.py\nWhat: W\nWhy: Y",
    );
    assert_eq!(
        parsed.changes,
        vec![change(Ch {
            path: "good.py",
            old_start: 4,
            old_count: 1,
            new_start: 4,
            new_count: 1,
            what: "W",
            why: "Y",
            ..Default::default()
        })]
    );
}

#[test]
fn parse_note_why_runs_to_the_next_change_and_keeps_labels_in_it() {
    let parsed = parse_note(
        "Change: -1 +1 a.py\nWhat: W\nWhy: line one\nWhat: still why\nChange-Model: also why\n\
         Change: -2 +2 b.py\nWhy: B",
    );
    let whys: Vec<&str> = parsed.changes.iter().map(|c| c.why.as_str()).collect();
    assert_eq!(
        whys,
        ["line one\nWhat: still why\nChange-Model: also why", "B"]
    );
    assert_eq!(parsed.change_model, None);
}

#[test]
fn parse_note_ignores_unknown_lines_before_why() {
    let parsed = parse_note("Change: -1 +1 a.py\nnoise\nWhat: W\nOther: x\nWhy: Y");
    assert_eq!(
        parsed.changes,
        vec![change(Ch {
            path: "a.py",
            old_count: 1,
            new_count: 1,
            what: "W",
            why: "Y",
            ..Default::default()
        })]
    );
}

#[test]
fn a_head_line_shaped_like_a_change_starts_the_tail() {
    let parsed = parse_note("Why: hand note\nChange: -1 +1 a.py\nWhy: Y");
    assert_eq!(parsed.why.as_deref(), Some("hand note"));
    assert_eq!(parsed.changes.len(), 1);
}

#[test]
fn a_change_guide_round_trips_before_its_why() {
    let mut guided = top();
    guided.why = "Needed.\nKind: not a field in the Why".into();
    guided.guide = ChangeGuide {
        kind: Some(ChangeKind::Behavior),
        order: Some(3),
        was: Some("one card per hunk".into()),
        now: Some("one card\nper block".into()),
        check: Some("Old notes still match.".into()),
        steps: vec![
            ChangeStep {
                lines: Some(StepLines {
                    side: Side::New,
                    first: 1,
                    last: 3,
                }),
                text: "Reads the\nsettings.".into(),
            },
            ChangeStep {
                lines: Some(StepLines {
                    side: Side::Old,
                    first: 2,
                    last: 2,
                }),
                text: "Drops the cache.".into(),
            },
            ChangeStep {
                lines: None,
                text: "Ends early.".into(),
            },
        ],
    };
    let text = compose_note("", "m", &[guided.clone(), bottom()]);
    assert!(text.contains(
        "Kind: behavior\nOrder: 3\nWhat: top\nWas: one card per hunk\nNow: one card per block\nCheck: Old notes still match.\nStep: +1-3 Reads the settings.\nStep: -2 Drops the cache.\nStep: Ends early.\nWhy: Needed."
    ));
    let parsed = parse_note(&text);
    guided.guide.now = Some("one card per block".into());
    guided.guide.steps[0].text = "Reads the settings.".into();
    assert_eq!(parsed.changes[0], guided);
    assert!(parsed.changes[1].guide.is_empty());
}

#[test]
fn notes_without_a_guide_read_as_before() {
    let parsed = parse_note("Change-Model: m\nChange: -1 +1 a.py\nWhat: W\nWhy: Y\nKind: fix");
    assert_eq!(parsed.changes[0].why, "Y\nKind: fix");
    assert!(parsed.changes[0].guide.is_empty());
}

// -- find_change_explanations --

fn top() -> ChangeExplanation {
    change(Ch {
        old_start: 1,
        old_count: 4,
        new_start: 1,
        new_count: 5,
        what: "top",
        ..Default::default()
    })
}

fn bottom() -> ChangeExplanation {
    change(Ch {
        old_start: 20,
        old_count: 3,
        new_start: 21,
        new_count: 2,
        what: "bottom",
        ..Default::default()
    })
}

fn renamed() -> ChangeExplanation {
    change(Ch {
        path: "b.ts",
        original_path: Some("old.ts"),
        old_start: 1,
        old_count: 3,
        new_start: 1,
        new_count: 3,
        what: "renamed",
        ..Default::default()
    })
}

fn found(origins: &[LineOrigin], changes: &[ChangeExplanation]) -> Vec<ChangeExplanation> {
    find_change_explanations(origins, changes)
        .into_iter()
        .cloned()
        .collect()
}

#[test]
fn finds_the_hunks_the_lines_fall_in_by_side_and_path_in_the_order_the_lines_reach_them() {
    let result = found(
        &[
            origin("a.ts", Side::Old, 22),
            origin("a.ts", Side::New, 5),
            origin("a.ts", Side::New, 1),
        ],
        &[top(), bottom(), renamed()],
    );
    let whats: Vec<&str> = result.iter().map(|c| c.what.as_str()).collect();
    assert_eq!(whats, ["bottom", "top"]);
}

#[test]
fn matches_old_lines_of_a_renamed_file_by_its_old_path() {
    let changes = [top(), bottom(), renamed()];
    assert_eq!(
        found(&[origin("old.ts", Side::Old, 2)], &changes),
        [renamed()]
    );
    assert!(found(&[origin("b.ts", Side::Old, 2)], &changes).is_empty());
    assert_eq!(
        found(&[origin("b.ts", Side::New, 3)], &changes),
        [renamed()]
    );
}

#[test]
fn finds_nothing_outside_every_hunk() {
    assert!(found(&[origin("a.ts", Side::New, 6)], &[top(), bottom()]).is_empty());
    assert!(found(&[origin("a.ts", Side::New, 23)], &[top(), bottom()]).is_empty());
    // Hunk edges: the start is in, start + count is out
    assert_eq!(
        found(&[origin("a.ts", Side::New, 21)], &[bottom()]),
        [bottom()]
    );
    assert!(found(&[origin("a.ts", Side::Old, 23)], &[bottom()]).is_empty());
}

#[test]
fn finds_nothing_for_empty_input_or_empty_hunks() {
    assert!(found(&[], &[top()]).is_empty());
    assert!(found(&[origin("a.ts", Side::New, 1)], &[]).is_empty());
    // A pure deletion has no new lines
    let empty = change(Ch {
        old_start: 5,
        old_count: 2,
        new_start: 4,
        new_count: 0,
        ..Default::default()
    });
    assert!(
        found(
            &[origin("a.ts", Side::New, 4)],
            std::slice::from_ref(&empty)
        )
        .is_empty()
    );
    assert_eq!(
        found(
            &[origin("a.ts", Side::Old, 6)],
            std::slice::from_ref(&empty)
        ),
        [empty]
    );
}

#[test]
fn finds_each_change_once() {
    let result = found(
        &[origin("a.ts", Side::New, 1), origin("a.ts", Side::New, 2)],
        &[top()],
    );
    assert_eq!(result, [top()]);
}

// -- justifications --

fn justified(c: Ch<'_>, justification: &str) -> ChangeExplanation {
    ChangeExplanation {
        justification: Some(justification.into()),
        ..change(c)
    }
}

#[test]
fn format_note_writes_the_justification_after_the_why() {
    let text = format_note(
        "Adds X",
        "Because Y.\n\nAnd Z.",
        Some("  The diff shows Y.\nA flag would not do.  "),
        Inferred,
        Some("claude-sonnet-5-5"),
    );
    assert_eq!(
        text,
        "Source: inferred\nModel: claude-sonnet-5-5\nWhat: Adds X\nWhy: Because Y.\n\nAnd Z.\n\
         Justification: The diff shows Y.\nA flag would not do.\n"
    );
    let parsed = parse_note(&text);
    assert_eq!(parsed.why.as_deref(), Some("Because Y.\n\nAnd Z."));
    assert_eq!(
        parsed.justification.as_deref(),
        Some("The diff shows Y.\nA flag would not do.")
    );
    assert_eq!(parsed.what.as_deref(), Some("Adds X"));
    assert_eq!(parsed.source, Inferred);
    assert_eq!(parsed.head, text.trim());
}

#[test]
fn format_note_leaves_out_an_empty_justification() {
    let plain = format_note("W", "Y", None, Note, None);
    assert_eq!(format_note("W", "Y", Some("  "), Note, None), plain);
    assert_eq!(parse_note(&plain).justification, None);
}

#[test]
fn old_notes_without_a_justification_read_as_before() {
    let old = "Source: inferred\nModel: m\nWhat: Adds X\nWhy: Because Y.\n\n\
               Change-Model: m\n\nChange: -1,2 +1,3 a.ts\nWhat: w\nWhy: y\nmore y\n";
    let parsed = parse_note(old);
    assert_eq!(parsed.why.as_deref(), Some("Because Y."));
    assert_eq!(parsed.justification, None);
    assert_eq!(parsed.changes.len(), 1);
    assert_eq!(parsed.changes[0].why, "y\nmore y");
    assert_eq!(parsed.changes[0].justification, None);
    assert_eq!(
        explain(C {
            note: Some(old),
            ..Default::default()
        }),
        expl("Adds X", Some("Because Y."), Inferred, Some("m"))
    );
}

#[test]
fn a_hand_written_why_stops_at_a_justification_line() {
    let parsed =
        parse_note("Why: Users asked.\nTwice.\njustification:  The log shows it.\n\nMore.");
    assert_eq!(parsed.why.as_deref(), Some("Users asked.\nTwice."));
    assert_eq!(
        parsed.justification.as_deref(),
        Some("The log shows it.\n\nMore.")
    );
    // Only after the Why's own line, and only a line that starts with it
    let parsed = parse_note("Why: Justification: is the word\n The Justification: indented");
    assert_eq!(
        parsed.why.as_deref(),
        Some("Justification: is the word\n The Justification: indented")
    );
    assert_eq!(parsed.justification, None);
    // An empty Justification is none
    let parsed = parse_note("Why: W\nJustification:   \n");
    assert_eq!(parsed.why.as_deref(), Some("W"));
    assert_eq!(parsed.justification, None);
}

#[test]
fn explain_commit_takes_the_justification_of_a_note() {
    let explanation = explain(C {
        note: Some("Source: inferred\nModel: m\nWhat: W\nWhy: Y\nJustification: J"),
        ..Default::default()
    });
    assert_eq!(explanation.why.as_deref(), Some("Y"));
    assert_eq!(explanation.justification.as_deref(), Some("J"));
}

#[test]
fn a_why_line_shaped_like_a_justification_is_escaped() {
    let text = format_note(
        "W",
        "First.\nJustification: is not one\nLast.",
        Some("Real.\nChange: -1,1 +1,1 x.ts"),
        Note,
        None,
    );
    assert!(text.contains("\n Justification: is not one\n"), "{text}");
    assert!(text.contains("\n Change: -1,1 +1,1 x.ts\n"), "{text}");
    let parsed = parse_note(&text);
    assert_eq!(
        parsed.why.as_deref(),
        Some("First.\n Justification: is not one\nLast.")
    );
    assert_eq!(
        parsed.justification.as_deref(),
        Some("Real.\n Change: -1,1 +1,1 x.ts")
    );
    assert!(parsed.changes.is_empty() && parsed.change_model.is_none());
}

#[test]
fn compose_note_writes_each_changes_justification_after_its_why() {
    let head = format_note("Adds X", "Because.", Some("Evidence."), Inferred, Some("m"));
    let changes = vec![
        justified(
            Ch {
                old_start: 3,
                old_count: 2,
                new_start: 3,
                new_count: 4,
                why: "Needed.\nJustification: in the Why",
                ..Default::default()
            },
            "  The test fails without it.\nChange: -9,1 +9,1 a.ts looks like a header  ",
        ),
        // Written before justifications were asked for
        change(Ch {
            old_start: 20,
            old_count: 1,
            new_start: 22,
            new_count: 1,
            why: "Other.",
            ..Default::default()
        }),
    ];
    let text = compose_note(&head, "m", &changes);
    assert_eq!(
        text,
        [
            "Source: inferred",
            "Model: m",
            "What: Adds X",
            "Why: Because.",
            "Justification: Evidence.",
            "",
            "Change-Model: m",
            "Change-Unit: block",
            "",
            "Change: -3,2 +3,4 a.ts",
            "What: w",
            "Why: Needed.",
            " Justification: in the Why",
            "Justification: The test fails without it.",
            " Change: -9,1 +9,1 a.ts looks like a header",
            "",
            "Change: -20,1 +22,1 a.ts",
            "What: w",
            "Why: Other.",
            "",
        ]
        .join("\n")
    );

    let parsed = parse_note(&text);
    assert_eq!(parsed.why.as_deref(), Some("Because."));
    assert_eq!(parsed.justification.as_deref(), Some("Evidence."));
    assert_eq!(parsed.changes.len(), 2);
    assert_eq!(parsed.changes[0].why, "Needed.\n Justification: in the Why");
    assert_eq!(
        parsed.changes[0].justification.as_deref(),
        Some("The test fails without it.\n Change: -9,1 +9,1 a.ts looks like a header")
    );
    assert_eq!(parsed.changes[1], changes[1]);
    // Read back and written again, the note stays the same
    assert_eq!(compose_note(&parsed.head, "m", &parsed.changes), text);
}

#[test]
fn a_change_without_a_why_keeps_no_justification() {
    let parsed = parse_note("Change-Model: m\n\nChange: -1 +1 a.ts\nWhat: w\nJustification: j\n");
    assert!(parsed.changes.is_empty());
}

#[test]
fn labeled_messages_can_carry_a_justification() {
    // Body paragraphs
    let explanation = explain(C {
        body: "What: Adds X\n\nWhy: Users asked.\n\nJustification: The issue\nshows it.",
        ..Default::default()
    });
    assert_eq!(explanation.why.as_deref(), Some("Users asked."));
    assert_eq!(
        explanation.justification.as_deref(),
        Some("The issue\nshows it.")
    );
    assert_eq!(explanation.source, Commit);

    // A Justification line in the Why's paragraph ends the Why
    let explanation = explain(C {
        body: "Why: Users asked.\nAgent-Justification: The issue shows it.",
        ..Default::default()
    });
    assert_eq!(explanation.why.as_deref(), Some("Users asked."));
    assert_eq!(
        explanation.justification.as_deref(),
        Some("The issue shows it.")
    );

    // Trailers, which beat paragraphs
    let explanation = explain(C {
        body: "Justification: from the body",
        trailers: vec![
            GitTrailer::new("Why", "Users asked."),
            GitTrailer::new("justification", "From a trailer."),
        ],
        ..Default::default()
    });
    assert_eq!(
        explanation.justification.as_deref(),
        Some("From a trailer.")
    );

    // Without a Why there is nothing to justify from the message
    let explanation = explain(C {
        body: "Justification: alone",
        ..Default::default()
    });
    assert_eq!(explanation.justification, None);
    assert_eq!(explanation.why.as_deref(), Some("Justification: alone"));

    // Configured labels
    let format = MessageFormat {
        what_keys: vec![],
        why_keys: vec!["Reason".into()],
        justification_keys: vec!["Evidence".into()],
    };
    let reason = commit(C {
        body: "Reason: R\n\nEvidence: E\n\nJustification: ignored",
        ..Default::default()
    });
    let explanation = explain_commit(&reason, &format);
    assert_eq!(explanation.why.as_deref(), Some("R"));
    assert_eq!(explanation.justification.as_deref(), Some("E"));
}

#[test]
fn change_unit_says_whether_changes_are_change_blocks() {
    let changes = "\n\nChange: -1 +1 a.py\nWhat: W\nWhy: Y\n";
    assert!(!parse_note(&format!("Change-Model: m{changes}")).changes_by_block);
    assert!(parse_note(&format!("Change-Model: m\nChange-Unit: block{changes}")).changes_by_block);
    assert!(
        parse_note(&format!("Change-Model: m\nChange-Unit:  Block {changes}")).changes_by_block
    );
    assert!(!parse_note(&format!("Change-Model: m\nChange-Unit: hunk{changes}")).changes_by_block);

    // Inside a change it is just part of the Why
    let parsed =
        parse_note("Change-Model: m\n\nChange: -1 +1 a.py\nWhat: W\nWhy: Y\nChange-Unit: block\n");
    assert!(!parsed.changes_by_block);
    assert_eq!(parsed.changes[0].why, "Y\nChange-Unit: block");
    // A Change-Model line is still what starts a note's changes
    assert_eq!(
        parse_note("By hand.\n\nChange-Unit: block\n").head,
        "By hand.\n\nChange-Unit: block"
    );
}
