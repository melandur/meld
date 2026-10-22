//! Port of test/ailens/test_generate_build.py (itself ported from AI Lens
//! src/build/__tests__/generateBuild.test.ts, the writing half, plus edge
//! cases)

use std::path::{Path, PathBuf};

use meld_ailens::build_facts::{
    BuildAgent, BuildChange, BuildCommit, BuildFacts, BuildFile, BuildFileEntry, BuildRevision,
    BuildStats,
};
use meld_ailens::build_file::render_index;
use meld_ailens::build_index::read_build_headers;
use meld_ailens::build_narrative::{BuildDecision, BuildNarrative};
use meld_ailens::generate_build::{GeneratedBuild, write_build, write_text_atomic};
use meld_ailens::time::timestamp_utc;
use meld_ailens::{ComparisonRelation, ExplanationSource, GitFileStatus, LineRange, Result};

const GENERATOR: &str = "meld-ailens@test";

fn base() -> String {
    "1".repeat(40)
}
fn agent() -> String {
    "2".repeat(40)
}
fn human() -> String {
    "3".repeat(40)
}

fn created() -> f64 {
    timestamp_utc(2026, 10, 8, 12, 0, 0.0)
}

fn revision(sha: String, ref_name: Option<&str>) -> BuildRevision {
    BuildRevision {
        short_sha: sha[..7].to_string(),
        sha,
        ref_name: ref_name.map(str::to_string),
    }
}

fn make_facts() -> BuildFacts {
    BuildFacts {
        base: revision(base(), None),
        target: revision(human(), Some("main")),
        merge_base: Some(base()),
        relation: ComparisonRelation::Ancestor,
        stats: BuildStats {
            files: 5,
            insertions: 5,
            deletions: 2,
            commits: 2,
        },
        commits: vec![
            BuildCommit {
                sha: agent(),
                short_sha: agent()[..7].into(),
                patch_id: Some("4".repeat(40)),
                author: "Jane Doe".into(),
                agent: BuildAgent::ClaudeCode,
                agent_model: Some("claude-opus-5-5".into()),
                agent_name: Some("Claude Opus 5.5".into()),
                what: "Moves and extends the module".into(),
                why: Some("The module grew past its old name.".into()),
                why_source: ExplanationSource::Commit,
                inferred_by: None,
                changes_inferred_by: Some("claude-sonnet-5-5".into()),
            },
            BuildCommit {
                sha: human(),
                short_sha: human()[..7].into(),
                patch_id: Some("5".repeat(40)),
                author: "Jane Doe".into(),
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
        files: vec![
            BuildFile {
                binary: true,
                touched_by: vec![agent()],
                ..BuildFile::new("img.bin", GitFileStatus::Added, None, None)
            },
            BuildFile {
                ignored: true,
                ..BuildFile::new("pnpm-lock.yaml", GitFileStatus::Added, Some(1), Some(0))
            },
            BuildFile {
                ignored: true,
                ..BuildFile::new("secrets/key.txt", GitFileStatus::Modified, Some(1), Some(1))
            },
            BuildFile {
                original_path: Some("src/old.ts".into()),
                entries: vec![
                    BuildFileEntry {
                        sha: Some(human()),
                        added: vec![LineRange::new(1, 1)],
                        deleted: vec![LineRange::new(1, 1)],
                        changes: None,
                    },
                    BuildFileEntry {
                        sha: Some(agent()),
                        added: vec![LineRange::new(11, 11)],
                        deleted: vec![],
                        changes: Some(vec![BuildChange {
                            what: "Adds a closing line".into(),
                            why: "Marks where the agent work ends.".into(),
                        }]),
                    },
                ],
                touched_by: vec![human(), agent()],
                ..BuildFile::new("src/module.ts", GitFileStatus::Renamed, Some(2), Some(1))
            },
        ],
    }
}

fn narrative() -> BuildNarrative {
    BuildNarrative {
        title: "Renames the module".into(),
        summary: "The module was renamed and extended.".into(),
        decisions: vec![BuildDecision {
            text: "Rename for clarity.".into(),
            commits: vec![],
        }],
        risks: vec!["One commit gives no reason.".into()],
        model: "claude-opus-5-5".into(),
    }
}

/// A temporary folder with an empty `repo` folder in it
fn repo() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    (tmp, repo)
}

fn write_full(
    repo: &Path,
    folder: &str,
    facts: &BuildFacts,
    narrative: &BuildNarrative,
    created: f64,
) -> Result<GeneratedBuild> {
    write_build(repo, folder, facts, narrative, GENERATOR, created)
}

fn write(repo: &Path) -> Result<GeneratedBuild> {
    write_full(repo, "agent", &make_facts(), &narrative(), created())
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

fn listdir(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(path)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn assert_err(result: Result<GeneratedBuild>, text: &str) {
    let err = result.unwrap_err();
    assert!(err.to_string().contains(text), "{err}");
}

// -- Ported --

#[test]
fn writes_build_and_index_the_same_way_every_time() {
    let (_tmp, repo) = repo();
    let build = write(&repo).unwrap();
    let (b, t, a) = (&base()[..7], &human()[..7], &agent()[..7]);
    assert_eq!(
        build.path,
        repo.join("agent").join(format!("{b}_{t}_build.md"))
    );
    assert_eq!(build.index_path, repo.join("agent").join("index.md"));
    assert_eq!(build.narrative, narrative());

    let text = read(&build.path);
    let contains = |needle: &str| assert!(text.contains(needle), "missing {needle:?}");
    contains("title: \"Renames the module\"");
    contains(&format!("  sha: {}\n  ref: null", base()));
    contains(&format!("  sha: {}\n  ref: \"main\"", human()));
    contains("build_model: \"claude-opus-5-5\"");
    contains(&format!("generator: \"{GENERATOR}\""));
    contains("agent: claude-code\n    agent_model: \"claude-opus-5-5\"\n    why_source: commit");
    contains("### src/module.ts (from src/old.ts) · R · +2 \u{2212}1");
    contains(&format!(
        "### `{a}` Moves and extends the module\n\n- claude-code (claude-opus-5-5) · source: commit\n- Why: The module grew past its old name."
    ));
    contains(&format!(
        "### `{t}` tweak\n\n- human (Jane Doe) · source: commit\n- Why: not recorded."
    ));
    contains(&format!(
        "- **L11** · `{a}` · Moves and extends the module\n  - What: Adds a closing line\n  - Why (inferred by claude-sonnet-5-5, not the author’s words): Marks where the agent work ends."
    ));
    contains(&format!("- **L1**, base **L1** · `{t}` · tweak\n"));
    contains("### img.bin · A · binary");
    contains("### pnpm-lock.yaml · A · +1 \u{2212}0\n\n- Left out");
    contains("### secrets/key.txt · M · +1 \u{2212}1\n\n- Left out");

    let index = read(&build.index_path);
    assert!(index.contains(&format!(
        "| [{b}_{t}]({b}_{t}_build.md) | {b} → main | 2026-10-08 | claude-opus-5-5 | Renames the module |"
    )));

    // Same facts, same answer, same clock: byte-identical; another clock
    // changes only `created`
    write(&repo).unwrap();
    assert_eq!(read(&build.path), text);
    write_full(
        &repo,
        "agent",
        &make_facts(),
        &narrative(),
        timestamp_utc(2026, 10, 9, 8, 0, 0.0),
    )
    .unwrap();
    assert_eq!(
        read(&build.path),
        text.replace(
            "created: 2026-10-08T12:00:00Z",
            "created: 2026-10-09T08:00:00Z"
        )
    );
}

// -- Edge cases --

#[test]
fn index_lists_every_build_and_tolerates_junk() {
    let (_tmp, repo) = repo();
    let folder = repo.join("docs").join("agent");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("0000000_1111111_build.md"),
        "---\ntitle: \"Older\"\ncreated: 2026-01-01T00:00:00Z\nbase:\n  sha: 0000000000\n  ref: null\ntarget:\n  sha: 1111111111\n  ref: \"v1\"\n---\n",
    )
    .unwrap();
    std::fs::write(folder.join("junk_build.md"), b"\xff\xfe not a build").unwrap();
    std::fs::create_dir(folder.join("dir_build.md")).unwrap();
    std::fs::write(folder.join("index.md"), "stale").unwrap();

    let build = write_full(&repo, "docs/agent", &make_facts(), &narrative(), created()).unwrap();
    let index = read(&build.index_path);
    assert_eq!(index, render_index(&read_build_headers(&folder).unwrap()));
    let rows: Vec<&str> = index.split('\n').collect();
    let rows = &rows[7..rows.len() - 1];
    let starts: Vec<&str> = rows
        .iter()
        .map(|row| row.split("](").next().unwrap())
        .collect();
    assert_eq!(
        starts,
        [
            format!("| [{}_{}", &base()[..7], &human()[..7]).as_str(),
            "| [0000000_1111111",
            "| [junk",
        ]
    );
    assert!(rows[1].contains("| 0000000 → v1 | 2026-01-01 |"));
    assert_eq!(rows[2], "| [junk](junk_build.md) | ? → ? |  |  |  |");
}

#[test]
fn creates_nested_folder() {
    let (_tmp, repo) = repo();
    let build = write_full(&repo, "a/b/c", &make_facts(), &narrative(), created()).unwrap();
    assert!(build.path.is_file());
    assert_eq!(
        build.path.parent().unwrap(),
        repo.join("a").join("b").join("c")
    );
}

#[test]
fn leaves_no_temp_files() {
    let (_tmp, repo) = repo();
    write(&repo).unwrap();
    write(&repo).unwrap();
    assert_eq!(
        listdir(&repo.join("agent")),
        [
            format!("{}_{}_build.md", &base()[..7], &human()[..7]),
            "index.md".to_string()
        ]
    );
}

#[test]
fn writes_utf8_with_lf() {
    let (_tmp, repo) = repo();
    let mut facts = make_facts();
    facts.target = revision(human(), Some("féat/\u{1f600}"));
    let narrative = BuildNarrative {
        title: "\"Quoted\" → tïtle\nwith a break".into(),
        summary: "S".into(),
        decisions: vec![],
        risks: vec![],
        model: "m".into(),
    };
    let build = write_full(&repo, "agent", &facts, &narrative, created()).unwrap();
    let data = std::fs::read(&build.path).unwrap();
    let text = String::from_utf8(data).unwrap();
    assert!(!text.contains('\r'));
    assert!(text.contains("  ref: \"féat/\u{1f600}\""));
    assert!(text.contains("\"\\\"Quoted\\\" → tïtle\\nwith a break\""));
}

// test_bytes_kept_through_surrogateescape_are_written_back and
// test_other_lone_surrogates_become_replacement_characters: a Rust String
// can hold neither surrogateescape'd bytes nor lone surrogates (git output
// is decoded lossily before it gets here), so there is nothing to port.

#[test]
fn refuses_folder_outside_repo() {
    let (_tmp, repo) = repo();
    for folder in ["../out", "/tmp/agent", ".."] {
        assert_err(
            write_full(&repo, folder, &make_facts(), &narrative(), created()),
            "inside the repository",
        );
    }
    assert!(!repo.parent().unwrap().join("out").exists());
}

#[cfg(unix)]
#[test]
fn refuses_symlink_out_of_repo() {
    let (tmp, repo) = repo();
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, repo.join("agent")).unwrap();
    assert_err(write(&repo), "inside the repository");
    assert!(listdir(&outside).is_empty());
}

#[test]
fn refuses_unsafe_build_names() {
    let (_tmp, repo) = repo();
    let mut facts = make_facts();
    facts.base = BuildRevision {
        sha: base(),
        short_sha: "../../x".into(),
        ref_name: None,
    };
    assert_err(
        write_full(&repo, "agent", &facts, &narrative(), created()),
        "Cannot name a build file",
    );
    assert!(!repo.join("agent").exists());
}

#[test]
fn build_folder_that_is_a_file() {
    let (_tmp, repo) = repo();
    std::fs::write(repo.join("agent"), "a file").unwrap();
    assert_err(write(&repo), "Could not create the build folder");
    assert_eq!(read(&repo.join("agent")), "a file");
}

#[test]
fn build_path_that_is_a_folder() {
    let (_tmp, repo) = repo();
    let name = format!("{}_{}_build.md", &base()[..7], &human()[..7]);
    std::fs::create_dir_all(repo.join("agent").join(&name)).unwrap();
    assert_err(write(&repo), "Could not write");
    // The temporary file is cleaned up
    assert_eq!(listdir(&repo.join("agent")), [name]);
}

#[cfg(unix)]
unsafe extern "C" {
    fn geteuid() -> u32;
    fn umask(mask: u32) -> u32;
}

#[cfg(unix)]
fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions
    unsafe { geteuid() == 0 }
}

#[cfg(unix)]
#[test]
fn read_only_folder() {
    use std::os::unix::fs::PermissionsExt;
    if is_root() {
        return;
    }
    let (_tmp, repo) = repo();
    let folder = repo.join("agent");
    std::fs::create_dir(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();
    let result = write(&repo);
    let left = listdir(&folder);
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_err(result, "Could not write");
    assert!(left.is_empty());
}

#[cfg(unix)]
#[test]
fn read_only_parent() {
    use std::os::unix::fs::PermissionsExt;
    if is_root() {
        return;
    }
    let (_tmp, repo) = repo();
    std::fs::set_permissions(&repo, std::fs::Permissions::from_mode(0o555)).unwrap();
    let result = write(&repo);
    std::fs::set_permissions(&repo, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_err(result, "Could not create the build folder");
}

#[cfg(unix)]
#[test]
fn write_text_atomic_replaces_and_keeps_umask_mode() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("f.md");
    std::fs::write(&path, "old").unwrap();
    write_text_atomic(&path, "new é").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), "new é".as_bytes());

    // SAFETY: umask has no preconditions; it is set back right away
    let mask = unsafe {
        let mask = umask(0o022);
        umask(mask);
        mask
    };
    let fresh = tmp.path().join("g.md");
    write_text_atomic(&fresh, "x").unwrap();
    let mode = std::fs::metadata(&fresh).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o666 & !mask);
}

#[test]
fn write_text_atomic_missing_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let err = write_text_atomic(&tmp.path().join("nope").join("f.md"), "x").unwrap_err();
    assert!(err.to_string().contains("Could not write"), "{err}");
}
