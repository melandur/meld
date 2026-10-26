//! Port of test/ailens/test_comparison.py

mod common;

use std::path::PathBuf;

use common::{CommitOptions, Content, FixtureRepo};
use meld_ailens::comparison::{
    changed_files, changed_files_with_stats, file_diff, resolve_comparison,
};
use meld_ailens::git::GitRepository;
use meld_ailens::parsers::GitFileStats;
use meld_ailens::{
    CancelToken, Comparison, ComparisonRelation, DiffLineKind, Error, GitDiffLine, GitFileChange,
    GitFileDiff, GitFileStatus, Revision, RevisionKind,
};

fn c() -> CancelToken {
    CancelToken::new()
}

fn commit(name: &str) -> Revision {
    Revision::commit(name)
}

fn index() -> Revision {
    Revision::index(None)
}

fn working_tree() -> Revision {
    Revision::working_tree(None)
}

fn resolve(repo: &GitRepository, base: Revision, target: Revision) -> Comparison {
    resolve_comparison(repo, &base, &target, &c()).unwrap()
}

fn resolve_err(repo: &GitRepository, base: Revision, target: Revision) -> Error {
    resolve_comparison(repo, &base, &target, &c()).unwrap_err()
}

fn assert_message(err: Error, text: &str) {
    assert!(
        err.to_string().contains(text),
        "{err:?} doesn't say {text:?}"
    );
}

fn changes(repo: &GitRepository, comparison: &Comparison) -> Vec<GitFileChange> {
    changed_files(repo, comparison, &c()).unwrap()
}

fn diff_of(
    repo: &GitRepository,
    comparison: &Comparison,
    change: &GitFileChange,
) -> Option<GitFileDiff> {
    file_diff(repo, comparison, change, 3, &c()).unwrap()
}

fn modified(path: &str) -> GitFileChange {
    GitFileChange::new(GitFileStatus::Modified, path)
}

fn build_forked(fixture: &mut FixtureRepo) -> GitRepository {
    fixture.commit("Initial", files![("a.ts", "1\n2\n3\n")], Some("base"));
    fixture.tag("v1", "HEAD", None);
    fixture.branch("feature", "HEAD");
    fixture.commit(
        "Feature work",
        files![("a.ts", "1\ntwo\n3\n")],
        Some("feature"),
    );
    fixture.checkout("main");
    fixture.commit("Main work", files![("b.ts", "b\n")], Some("main"));
    fixture.open()
}

fn non_context(diff: &GitFileDiff) -> Vec<GitDiffLine> {
    diff.hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind != DiffLineKind::Context)
        .cloned()
        .collect()
}

fn line(kind: DiffLineKind, content: &str, old: Option<usize>, new: Option<usize>) -> GitDiffLine {
    GitDiffLine::new(kind, content, old, new)
}

fn no_newline(mut line: GitDiffLine) -> GitDiffLine {
    line.no_newline_at_end = true;
    line
}

// -- resolve_comparison --

#[test]
fn uses_the_base_when_it_is_an_ancestor_of_the_target() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);

    assert_eq!(
        resolve(&repo, commit("v1"), commit("feature")),
        Comparison {
            base: commit("v1"),
            target: commit("feature"),
            base_sha: f.sha("base"),
            target_sha: f.sha("feature"),
            merge_base: Some(f.sha("base")),
            relation: ComparisonRelation::Ancestor,
            from_sha: f.sha("base"),
            worktree_path: f.path.clone(),
        }
    );
}

#[test]
fn starts_from_the_merge_base_when_the_branches_diverged() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);

    let comparison = resolve(&repo, commit("main"), commit("feature"));

    assert_eq!(
        (
            comparison.base_sha,
            comparison.relation,
            comparison.from_sha
        ),
        (f.sha("main"), ComparisonRelation::Diverged, f.sha("base"))
    );
}

#[test]
fn compares_from_a_detached_head() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    f.git(&["checkout", "--quiet", "--detach", &f.sha("feature")]);

    let comparison = resolve(&repo, commit("v1"), commit("HEAD"));
    assert_eq!(
        (comparison.target_sha, comparison.relation),
        (f.sha("feature"), ComparisonRelation::Ancestor)
    );
    let uncommitted = resolve(&repo, commit("v1"), working_tree());
    assert_eq!(
        (uncommitted.target_sha, uncommitted.relation),
        (f.sha("feature"), ComparisonRelation::Ancestor)
    );
}

#[test]
fn annotated_tags_are_peeled() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    f.tag("v2", "feature", Some("Release"));

    let comparison = resolve(&repo, commit("v1"), commit("v2"));

    assert_eq!(comparison.target_sha, f.sha("feature"));
}

#[test]
fn flags_reversed_same_and_unrelated_comparisons() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);

    let reversed = resolve(&repo, commit("feature"), commit("v1"));
    assert_eq!(
        (reversed.relation, reversed.from_sha),
        (ComparisonRelation::Reversed, f.sha("feature"))
    );
    let same = resolve(&repo, commit("main"), commit(&f.sha("main")));
    assert_eq!(
        (same.relation, same.merge_base),
        (ComparisonRelation::Same, Some(f.sha("main")))
    );

    f.git(&["checkout", "--quiet", "--orphan", "orphan"]);
    f.commit("Unrelated", files![("o.txt", "o\n")], None);
    let unrelated = resolve(&repo, commit("main"), commit("orphan"));
    assert_eq!(
        (unrelated.relation, unrelated.merge_base, unrelated.from_sha),
        (ComparisonRelation::Unrelated, None, f.sha("main"))
    );
}

#[test]
fn uncommitted_targets_end_at_head() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);

    let staged = resolve(&repo, commit("v1"), index());
    assert_eq!(
        (
            staged.target_sha.clone(),
            staged.relation,
            staged.uncommitted()
        ),
        (f.sha("main"), ComparisonRelation::Ancestor, true)
    );
    let on_head = resolve(&repo, commit("main"), working_tree());
    assert_eq!(
        (on_head.relation, on_head.from_sha),
        (ComparisonRelation::Same, f.sha("main"))
    );
    let diverged = resolve(&repo, commit("feature"), working_tree());
    assert_eq!(
        (diverged.relation, diverged.from_sha),
        (ComparisonRelation::Diverged, f.sha("base"))
    );
}

#[test]
fn index_to_working_tree() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);

    let comparison = resolve(&repo, index(), working_tree());

    assert_eq!(
        comparison,
        Comparison {
            base: index(),
            target: working_tree(),
            base_sha: f.sha("main"),
            target_sha: f.sha("main"),
            merge_base: Some(f.sha("main")),
            relation: ComparisonRelation::Same,
            from_sha: f.sha("main"),
            worktree_path: f.path.clone(),
        }
    );
}

#[test]
fn rejects_unsupported_pairs() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    let other = f.worktree(&f.tmp_path().join("other"), "other", "HEAD");

    assert_message(
        resolve_err(&repo, working_tree(), commit("main")),
        "working tree can only be compared as the target",
    );
    assert_message(
        resolve_err(&repo, working_tree(), index()),
        "working tree can only be compared as the target",
    );
    assert_message(
        resolve_err(&repo, index(), commit("main")),
        "Staged changes can only be the base",
    );
    assert_message(
        resolve_err(&repo, index(), index()),
        "Staged changes can only be the base",
    );
    assert_message(
        resolve_err(&repo, index(), Revision::working_tree(Some(other))),
        "same worktree",
    );
    assert_message(
        resolve_err(&repo, commit("missing"), commit("main")),
        "“missing” isn't a commit",
    );
    assert_message(
        resolve_err(&repo, commit("main"), commit("missing")),
        "isn't a commit",
    );
    let no_ref = Revision {
        kind: RevisionKind::Commit,
        ref_name: None,
        worktree: None,
    };
    assert_message(resolve_err(&repo, no_ref, commit("main")), "Pick a branch");
}

#[test]
fn rejects_worktrees_of_other_repositories() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    let mut stranger = FixtureRepo::at(&f.tmp_path().join("stranger"), "main");
    stranger.commit("Initial", files![("s", "s\n")], None);

    assert_message(
        resolve_err(
            &repo,
            commit("main"),
            Revision::working_tree(Some(stranger.path.clone())),
        ),
        "isn't a worktree of this repository",
    );
    assert_message(
        resolve_err(
            &repo,
            commit("main"),
            Revision::index(Some(f.path.join("x"))),
        ),
        "isn't a worktree of this repository",
    );
}

#[test]
fn rejects_a_missing_worktree() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    let other = f.worktree(&f.tmp_path().join("other"), "other", "HEAD");
    std::fs::remove_dir_all(&other).unwrap();

    assert_message(
        resolve_err(&repo, commit("main"), Revision::working_tree(Some(other))),
        "missing",
    );
}

#[test]
fn unborn_branches() {
    let f = FixtureRepo::new();
    let repo = f.open();
    f.write_files(files![("a.txt", "staged\n")]);
    f.stage(&[]);
    f.write_files(files![("a.txt", "staged\nunstaged\n"), ("u.txt", "u\n")]);

    let comparison = resolve(&repo, index(), working_tree());
    let empty = repo.empty_tree(&c()).unwrap();
    assert_eq!(
        (
            &comparison.base_sha,
            &comparison.target_sha,
            &comparison.from_sha
        ),
        (&empty, &empty, &empty)
    );
    assert_eq!(
        (comparison.relation, comparison.merge_base.clone()),
        (ComparisonRelation::Same, None)
    );
    assert_eq!(
        changes(&repo, &comparison),
        [modified("a.txt"), GitFileChange::untracked("u.txt")]
    );

    f.git(&["checkout", "--quiet", "--orphan", "x"]);
    assert_message(
        resolve_err(&repo, commit("HEAD"), working_tree()),
        "isn't a commit",
    );
}

#[test]
fn unborn_worktree_target_with_a_commit_base() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    let orphan = f.tmp_path().join("orphan");
    f.git(&[
        "worktree",
        "add",
        "--quiet",
        "--orphan",
        "-b",
        "orphan",
        orphan.to_str().unwrap(),
    ]);

    assert_message(
        resolve_err(&repo, commit("main"), Revision::index(Some(orphan))),
        "has no commits yet",
    );
}

#[test]
fn cancelled_resolution() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    let cancel = CancelToken::new();
    cancel.cancel();

    let err = resolve_comparison(&repo, &commit("v1"), &commit("feature"), &cancel).unwrap_err();
    assert!(err.is_cancelled());
}

// -- changed files --

#[test]
fn detects_renames_deletes_and_adds() {
    let mut f = FixtureRepo::new();
    f.commit(
        "Initial",
        files![("old.ts", "one\ntwo\nthree\nfour\n"), ("x.ts", "x\n")],
        Some("a"),
    );
    f.commit_with(
        "Moves",
        files![
            ("src/new.ts", "one\ntwo\nthree\nfour\nfive\n"),
            ("x.ts", Content::Delete),
            ("y.ts", "y\n"),
        ],
        CommitOptions::label("b").with_rename("old.ts", "src/new.ts"),
    );
    let repo = f.open();
    let comparison = resolve(&repo, commit(&f.sha("a")), commit(&f.sha("b")));

    let found = changes(&repo, &comparison);

    assert_eq!(
        found,
        [
            GitFileChange::renamed(
                GitFileStatus::Renamed,
                "src/new.ts",
                "old.ts",
                found[0].similarity
            ),
            GitFileChange::new(GitFileStatus::Deleted, "x.ts"),
            GitFileChange::new(GitFileStatus::Added, "y.ts"),
        ]
    );
    let diff = diff_of(&repo, &comparison, &found[0]).unwrap();
    assert_eq!(
        (
            diff.old_path.as_deref(),
            diff.new_path.as_deref(),
            diff.binary
        ),
        (Some("old.ts"), Some("src/new.ts"), false)
    );
    assert_eq!(
        non_context(&diff),
        [line(DiffLineKind::Add, "five", None, Some(5))]
    );
    let deleted = diff_of(&repo, &comparison, &found[1]).unwrap();
    assert_eq!(
        (deleted.old_path.as_deref(), deleted.new_path.as_deref()),
        (Some("x.ts"), None)
    );
}

fn build_layered(fixture: &mut FixtureRepo) -> GitRepository {
    fixture.commit(
        "Initial",
        files![
            ("a.ts", "1\n2\n"),
            ("s.ts", "s\n"),
            (".gitignore", "out/\n")
        ],
        Some("a"),
    );
    fixture.write_files(files![("a.ts", "1\nstaged\n"), ("s.ts", Content::Delete)]);
    fixture.stage(&[]);
    fixture.write_files(files![
        ("a.ts", "1\nstaged\nunstaged\n"),
        ("new/u.ts", "u\nlast"),
        ("bin.dat", b"a\0b"),
        ("out/ignored.js", "x\n"),
    ]);
    fixture.open()
}

#[test]
fn lists_staged_unstaged_and_untracked_changes() {
    let mut f = FixtureRepo::new();
    let repo = build_layered(&mut f);
    let base = commit(&f.sha("a"));

    let staged = resolve(&repo, base.clone(), index());
    assert_eq!(
        changes(&repo, &staged),
        [
            modified("a.ts"),
            GitFileChange::new(GitFileStatus::Deleted, "s.ts")
        ]
    );
    let in_working_tree = resolve(&repo, base, working_tree());
    assert_eq!(
        changes(&repo, &in_working_tree),
        [
            modified("a.ts"),
            GitFileChange::untracked("bin.dat"),
            GitFileChange::untracked("new/u.ts"),
            GitFileChange::new(GitFileStatus::Deleted, "s.ts"),
        ]
    );
    let unstaged = resolve(&repo, index(), working_tree());
    assert_eq!(
        changes(&repo, &unstaged),
        [
            modified("a.ts"),
            GitFileChange::untracked("bin.dat"),
            GitFileChange::untracked("new/u.ts"),
        ]
    );
}

fn sorted_stats(mut stats: Vec<GitFileStats>) -> Vec<GitFileStats> {
    stats.sort_by(|a, b| a.path.cmp(&b.path));
    stats
}

#[test]
fn counts_lines_per_file_untracked_ones_too() {
    let mut f = FixtureRepo::new();
    let repo = build_layered(&mut f);
    let comparison = resolve(&repo, commit(&f.sha("a")), working_tree());

    let (files, stats) = changed_files_with_stats(&repo, &comparison, &c()).unwrap();

    assert_eq!(
        sorted_stats(stats),
        [
            GitFileStats::new("a.ts", Some(2), Some(1)),
            GitFileStats::new("bin.dat", None, None),
            GitFileStats::new("new/u.ts", Some(2), Some(0)),
            GitFileStats::new("s.ts", Some(0), Some(1)),
        ]
    );
    assert_eq!(files, changes(&repo, &comparison));
    let staged = resolve(&repo, commit(&f.sha("a")), index());
    assert_eq!(
        sorted_stats(changed_files_with_stats(&repo, &staged, &c()).unwrap().1),
        [
            GitFileStats::new("a.ts", Some(1), Some(1)),
            GitFileStats::new("s.ts", Some(0), Some(1)),
        ]
    );
}

#[test]
fn file_diffs_of_each_layer() {
    let mut f = FixtureRepo::new();
    let repo = build_layered(&mut f);
    let base = commit(&f.sha("a"));
    let a = modified("a.ts");

    let staged = diff_of(&repo, &resolve(&repo, base.clone(), index()), &a).unwrap();
    assert_eq!(
        non_context(&staged),
        [
            line(DiffLineKind::Delete, "2", Some(2), None),
            line(DiffLineKind::Add, "staged", None, Some(2)),
        ]
    );
    let on_disk = diff_of(&repo, &resolve(&repo, base.clone(), working_tree()), &a).unwrap();
    assert_eq!(
        non_context(&on_disk),
        [
            line(DiffLineKind::Delete, "2", Some(2), None),
            line(DiffLineKind::Add, "staged", None, Some(2)),
            line(DiffLineKind::Add, "unstaged", None, Some(3)),
        ]
    );
    let unstaged = diff_of(&repo, &resolve(&repo, index(), working_tree()), &a).unwrap();
    assert_eq!(
        non_context(&unstaged),
        [line(DiffLineKind::Add, "unstaged", None, Some(3))]
    );

    let untracked = diff_of(
        &repo,
        &resolve(&repo, base.clone(), working_tree()),
        &GitFileChange::untracked("new/u.ts"),
    )
    .unwrap();
    assert_eq!(
        (untracked.old_path.as_deref(), untracked.new_path.as_deref()),
        (None, Some("new/u.ts"))
    );
    assert_eq!(
        non_context(&untracked).last().cloned(),
        Some(no_newline(line(DiffLineKind::Add, "last", None, Some(2))))
    );
    let binary = diff_of(
        &repo,
        &resolve(&repo, base, working_tree()),
        &GitFileChange::untracked("bin.dat"),
    )
    .unwrap();
    assert!(binary.binary);
}

#[test]
fn a_file_without_textual_changes_has_no_diff() {
    let mut f = FixtureRepo::new();
    f.commit("Initial", files![("run.sh", "echo\n")], Some("a"));
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            f.path.join("run.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    f.commit("Executable", vec![], Some("b"));
    let repo = f.open();
    let comparison = resolve(&repo, commit(&f.sha("a")), commit(&f.sha("b")));

    let found = changes(&repo, &comparison);
    assert_eq!(found, [modified("run.sh")]);
    let diff = diff_of(&repo, &comparison, &found[0]).unwrap();

    assert_eq!(
        (diff.new_path.as_deref(), diff.hunks.len()),
        (Some("run.sh"), 0)
    );
    assert_eq!(
        diff_of(&repo, &comparison, &modified("unchanged.txt")),
        None
    );
}

#[test]
fn another_worktrees_staged_and_working_tree_changes() {
    let mut f = FixtureRepo::new();
    f.commit("Initial", files![("a.ts", "1\n2\n")], Some("a"));
    let other = f.worktree(&f.tmp_path().join("other"), "other", "HEAD");
    f.commit_with(
        "There",
        files![("a.ts", "1\n2\n3\n")],
        CommitOptions::label("there").in_worktree(&other),
    );
    f.write_files_in(&other, files![("a.ts", "1\n2\n3\nstaged\n")]);
    f.git_in(&other, &["add", "a.ts"]);
    f.write_files_in(
        &other,
        files![("a.ts", "1\n2\n3\nstaged\nunstaged\n"), ("u.ts", "u\n")],
    );
    // Changes in the repository's own worktree must not show up
    f.write_files(files![("a.ts", "mine\n"), ("mine.ts", "m\n")]);
    let repo = f.open();
    let base = commit(&f.sha("a"));
    let there = || Some(PathBuf::from(&other));

    let staged = resolve(&repo, base.clone(), Revision::index(there()));
    assert_eq!(
        (&staged.target_sha, staged.relation, &staged.worktree_path),
        (&f.sha("there"), ComparisonRelation::Ancestor, &other)
    );
    assert_eq!(changes(&repo, &staged), [modified("a.ts")]);
    assert_eq!(
        non_context(&diff_of(&repo, &staged, &modified("a.ts")).unwrap()),
        [
            line(DiffLineKind::Add, "3", None, Some(3)),
            line(DiffLineKind::Add, "staged", None, Some(4)),
        ]
    );

    let on_disk = resolve(&repo, base, Revision::working_tree(there()));
    assert_eq!(
        changes(&repo, &on_disk),
        [modified("a.ts"), GitFileChange::untracked("u.ts")]
    );
    let unstaged = resolve(
        &repo,
        Revision::index(there()),
        Revision::working_tree(there()),
    );
    assert_eq!(unstaged.base_sha, f.sha("there"));
    assert_eq!(
        non_context(&diff_of(&repo, &unstaged, &modified("a.ts")).unwrap()),
        [line(DiffLineKind::Add, "unstaged", None, Some(5))]
    );
}

#[test]
fn the_repositorys_own_worktree_by_path() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    f.write_files(files![("b.ts", "changed\n")]);

    let comparison = resolve(&repo, Revision::index(Some(f.path.clone())), working_tree());

    assert_eq!(comparison.worktree_path, f.path);
    assert_eq!(changes(&repo, &comparison), [modified("b.ts")]);
}

#[test]
fn paths_with_spaces_quotes_unicode_and_newlines() {
    let names = [
        "a b.txt",
        "quo\"te.txt",
        "日本/ü.txt",
        "new\nline.txt",
        "glob[1]*.txt",
    ];
    let all = |content: &str| -> Vec<(String, Content)> {
        names
            .iter()
            .map(|name| (name.to_string(), Content::from(content)))
            .collect()
    };
    let mut f = FixtureRepo::new();
    f.commit("Initial", all("1\n"), Some("a"));
    f.commit("Edit", all("1\n2\n"), Some("b"));
    f.write_files(all("1\n2\n3\n"));
    f.write_files(files![("untracked ü\n.txt", "u\n")]);
    let repo = f.open();

    let committed = resolve(&repo, commit(&f.sha("a")), commit(&f.sha("b")));
    let found = changes(&repo, &committed);
    let mut paths: Vec<&str> = found.iter().map(|change| change.path.as_str()).collect();
    paths.sort();
    let mut expected = names.to_vec();
    expected.sort();
    assert_eq!(paths, expected);
    for change in &found {
        let diff = diff_of(&repo, &committed, change).unwrap();
        assert_eq!(
            (diff.old_path.as_deref(), diff.new_path.as_deref()),
            (Some(change.path.as_str()), Some(change.path.as_str()))
        );
        assert_eq!(
            non_context(&diff),
            [line(DiffLineKind::Add, "2", None, Some(2))]
        );
    }

    let on_disk = resolve(&repo, commit(&f.sha("b")), working_tree());
    let found = changes(&repo, &on_disk);
    // git's order: by bytes
    let mut expected: Vec<&str> = names.to_vec();
    expected.push("untracked ü\n.txt");
    expected.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    assert_eq!(
        found
            .iter()
            .map(|change| change.path.as_str())
            .collect::<Vec<_>>(),
        expected
    );
    let untracked = found.iter().find(|change| change.untracked).unwrap();
    assert_eq!(
        non_context(&diff_of(&repo, &on_disk, untracked).unwrap()),
        [line(DiffLineKind::Add, "u", None, Some(1))]
    );
    let (_, stats) = changed_files_with_stats(&repo, &on_disk, &c()).unwrap();
    let stat = |path: &str| stats.iter().find(|stat| stat.path == path).cloned();
    assert_eq!(
        stat("new\nline.txt"),
        Some(GitFileStats::new("new\nline.txt", Some(1), Some(0)))
    );
    assert_eq!(
        stat("untracked ü\n.txt"),
        Some(GitFileStats::new("untracked ü\n.txt", Some(1), Some(0)))
    );
}

#[test]
fn crlf_and_missing_final_newlines_are_kept() {
    let mut f = FixtureRepo::new();
    f.commit(
        "Initial",
        files![("w.txt", "a\r\nb\r\n"), ("n.txt", "x\ny")],
        Some("a"),
    );
    f.commit(
        "Edit",
        files![("w.txt", "a\r\nB\r\n"), ("n.txt", "x\nz")],
        Some("b"),
    );
    let repo = f.open();
    let comparison = resolve(&repo, commit(&f.sha("a")), commit(&f.sha("b")));

    let crlf = diff_of(&repo, &comparison, &modified("w.txt")).unwrap();
    let missing_newline = diff_of(&repo, &comparison, &modified("n.txt")).unwrap();

    assert_eq!(
        non_context(&crlf),
        [
            line(DiffLineKind::Delete, "b\r", Some(2), None),
            line(DiffLineKind::Add, "B\r", None, Some(2)),
        ]
    );
    assert_eq!(
        non_context(&missing_newline),
        [
            no_newline(line(DiffLineKind::Delete, "y", Some(2), None)),
            no_newline(line(DiffLineKind::Add, "z", None, Some(2))),
        ]
    );
}

#[test]
fn submodules_are_single_entries() {
    let mut f = FixtureRepo::new();
    f.commit("Initial", files![("a.ts", "a\n")], Some("a"));
    let ones = format!("160000,{},sub", "1".repeat(40));
    f.git(&["update-index", "--add", "--cacheinfo", &ones]);
    let env = f.commit_env(("x", "x@x"));
    f.git_full(
        &["commit", "--quiet", "-m", "Adds sub"],
        Some(&env),
        None,
        None,
    )
    .unwrap();
    let first = f.git(&["rev-parse", "HEAD"]).trim().to_string();
    let twos = format!("160000,{},sub", "2".repeat(40));
    f.git(&["update-index", "--cacheinfo", &twos]);
    let env = f.commit_env(("x", "x@x"));
    f.git_full(
        &["commit", "--quiet", "-m", "Moves sub"],
        Some(&env),
        None,
        None,
    )
    .unwrap();
    let repo = f.open();

    let comparison = resolve(&repo, commit(&first), commit("HEAD"));

    assert_eq!(changes(&repo, &comparison), [modified("sub")]);
    let on_disk = resolve(&repo, commit(&first), working_tree());
    assert_eq!(
        changes(&repo, &on_disk)
            .into_iter()
            .map(|change| change.path)
            .collect::<Vec<_>>(),
        ["sub"]
    );
}

#[test]
fn cancelled_listing() {
    let mut f = FixtureRepo::new();
    let repo = build_layered(&mut f);
    let comparison = resolve(&repo, commit(&f.sha("a")), working_tree());
    let cancel = CancelToken::new();
    cancel.cancel();

    assert!(
        changed_files(&repo, &comparison, &cancel)
            .unwrap_err()
            .is_cancelled()
    );
    assert!(
        changed_files_with_stats(&repo, &comparison, &cancel)
            .unwrap_err()
            .is_cancelled()
    );
    assert!(
        file_diff(&repo, &comparison, &modified("a.ts"), 3, &cancel)
            .unwrap_err()
            .is_cancelled()
    );
}
