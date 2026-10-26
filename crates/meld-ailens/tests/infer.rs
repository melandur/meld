//! Port of test/ailens/test_infer.py

use std::sync::{Arc, Mutex};

use meld_ailens::claude::{ClaudeRequest, ClaudeResponse, RunClaude};
use meld_ailens::explanation::{MessageFormat, compose_note, format_note, parse_note};
use meld_ailens::infer::{
    FileDiff, InferOptions, InferredExplanation, MAX_PATCH_CHARS, MAX_PATCH_CHARS_PER_FILE, SCHEMA,
    SYSTEM_PROMPT, UNCOMMITTED_SCHEMA, build_prompt, compose_inferred_note, infer_explanation,
    infer_uncommitted_explanation, parse_changes,
};
use meld_ailens::{
    CancelToken, ChangeExplanation, ChangeGuide, ChangeKind, ChangeStep, ClaudeErrorReason,
    CommitExplanation, CommitHunk, DiffLineKind, Error, ExplanationSource, GitCommit, GitDiffHunk,
    GitDiffLine, GitFileChange, GitFileDiff, GitFileStatus, GitIdentity, Side, StepLines,
    UncommittedLayer,
};
use serde_json::{Value, json};

/// A hunk from `git diff` body lines (` `, `+` or `-` first)
fn hunk_s(old_start: usize, new_start: usize, lines: &[&str], section: &str) -> GitDiffHunk {
    let mut parsed = Vec::new();
    let (mut old, mut new) = (old_start, new_start);
    for line in lines {
        let (kind, content) = line.split_at(1);
        match kind {
            "+" => {
                parsed.push(GitDiffLine::new(
                    DiffLineKind::Add,
                    content,
                    None,
                    Some(new),
                ));
                new += 1;
            }
            "-" => {
                parsed.push(GitDiffLine::new(
                    DiffLineKind::Delete,
                    content,
                    Some(old),
                    None,
                ));
                old += 1;
            }
            _ => {
                parsed.push(GitDiffLine::new(
                    DiffLineKind::Context,
                    content,
                    Some(old),
                    Some(new),
                ));
                old += 1;
                new += 1;
            }
        }
    }
    GitDiffHunk {
        old_start,
        old_count: lines.iter().filter(|l| !l.starts_with('+')).count(),
        new_start,
        new_count: lines.iter().filter(|l| !l.starts_with('-')).count(),
        section: section.into(),
        lines: parsed,
    }
}

fn hunk(old_start: usize, new_start: usize, lines: &[&str]) -> GitDiffHunk {
    hunk_s(old_start, new_start, lines, "")
}

fn hunk_owned(old_start: usize, new_start: usize, lines: &[String]) -> GitDiffHunk {
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    hunk(old_start, new_start, &lines)
}

fn diff(
    old: Option<&str>,
    new: Option<&str>,
    binary: bool,
    hunks: Vec<GitDiffHunk>,
) -> GitFileDiff {
    GitFileDiff {
        old_path: old.map(str::to_string),
        new_path: new.map(str::to_string),
        binary,
        hunks,
    }
}

fn modified(path: &str, hunks: Vec<GitDiffHunk>) -> FileDiff {
    (
        GitFileChange::new(GitFileStatus::Modified, path),
        Some(diff(Some(path), Some(path), false, hunks)),
    )
}

fn added(path: &str, lines: &[&str]) -> FileDiff {
    let body: Vec<String> = lines.iter().map(|l| format!("+{l}")).collect();
    (
        GitFileChange::new(GitFileStatus::Added, path),
        Some(diff(None, Some(path), false, vec![hunk_owned(0, 1, &body)])),
    )
}

fn commit_with(subject: &str, body: &str) -> GitCommit {
    let identity = GitIdentity {
        name: "Jane Doe".into(),
        email: "jane@example.com".into(),
        date: "2024-01-01T00:00:00Z".into(),
    };
    GitCommit {
        sha: "c".repeat(40),
        parents: vec!["b".repeat(40)],
        author: identity.clone(),
        committer: identity,
        subject: subject.into(),
        body: body.into(),
        trailers: Vec::new(),
        note: None,
    }
}

fn commit() -> GitCommit {
    commit_with("Changes a", "Some body.")
}

fn hunk_ref(
    path: &str,
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
) -> CommitHunk {
    CommitHunk {
        path: path.into(),
        old_start,
        old_count,
        new_start,
        new_count,
        original_path: None,
    }
}

/// Records the request and answers with `output`
struct FakeClaude {
    output: Value,
    model: String,
    requests: Mutex<Vec<ClaudeRequest>>,
    cancels: Mutex<Vec<CancelToken>>,
}

impl FakeClaude {
    fn new(output: Value) -> Arc<Self> {
        Self::with_model(output, "claude-sonnet-5-5")
    }

    fn with_model(output: Value, model: &str) -> Arc<Self> {
        Arc::new(Self {
            output,
            model: model.into(),
            requests: Mutex::new(Vec::new()),
            cancels: Mutex::new(Vec::new()),
        })
    }

    fn runner(self: &Arc<Self>) -> RunClaude {
        let fake = self.clone();
        Arc::new(move |request, cancel| {
            fake.requests.lock().unwrap().push(request.clone());
            fake.cancels.lock().unwrap().push(cancel.clone());
            let mut response = ClaudeResponse::new(fake.output.clone(), "", fake.model.clone());
            response.cost_usd = Some(0.0);
            Ok(response)
        })
    }

    fn prompt(&self) -> String {
        self.requests.lock().unwrap().last().unwrap().prompt.clone()
    }

    fn request(&self, index: usize) -> ClaudeRequest {
        self.requests.lock().unwrap()[index].clone()
    }
}

fn never_called() -> RunClaude {
    Arc::new(|_, _| panic!("Claude Code must not be called"))
}

fn options(model: &str, run: RunClaude) -> InferOptions<'_> {
    let mut options = InferOptions::new(model);
    options.run = Some(run);
    options
}

fn never() -> CancelToken {
    CancelToken::new()
}

fn assert_no_explanation(err: Error) {
    assert_eq!(
        err.claude_reason(),
        Some(ClaudeErrorReason::InvalidOutput),
        "{err}"
    );
    assert!(err.to_string().contains("no explanation"), "{err}");
}

fn change_files() -> Vec<FileDiff> {
    vec![
        modified("src/a.ts", vec![hunk(1, 1, &["-one", "+two"])]),
        added("pnpm-lock.yaml", &["lock"]),
        modified(
            "secrets/key.txt",
            vec![hunk(1, 1, &["-old", "+SECRET-VALUE"])],
        ),
    ]
}

// -- infer_explanation --

#[test]
fn sends_the_message_and_diff_leaving_out_ignored_and_generated_files() {
    let run = FakeClaude::new(json!({
        "what": " Changes a. ",
        "why": " Because. ",
        "changes": [
            {"change": 1, "what": " Swaps one for two. ", "why": " Two is right. "},
            {"change": 1, "what": "Duplicate", "why": "Ignored."},
            {"change": 7, "what": "Unknown", "why": "No such change."},
        ],
    }));
    let cancel = CancelToken::new();
    let mut opts = options("claude-sonnet-5-5", run.runner());
    opts.ignore_rules = Some("secrets/\n");
    opts.claude_path = "/opt/claude";
    let explanation = infer_explanation(&commit(), &change_files(), &opts, &cancel).unwrap();

    assert_eq!(
        explanation.explanation,
        CommitExplanation {
            what: "Changes a.".into(),
            why: Some("Because.".into()),
            justification: None,
            source: ExplanationSource::Inferred,
            model: Some("claude-sonnet-5-5".into()),
        }
    );
    assert_eq!(explanation.model, "claude-sonnet-5-5");
    assert_eq!(
        explanation.changes,
        vec![ChangeExplanation {
            hunk: hunk_ref("src/a.ts", 1, 1, 1, 1),
            what: "Swaps one for two.".into(),
            why: "Two is right.".into(),
            justification: None,
            guide: Default::default(),
        }]
    );

    let sent = run.request(0);
    assert_eq!(sent.model, "claude-sonnet-5-5");
    assert_eq!(sent.claude_path, "/opt/claude");
    assert_eq!(sent.system_prompt.as_deref(), Some(SYSTEM_PROMPT));
    assert_eq!(sent.json_schema.as_ref(), Some(&*SCHEMA));
    assert_eq!(*run.cancels.lock().unwrap(), vec![cancel]);
    assert!(
        sent.prompt
            .contains("[change 1] @@ -1 +1 @@\no1    -one\nn1    +two")
    );
    assert!(!sent.prompt.contains("[change 2]"));
    assert!(sent.prompt.contains("Changes a\n\nSome body."));
    assert!(
        sent.prompt
            .contains("2 of these files are left out of the diff")
    );
    assert!(!sent.prompt.contains("SECRET-VALUE"));
    assert!(!sent.prompt.contains("+lock"));
}

#[test]
fn rejects_an_answer_without_a_why() {
    let run = FakeClaude::with_model(json!({"what": "x", "why": ""}), "m");
    let err = infer_explanation(
        &commit(),
        &[added("a.ts", &["a"])],
        &options("m", run.runner()),
        &never(),
    )
    .unwrap_err();
    assert_no_explanation(err);
}

#[test]
fn rejects_malformed_answers() {
    for output in [
        Value::Null,
        json!("text"),
        json!([]),
        json!({"what": 1, "why": "y"}),
        json!({"what": "w"}),
        json!({"what": "w", "why": "  "}),
    ] {
        let run = FakeClaude::new(output);
        let err = infer_explanation(
            &commit(),
            &[added("a.ts", &["a"])],
            &options("m", run.runner()),
            &never(),
        )
        .unwrap_err();
        assert_no_explanation(err);
    }
}

#[test]
fn uses_the_subject_for_an_empty_what_and_the_model_that_answered() {
    let run = FakeClaude::with_model(
        json!({"what": "  ", "why": "Y", "changes": "not a list"}),
        "claude-x",
    );
    let explanation = infer_explanation(
        &commit_with("Subj", "Some body."),
        &[],
        &options("m", run.runner()),
        &never(),
    )
    .unwrap();
    assert_eq!(
        explanation.explanation,
        CommitExplanation {
            what: "Subj".into(),
            why: Some("Y".into()),
            justification: None,
            source: ExplanationSource::Inferred,
            model: Some("claude-x".into()),
        }
    );
    assert_eq!(explanation.model, "claude-x");
    assert!(explanation.changes.is_empty());
}

#[test]
fn change_answers_are_validated() {
    let files = vec![modified(
        "a.py",
        vec![
            hunk(1, 1, &["-a", "+b"]),
            hunk(10, 10, &["-c", "+d"]),
            hunk(20, 20, &["+e"]),
        ],
    )];
    let run = FakeClaude::new(json!({
        "what": "W",
        "why": "Y",
        "changes": [
            "not an object",
            {"change": "1", "what": "string number", "why": "dropped"},
            {"change": true, "what": "bool", "why": "dropped"},
            {"change": 1.5, "what": "fraction", "why": "dropped"},
            {"change": 1, "what": "no why", "why": "  "},
            {"change": 1, "what": "first", "why": "kept"},
            {"change": 3.0, "what": "float", "why": "kept"},
            {"change": 2, "what": null, "why": "dropped"},
            {"change": 0, "what": "zero", "why": "dropped"},
            {"change": -1, "what": "negative", "why": "dropped"},
        ],
    }));
    let explanation =
        infer_explanation(&commit(), &files, &options("m", run.runner()), &never()).unwrap();
    let got: Vec<(usize, &str)> = explanation
        .changes
        .iter()
        .map(|c| (c.hunk.old_start, c.what.as_str()))
        .collect();
    assert_eq!(got, [(1, "first"), (20, "float")]);
}

#[test]
fn hunks_are_numbered_across_files_in_diff_order() {
    let files = vec![
        modified(
            "a.py",
            vec![hunk(1, 1, &["-a", "+b"]), hunk(9, 9, &["-c", "+d"])],
        ),
        modified("b.py", vec![hunk(5, 5, &["-x", "+y"])]),
    ];
    let run = FakeClaude::new(json!({
        "what": "W",
        "why": "Y",
        "changes": [
            {"change": 3, "what": "third", "why": "c"},
            {"change": 1, "what": "first", "why": "a"},
        ],
    }));
    let explanation =
        infer_explanation(&commit(), &files, &options("m", run.runner()), &never()).unwrap();
    let got: Vec<(&str, &str)> = explanation
        .changes
        .iter()
        .map(|c| (c.hunk.path.as_str(), c.what.as_str()))
        .collect();
    assert_eq!(got, [("a.py", "first"), ("b.py", "third")]);
    let prompt = run.prompt();
    assert!(prompt.contains("[change 1] @@ -1 +1 @@"));
    assert!(prompt.contains("[change 2] @@ -9 +9 @@"));
    assert!(prompt.contains("[change 3] @@ -5 +5 @@"));
}

#[test]
fn changes_git_joined_into_one_hunk_are_numbered_apart() {
    // Six lines apart, so git prints them as one hunk
    let joined = hunk(
        1,
        1,
        &[
            "-one", "+ONE", " 2", " 3", " 4", " 5", " 6", " 7", "-eight", "+EIGHT",
        ],
    );
    let run = FakeClaude::new(json!({
        "what": "W",
        "why": "Y",
        "changes": [
            {"change": 1, "what": "first", "why": "a"},
            {"change": 2, "what": "second", "why": "b"},
        ],
    }));
    let explanation = infer_explanation(
        &commit(),
        &[modified("a.py", vec![joined])],
        &options("m", run.runner()),
        &never(),
    )
    .unwrap();

    let prompt = run.prompt();
    assert!(prompt.contains(
        "[change 1] @@ -1,4 +1,4 @@\no1    -one\nn1    +ONE\nn2     2\nn3     3\nn4     4\n"
    ));
    assert!(prompt.contains(
        "[change 2] @@ -5,4 +5,4 @@\nn5     5\nn6     6\nn7     7\no8    -eight\nn8    +EIGHT\n"
    ));
    let got: Vec<(&CommitHunk, &str)> = explanation
        .changes
        .iter()
        .map(|c| (&c.hunk, c.what.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            (&hunk_ref("a.py", 1, 4, 1, 4), "first"),
            (&hunk_ref("a.py", 5, 4, 5, 4), "second"),
        ]
    );
}

#[test]
fn answers_for_hunks_that_match_nothing_are_dropped() {
    let run = FakeClaude::new(
        json!({"what": "W", "why": "Y", "changes": [{"change": 1, "what": "w", "why": "y"}]}),
    );
    let files = vec![(GitFileChange::new(GitFileStatus::Modified, "mode.sh"), None)];
    let explanation =
        infer_explanation(&commit(), &files, &options("m", run.runner()), &never()).unwrap();
    assert!(explanation.changes.is_empty());
    assert!(run.prompt().contains("(no textual changes to show)"));
}

// -- build_prompt --

#[test]
fn build_prompt_layout() {
    let prompt = build_prompt(
        &commit(),
        &[modified(
            "a.py",
            vec![hunk_s(3, 3, &[" ctx", "-old", "+new"], "def f():")],
        )],
        None,
    );
    let expected = [
        format!("Commit {} by Jane Doe", "c".repeat(40)),
        "".into(),
        "Message:".into(),
        "Changes a\n\nSome body.".into(),
        "".into(),
        "Changed files:".into(),
        "M a.py".into(),
        "".into(),
        "Diff:".into(),
        "diff --git a/a.py b/a.py".into(),
        "--- a/a.py".into(),
        "+++ b/a.py".into(),
        "[change 1] @@ -3,2 +3,2 @@ def f():".into(),
        "n3     ctx".into(),
        "o4    -old".into(),
        "n4    +new".into(),
        "".into(),
    ]
    .join("\n");
    assert_eq!(prompt.text, expected);
    assert_eq!(prompt.hunks, vec![hunk_ref("a.py", 3, 2, 3, 2)]);
}

#[test]
fn build_prompt_with_an_empty_body_and_no_files() {
    let prompt = build_prompt(&commit_with("Changes a", "  \n"), &[], None);
    assert!(
        prompt.text.contains(
            "Message:\nChanges a\n\nChanged files:\n\nDiff:\n(no textual changes to show)"
        )
    );
    assert!(prompt.hunks.is_empty());
}

#[test]
fn build_prompt_renders_renames_additions_deletions_and_binaries() {
    let renamed: FileDiff = (
        GitFileChange::renamed(GitFileStatus::Renamed, "new.py", "old.py", Some(90)),
        Some(diff(
            Some("old.py"),
            Some("new.py"),
            false,
            vec![hunk(1, 1, &["-a", "+b"])],
        )),
    );
    let deleted: FileDiff = (
        GitFileChange::new(GitFileStatus::Deleted, "gone.py"),
        Some(diff(
            Some("gone.py"),
            None,
            false,
            vec![hunk(1, 0, &["-bye"])],
        )),
    );
    let binary: FileDiff = (
        GitFileChange::new(GitFileStatus::Modified, "logo.png"),
        Some(diff(Some("logo.png"), Some("logo.png"), true, vec![])),
    );
    let mut x = GitDiffLine::new(DiffLineKind::Delete, "x", Some(1), None);
    x.no_newline_at_end = true;
    let mut y = GitDiffLine::new(DiffLineKind::Add, "y", None, Some(1));
    y.no_newline_at_end = true;
    let no_newline: FileDiff = (
        GitFileChange::new(GitFileStatus::Modified, "end.txt"),
        Some(diff(
            Some("end.txt"),
            Some("end.txt"),
            false,
            vec![GitDiffHunk {
                old_start: 1,
                old_count: 1,
                new_start: 1,
                new_count: 1,
                section: String::new(),
                lines: vec![x, y],
            }],
        )),
    );
    let prompt = build_prompt(
        &commit(),
        &[
            renamed,
            added("new.txt", &["hi"]),
            deleted,
            binary,
            no_newline,
        ],
        None,
    );
    let text = &prompt.text;

    assert!(text.contains("R old.py -> new.py\nA new.txt\nD gone.py\nM logo.png\nM end.txt\n"));
    assert!(text.contains(
        "diff --git a/old.py b/new.py\nsimilarity index 90%\nrename from old.py\n\
         rename to new.py\n--- a/old.py\n+++ b/new.py\n[change 1] @@ -1 +1 @@\no1    -a\nn1    +b\n"
    ));
    assert!(text.contains(
        "diff --git a/new.txt b/new.txt\n--- /dev/null\n+++ b/new.txt\n\
         [change 2] @@ -0,0 +1 @@\nn1    +hi\n"
    ));
    assert!(text.contains(
        "diff --git a/gone.py b/gone.py\n--- a/gone.py\n+++ /dev/null\n\
         [change 3] @@ -1 +0,0 @@\no1    -bye\n"
    ));
    assert!(text.contains(
        "diff --git a/logo.png b/logo.png\nBinary files a/logo.png and b/logo.png differ\n"
    ));
    assert!(text.contains(
        "[change 4] @@ -1 +1 @@\no1    -x\n\\ No newline at end of file\nn1    +y\n\
         \\ No newline at end of file\n"
    ));
    let mut renamed_hunk = hunk_ref("new.py", 1, 1, 1, 1);
    renamed_hunk.original_path = Some("old.py".into());
    assert_eq!(
        prompt.hunks,
        vec![
            renamed_hunk,
            hunk_ref("new.txt", 0, 0, 1, 1),
            hunk_ref("gone.py", 1, 1, 0, 0),
            hunk_ref("end.txt", 1, 1, 1, 1),
        ]
    );
}

#[test]
fn a_rename_is_left_out_when_either_path_is_ignored() {
    let renamed: FileDiff = (
        GitFileChange::renamed(
            GitFileStatus::Renamed,
            "public/key.txt",
            "secrets/key.txt",
            None,
        ),
        Some(diff(
            Some("secrets/key.txt"),
            Some("public/key.txt"),
            false,
            vec![hunk(1, 1, &["-s", "+t"])],
        )),
    );
    let prompt = build_prompt(&commit(), &[renamed], Some("secrets/"));
    assert!(prompt.text.contains("1 of these files are left out"));
    assert!(prompt.text.contains("R secrets/key.txt -> public/key.txt"));
    assert!(!prompt.text.contains("+t"));
    assert!(prompt.hunks.is_empty());
}

#[test]
fn unicode_paths_and_content() {
    let prompt = build_prompt(
        &commit_with("Grüße", ""),
        &[modified("données/é.py", vec![hunk(1, 1, &["-☃", "+雪"])])],
        None,
    );
    assert!(prompt.text.contains("Message:\nGrüße\n"));
    assert!(
        prompt
            .text
            .contains("diff --git a/données/é.py b/données/é.py")
    );
    assert!(prompt.text.contains("o1    -☃\nn1    +雪"));
}

#[test]
fn a_huge_file_is_cut_and_listed() {
    let lines: Vec<String> = (0..2000).map(|i| format!("+line {i:05}")).collect();
    let big = modified("big.txt", vec![hunk_owned(1, 1, &lines)]);
    let small = modified("small.txt", vec![hunk(1, 1, &["-a", "+b"])]);
    let prompt = build_prompt(&commit(), &[big, small], None);

    assert!(prompt.text.contains("The diff is cut short for: big.txt."));
    assert!(prompt.text.contains("[… rest of big.txt left out]"));
    assert!(!prompt.text.contains("+line 01999"));
    // The next file is still shown, and every hunk keeps its number
    assert!(
        prompt
            .text
            .contains("[change 2] @@ -1 +1 @@\no1    -a\nn1    +b")
    );
    assert_eq!(prompt.hunks.len(), 2);
    let diff = prompt.text.split_once("\nDiff:\n").unwrap().1;
    let big_part = diff.split_once("diff --git a/small.txt").unwrap().0;
    // Python's len() counts characters
    assert_eq!(
        big_part.chars().count(),
        MAX_PATCH_CHARS_PER_FILE + "\n[… rest of big.txt left out]\n".chars().count()
    );
}

#[test]
fn files_past_the_total_budget_are_left_out() {
    let files: Vec<FileDiff> = (0..8)
        .map(|i| {
            let lines: Vec<String> = (0..1500).map(|n| format!("+{i}-{n:04}")).collect();
            modified(&format!("f{i}.txt"), vec![hunk_owned(1, 1, &lines)])
        })
        .collect();
    let prompt = build_prompt(&commit(), &files, None);
    let diff = prompt.text.split_once("\nDiff:\n").unwrap().1;

    assert!(diff.chars().count() <= MAX_PATCH_CHARS);
    // Each file is cut to its cap, so four fit; the rest are left out
    assert_eq!(diff.matches("diff --git ").count(), 4);
    assert!(
        prompt.text.contains(
            "The diff is cut short for: f0.txt, f1.txt, f2.txt, f3.txt, f4.txt, f4.txt, "
        )
    );
    assert!(prompt.text.contains("f7.txt, f7.txt."));
    // Hunks left out of the prompt are still numbered, so answers line up
    assert_eq!(prompt.hunks.len(), 8);
}

// -- compose_inferred_note --

fn inferred(changes: Value) -> InferredExplanation {
    let run = FakeClaude::with_model(
        json!({"what": "Adds X", "why": "Because.", "changes": changes}),
        "claude-x",
    );
    infer_explanation(
        &commit(),
        &[modified("a.py", vec![hunk(1, 1, &["-a", "+b"])])],
        &options("m", run.runner()),
        &never(),
    )
    .unwrap()
}

#[test]
fn inferred_note_for_a_commit_without_a_why() {
    let explanation = inferred(json!([{"change": 1, "what": "Swaps", "why": "Needed."}]));
    let note = compose_inferred_note(
        &commit_with("Changes a", ""),
        &explanation,
        &MessageFormat::default(),
    );
    assert_eq!(
        note,
        compose_note(
            &format_note(
                "Adds X",
                "Because.",
                None,
                ExplanationSource::Inferred,
                Some("claude-x")
            ),
            "claude-x",
            &explanation.changes,
        )
    );
    let parsed = parse_note(&note);
    assert_eq!(parsed.why.as_deref(), Some("Because."));
    assert_eq!(parsed.source, ExplanationSource::Inferred);
    assert_eq!(parsed.change_model.as_deref(), Some("claude-x"));
    assert_eq!(parsed.changes.len(), 1);
}

#[test]
fn inferred_note_keeps_a_why_from_the_message() {
    let explanation = inferred(json!([]));
    let note = compose_inferred_note(
        &commit_with("Changes a", "Real reason."),
        &explanation,
        &MessageFormat::default(),
    );
    assert_eq!(note, "Change-Model: claude-x\nChange-Unit: block\n");
}

#[test]
fn inferred_note_keeps_a_hand_written_note() {
    let explanation = inferred(json!([]));
    let mut with_note = commit_with("Changes a", "");
    with_note.note = Some("By hand.\n\nChange-Model: old\n".into());
    assert_eq!(
        compose_inferred_note(&with_note, &explanation, &MessageFormat::default()),
        "By hand.\n\nChange-Model: claude-x\nChange-Unit: block\n"
    );
}

#[test]
fn inferred_note_replaces_an_older_inferred_explanation() {
    let explanation = inferred(json!([]));
    let old = format_note(
        "Old",
        "Old why.",
        None,
        ExplanationSource::Inferred,
        Some("claude-old"),
    );
    let mut with_note = commit_with("Changes a", "");
    with_note.note = Some(old);
    let note = compose_inferred_note(&with_note, &explanation, &MessageFormat::default());
    assert_eq!(parse_note(&note).why.as_deref(), Some("Because."));
    assert_eq!(parse_note(&note).model.as_deref(), Some("claude-x"));
}

// -- infer_uncommitted_explanation --

fn patch() -> GitFileDiff {
    diff(
        Some("a.ts"),
        Some("a.ts"),
        false,
        vec![
            hunk(1, 1, &["-one", "+two"]),
            hunk(9, 9, &["-nine", "+NINE"]),
        ],
    )
}

#[test]
fn explains_each_hunk_from_the_patch_alone() {
    let run = FakeClaude::new(
        json!({"changes": [{"change": 2, "what": " Shouts nine. ", "why": " Louder. "}]}),
    );
    let explained = infer_uncommitted_explanation(
        UncommittedLayer::Staged,
        &patch(),
        &options("claude-sonnet-5-5", run.runner()),
        &never(),
    )
    .unwrap();

    let prompt = run.prompt();
    assert!(prompt.contains("Staged changes to a.ts, not committed yet"));
    assert!(prompt.contains("[change 1] @@ -1 +1 @@"));
    assert!(prompt.contains("[change 2] @@ -9 +9 @@"));
    let schema = run.request(0).json_schema.unwrap();
    assert_eq!(schema, *UNCOMMITTED_SCHEMA);
    assert_eq!(schema["required"], json!(["changes"]));
    assert_eq!(explained.model, "claude-sonnet-5-5");
    assert_eq!(
        explained.changes,
        vec![ChangeExplanation {
            hunk: hunk_ref("a.ts", 9, 1, 9, 1),
            what: "Shouts nine.".into(),
            why: "Louder.".into(),
            justification: None,
            guide: Default::default(),
        }]
    );
}

#[test]
fn uncommitted_prompt_layout_for_a_rename() {
    let renamed = diff(
        Some("old.ts"),
        Some("new.ts"),
        false,
        vec![hunk(1, 1, &["-a", "+b"])],
    );
    let run = FakeClaude::new(json!({"changes": [{"change": 1, "what": "w", "why": "y"}]}));
    let explained = infer_uncommitted_explanation(
        UncommittedLayer::Unstaged,
        &renamed,
        &options("m", run.runner()),
        &never(),
    )
    .unwrap();
    let expected = [
        "Unstaged changes to old.ts -> new.ts, not committed yet",
        "",
        "Diff:",
        "diff --git a/old.ts b/new.ts",
        "rename from old.ts",
        "rename to new.ts",
        "--- a/old.ts",
        "+++ b/new.ts",
        "[change 1] @@ -1 +1 @@",
        "o1    -a",
        "n1    +b",
        "",
    ];
    assert_eq!(run.prompt(), expected.join("\n"));
    assert_eq!(
        explained.changes[0].hunk.original_path.as_deref(),
        Some("old.ts")
    );
}

#[test]
fn a_huge_uncommitted_diff_is_cut() {
    let lines: Vec<String> = (0..3000).map(|n| format!("+{n:06}")).collect();
    let big = diff(Some("a"), Some("a"), false, vec![hunk_owned(1, 1, &lines)]);
    let run = FakeClaude::new(json!({"changes": [{"change": 1, "what": "w", "why": "y"}]}));
    infer_uncommitted_explanation(
        UncommittedLayer::Unstaged,
        &big,
        &options("m", run.runner()),
        &never(),
    )
    .unwrap();
    let prompt = run.prompt();
    assert!(prompt.contains("\n\nThe diff is cut short.\n\nDiff:\n"));
    assert!(prompt.contains("[… rest of a left out]"));
}

fn assert_ignored(err: Error) {
    assert!(matches!(err, Error::IgnoredFile(_)), "{err:?}");
}

#[test]
fn never_sends_ignored_or_generated_files() {
    let hunks = patch().hunks;
    let secret = diff(
        Some("secrets/key.txt"),
        Some("secrets/key.txt"),
        false,
        hunks.clone(),
    );
    let mut opts = options("m", never_called());
    opts.ignore_rules = Some("secrets/\n");
    let err = infer_uncommitted_explanation(UncommittedLayer::Unstaged, &secret, &opts, &never())
        .unwrap_err();
    assert!(err.to_string().contains("isn't sent"));
    assert_ignored(err);

    let lock = diff(
        Some("pnpm-lock.yaml"),
        Some("pnpm-lock.yaml"),
        false,
        hunks.clone(),
    );
    let err = infer_uncommitted_explanation(
        UncommittedLayer::Unstaged,
        &lock,
        &options("m", never_called()),
        &never(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("isn't sent"));
    assert_ignored(err);

    // By the old path of a rename too
    let moved = diff(
        Some("secrets/key.txt"),
        Some("public/key.txt"),
        false,
        hunks,
    );
    let mut opts = options("m", never_called());
    opts.ignore_rules = Some("secrets/");
    let err = infer_uncommitted_explanation(UncommittedLayer::Staged, &moved, &opts, &never())
        .unwrap_err();
    assert_ignored(err);
}

#[test]
fn a_diff_without_hunks_is_not_sent() {
    let empty = diff(Some("a.ts"), Some("a.ts"), true, vec![]);
    let err = infer_uncommitted_explanation(
        UncommittedLayer::Staged,
        &empty,
        &options("m", never_called()),
        &never(),
    )
    .unwrap_err();
    assert!(matches!(err, Error::NothingToExplain(_)), "{err:?}");
    assert!(err.to_string().contains("a.ts has no staged changes"));
}

#[test]
fn an_answer_that_explains_no_hunk_is_an_error() {
    let run = FakeClaude::new(json!({"changes": [{"change": 5, "what": "w", "why": "y"}]}));
    let err = infer_uncommitted_explanation(
        UncommittedLayer::Staged,
        &patch(),
        &options("m", run.runner()),
        &never(),
    )
    .unwrap_err();
    assert_no_explanation(err);
    let run = FakeClaude::new(json!("text"));
    let err = infer_uncommitted_explanation(
        UncommittedLayer::Staged,
        &patch(),
        &options("m", run.runner()),
        &never(),
    )
    .unwrap_err();
    assert_no_explanation(err);
}

#[test]
fn an_untracked_file_is_an_addition() {
    let new_file = diff(None, Some("new.txt"), false, vec![hunk(0, 1, &["+hello"])]);
    let run = FakeClaude::new(json!({"changes": [{"change": 1, "what": "w", "why": "y"}]}));
    let explained = infer_uncommitted_explanation(
        UncommittedLayer::Unstaged,
        &new_file,
        &options("m", run.runner()),
        &never(),
    )
    .unwrap();
    assert!(
        run.prompt()
            .contains("--- /dev/null\n+++ b/new.txt\n[change 1] @@ -0,0 +1 @@\nn1    +hello")
    );
    assert_eq!(explained.changes[0].hunk, hunk_ref("new.txt", 0, 0, 1, 1));
}

// -- justifications --

#[test]
fn reads_the_justifications_of_the_commit_and_its_changes() {
    let files = vec![modified(
        "a.py",
        vec![
            hunk(1, 1, &["-a", "+b"]),
            hunk(10, 10, &["-c", "+d"]),
            hunk(20, 20, &["+e"]),
        ],
    )];
    let run = FakeClaude::new(json!({
        "what": "W",
        "why": "Y",
        "justification": " The diff shows it; a flag would not do. ",
        "changes": [
            {"change": 1, "what": "a", "why": "ya", "justification": " Ja. "},
            // Missing, empty or not a string: the change is kept without one
            {"change": 2, "what": "b", "why": "yb"},
            {"change": 3, "what": "c", "why": "yc", "justification": "  "},
        ],
    }));
    let explanation =
        infer_explanation(&commit(), &files, &options("m", run.runner()), &never()).unwrap();
    assert_eq!(
        explanation.explanation.justification.as_deref(),
        Some("The diff shows it; a flag would not do.")
    );
    let justifications: Vec<Option<&str>> = explanation
        .changes
        .iter()
        .map(|c| c.justification.as_deref())
        .collect();
    assert_eq!(justifications, [Some("Ja."), None, None]);

    let note = compose_inferred_note(
        &commit_with("Changes a", ""),
        &explanation,
        &MessageFormat::default(),
    );
    assert!(
        note.contains("Why: Y\nJustification: The diff shows it; a flag would not do.\n"),
        "{note}"
    );
    assert!(note.contains("Why: ya\nJustification: Ja.\n"), "{note}");
    let parsed = parse_note(&note);
    assert_eq!(
        parsed.justification.as_deref(),
        Some("The diff shows it; a flag would not do.")
    );
    assert_eq!(parsed.changes, explanation.changes);
}

#[test]
fn an_answer_without_justifications_still_explains() {
    for justification in [Value::Null, json!(""), json!(7)] {
        let mut output = json!({
            "what": "W",
            "why": "Y",
            "changes": [{"change": 1, "what": "w", "why": "y", "justification": justification}],
        });
        output["justification"] = justification.clone();
        let run = FakeClaude::new(output);
        let explanation = infer_explanation(
            &commit(),
            &[added("a.ts", &["a"])],
            &options("m", run.runner()),
            &never(),
        )
        .unwrap();
        assert_eq!(explanation.explanation.why.as_deref(), Some("Y"));
        assert_eq!(explanation.explanation.justification, None);
        assert_eq!(explanation.changes.len(), 1);
        assert_eq!(explanation.changes[0].justification, None);
        // The note is written as it was before justifications
        let note = compose_inferred_note(
            &commit_with("S", ""),
            &explanation,
            &MessageFormat::default(),
        );
        assert!(!note.contains("Justification"), "{note}");
    }
}

#[test]
fn uncommitted_answers_carry_justifications() {
    let run = FakeClaude::new(json!({"changes": [
        {"change": 1, "what": "w", "why": "y", "justification": " Only this fixes it. "},
    ]}));
    let result = infer_uncommitted_explanation(
        UncommittedLayer::Unstaged,
        &diff(
            Some("a.ts"),
            Some("a.ts"),
            false,
            vec![hunk(1, 1, &["-a", "+b"])],
        ),
        &options("m", run.runner()),
        &never(),
    )
    .unwrap();
    assert_eq!(
        result.changes[0].justification.as_deref(),
        Some("Only this fixes it.")
    );

    let run = FakeClaude::new(json!({"changes": [{"change": 1, "what": "w", "why": "y"}]}));
    let result = infer_uncommitted_explanation(
        UncommittedLayer::Unstaged,
        &diff(
            Some("a.ts"),
            Some("a.ts"),
            false,
            vec![hunk(1, 1, &["-a", "+b"])],
        ),
        &options("m", run.runner()),
        &never(),
    )
    .unwrap();
    assert_eq!(result.changes[0].justification, None);
}

#[test]
fn the_prompts_and_schemas_ask_for_justifications() {
    for prompt in [SYSTEM_PROMPT, meld_ailens::infer::UNCOMMITTED_SYSTEM_PROMPT] {
        assert!(prompt.contains("\"justification\": one to three plain sentences"));
        assert!(prompt.contains("which alternatives or simpler fixes it rules out"));
        assert!(prompt.contains("if that evidence is thin, say so plainly"));
        assert!(prompt.contains("No other markdown, no\nheadings, no lists."));
    }
    assert_eq!(
        SCHEMA["required"],
        json!(["what", "why", "justification", "changes"])
    );
    for schema in [&*SCHEMA, &*UNCOMMITTED_SCHEMA] {
        let items = &schema["properties"]["changes"]["items"];
        assert_eq!(
            items["properties"]["justification"],
            json!({"type": "string"})
        );
        assert_eq!(
            items["required"],
            json!([
                "change",
                "kind",
                "order",
                "what",
                "steps",
                "was",
                "now",
                "why",
                "check",
                "justification"
            ])
        );
        assert_eq!(
            items["properties"]["kind"]["enum"],
            json!([
                "behavior",
                "fix",
                "refactor",
                "rename",
                "mechanical",
                "test"
            ])
        );
    }
}

#[test]
fn the_prompts_ask_for_a_guide_to_each_change() {
    for prompt in [SYSTEM_PROMPT, meld_ailens::infer::UNCOMMITTED_SYSTEM_PROMPT] {
        assert!(prompt.contains("\"what\": one plain sentence saying what the changed code does"));
        assert!(prompt.contains("\"steps\": the changed lines read line by line"));
        assert!(prompt.contains("\"nN\" for line N of the new version"));
        assert!(prompt.contains("\"was\": what the code did before the change"));
        assert!(prompt.contains("\"check\": one plain sentence on what a reviewer should verify"));
        assert!(prompt.contains("in backticks"));
    }
    assert!(SYSTEM_PROMPT.ends_with("the commit message and diff you are given."));
}

#[test]
fn answers_give_each_change_its_guide() {
    let hunks = [hunk_ref("a.rs", 1, 1, 1, 1), hunk_ref("a.rs", 9, 1, 9, 1)];
    let answer = json!([
        {
            "change": 1, "kind": "Fix", "order": 2, "what": "Stops at the end",
            "was": " loops forever ", "now": "stops at `end`", "why": "Hangs.",
            "check": "Callers with an empty list.", "justification": "",
        },
        {
            "change": 2, "kind": "sideways", "order": 0, "what": "Renames",
            "was": "", "now": "", "why": "Clearer.", "check": "",
        },
    ]);
    let changes = parse_changes(Some(&answer), &hunks);
    assert_eq!(
        changes[0].guide,
        ChangeGuide {
            kind: Some(ChangeKind::Fix),
            order: Some(2),
            was: Some("loops forever".into()),
            now: Some("stops at `end`".into()),
            check: Some("Callers with an empty list.".into()),
            steps: Vec::new(),
        }
    );
    assert_eq!(changes[0].justification, None);
    // Unknown kinds, orders below 1 and empty texts are left out
    assert!(changes[1].guide.is_empty());
}

#[test]
fn answers_read_each_change_line_by_line() {
    // Lines 10-14 new, 7-8 old
    let hunks = [hunk_ref("a.rs", 7, 2, 10, 5)];
    let answer = json!([{
        "change": 1, "kind": "fix", "order": 1, "what": "Stops at the end",
        "was": "", "now": "", "why": "Hangs.", "check": "", "justification": "",
        "steps": [
            {"lines": "n10", "says": "Reads the end."},
            {"lines": "n12-n16", "says": "Loops up to it."},
            {"lines": "o8", "says": "Drops the old loop."},
            {"lines": "n40", "says": "Not a line of the change."},
            {"lines": "n11", "says": "  "},
        ],
    }]);
    let changes = parse_changes(Some(&answer), &hunks);
    let at = |side, first, last| Some(StepLines { side, first, last });
    assert_eq!(
        changes[0].guide.steps,
        [
            ChangeStep {
                lines: at(Side::New, 1, 1),
                text: "Reads the end.".into()
            },
            // Cut to the change's last line
            ChangeStep {
                lines: at(Side::New, 3, 5),
                text: "Loops up to it.".into()
            },
            ChangeStep {
                lines: at(Side::Old, 2, 2),
                text: "Drops the old loop.".into()
            },
            ChangeStep {
                lines: None,
                text: "Not a line of the change.".into()
            },
        ]
    );
}

#[test]
fn prompt_lines_are_tagged_with_their_line_numbers() {
    let files = [modified(
        "a.rs",
        vec![hunk(3, 3, &[" keep", "-old", "+new"])],
    )];
    let prompt = build_prompt(&commit(), &files, None);
    assert!(
        prompt.text.contains("n3     keep\no4    -old\nn4    +new"),
        "{}",
        prompt.text
    );
}
