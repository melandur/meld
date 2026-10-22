//! Port of test/ailens/test_attribution.py

mod common;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{CommitOptions, Content, FixtureRepo, HUMAN, co_authored_by_claude, lines};
use meld_ailens::attribution::ComparisonAttributor;
use meld_ailens::comparison::resolve_comparison;
use meld_ailens::git::{GitOptions, GitRepository};
use meld_ailens::{
    CancelToken, ChangeGroup, Comparison, ComparisonRelation, Error, FileAttribution,
    GitFileChange, GitFileStatus, LayerDiffsUsed, LineOrigin, Revision, Side,
};

/// Each character of `text` as a line
fn char_lines(text: &str) -> String {
    text.chars().map(|c| format!("{c}\n")).collect()
}

fn never() -> CancelToken {
    CancelToken::new()
}

/// A commit revision by fixture label (or ref)
fn rev(f: &FixtureRepo, label: &str) -> Revision {
    Revision::commit(f.resolve(label))
}

fn old(path: &str, line: usize) -> LineOrigin {
    LineOrigin::new(path, Side::Old, line)
}

fn new(path: &str, line: usize) -> LineOrigin {
    LineOrigin::new(path, Side::New, line)
}

#[derive(Debug, PartialEq, Eq)]
struct Group {
    by: String,
    hunk: usize,
    added: Vec<String>,
    deleted: Vec<String>,
}

fn group(by: &str, added: &[&str], deleted: &[&str]) -> Group {
    group_in(by, added, deleted, 0)
}

fn group_in(by: &str, added: &[&str], deleted: &[&str], hunk: usize) -> Group {
    Group {
        by: by.into(),
        hunk,
        added: added.iter().map(|s| s.to_string()).collect(),
        deleted: deleted.iter().map(|s| s.to_string()).collect(),
    }
}

struct Setup {
    labels: HashMap<String, String>,
    comparison: Comparison,
    /// In the order the attributor lists them
    files: Vec<(String, FileAttribution)>,
}

impl Setup {
    fn new(fixture: &FixtureRepo, base: Revision, target: Revision) -> Self {
        let repo = fixture.open();
        let comparison = resolve_comparison(&repo, &base, &target, &never()).unwrap();
        let attributor = ComparisonAttributor::new(repo, comparison.clone());
        let files = attributor
            .changed_files(&never())
            .unwrap()
            .into_iter()
            .map(|change| {
                let attribution = attributor.attribute(&change, &never()).unwrap();
                (change.path, attribution)
            })
            .collect();
        Self {
            labels: fixture
                .shas
                .iter()
                .map(|(label, sha)| (sha.clone(), label.clone()))
                .collect(),
            comparison,
            files,
        }
    }

    fn paths(&self) -> Vec<&str> {
        self.files.iter().map(|(path, _)| path.as_str()).collect()
    }

    fn file(&self, path: &str) -> &FileAttribution {
        &self
            .files
            .iter()
            .find(|(p, _)| p == path)
            .unwrap_or_else(|| panic!("no file {path:?}"))
            .1
    }

    fn label(&self, sha: Option<&str>) -> String {
        match sha {
            None => "?".into(),
            Some(sha) => self
                .labels
                .get(sha)
                .cloned()
                .unwrap_or_else(|| sha.to_string()),
        }
    }

    /// Groups with SHAs replaced by fixture labels, ranges as start-end
    fn describe(&self, path: &str) -> Vec<Group> {
        let ranges = |group: &ChangeGroup, added: bool| -> Vec<String> {
            let ranges = if added { &group.added } else { &group.deleted };
            ranges
                .iter()
                .map(|r| format!("{}-{}", r.start, r.end))
                .collect()
        };
        self.file(path)
            .groups
            .iter()
            .map(|group| Group {
                by: match group.uncommitted {
                    Some(layer) => layer.as_str().to_string(),
                    None => self.label(group.sha.as_deref()),
                },
                hunk: group.hunk_index,
                added: ranges(group, true),
                deleted: ranges(group, false),
            })
            .collect()
    }
}

/// Merge `reference` without committing (conflicts allowed), write `files`
/// as the resolution and commit it
fn merge_by_hand(f: &mut FixtureRepo, reference: &str, files: Vec<(String, Content)>) -> String {
    let env = f.commit_env(HUMAN);
    // Conflicts make it fail
    let _ = f.git_full(
        &["merge", "--quiet", "--no-ff", "--no-commit", reference],
        Some(&env),
        None,
        None,
    );
    f.write_files(files);
    f.git(&["add", "--all"]);
    f.git_full(&["commit", "--quiet", "--no-edit"], Some(&env), None, None)
        .unwrap();
    f.git(&["rev-parse", "HEAD"]).trim().to_string()
}

// -- committed changes --

#[test]
fn credits_each_added_and_deleted_line_to_the_commit_that_made_it() {
    let mut f = FixtureRepo::new();
    f.commit(
        "Initial",
        files![("a.ts", lines(&["1", "2", "3", "4", "5", "6", "7"]))],
        Some("base"),
    );
    f.commit_with(
        "Agent work",
        files![("a.ts", lines(&["1", "2a", "3", "new", "4", "5", "6", "7a"]))],
        CommitOptions::label("agent").with_trailer(co_authored_by_claude("Opus 5.5")),
    );
    f.commit(
        "Human work",
        files![("a.ts", lines(&["1", "2a", "3", "new", "4", "6", "7b"]))],
        Some("human"),
    );

    let setup = Setup::new(&f, rev(&f, "base"), rev(&f, "human"));

    assert_eq!(
        setup.describe("a.ts"),
        [
            // 2 -> 2a, all by agent
            group("agent", &["2-2"], &["2-2"]),
            // new line
            group("agent", &["4-4"], &[]),
            // 5 deleted by human; 6 survives
            group("human", &[], &["5-5"]),
            // 7 -> 7a by agent, then 7a -> 7b by human: the base line went
            // in agent, the final text came from human
            group("agent", &[], &["7-7"]),
            group("human", &["7-7"], &[]),
        ]
    );
    assert_eq!(
        setup.file("a.ts").touched_by,
        [f.sha("human"), f.sha("agent")]
    );
    assert_eq!(setup.file("a.ts").layers, None);
}

#[test]
fn follows_renames_crediting_edits_before_and_after_the_rename() {
    let mut f = FixtureRepo::new();
    let original = ["l1", "l2", "l3", "l4", "l5", "l6", "l7", "l8"];
    f.commit(
        "Initial",
        files![("old.txt", lines(&original))],
        Some("base"),
    );
    let mut edited = vec!["L1"];
    edited.extend(&original[1..]);
    f.commit(
        "Edits l1",
        files![("old.txt", lines(&edited))],
        Some("edit"),
    );
    f.commit_with(
        "Renames",
        vec![],
        CommitOptions::label("rename").with_rename("old.txt", "new.txt"),
    );
    f.commit(
        "Deletes l4",
        files![(
            "new.txt",
            lines(&["L1", "l2", "l3", "l5", "l6", "l7", "l8"])
        )],
        Some("delete"),
    );
    f.commit(
        "Replaces l8",
        files![("new.txt", lines(&["L1", "l2", "l3", "l5", "l6", "l7", "X"]))],
        Some("replace"),
    );

    let setup = Setup::new(&f, rev(&f, "base"), rev(&f, "replace"));

    let change = &setup.file("new.txt").change;
    assert_eq!(
        (change.status, change.original_path.as_deref()),
        (GitFileStatus::Renamed, Some("old.txt"))
    );
    assert_eq!(
        setup.describe("new.txt"),
        [
            group("edit", &["1-1"], &["1-1"]),
            // The 3-line contexts overlap, so git puts all three in one hunk
            group("delete", &[], &["4-4"]),
            group("replace", &["7-7"], &["8-8"]),
        ]
    );
}

#[test]
fn diverged_branches_credit_only_the_target_side() {
    let mut f = FixtureRepo::new();
    f.commit(
        "Initial",
        files![("a.ts", char_lines("123456789"))],
        Some("base"),
    );
    f.branch("feature", "HEAD");
    f.commit(
        "Feature",
        files![("a.ts", format!("1\ntwo\n{}", char_lines("3456789")))],
        Some("feature"),
    );
    f.checkout("main");
    f.commit(
        "Main",
        files![
            ("a.ts", format!("{}nine\n", char_lines("12345678"))),
            ("m.ts", "m\n")
        ],
        Some("main"),
    );

    let setup = Setup::new(&f, rev(&f, "main"), rev(&f, "feature"));

    assert_eq!(
        (setup.comparison.relation, setup.comparison.from_sha.clone()),
        (ComparisonRelation::Diverged, f.sha("base"))
    );
    assert_eq!(setup.paths(), ["a.ts"]);
    assert_eq!(
        setup.describe("a.ts"),
        [group("feature", &["2-2"], &["2-2"])]
    );
}

#[test]
fn deletions_through_a_merge_go_to_the_commit_on_the_merged_side() {
    let mut f = FixtureRepo::new();
    f.commit(
        "Initial",
        files![("f.txt", char_lines("abcdefghij"))],
        Some("base"),
    );
    f.branch("side", "HEAD");
    f.commit(
        "Side deletes b",
        files![("f.txt", char_lines("acdefghij"))],
        Some("side"),
    );
    f.checkout("main");
    f.commit(
        "Main deletes i",
        files![("f.txt", char_lines("abcdefghj"))],
        Some("main"),
    );
    f.merge("side", "Merges side", Some("merge"), None);

    let setup = Setup::new(&f, rev(&f, "base"), rev(&f, "merge"));

    assert_eq!(
        setup.describe("f.txt"),
        [
            group("side", &[], &["2-2"]),
            // The two hunks' contexts touch, so git joins them
            group("main", &[], &["9-9"]),
        ]
    );
}

#[test]
fn a_deletion_made_by_the_merge_itself_goes_to_the_merge() {
    let mut f = FixtureRepo::new();
    f.commit(
        "Initial",
        files![("f.txt", char_lines("abcdefghij"))],
        Some("base"),
    );
    f.branch("side", "HEAD");
    f.commit(
        "Side deletes b",
        files![("f.txt", char_lines("acdefghij"))],
        Some("side"),
    );
    f.checkout("main");
    f.commit(
        "Main adds k",
        files![("f.txt", char_lines("abcdefghijk"))],
        Some("main"),
    );
    // A merge that also deletes f, which neither parent did
    let merge = merge_by_hand(&mut f, "side", files![("f.txt", char_lines("acdeghijk"))]);

    let setup = Setup::new(&f, rev(&f, "base"), Revision::commit(merge.clone()));

    assert_eq!(
        setup.describe("f.txt"),
        [
            group("side", &[], &["2-2"]),
            group(&merge, &[], &["6-6"]),
            group("main", &["9-9"], &[]),
        ]
    );
}

#[test]
fn a_line_dropped_while_resolving_a_conflict_goes_to_the_merge() {
    let mut f = FixtureRepo::new();
    f.commit(
        "Initial",
        files![("f.txt", char_lines("abcdefghij"))],
        Some("base"),
    );
    f.branch("side", "HEAD");
    f.commit(
        "Side",
        files![("f.txt", format!("{}J-side\n", char_lines("abcdefghi")))],
        Some("side"),
    );
    f.checkout("main");
    f.commit(
        "Main",
        files![("f.txt", format!("{}J-main\n", char_lines("abcdefghi")))],
        Some("main"),
    );
    // Takes main's side of the conflict and drops b as well
    let merge = merge_by_hand(
        &mut f,
        "side",
        files![("f.txt", format!("{}J-main\n", char_lines("acdefghi")))],
    );

    let setup = Setup::new(&f, rev(&f, "base"), Revision::commit(merge.clone()));

    assert_eq!(
        setup.describe("f.txt"),
        [
            group_in(&merge, &[], &["2-2"], 0),
            // Both parents deleted j; the first of them in git's order is
            // credited
            group_in("main", &["9-9"], &["10-10"], 1),
        ]
    );
}

#[test]
fn credits_added_and_deleted_files() {
    let mut f = FixtureRepo::new();
    f.commit(
        "Initial",
        files![("gone.ts", lines(&["x", "y"]))],
        Some("base"),
    );
    f.commit(
        "Adds new.ts",
        files![("new.ts", lines(&["1", "2", "3"]))],
        Some("add"),
    );
    f.commit(
        "Edits new.ts",
        files![("new.ts", lines(&["1", "two", "3", "4"]))],
        Some("edit"),
    );
    f.commit(
        "Deletes gone.ts",
        files![("gone.ts", None::<&str>)],
        Some("remove"),
    );

    let setup = Setup::new(&f, rev(&f, "base"), rev(&f, "remove"));

    assert_eq!(
        setup.describe("new.ts"),
        [
            group("add", &["1-1", "3-3"], &[]),
            group("edit", &["2-2", "4-4"], &[])
        ]
    );
    assert_eq!(setup.describe("gone.ts"), [group("remove", &[], &["1-2"])]);
}

#[test]
fn binary_files_get_only_the_commits_that_touched_them() {
    let mut f = FixtureRepo::new();
    f.commit("Initial", files![("img.bin", b"a\0b")], Some("base"));
    f.commit(
        "Changes image",
        files![("img.bin", b"a\0c")],
        Some("change"),
    );
    f.commit("Unrelated", files![("other.txt", "o\n")], Some("other"));

    let setup = Setup::new(&f, rev(&f, "base"), rev(&f, "other"));

    let attribution = setup.file("img.bin");
    assert!(attribution.diff.as_ref().unwrap().binary);
    assert!(attribution.groups.is_empty());
    assert_eq!(attribution.touched_by, [f.sha("change")]);
}

#[test]
fn mode_changes_have_no_groups() {
    use std::os::unix::fs::PermissionsExt;
    let mut f = FixtureRepo::new();
    f.commit("Initial", files![("run.sh", "echo\n")], Some("base"));
    std::fs::set_permissions(
        f.path.join("run.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    f.commit("Makes it executable", vec![], Some("chmod"));

    let setup = Setup::new(&f, rev(&f, "base"), rev(&f, "chmod"));

    let attribution = setup.file("run.sh");
    assert!(attribution.diff.as_ref().unwrap().hunks.is_empty());
    assert!(attribution.groups.is_empty());
    assert_eq!(attribution.touched_by, [f.sha("chmod")]);
}

#[test]
fn credits_nothing_when_the_target_is_an_ancestor_of_the_base() {
    let mut f = FixtureRepo::new();
    f.commit("Initial", files![("a.ts", lines(&["1", "2"]))], Some("old"));
    f.commit(
        "Change",
        files![("a.ts", lines(&["1", "two"]))],
        Some("new"),
    );

    let setup = Setup::new(&f, rev(&f, "new"), rev(&f, "old"));

    assert_eq!(setup.comparison.relation, ComparisonRelation::Reversed);
    assert_eq!(setup.describe("a.ts"), [group("?", &["2-2"], &["2-2"])]);
    assert!(setup.file("a.ts").touched_by.is_empty());
}

#[test]
fn unrelated_histories_credit_the_targets_commits_root_included() {
    let mut f = FixtureRepo::new();
    f.commit(
        "Initial",
        files![("a.ts", lines(&["1", "2", "3"]))],
        Some("base"),
    );
    f.git(&["checkout", "--quiet", "--orphan", "other"]);
    f.git(&["rm", "--quiet", "-rf", "."]);
    f.commit(
        "Other root",
        files![("a.ts", lines(&["1", "x", "3"]))],
        Some("root"),
    );
    f.commit(
        "Other edit",
        files![("a.ts", lines(&["1", "x", "3", "4"]))],
        Some("edit"),
    );

    let setup = Setup::new(&f, rev(&f, "base"), rev(&f, "edit"));

    assert_eq!(setup.comparison.relation, ComparisonRelation::Unrelated);
    // Deleted lines can't be traced without common history
    assert_eq!(
        setup.describe("a.ts"),
        [
            group("?", &[], &["2-2"]),
            group("root", &["2-2"], &[]),
            group("edit", &["4-4"], &[])
        ]
    );
    assert_eq!(
        setup.file("a.ts").touched_by,
        [f.sha("edit"), f.sha("root")]
    );
}

#[test]
fn places_each_line_in_the_crediting_commits_own_diff_across_a_rename() {
    let mut f = FixtureRepo::new();
    f.commit(
        "Initial",
        files![("a.ts", "l1\nl2\nl3\nl4\nl5\n")],
        Some("base"),
    );
    f.commit(
        "Insert",
        files![("a.ts", "l1\nx\nl2\nl3\nl4\nl5\n")],
        Some("insert"),
    );
    f.commit(
        "Remove",
        files![("a.ts", "l1\nx\nl2\nl3\nl5\n")],
        Some("remove"),
    );
    f.commit_with(
        "Rename",
        files![("b.ts", "l1\nx\nl2\nl3\nl5\nend\n")],
        CommitOptions::label("rename").with_rename("a.ts", "b.ts"),
    );

    let setup = Setup::new(&f, rev(&f, "base"), rev(&f, "rename"));

    let described: Vec<(String, Vec<LineOrigin>)> = setup
        .file("b.ts")
        .groups
        .iter()
        .map(|g| (setup.label(g.sha.as_deref()), g.origins.clone()))
        .collect();
    assert_eq!(
        described,
        [
            ("insert".to_string(), vec![new("a.ts", 2)]),
            // Line 5 of the file as remove's parent had it
            ("remove".to_string(), vec![old("a.ts", 5)]),
            ("rename".to_string(), vec![new("b.ts", 6)]),
        ]
    );
}

#[test]
fn crlf_lines_and_missing_final_newlines() {
    let mut f = FixtureRepo::new();
    f.commit("Initial", files![("w.txt", "a\r\nb\r\nc")], Some("base"));
    f.commit("Edits b", files![("w.txt", "a\r\nB\r\nc")], Some("b"));
    f.commit("Edits c", files![("w.txt", "a\r\nB\r\nC")], Some("c"));

    let setup = Setup::new(&f, rev(&f, "base"), rev(&f, "c"));

    assert_eq!(
        setup.describe("w.txt"),
        [
            group("b", &["2-2"], &["2-2"]),
            group("c", &["3-3"], &["3-3"])
        ]
    );
}

#[test]
fn repeated_lines() {
    let mut f = FixtureRepo::new();
    f.commit(
        "Initial",
        files![("r.txt", lines(&["x", "x", "x", "y"]))],
        Some("base"),
    );
    f.commit(
        "Adds an x",
        files![("r.txt", lines(&["x", "x", "x", "x", "y"]))],
        Some("more"),
    );
    f.commit(
        "Removes y",
        files![("r.txt", lines(&["x", "x", "x", "x"]))],
        Some("less"),
    );

    let setup = Setup::new(&f, rev(&f, "base"), rev(&f, "less"));

    let attribution = setup.file("r.txt");
    let count = |pick: fn(&ChangeGroup) -> &Vec<meld_ailens::LineRange>| -> usize {
        attribution
            .groups
            .iter()
            .flat_map(&pick)
            .map(|r| r.end - r.start + 1)
            .sum()
    };
    assert_eq!((count(|g| &g.added), count(|g| &g.deleted)), (1, 1));
    let labels: HashSet<String> = attribution
        .groups
        .iter()
        .map(|g| setup.label(g.sha.as_deref()))
        .collect();
    let allowed: HashSet<String> = ["more", "less", "?"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert!(labels.is_subset(&allowed), "{labels:?}");
    assert!(
        attribution
            .groups
            .iter()
            .any(|g| g.sha.as_deref() == Some(&f.sha("less")) && !g.deleted.is_empty())
    );
}

#[test]
fn odd_paths() {
    let mut f = FixtureRepo::new();
    let names = [
        "a b.txt",
        "quo\"te.txt",
        "日本/ü.txt",
        "new\nline.txt",
        "glob[1]*.txt",
    ];
    let files = |content: &str| -> Vec<(String, Content)> {
        names
            .iter()
            .map(|name| (name.to_string(), Content::from(content)))
            .collect()
    };
    f.commit("Initial", files(&lines(&["1", "2"])), Some("base"));
    f.commit("Edits", files(&lines(&["1", "two"])), Some("edit"));

    let setup = Setup::new(&f, rev(&f, "base"), rev(&f, "edit"));

    for name in names {
        assert_eq!(
            setup.describe(name),
            [group("edit", &["2-2"], &["2-2"])],
            "{name:?}"
        );
        assert_eq!(setup.file(name).touched_by, [f.sha("edit")]);
    }
}

#[test]
fn cancelled_attribution() {
    let mut f = FixtureRepo::new();
    f.commit(
        "Initial",
        files![("a.ts", lines(&["1", "2"]))],
        Some("base"),
    );
    f.commit(
        "Change",
        files![("a.ts", lines(&["1", "two"]))],
        Some("change"),
    );
    let repo = f.open();
    let comparison =
        resolve_comparison(&repo, &rev(&f, "base"), &rev(&f, "change"), &never()).unwrap();
    let attributor = ComparisonAttributor::new(repo, comparison);
    let changes = attributor.changed_files(&never()).unwrap();
    assert_eq!(changes.len(), 1);
    let cancel = CancelToken::new();
    cancel.cancel();

    assert!(matches!(
        attributor.attribute(&changes[0], &cancel),
        Err(Error::Cancelled)
    ));
}

/// A `git` that counts its calls in `<folder>/count` and, while
/// `<folder>/slow` exists, takes long on its third call
fn counting_git(folder: &Path) -> String {
    let script = folder.join("git");
    let dir = folder.display();
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nn=$(cat '{dir}/count' 2>/dev/null || echo 0)\nn=$((n+1))\necho $n > '{dir}/count'\n\
             if [ -e '{dir}/slow' ] && [ $n -eq 3 ]; then sleep 30; fi\nexec git \"$@\"\n"
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script.display().to_string()
}

#[test]
fn cancelled_midway_leaves_the_attributor_usable() {
    // Python overrides GitRepository.run to cancel on the third git call; here
    // a wrapper git makes the third call slow and the token is cancelled
    // while it runs
    let mut f = FixtureRepo::new();
    f.commit(
        "Initial",
        files![("a.ts", lines(&["1", "2", "3"]))],
        Some("base"),
    );
    f.commit(
        "Change",
        files![("a.ts", lines(&["1", "two"]))],
        Some("change"),
    );
    let scripts = tempfile::tempdir().unwrap();
    let options = GitOptions {
        git_path: counting_git(scripts.path()),
        env: Some(f.env.clone()),
    };
    let repo = GitRepository::new(&f.path, &options);
    let comparison =
        resolve_comparison(&repo, &rev(&f, "base"), &rev(&f, "change"), &never()).unwrap();
    let attributor = ComparisonAttributor::new(repo, comparison);
    let changes = attributor.changed_files(&never()).unwrap();
    assert_eq!(changes.len(), 1);

    let count = scripts.path().join("count");
    std::fs::write(&count, "0\n").unwrap();
    std::fs::write(scripts.path().join("slow"), "").unwrap();
    let token = CancelToken::new();
    let canceller = {
        let token = token.clone();
        let count = count.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            while Instant::now() < deadline {
                let calls: u32 = std::fs::read_to_string(&count)
                    .ok()
                    .and_then(|text| text.trim().parse().ok())
                    .unwrap_or(0);
                if calls >= 3 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            token.cancel();
        })
    };
    let started = Instant::now();
    assert!(matches!(
        attributor.attribute(&changes[0], &token),
        Err(Error::Cancelled)
    ));
    assert!(started.elapsed() < Duration::from_secs(20));
    canceller.join().unwrap();

    std::fs::remove_file(scripts.path().join("slow")).unwrap();
    let attribution = attributor.attribute(&changes[0], &never()).unwrap();
    let groups: Vec<(Option<String>, usize, usize)> = attribution
        .groups
        .iter()
        .map(|g| (g.sha.clone(), g.added.len(), g.deleted.len()))
        .collect();
    assert_eq!(groups, [(Some(f.sha("change")), 1, 1)]);
}

#[test]
fn one_attributor_from_several_threads() {
    let mut f = FixtureRepo::new();
    let files = |content: &str| -> Vec<(String, Content)> {
        (0..6)
            .map(|n| (format!("{n}.txt"), Content::from(content)))
            .collect()
    };
    f.commit("Initial", files(&lines(&["1", "2"])), Some("base"));
    f.commit("Change", files(&lines(&["1", "two"])), Some("c"));
    let repo = f.open();
    let comparison = resolve_comparison(&repo, &rev(&f, "base"), &rev(&f, "c"), &never()).unwrap();
    let attributor = Arc::new(ComparisonAttributor::new(repo, comparison));
    let results: Arc<Mutex<HashMap<String, Vec<ChangeGroup>>>> = Arc::default();

    let threads: Vec<_> = attributor
        .changed_files(&never())
        .unwrap()
        .into_iter()
        .map(|change: GitFileChange| {
            let attributor = attributor.clone();
            let results = results.clone();
            std::thread::spawn(move || {
                let groups = attributor.attribute(&change, &never()).unwrap().groups;
                results.lock().unwrap().insert(change.path, groups);
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }

    let results = results.lock().unwrap();
    assert_eq!(results.len(), 6);
    for groups in results.values() {
        let shas: Vec<Option<String>> = groups.iter().map(|g| g.sha.clone()).collect();
        assert_eq!(shas, [Some(f.sha("c"))]);
    }
}

// -- uncommitted changes --

/// A committed edit, then staged and unstaged edits, and an untracked file
fn build_layered(f: &mut FixtureRepo) {
    let numbers: Vec<String> = (1..13).map(|n| n.to_string()).collect();
    let numbers: Vec<&str> = numbers.iter().map(String::as_str).collect();
    f.commit("Initial", files![("a.ts", lines(&numbers))], Some("base"));
    let mut agent = vec!["1", "2a"];
    agent.extend(&numbers[2..]);
    f.commit("Agent work", files![("a.ts", lines(&agent))], Some("agent"));
    // Staged: 3 -> 3s; 6 deleted
    f.write_files(files![(
        "a.ts",
        lines(&["1", "2a", "3s", "4", "5", "7", "8", "9", "10", "11", "12"])
    )]);
    f.stage(&[]);
    // Unstaged: 10 -> 10u; a line added at the end; an untracked file
    f.write_files(files![
        (
            "a.ts",
            lines(&[
                "1", "2a", "3s", "4", "5", "7", "8", "9", "10u", "11", "12", "end"
            ])
        ),
        ("u.ts", lines(&["x", "y"])),
    ]);
}

#[test]
fn credits_working_tree_lines_to_their_layer_or_the_commit_below() {
    let mut f = FixtureRepo::new();
    build_layered(&mut f);

    let setup = Setup::new(&f, rev(&f, "base"), Revision::working_tree(None));

    assert_eq!(setup.paths(), ["a.ts", "u.ts"]);
    assert_eq!(
        setup.describe("a.ts"),
        [
            group("agent", &["2-2"], &["2-2"]),
            group("staged", &["3-3"], &["3-3"]),
            group("staged", &[], &["6-6"]),
            group("unstaged", &["9-9"], &["10-10"]),
            group("unstaged", &["12-12"], &[]),
        ]
    );
    assert_eq!(setup.describe("u.ts"), [group("unstaged", &["1-2"], &[])]);
    assert_eq!(setup.file("u.ts").change, GitFileChange::untracked("u.ts"));
    assert_eq!(setup.file("a.ts").touched_by, [f.sha("agent")]);
}

#[test]
fn places_uncommitted_lines_in_their_layers_own_diff() {
    let mut f = FixtureRepo::new();
    build_layered(&mut f);

    let setup = Setup::new(&f, rev(&f, "base"), Revision::working_tree(None));
    let file = setup.file("a.ts");

    let origins: Vec<Vec<LineOrigin>> = file.groups.iter().map(|g| g.origins.clone()).collect();
    assert_eq!(
        origins,
        [
            // The commit's own diff
            vec![old("a.ts", 2), new("a.ts", 2)],
            // The staged diff: HEAD line 3 and index line 3
            vec![old("a.ts", 3), new("a.ts", 3)],
            vec![old("a.ts", 6)],
            // The unstaged diff: index line 9 and working tree line 9
            vec![old("a.ts", 9), new("a.ts", 9)],
            vec![new("a.ts", 12)],
        ]
    );
    let layers = file.layers.as_ref().unwrap();
    let paths = |diffs: &[meld_ailens::GitFileDiff]| -> Vec<Option<String>> {
        diffs.iter().map(|d| d.new_path.clone()).collect()
    };
    assert_eq!(paths(&layers.staged), [Some("a.ts".to_string())]);
    assert_eq!(paths(&layers.unstaged), [Some("a.ts".to_string())]);
    let untracked = setup.file("u.ts");
    assert_eq!(
        untracked.layers,
        Some(LayerDiffsUsed {
            staged: vec![],
            unstaged: vec![untracked.diff.clone().unwrap()],
        })
    );
}

#[test]
fn compares_with_the_index_without_unstaged_or_untracked_changes() {
    let mut f = FixtureRepo::new();
    build_layered(&mut f);

    let setup = Setup::new(&f, rev(&f, "base"), Revision::index(None));

    assert_eq!(setup.paths(), ["a.ts"]);
    assert_eq!(
        setup.describe("a.ts"),
        [
            group("agent", &["2-2"], &["2-2"]),
            group("staged", &["3-3"], &["3-3"]),
            group("staged", &[], &["6-6"]),
        ]
    );
    // Both layers are there; the unstaged one is empty
    assert!(
        setup
            .file("a.ts")
            .layers
            .as_ref()
            .unwrap()
            .unstaged
            .is_empty()
    );
}

#[test]
fn compares_head_with_the_working_tree() {
    let mut f = FixtureRepo::new();
    build_layered(&mut f);

    let setup = Setup::new(&f, rev(&f, "agent"), Revision::working_tree(None));

    assert_eq!(setup.comparison.relation, ComparisonRelation::Same);
    assert_eq!(
        setup.describe("a.ts"),
        [
            group("staged", &["3-3"], &["3-3"]),
            group("staged", &[], &["6-6"]),
            group("unstaged", &["9-9"], &["10-10"]),
            group("unstaged", &["12-12"], &[]),
        ]
    );
    assert!(setup.file("a.ts").touched_by.is_empty());
}

#[test]
fn index_to_working_tree_is_all_unstaged() {
    let mut f = FixtureRepo::new();
    build_layered(&mut f);

    let setup = Setup::new(&f, Revision::index(None), Revision::working_tree(None));

    assert_eq!(setup.paths(), ["a.ts", "u.ts"]);
    assert_eq!(
        setup.describe("a.ts"),
        [
            group("unstaged", &["9-9"], &["9-9"]),
            group("unstaged", &["12-12"], &[])
        ]
    );
    assert_eq!(setup.describe("u.ts"), [group("unstaged", &["1-2"], &[])]);
    let file = setup.file("a.ts");
    let origins: Vec<Vec<LineOrigin>> = file.groups.iter().map(|g| g.origins.clone()).collect();
    assert_eq!(
        origins,
        [vec![old("a.ts", 9), new("a.ts", 9)], vec![new("a.ts", 12)]]
    );
    assert_eq!(
        file.layers,
        Some(LayerDiffsUsed {
            staged: vec![],
            unstaged: vec![file.diff.clone().unwrap()],
        })
    );
    assert!(file.touched_by.is_empty());
}

#[test]
fn index_to_working_tree_with_unstaged_deletions() {
    let mut f = FixtureRepo::new();
    f.commit(
        "Initial",
        files![("a.ts", lines(&["1", "2", "3"])), ("gone.ts", "g\n")],
        Some("base"),
    );
    f.write_files(files![
        ("a.ts", lines(&["1", "3"])),
        ("gone.ts", None::<&str>)
    ]);

    let setup = Setup::new(&f, Revision::index(None), Revision::working_tree(None));

    assert_eq!(setup.describe("a.ts"), [group("unstaged", &[], &["2-2"])]);
    assert_eq!(
        setup.describe("gone.ts"),
        [group("unstaged", &[], &["1-1"])]
    );
}

#[test]
fn index_to_working_tree_on_an_unborn_branch() {
    let f = FixtureRepo::new();
    f.write_files(files![("a.txt", lines(&["staged"]))]);
    f.stage(&[]);
    f.write_files(files![
        ("a.txt", lines(&["staged", "unstaged"])),
        ("u.txt", "u\n")
    ]);

    let setup = Setup::new(&f, Revision::index(None), Revision::working_tree(None));

    assert_eq!(setup.describe("a.txt"), [group("unstaged", &["2-2"], &[])]);
    assert_eq!(setup.describe("u.txt"), [group("unstaged", &["1-1"], &[])]);
}

#[test]
fn follows_a_staged_rename_back_to_the_commits_before_it() {
    let mut f = FixtureRepo::new();
    f.commit(
        "Initial",
        files![("a.ts", lines(&["1", "2", "3", "4", "5"]))],
        Some("base"),
    );
    f.commit(
        "Edit",
        files![("a.ts", lines(&["1", "2e", "3", "4", "5"]))],
        Some("edit"),
    );
    f.git(&["mv", "a.ts", "b.ts"]);
    f.write_files(files![("b.ts", lines(&["1", "2e", "3", "4", "5u"]))]);

    let setup = Setup::new(&f, rev(&f, "base"), Revision::working_tree(None));

    let change = &setup.file("b.ts").change;
    assert_eq!(
        (change.status, change.original_path.as_deref()),
        (GitFileStatus::Renamed, Some("a.ts"))
    );
    assert_eq!(
        setup.describe("b.ts"),
        [
            group("edit", &["2-2"], &["2-2"]),
            group("unstaged", &["5-5"], &["5-5"])
        ]
    );
    assert_eq!(
        setup.file("b.ts").groups[0].origins,
        [old("a.ts", 2), new("a.ts", 2)]
    );
}

#[test]
fn credits_deleted_files_to_the_layer_that_deleted_them() {
    let mut f = FixtureRepo::new();
    f.commit(
        "Initial",
        files![("s.ts", "s\n"), ("u.ts", "u\n")],
        Some("base"),
    );
    f.write_files(files![("s.ts", None::<&str>), ("u.ts", None::<&str>)]);
    f.stage(&["s.ts"]);

    let setup = Setup::new(&f, rev(&f, "base"), Revision::working_tree(None));

    assert_eq!(setup.describe("s.ts"), [group("staged", &[], &["1-1"])]);
    assert_eq!(setup.describe("u.ts"), [group("unstaged", &[], &["1-1"])]);
}

#[test]
fn uncommitted_binary_and_empty_files() {
    let mut f = FixtureRepo::new();
    f.commit("Initial", files![("img.bin", b"a\0b")], Some("base"));
    f.write_files(files![
        ("img.bin", b"a\0c"),
        ("new.bin", b"\0\0"),
        ("empty.txt", "")
    ]);

    let setup = Setup::new(&f, rev(&f, "base"), Revision::working_tree(None));

    for path in ["img.bin", "new.bin"] {
        assert!(setup.file(path).diff.as_ref().unwrap().binary, "{path}");
        assert!(setup.file(path).groups.is_empty());
    }
    assert!(
        setup
            .file("empty.txt")
            .diff
            .as_ref()
            .unwrap()
            .hunks
            .is_empty()
    );
    assert!(setup.file("empty.txt").groups.is_empty());
}

#[test]
fn uncommitted_changes_in_another_worktree() {
    let mut f = FixtureRepo::new();
    let eight = ["1", "2", "3", "4", "5", "6", "7", "8"];
    f.commit("Initial", files![("a.ts", lines(&eight))], Some("base"));
    let other = f.worktree(&f.tmp_path().join("other"), "other", "HEAD");
    f.commit_with(
        "There",
        files![("a.ts", lines(&["1", "2t", "3", "4", "5", "6", "7", "8"]))],
        CommitOptions::label("there").in_worktree(&other),
    );
    f.write_files_in(
        &other,
        files![("a.ts", lines(&["1", "2t", "3", "4s", "5", "6", "7", "8"]))],
    );
    f.git_in(&other, &["add", "a.ts"]);
    f.write_files_in(
        &other,
        files![("a.ts", lines(&["1", "2t", "3", "4s", "5", "6", "7", "8u"]))],
    );
    // The repository's own worktree has other uncommitted changes
    f.write_files(files![("a.ts", lines(&["mine"]))]);
    f.stage(&[]);

    let setup = Setup::new(
        &f,
        rev(&f, "base"),
        Revision::working_tree(Some(other.clone())),
    );

    assert_eq!(setup.comparison.worktree_path, other);
    assert_eq!(
        setup.describe("a.ts"),
        [
            group("there", &["2-2"], &["2-2"]),
            group("staged", &["4-4"], &["4-4"]),
            group("unstaged", &["8-8"], &["8-8"]),
        ]
    );

    let staged = Setup::new(&f, rev(&f, "base"), Revision::index(Some(other.clone())));
    assert_eq!(
        staged.describe("a.ts"),
        [
            group("there", &["2-2"], &["2-2"]),
            group("staged", &["4-4"], &["4-4"])
        ]
    );
    let unstaged = Setup::new(
        &f,
        Revision::index(Some(other.clone())),
        Revision::working_tree(Some(other)),
    );
    assert_eq!(
        unstaged.describe("a.ts"),
        [group("unstaged", &["8-8"], &["8-8"])]
    );
}

#[test]
fn uncommitted_target_with_a_detached_head() {
    let mut f = FixtureRepo::new();
    f.commit(
        "Initial",
        files![("a.ts", lines(&["1", "2"]))],
        Some("base"),
    );
    f.commit("Edit", files![("a.ts", lines(&["1", "two"]))], Some("edit"));
    f.git(&["checkout", "--quiet", "--detach"]);
    f.write_files(files![("a.ts", lines(&["1", "two", "three"]))]);

    let setup = Setup::new(&f, rev(&f, "base"), Revision::working_tree(None));

    assert_eq!(
        setup.describe("a.ts"),
        [
            group("edit", &["2-2"], &["2-2"]),
            group("unstaged", &["3-3"], &[])
        ]
    );
}

#[test]
fn uncommitted_target_after_a_merge() {
    let mut f = FixtureRepo::new();
    f.commit(
        "Initial",
        files![("f.txt", char_lines("abcdefghij"))],
        Some("base"),
    );
    f.branch("side", "HEAD");
    f.commit(
        "Side deletes b",
        files![("f.txt", char_lines("acdefghij"))],
        Some("side"),
    );
    f.checkout("main");
    f.commit(
        "Main deletes i",
        files![("f.txt", char_lines("abcdefghj"))],
        Some("main"),
    );
    f.merge("side", "Merges side", Some("merge"), None);
    f.write_files(files![("f.txt", format!("{}J\n", char_lines("acdefgh")))]);

    let setup = Setup::new(&f, rev(&f, "base"), Revision::working_tree(None));

    assert_eq!(
        setup.describe("f.txt"),
        [
            group("side", &[], &["2-2"]),
            group("main", &[], &["9-9"]),
            group("unstaged", &["8-8"], &["10-10"]),
        ]
    );
}

#[test]
fn symlinks_in_the_working_tree() {
    let mut f = FixtureRepo::new();
    f.commit("Initial", files![("a.ts", "a\n")], Some("base"));
    std::os::unix::fs::symlink("a.ts", f.path.join("link")).unwrap();

    let setup = Setup::new(&f, rev(&f, "base"), Revision::working_tree(None));

    assert_eq!(setup.describe("link"), [group("unstaged", &["1-1"], &[])]);
}
