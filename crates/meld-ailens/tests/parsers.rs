//! Port of test/ailens/test_parsers.py (git output parsers and line maps)

use std::path::PathBuf;

use meld_ailens::linemap::{new_to_old, old_to_new};
use meld_ailens::parsers::{
    GitFileStats, log_format, parse_blame, parse_diff, parse_log, parse_name_status, parse_numstat,
    parse_patch_ids, parse_refs, parse_rev_list_parents_ordered, parse_worktrees, unquote_git_path,
};
use meld_ailens::{
    DiffLineKind, Error, GitBlameLine, GitCommit, GitDiffHunk, GitDiffLine, GitFileChange,
    GitFileDiff, GitFileStatus, GitIdentity, GitRef, GitRefType, GitTrailer, GitWorktree,
};

fn text(lines: &[&str]) -> String {
    lines.join("\n")
}

fn changed(diff: &GitFileDiff) -> Vec<GitDiffLine> {
    diff.hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind != DiffLineKind::Context)
        .cloned()
        .collect()
}

fn one(output: &str) -> GitFileDiff {
    let mut files = parse_diff(output).unwrap();
    assert_eq!(files.len(), 1);
    files.remove(0)
}

fn line(kind: DiffLineKind, content: &str, old: Option<usize>, new: Option<usize>) -> GitDiffLine {
    GitDiffLine::new(kind, content, old, new)
}

fn no_newline(mut line: GitDiffLine) -> GitDiffLine {
    line.no_newline_at_end = true;
    line
}

fn parse_error_contains(result: Result<impl std::fmt::Debug, Error>, needles: &[&str]) {
    match result {
        Err(Error::Parse(message)) => assert!(
            needles.iter().any(|needle| message.contains(needle)),
            "unexpected message {message:?}"
        ),
        other => panic!("expected a parse error, got {other:?}"),
    }
}

use DiffLineKind::{Add, Context, Delete};

// -- parse_diff --

#[test]
fn diff_numbers_lines_on_both_sides_across_hunks() {
    let file = one(&text(&[
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
    ]));

    assert_eq!(
        file,
        GitFileDiff {
            old_path: Some("src/cache.ts".into()),
            new_path: Some("src/cache.ts".into()),
            binary: false,
            hunks: vec![
                GitDiffHunk {
                    old_start: 1,
                    old_count: 3,
                    new_start: 1,
                    new_count: 4,
                    section: "export class Cache {".into(),
                    lines: vec![
                        line(Context, "a", Some(1), Some(1)),
                        line(Delete, "b", Some(2), None),
                        line(Add, "B", None, Some(2)),
                        line(Add, "B2", None, Some(3)),
                        line(Context, "c", Some(3), Some(4)),
                    ],
                },
                GitDiffHunk {
                    old_start: 10,
                    old_count: 1,
                    new_start: 11,
                    new_count: 0,
                    section: String::new(),
                    lines: vec![line(Delete, "gone", Some(10), None)],
                },
            ],
        }
    );
}

#[test]
fn diff_marks_lines_without_a_trailing_newline() {
    let file = one(&text(&[
        "diff --git a/x b/x",
        "--- a/x",
        "+++ b/x",
        "@@ -1 +1 @@",
        "-old",
        "\\ No newline at end of file",
        "+new",
        "\\ No newline at end of file",
        "",
    ]));

    assert_eq!(
        file.hunks[0].lines,
        vec![
            no_newline(line(Delete, "old", Some(1), None)),
            no_newline(line(Add, "new", None, Some(1))),
        ]
    );
}

#[test]
fn diff_no_newline_marker_on_a_context_line_mid_file() {
    let file = one(&text(&[
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
    ]));

    let flags: Vec<bool> = file.hunks[0]
        .lines
        .iter()
        .map(|l| l.no_newline_at_end)
        .collect();
    assert_eq!(flags, [false, true, false, true]);
}

#[test]
fn diff_reads_hunk_bodies_by_count_so_header_like_content_stays_content() {
    let files = parse_diff(&text(&[
        "diff --git a/x b/x",
        "--- a/x",
        "+++ b/x",
        "@@ -0,0 +1,4 @@",
        "+diff --git a/y b/y",
        "+@@ -1 +1 @@",
        "+--- a/z",
        "+Binary files a and b differ",
        "",
    ]))
    .unwrap();

    assert_eq!(files.len(), 1);
    assert!(!files[0].binary);
    let contents: Vec<&str> = files[0].hunks[0]
        .lines
        .iter()
        .map(|l| l.content.as_str())
        .collect();
    assert_eq!(
        contents,
        [
            "diff --git a/y b/y",
            "@@ -1 +1 @@",
            "--- a/z",
            "Binary files a and b differ",
        ]
    );
}

#[test]
fn diff_handles_added_deleted_renamed_and_binary_files() {
    let files = parse_diff(&text(&[
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
    ]))
    .unwrap();

    let summary: Vec<(Option<&str>, Option<&str>, bool, usize)> = files
        .iter()
        .map(|f| {
            (
                f.old_path.as_deref(),
                f.new_path.as_deref(),
                f.binary,
                f.hunks.len(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (None, Some("new.ts"), false, 1),
            (Some("old.ts"), None, false, 1),
            (Some("a.ts"), Some("b.ts"), false, 0),
            // Paths from the diff --git line (ai_lens leaves them unset)
            (Some("img.png"), Some("img.png"), true, 0),
            (None, Some("new.png"), true, 0),
        ]
    );
}

#[test]
fn diff_empty_files_and_mode_changes_take_paths_from_the_header() {
    let files = parse_diff(&text(&[
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
    ]))
    .unwrap();

    let summary: Vec<(Option<&str>, Option<&str>, usize)> = files
        .iter()
        .map(|f| (f.old_path.as_deref(), f.new_path.as_deref(), f.hunks.len()))
        .collect();
    assert_eq!(
        summary,
        [
            (None, Some("empty file"), 0),
            (Some("gone"), None, 0),
            (Some("run.sh"), Some("run.sh"), 0),
        ]
    );
}

#[test]
fn diff_copies() {
    let file = one(&text(&[
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
    ]));

    assert_eq!(
        (file.old_path.as_deref(), file.new_path.as_deref()),
        (Some("a.txt"), Some("c.txt"))
    );
}

#[test]
fn diff_unquotes_paths_and_drops_the_tab_after_paths_with_spaces() {
    let file = one(&text(&[
        "diff --git \"a/t\\303\\244b\\tx\" \"b/my file\"",
        "--- \"a/t\\303\\244b\\tx\"",
        "+++ b/my file\t",
        "@@ -1 +1 @@",
        "-a",
        "+b",
        "",
    ]));

    assert_eq!(file.old_path.as_deref(), Some("täb\tx"));
    assert_eq!(file.new_path.as_deref(), Some("my file"));
}

#[test]
fn diff_quoted_header_paths_for_binary_files() {
    let file = one(&text(&[
        "diff --git \"a/new\\nline.bin\" \"b/new\\nline.bin\"",
        "index 1111111..2222222 100644",
        "Binary files \"a/new\\nline.bin\" and \"b/new\\nline.bin\" differ",
        "",
    ]));

    assert_eq!(
        (
            file.old_path.as_deref(),
            file.new_path.as_deref(),
            file.binary
        ),
        (Some("new\nline.bin"), Some("new\nline.bin"), true)
    );
}

#[test]
fn diff_keeps_carriage_returns_and_odd_whitespace_in_content() {
    let file = one(&text(&[
        "diff --git a/w.txt b/w.txt",
        "--- a/w.txt",
        "+++ b/w.txt",
        "@@ -1,2 +1,2 @@",
        "-a\r",
        "-form\x0cfeed sep",
        "+a",
        "+form\x0cfeed sep\r",
        "",
    ]));

    let contents: Vec<String> = changed(&file).into_iter().map(|l| l.content).collect();
    assert_eq!(
        contents,
        ["a\r", "form\x0cfeed sep", "a", "form\x0cfeed sep\r"]
    );
}

#[test]
fn diff_blank_context_lines() {
    let file = one(&text(&[
        "diff --git a/x b/x",
        "--- a/x",
        "+++ b/x",
        "@@ -1,3 +1,3 @@",
        " ",
        "-a",
        "+b",
        " ",
        "",
    ]));

    let contents: Vec<&str> = file.hunks[0]
        .lines
        .iter()
        .map(|l| l.content.as_str())
        .collect();
    assert_eq!(contents, ["", "a", "b", ""]);
}

#[test]
fn diff_rejects_hunks_shorter_than_their_header_says() {
    parse_error_contains(
        parse_diff(&text(&["diff --git a/x b/x", "@@ -1,2 +1,2 @@", " a", ""])),
        &["Truncated", "Unexpected"],
    );
}

#[test]
fn diff_rejects_bad_lines_and_headers() {
    parse_error_contains(
        parse_diff(&text(&[
            "diff --git a/x b/x",
            "@@ -1 +1 @@",
            "?a",
            "+b",
            "",
        ])),
        &["Unexpected git diff line"],
    );
    parse_error_contains(
        parse_diff(&text(&["diff --git a/x b/x", "@@ nonsense @@", ""])),
        &["hunk header"],
    );
}

#[test]
fn diff_of_nothing() {
    assert_eq!(parse_diff("").unwrap(), Vec::<GitFileDiff>::new());
}

#[test]
fn unquote_leaves_unquoted_paths_alone() {
    assert_eq!(unquote_git_path("src/a b.ts"), "src/a b.ts");
    assert_eq!(unquote_git_path("\""), "\"");
}

#[test]
fn unquote_decodes_escapes_and_utf8_octal_bytes() {
    assert_eq!(
        unquote_git_path("\"a\\\"b\\\\c\\n\\346\\227\\245\""),
        "a\"b\\c\n日"
    );
    assert_eq!(
        unquote_git_path("\"\\a\\b\\f\\r\\t\\v\""),
        "\x07\x08\x0c\r\t\x0b"
    );
}

#[test]
fn unquote_replaces_invalid_utf8() {
    // Python keeps invalid UTF-8 as surrogates (surrogateescape); a Rust
    // String can't hold it, so it is replaced with U+FFFD
    assert_eq!(unquote_git_path("\"\\377x\""), "\u{FFFD}x");
}

// -- parse_blame --

const BASE: &str = "1f15d8ef7070dc0f28bf266db22c37c813d56c4d";
const CHANGE: &str = "c5872d2919c20e8f16dedc70223500b70e1d4a94";

fn commit_info(summary: &str) -> Vec<String> {
    vec![
        "author a".into(),
        "author-mail <a@a>".into(),
        "author-time 1791446524".into(),
        "author-tz +0200".into(),
        "committer a".into(),
        "committer-mail <a@a>".into(),
        "committer-time 1791446524".into(),
        "committer-tz +0200".into(),
        format!("summary {summary}"),
    ]
}

fn blame(
    sha: &str,
    original: usize,
    final_line: usize,
    boundary: bool,
    filename: &str,
) -> GitBlameLine {
    GitBlameLine {
        sha: sha.into(),
        original_line: original,
        final_line,
        boundary,
        filename: filename.into(),
    }
}

fn joined(parts: Vec<Vec<String>>) -> String {
    parts.concat().join("\n")
}

fn s(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

#[test]
fn blame_reads_boundary_and_filename_once_per_commit() {
    let output = joined(vec![
        vec![format!("{BASE} 1 1 1")],
        commit_info("base"),
        s(&["boundary", "filename old.txt", "\tl1"]),
        vec![format!("{CHANGE} 5 2 1")],
        commit_info("replace l6"),
        vec![format!("previous {BASE} old.txt")],
        s(&["filename new.txt", "\tX"]),
        vec![format!("{BASE} 3 3 2"), "\tl3".into()],
        vec![format!("{BASE} 4 4"), "\tl4".into(), String::new()],
    ]);

    assert_eq!(
        parse_blame(&output).unwrap(),
        vec![
            blame(BASE, 1, 1, true, "old.txt"),
            blame(CHANGE, 5, 2, false, "new.txt"),
            blame(BASE, 3, 3, true, "old.txt"),
            blame(BASE, 4, 4, true, "old.txt"),
        ]
    );
}

#[test]
fn blame_sorts_by_final_line() {
    let output = text(&[
        &format!("{CHANGE} 2 2 1"),
        "filename f",
        "\tb",
        &format!("{BASE} 1 1 1"),
        "filename f",
        "\ta",
        "",
    ]);

    let finals: Vec<usize> = parse_blame(&output)
        .unwrap()
        .iter()
        .map(|l| l.final_line)
        .collect();
    assert_eq!(finals, [1, 2]);
}

#[test]
fn blame_keeps_content_that_looks_like_a_header() {
    let output = joined(vec![
        vec![format!("{CHANGE} 1 1 1")],
        commit_info("x"),
        vec![
            "filename f".into(),
            format!("\t{BASE} 1 1 1"),
            String::new(),
        ],
    ]);

    assert_eq!(parse_blame(&output).unwrap().len(), 1);
}

#[test]
fn blame_unquotes_filenames() {
    let output = text(&[
        &format!("{CHANGE} 1 1 1"),
        "filename \"a\\nb \\\"q\\\"\"",
        "\tx",
        "",
    ]);
    let lines = parse_blame(&output).unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].filename, "a\nb \"q\"");
}

#[test]
fn blame_rejects_truncated_and_garbled_output() {
    parse_error_contains(
        parse_blame(&format!("{CHANGE} 1 1 1\nfilename f\n")),
        &["Truncated git blame output"],
    );
    parse_error_contains(parse_blame("nonsense\n"), &["Unexpected git blame line"]);
}

#[test]
fn blame_of_nothing() {
    assert!(parse_blame("").unwrap().is_empty());
}

// -- parse_log --

#[derive(Default)]
struct Record<'a> {
    sha: Option<&'a str>,
    parents: &'a str,
    subject: Option<&'a str>,
    body: &'a str,
    trailers: &'a str,
    note: Option<&'a str>,
}

fn record(r: Record<'_>) -> String {
    let sha = r.sha.map_or_else(|| "a".repeat(40), str::to_string);
    let mut fields = vec![
        sha,
        r.parents.to_string(),
        "Jane Doe".into(),
        "jane@example.com".into(),
        "2023-11-14T22:13:20+00:00".into(),
        "Jane Doe".into(),
        "jane@example.com".into(),
        "2023-11-14T22:14:20+00:00".into(),
        r.subject.unwrap_or("Subject").to_string(),
        r.body.to_string(),
        r.trailers.to_string(),
    ];
    if let Some(note) = r.note {
        fields.push(note.to_string());
    }
    fields.join("\0")
}

#[test]
fn log_parses_records_parents_and_trailers() {
    let b = "b".repeat(40);
    let parents = format!("{} {}", "a".repeat(40), "c".repeat(40));
    let output = record(Record {
        sha: Some(&b),
        parents: &parents,
        subject: Some("Makes load async"),
        body: "Why: the cache needs I/O\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>\n",
        trailers: "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>\x1fSigned-off-by: Jane Doe <jane@example.com>",
        ..Default::default()
    }) + "\0"
        + &record(Record::default())
        + "\0";

    let commits = parse_log(&output, false).unwrap();

    assert_eq!(commits.len(), 2);
    assert_eq!(
        commits[0],
        GitCommit {
            sha: b.clone(),
            parents: vec!["a".repeat(40), "c".repeat(40)],
            author: GitIdentity {
                name: "Jane Doe".into(),
                email: "jane@example.com".into(),
                date: "2023-11-14T22:13:20+00:00".into(),
            },
            committer: GitIdentity {
                name: "Jane Doe".into(),
                email: "jane@example.com".into(),
                date: "2023-11-14T22:14:20+00:00".into(),
            },
            subject: "Makes load async".into(),
            body: "Why: the cache needs I/O\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>".into(),
            trailers: vec![
                GitTrailer::new("Co-Authored-By", "Claude Opus 5.5 <noreply@anthropic.com>"),
                GitTrailer::new("Signed-off-by", "Jane Doe <jane@example.com>"),
            ],
            note: None,
        }
    );
    assert!(commits[1].parents.is_empty());
    assert!(commits[1].trailers.is_empty());
    assert_eq!(commits[1].body, "");
    assert_eq!(commits[1].note, None);
}

#[test]
fn log_reads_the_note_trimmed() {
    let commits = parse_log(
        &(record(Record {
            note: Some("Why: because\n\n"),
            ..Default::default()
        }) + "\0"),
        true,
    )
    .unwrap();
    assert_eq!(commits[0].note.as_deref(), Some("Why: because"));

    let commits = parse_log(
        &(record(Record {
            note: Some(""),
            ..Default::default()
        }) + "\0"),
        true,
    )
    .unwrap();
    assert_eq!(commits[0].note, None);
}

#[test]
fn log_skips_trailer_lines_without_a_key() {
    let commits = parse_log(
        &(record(Record {
            trailers: ": x\x1fno colon\x1fKey: v",
            ..Default::default()
        }) + "\0"),
        false,
    )
    .unwrap();

    assert_eq!(commits[0].trailers, vec![GitTrailer::new("Key", "v")]);
}

#[test]
fn log_returns_nothing_for_empty_output() {
    assert!(parse_log("", false).unwrap().is_empty());
}

#[test]
fn log_rejects_output_with_a_partial_record() {
    parse_error_contains(parse_log("abc\0def\0", false), &["not a multiple"]);
}

#[test]
fn log_format_adds_notes_only_when_asked() {
    assert!(!log_format(false).contains("%N"));
    assert!(log_format(true).ends_with("%x00%N"));
    assert!(log_format(false).starts_with("--format=%H%x00%P"));
}

// -- parse_name_status, parse_numstat --

#[test]
fn name_status_parses_z_output_with_renames_and_copies() {
    let output = "M\0src/a.ts\0R087\0old name.ts\0src/new.ts\0A\0b.ts\0D\0c.ts\0C100\0x\0y\0T\0l\0";

    assert_eq!(
        parse_name_status(output).unwrap(),
        vec![
            GitFileChange::new(GitFileStatus::Modified, "src/a.ts"),
            GitFileChange::renamed(
                GitFileStatus::Renamed,
                "src/new.ts",
                "old name.ts",
                Some(87)
            ),
            GitFileChange::new(GitFileStatus::Added, "b.ts"),
            GitFileChange::new(GitFileStatus::Deleted, "c.ts"),
            GitFileChange::renamed(GitFileStatus::Copied, "y", "x", Some(100)),
            GitFileChange::new(GitFileStatus::TypeChanged, "l"),
        ]
    );
}

#[test]
fn name_status_keeps_newlines_and_tabs_in_paths() {
    assert_eq!(
        parse_name_status("M\0a\nb\tc\0").unwrap(),
        vec![GitFileChange::new(GitFileStatus::Modified, "a\nb\tc")]
    );
}

#[test]
fn name_status_rejects_unknown_statuses_and_truncated_entries() {
    parse_error_contains(parse_name_status("Q\0a\0"), &["Unexpected"]);
    parse_error_contains(parse_name_status("R100\0a\0"), &["Truncated"]);
    parse_error_contains(parse_name_status("M\0"), &["Truncated"]);
}

#[test]
fn numstat_reads_counts_binary_files_and_renames() {
    let output = "3\t1\tsrc/a.ts\0-\t-\timg.png\x002\t0\t\0old.ts\0src/new.ts\0";

    let stats = parse_numstat(output).unwrap();
    assert_eq!(
        stats,
        vec![
            GitFileStats::new("src/a.ts", Some(3), Some(1)),
            GitFileStats::new("img.png", None, None),
            GitFileStats::new("src/new.ts", Some(2), Some(0)),
        ]
    );
    assert!(stats[1].binary());
}

#[test]
fn numstat_keeps_newlines_in_paths() {
    assert_eq!(
        parse_numstat("1\t0\ta\nb\0").unwrap(),
        vec![GitFileStats::new("a\nb", Some(1), Some(0))]
    );
}

#[test]
fn numstat_rejects_malformed_entries() {
    parse_error_contains(parse_numstat("x\0"), &["Unexpected"]);
    parse_error_contains(parse_numstat("1\t1\t\0old.ts\0"), &["Truncated"]);
}

// -- small parsers --

#[test]
fn patch_ids_map_commit_to_patch_id() {
    let ids = parse_patch_ids("p1 c1\np2 c2\n").unwrap();
    assert_eq!(ids.len(), 2);
    assert_eq!(ids["c1"], "p1");
    assert_eq!(ids["c2"], "p2");
    assert!(matches!(parse_patch_ids("p1\n"), Err(Error::Parse(_))));
}

#[test]
fn rev_list_parents_map_commit_to_parents() {
    let parents = parse_rev_list_parents_ordered("m a b\na r\nr\n");
    assert_eq!(
        parents,
        [
            ("m".to_string(), vec!["a".to_string(), "b".to_string()]),
            ("a".to_string(), vec!["r".to_string()]),
            ("r".to_string(), vec![]),
        ]
    );
}

// -- parse_refs --

fn sha(c: char) -> String {
    c.to_string().repeat(40)
}

fn ref_line(fields: &[&str]) -> String {
    fields.join("\0") + "\n"
}

fn git_ref(
    kind: GitRefType,
    name: &str,
    full_name: &str,
    sha: String,
    subject: &str,
    date: &str,
    current: bool,
) -> GitRef {
    GitRef {
        kind,
        name: name.into(),
        full_name: full_name.into(),
        sha,
        subject: subject.into(),
        date: date.into(),
        current,
        worktree: None,
    }
}

#[test]
fn refs_reads_branches_remotes_and_tags_peeling_annotated_tags() {
    let output = ref_line(&[
        "refs/heads/main",
        "main",
        "",
        "*",
        "commit",
        &sha('a'),
        "Main tip",
        "2024-01-02T00:00:00Z",
        "",
        "",
        "",
        "",
    ]) + &ref_line(&[
        "refs/remotes/origin/main",
        "origin/main",
        "",
        " ",
        "commit",
        &sha('b'),
        "Remote tip",
        "2024-01-01T00:00:00Z",
        "",
        "",
        "",
        "",
    ]) + &ref_line(&[
        "refs/tags/v1",
        "v1",
        "",
        " ",
        "tag",
        &sha('c'),
        "Release notes",
        "",
        "commit",
        &sha('d'),
        "Tagged commit",
        "2023-12-01T00:00:00Z",
    ]);

    assert_eq!(
        parse_refs(&output).unwrap(),
        vec![
            git_ref(
                GitRefType::Branch,
                "main",
                "refs/heads/main",
                sha('a'),
                "Main tip",
                "2024-01-02T00:00:00Z",
                true
            ),
            git_ref(
                GitRefType::Remote,
                "origin/main",
                "refs/remotes/origin/main",
                sha('b'),
                "Remote tip",
                "2024-01-01T00:00:00Z",
                false,
            ),
            git_ref(
                GitRefType::Tag,
                "v1",
                "refs/tags/v1",
                sha('d'),
                "Tagged commit",
                "2023-12-01T00:00:00Z",
                false
            ),
        ]
    );
}

#[test]
fn refs_skips_symbolic_refs_and_refs_that_do_not_point_at_commits() {
    let output = ref_line(&[
        "refs/remotes/origin/HEAD",
        "origin/HEAD",
        "refs/remotes/origin/main",
        " ",
        "commit",
        &sha('b'),
        "s",
        "d",
        "",
        "",
        "",
        "",
    ]) + &ref_line(&[
        "refs/tags/tree",
        "tree",
        "",
        " ",
        "tree",
        &sha('e'),
        "",
        "",
        "",
        "",
        "",
        "",
    ]) + &ref_line(&[
        "refs/tags/blob-tag",
        "blob-tag",
        "",
        " ",
        "tag",
        &sha('f'),
        "m",
        "",
        "blob",
        &sha('g'),
        "",
        "",
    ]) + &ref_line(&[
        "refs/stash",
        "stash",
        "",
        " ",
        "commit",
        &sha('h'),
        "s",
        "d",
        "",
        "",
        "",
        "",
    ]);

    assert!(parse_refs(&output).unwrap().is_empty());
}

#[test]
fn refs_rejects_lines_with_the_wrong_field_count() {
    assert!(matches!(
        parse_refs("refs/heads/main\0main\n"),
        Err(Error::Parse(_))
    ));
}

// -- parse_worktrees --

fn worktree(path: &str, head: Option<String>, branch: Option<&str>) -> GitWorktree {
    GitWorktree {
        path: PathBuf::from(path),
        head,
        branch: branch.map(str::to_string),
        ..Default::default()
    }
}

#[test]
fn worktrees_reads_every_attribute() {
    let head_a = format!("HEAD {}", sha('a'));
    let head_b = format!("HEAD {}", sha('b'));
    let head_unborn = format!("HEAD {}", "0".repeat(40));
    let head_c = format!("HEAD {}", sha('c'));
    let output = [
        "worktree /repo",
        &head_a,
        "branch refs/heads/main",
        "",
        "worktree /wt/with space\nand newline",
        &head_b,
        "branch refs/heads/feature/x",
        "locked on usb",
        "",
        "worktree /wt/unborn",
        &head_unborn,
        "branch refs/heads/orphan",
        "",
        "worktree /wt/gone",
        &head_c,
        "detached",
        "prunable gitdir file points to non-existent location",
        "",
        "",
    ]
    .join("\0");

    assert_eq!(
        parse_worktrees(&output).unwrap(),
        vec![
            GitWorktree {
                main: true,
                ..worktree("/repo", Some(sha('a')), Some("main"))
            },
            worktree(
                "/wt/with space\nand newline",
                Some(sha('b')),
                Some("feature/x")
            ),
            worktree("/wt/unborn", None, Some("orphan")),
            GitWorktree {
                prunable: true,
                ..worktree("/wt/gone", Some(sha('c')), None)
            },
        ]
    );
}

#[test]
fn worktrees_of_a_bare_repository() {
    assert_eq!(
        parse_worktrees("worktree /r.git\0bare\0\0").unwrap(),
        vec![GitWorktree {
            bare: true,
            main: true,
            ..worktree("/r.git", None, None)
        }]
    );
}

#[test]
fn worktrees_rejects_records_without_a_path() {
    assert!(matches!(
        parse_worktrees("HEAD x\0\0"),
        Err(Error::Parse(_))
    ));
}

// -- line maps --

/// old: 1 2 3 4 5 6 7 8 9 -> new: 1 x 2 3 4 6 7 9 y
/// (x added, 5 and 8 deleted, y added at the end)
fn line_map_diff() -> GitFileDiff {
    one(&text(&[
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
    ]))
}

#[test]
fn new_to_old_with_none_for_added_lines() {
    let diff = line_map_diff();
    let mapped: Vec<Option<usize>> = (1..10).map(|line| new_to_old(&diff, line)).collect();
    assert_eq!(
        mapped,
        [
            Some(1),
            None,
            Some(2),
            Some(3),
            Some(4),
            Some(6),
            Some(7),
            Some(9),
            None
        ]
    );
}

#[test]
fn old_to_new_with_none_for_deleted_lines() {
    let diff = line_map_diff();
    let mapped: Vec<Option<usize>> = (1..10).map(|line| old_to_new(&diff, line)).collect();
    assert_eq!(
        mapped,
        [
            Some(1),
            Some(3),
            Some(4),
            Some(5),
            None,
            Some(6),
            Some(7),
            None,
            Some(8)
        ]
    );
}

#[test]
fn line_maps_beyond_the_last_hunk_shift() {
    let diff = one(&text(&[
        "diff --git a/f b/f",
        "--- a/f",
        "+++ b/f",
        "@@ -1 +1,2 @@",
        " a",
        "+b",
        "",
    ]));

    assert_eq!(new_to_old(&diff, 10), Some(9));
    assert_eq!(old_to_new(&diff, 9), Some(10));
}

#[test]
fn line_maps_nothing_in_a_file_that_is_all_added_or_all_deleted() {
    let added = one(
        "diff --git a/f b/f\nnew file mode 100644\n--- /dev/null\n+++ b/f\n@@ -0,0 +1,2 @@\n+a\n+b\n",
    );
    assert_eq!(new_to_old(&added, 1), None);
    assert_eq!(new_to_old(&added, 2), None);

    let deleted = one(
        "diff --git a/f b/f\ndeleted file mode 100644\n--- a/f\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-a\n-b\n",
    );
    assert_eq!(old_to_new(&deleted, 1), None);
    assert_eq!(old_to_new(&deleted, 2), None);
}

#[test]
fn line_maps_without_hunks_are_identity() {
    let diff = GitFileDiff {
        old_path: Some("f".into()),
        new_path: Some("f".into()),
        binary: false,
        hunks: Vec::new(),
    };

    assert_eq!(new_to_old(&diff, 3), Some(3));
    assert_eq!(old_to_new(&diff, 3), Some(3));
}

// -- change blocks --

fn header(hunk: &GitDiffHunk) -> (usize, usize, usize, usize) {
    (
        hunk.old_start,
        hunk.old_count,
        hunk.new_start,
        hunk.new_count,
    )
}

fn body(hunk: &GitDiffHunk) -> Vec<String> {
    hunk.lines
        .iter()
        .map(|line| format!("{}{}", line.kind.prefix(), line.content))
        .collect()
}

#[test]
fn blocks_split_changes_git_joined_into_one_hunk() {
    // Six lines apart: git joins them even with --inter-hunk-context=0
    let diff = one(&text(&[
        "diff --git a/f b/f",
        "--- a/f",
        "+++ b/f",
        "@@ -2,13 +2,14 @@ fn top",
        " 2",
        " 3",
        " 4",
        "-5",
        "+five",
        " 6",
        " 7",
        " 8",
        " 9",
        " 10",
        " 11",
        "+new",
        " 12",
        " 13",
        " 14",
    ]));
    let blocks = diff.hunks[0].blocks();

    // As `git diff -U3` prints each change on its own
    assert_eq!(
        blocks.iter().map(header).collect::<Vec<_>>(),
        [(2, 7, 2, 7), (9, 6, 9, 7)]
    );
    assert_eq!(
        body(&blocks[0]),
        [" 2", " 3", " 4", "-5", "+five", " 6", " 7", " 8"]
    );
    assert_eq!(
        body(&blocks[1]),
        [" 9", " 10", " 11", "+new", " 12", " 13", " 14"]
    );
    assert!(blocks.iter().all(|block| block.section == "fn top"));
}

#[test]
fn blocks_share_out_short_context_without_overlapping() {
    let diff = one(&text(&[
        "diff --git a/f b/f",
        "--- a/f",
        "+++ b/f",
        "@@ -1,9 +1,6 @@",
        "-a",
        " b",
        " c",
        " d",
        "-e",
        " f",
        " g",
        "-h",
        " i",
    ]));
    let blocks = diff.hunks[0].blocks();

    assert_eq!(
        blocks.iter().map(header).collect::<Vec<_>>(),
        [(1, 3, 1, 2), (4, 3, 3, 2), (7, 3, 5, 2)]
    );
    assert_eq!(body(&blocks[0]), ["-a", " b", " c"]);
    assert_eq!(body(&blocks[1]), [" d", "-e", " f"]);
    assert_eq!(body(&blocks[2]), [" g", "-h", " i"]);
}

#[test]
fn a_hunk_with_one_change_block_is_its_own_block() {
    let diff = one(&text(&[
        "diff --git a/f b/f",
        "--- a/f",
        "+++ b/f",
        "@@ -1,4 +1,4 @@",
        " a",
        "-b",
        "+B",
        "-c",
        "+C",
        " d",
    ]));
    assert_eq!(diff.hunks[0].blocks(), diff.hunks);

    let added = one(
        "diff --git a/f b/f\nnew file mode 100644\n--- /dev/null\n+++ b/f\n@@ -0,0 +1,2 @@\n+a\n+b\n",
    );
    assert_eq!(added.hunks[0].blocks(), added.hunks);
}
