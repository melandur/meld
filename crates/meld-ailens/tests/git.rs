//! Port of test/ailens/test_git.py

mod common;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use common::{CommitOptions, Content, FixtureRepo, HUMAN, co_authored_by_claude};
use meld_ailens::git::{GitOptions, GitRepository, LogQuery};
use meld_ailens::parsers::GitFileStats;
use meld_ailens::{
    CancelToken, DiffLineKind, Error, GitCommit, GitDiffLine, GitFileChange, GitFileDiff,
    GitFileStatus, GitIdentity, GitRefType, GitTrailer, UncommittedLayer,
};

fn c() -> CancelToken {
    CancelToken::new()
}

fn build_forked(fixture: &mut FixtureRepo) -> GitRepository {
    fixture.commit("Initial", files![("a.ts", "1\n2\n3\n")], Some("base"));
    fixture.tag("v1", "HEAD", None);
    fixture.branch("feature", "HEAD");
    fixture.commit_with(
        "Feature work",
        files![("a.ts", "1\ntwo\n3\n")],
        CommitOptions::label("feature").with_trailer(co_authored_by_claude("Opus 5.5")),
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

fn options_with(fixture: &FixtureRepo, extra: &[(&str, &str)]) -> GitOptions {
    let mut env = fixture.env.clone();
    for (key, value) in extra {
        env.insert(key.to_string(), value.to_string());
    }
    GitOptions {
        git_path: "git".into(),
        env: Some(env),
    }
}

fn assert_message(err: Error, text: &str) {
    assert!(
        err.to_string().contains(text),
        "{err:?} doesn't say {text:?}"
    );
}

// -- revisions --

#[test]
fn resolves_branches_tags_and_short_shas() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    let feature = f.sha("feature");
    f.tag("v2", &feature, Some("Release"));

    let resolve = |name: &str| repo.resolve_commit(name, &c()).unwrap();
    assert_eq!(resolve("feature"), Some(f.sha("feature")));
    assert_eq!(resolve("v1"), Some(f.sha("base")));
    // Annotated tags are peeled to their commit
    assert_eq!(resolve("v2"), Some(f.sha("feature")));
    assert_eq!(resolve(&f.sha("main")[..7]), Some(f.sha("main")));
    assert_eq!(resolve("HEAD"), Some(f.sha("main")));
    assert_eq!(resolve("main~1"), Some(f.sha("base")));
}

#[test]
fn resolve_commit_returns_none_for_anything_else() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    let tree = f.git(&["rev-parse", "HEAD^{tree}"]).trim().to_string();

    assert_eq!(repo.resolve_commit("missing", &c()).unwrap(), None);
    assert_eq!(repo.resolve_commit("", &c()).unwrap(), None);
    assert_eq!(repo.resolve_commit(&tree, &c()).unwrap(), None);
    // Never taken for an option
    assert_eq!(repo.resolve_commit("--all", &c()).unwrap(), None);
}

#[test]
fn short_sha() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    let sha = f.sha("main");

    assert_eq!(repo.short_sha(&sha, &c()).unwrap(), &sha[..7]);
}

#[test]
fn head_is_none_on_an_unborn_branch_and_follows_detached_heads() {
    let mut f = FixtureRepo::new();
    let repo = f.open();
    assert_eq!(repo.head(None, &c()).unwrap(), None);

    build_forked(&mut f);
    assert_eq!(repo.head(None, &c()).unwrap(), Some(f.sha("main")));

    f.git(&["checkout", "--quiet", "--detach", &f.sha("feature")]);
    assert_eq!(repo.head(None, &c()).unwrap(), Some(f.sha("feature")));
}

#[test]
fn merge_base_and_ancestry() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    let (main, feature, base) = (f.sha("main"), f.sha("feature"), f.sha("base"));

    assert_eq!(
        repo.merge_base(&main, &feature, &c()).unwrap(),
        Some(base.clone())
    );
    assert!(repo.is_ancestor(&base, &feature, &c()).unwrap());
    assert!(!repo.is_ancestor(&main, &feature, &c()).unwrap());

    f.git(&["checkout", "--quiet", "--orphan", "orphan"]);
    let unrelated = f.commit("Unrelated", files![("o.txt", "o\n")], None);
    assert_eq!(repo.merge_base(&main, &unrelated, &c()).unwrap(), None);
    assert!(!repo.is_ancestor(&main, &unrelated, &c()).unwrap());
}

#[test]
fn empty_tree_and_git_dir() {
    let f = FixtureRepo::new();
    let repo = f.open();

    assert_eq!(
        repo.empty_tree(&c()).unwrap(),
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    );
    assert_eq!(repo.git_dir(None, &c()).unwrap(), f.path.join(".git"));
    assert!(!repo.is_shallow(&c()).unwrap());
}

// -- discovery and errors --

#[test]
fn discover_from_a_folder_or_file_inside_the_worktree() {
    let mut f = FixtureRepo::new();
    f.commit("Initial", files![("src/deep/a.ts", "a\n")], None);
    let options = f.git_options();

    let from_folder = GitRepository::discover(&f.path.join("src/deep"), &options, &c()).unwrap();
    assert_eq!(from_folder.root, f.path);
    let from_file = GitRepository::discover(&f.path.join("src/deep/a.ts"), &options, &c()).unwrap();
    assert_eq!(from_file.root, f.path);
}

#[test]
fn discover_in_a_linked_worktree_gives_that_worktree() {
    let mut f = FixtureRepo::new();
    f.commit("Initial", files![("src/a.ts", "a\n")], None);
    let other = f.worktree(&f.tmp_path().join("other"), "other", "HEAD");

    let repo = GitRepository::discover(&other.join("src"), &f.git_options(), &c()).unwrap();

    assert_eq!(repo.root, other);
    let paths: Vec<_> = repo
        .worktrees(&c())
        .unwrap()
        .into_iter()
        .map(|w| w.path)
        .collect();
    assert_eq!(paths, [f.path.clone(), other]);
}

#[test]
fn discover_outside_a_repository() {
    let f = FixtureRepo::new();
    let tmp = f.tmp_path();
    let outside = tmp.join("outside");
    std::fs::create_dir(&outside).unwrap();
    let options = options_with(&f, &[("GIT_CEILING_DIRECTORIES", tmp.to_str().unwrap())]);

    let err = GitRepository::discover(&outside, &options, &c()).unwrap_err();
    assert_message(err, "isn't in a Git repository");
    let err = GitRepository::discover(&tmp.join("missing"), &options, &c()).unwrap_err();
    assert_message(err, "doesn't exist");
}

#[test]
fn discover_rejects_bare_repositories_and_git_folders() {
    let mut f = FixtureRepo::new();
    f.commit("Initial", files![("a.ts", "a\n")], None);
    let bare = f.tmp_path().join("bare.git");
    f.git(&["init", "--quiet", "--bare", bare.to_str().unwrap()]);

    let err = GitRepository::discover(&bare, &f.git_options(), &c()).unwrap_err();
    assert_message(err, "bare");
    let err = GitRepository::discover(&f.path.join(".git"), &f.git_options(), &c()).unwrap_err();
    assert_message(err, "inside a Git folder");
}

#[test]
fn git_missing_is_a_clear_error() {
    let f = FixtureRepo::new();
    let options = GitOptions {
        git_path: f.tmp_path().join("no/git").to_string_lossy().into_owned(),
        env: Some(f.env.clone()),
    };
    let repo = GitRepository::new(&f.path, &options);

    let err = repo.head(None, &c()).unwrap_err();
    assert!(err.is_git_not_found());
    assert_message(err, "Git wasn't found at");
    let err = GitRepository::discover(&f.path, &options, &c()).unwrap_err();
    assert_message(err, "Git wasn't found at");
}

#[test]
fn a_missing_folder_is_not_reported_as_missing_git() {
    let f = FixtureRepo::new();
    let repo = GitRepository::new(f.tmp_path().join("gone"), &f.git_options());

    let err = repo.head(None, &c()).unwrap_err();
    assert!(matches!(err, Error::Git { .. }));
    assert!(!err.is_git_not_found());
    assert_message(err, "doesn't exist");
}

#[test]
fn git_errors_carry_stderr_and_exit_code() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);

    let err = repo
        .run(&["rev-parse", "--verify", "no-such-ref"], &c())
        .unwrap_err();

    let Error::Git {
        returncode, stderr, ..
    } = &err
    else {
        panic!("not a git error: {err:?}");
    };
    assert_eq!(*returncode, Some(128));
    assert!(stderr.contains("Needed a single revision"));
    assert_message(err, "Needed a single revision");
}

#[test]
fn variables_that_point_git_elsewhere_are_dropped() {
    let mut f = FixtureRepo::new();
    build_forked(&mut f);
    let tmp = f.tmp_path();
    let options = options_with(
        &f,
        &[
            ("GIT_DIR", tmp.join("nowhere").to_str().unwrap()),
            ("GIT_WORK_TREE", tmp.to_str().unwrap()),
            ("GIT_INDEX_FILE", tmp.join("index").to_str().unwrap()),
        ],
    );

    let repo = GitRepository::new(&f.path, &options);

    assert_eq!(repo.head(None, &c()).unwrap(), Some(f.sha("main")));
}

#[test]
fn output_does_not_depend_on_the_users_config() {
    let mut f = FixtureRepo::new();
    build_forked(&mut f);
    for (key, value) in [
        ("diff.noprefix", "true"),
        ("diff.mnemonicPrefix", "true"),
        ("diff.suppressBlankEmpty", "true"),
        ("core.quotePath", "true"),
        ("color.ui", "always"),
        ("log.showSignature", "true"),
        ("diff.external", "false"),
    ] {
        f.git(&["config", key, value]);
    }
    f.commit("Unicode", files![("ü dir/a.ts", "x\n\ny\n")], Some("u"));
    f.commit("Edit", files![("ü dir/a.ts", "x\n\nz\n")], Some("edit"));
    let repo = f.open();

    let diffs = repo.commit_diffs(&f.sha("edit"), &c()).unwrap();

    assert_eq!(diffs.len(), 1);
    let (change, diff) = &diffs[0];
    assert_eq!(
        *change,
        GitFileChange::new(GitFileStatus::Modified, "ü dir/a.ts")
    );
    assert_eq!(
        (diff.old_path.as_deref(), diff.new_path.as_deref()),
        (Some("ü dir/a.ts"), Some("ü dir/a.ts"))
    );
    let contents: Vec<&str> = diff.hunks[0]
        .lines
        .iter()
        .map(|l| l.content.as_str())
        .collect();
    assert_eq!(contents, ["x", "", "y", "z"]);
}

// -- cancellation --

#[test]
fn cancelled_before_a_call() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    let cancel = CancelToken::new();
    cancel.cancel();

    assert!(repo.head(None, &cancel).unwrap_err().is_cancelled());
    assert!(
        repo.read_worktree_file("a.ts", None, &cancel)
            .unwrap_err()
            .is_cancelled()
    );
}

#[test]
fn cancelled_during_a_call_kills_git() {
    let f = FixtureRepo::new();
    let slow = f.tmp_path().join("slow-git");
    std::fs::write(&slow, "#!/bin/sh\nsleep 30\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&slow, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let options = GitOptions {
        git_path: slow.to_string_lossy().into_owned(),
        env: Some(f.env.clone()),
    };
    let repo = GitRepository::new(&f.path, &options);
    let cancel = CancelToken::new();
    let timer = {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            cancel.cancel();
        })
    };

    let start = Instant::now();
    let err = repo.log(&[], &LogQuery::default(), &cancel).unwrap_err();
    assert!(err.is_cancelled(), "{err:?}");
    assert!(start.elapsed() < Duration::from_secs(10));
    timer.join().unwrap();
}

// -- refs and worktrees --

#[test]
fn lists_branches_remote_branches_and_tags() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    f.git(&["update-ref", "refs/remotes/origin/main", &f.sha("base")]);
    f.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ]);
    let feature = f.sha("feature");
    f.tag("v2", &feature, Some("Release"));
    let other = f.worktree(&f.tmp_path().join("wt"), "wt-branch", "feature");

    let refs = repo.refs(&c()).unwrap();

    let described: Vec<_> = refs
        .iter()
        .map(|r| {
            (
                r.kind,
                r.name.as_str(),
                r.sha.clone(),
                r.current,
                r.worktree.clone(),
            )
        })
        .collect();
    assert_eq!(
        described,
        [
            (GitRefType::Branch, "feature", f.sha("feature"), false, None),
            (
                GitRefType::Branch,
                "main",
                f.sha("main"),
                true,
                Some(f.path.clone())
            ),
            (
                GitRefType::Branch,
                "wt-branch",
                f.sha("feature"),
                false,
                Some(other)
            ),
            (
                GitRefType::Remote,
                "origin/main",
                f.sha("base"),
                false,
                None
            ),
            (GitRefType::Tag, "v1", f.sha("base"), false, None),
            (GitRefType::Tag, "v2", f.sha("feature"), false, None),
        ]
    );
    let v2 = refs.iter().find(|r| r.name == "v2").unwrap();
    assert_eq!(v2.subject, "Feature work");
    assert_eq!(v2.full_name, "refs/tags/v2");
    assert_eq!(v2.date, "2023-11-14T22:14:20Z");
}

#[test]
fn refs_with_a_detached_head_have_no_current_branch() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    f.git(&["checkout", "--quiet", "--detach", &f.sha("feature")]);

    assert!(!repo.refs(&c()).unwrap().iter().any(|r| r.current));
}

#[test]
fn refs_and_worktrees_of_an_unborn_repository() {
    let f = FixtureRepo::new();
    let repo = f.open();

    assert!(repo.refs(&c()).unwrap().is_empty());
    let worktrees = repo.worktrees(&c()).unwrap();
    assert_eq!(worktrees.len(), 1);
    let worktree = &worktrees[0];
    assert_eq!(
        (
            &worktree.path,
            &worktree.head,
            worktree.branch.as_deref(),
            worktree.main
        ),
        (&f.path, &None, Some("main"), true)
    );
}

#[test]
fn worktrees() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    let tmp = f.tmp_path();
    let other = f.worktree(&tmp.join("other wt"), "other", "feature");
    f.git(&[
        "worktree",
        "add",
        "--quiet",
        "--detach",
        tmp.join("detached").to_str().unwrap(),
        "v1",
    ]);

    let worktrees = repo.worktrees(&c()).unwrap();

    // The main worktree first, the others in git's order
    let described: Vec<_> = worktrees
        .iter()
        .map(|w| {
            (
                w.path.clone(),
                w.head.clone(),
                w.branch.clone(),
                w.detached,
                w.main,
            )
        })
        .collect();
    assert_eq!(
        described,
        [
            (
                f.path.clone(),
                Some(f.sha("main")),
                Some("main".to_string()),
                false,
                true
            ),
            (
                std::fs::canonicalize(tmp.join("detached")).unwrap(),
                Some(f.sha("base")),
                None,
                true,
                false
            ),
            (
                other,
                Some(f.sha("feature")),
                Some("other".to_string()),
                false,
                false
            ),
        ]
    );
}

// -- commits --

#[test]
fn reads_commits_with_parents_authors_and_trailers() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    let identity = GitIdentity {
        name: HUMAN.0.into(),
        email: HUMAN.1.into(),
        date: "2023-11-14T22:14:20Z".into(),
    };
    let range = format!("{}..feature", f.sha("base"));

    assert_eq!(
        repo.log(&[&range], &LogQuery::default(), &c()).unwrap(),
        [GitCommit {
            sha: f.sha("feature"),
            parents: vec![f.sha("base")],
            author: identity.clone(),
            committer: identity,
            subject: "Feature work".into(),
            body: "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>".into(),
            trailers: vec![GitTrailer::new(
                "Co-Authored-By",
                "Claude Opus 5.5 <noreply@anthropic.com>"
            )],
            note: None,
        }]
    );
}

fn all_refs<'a>() -> LogQuery<'a> {
    LogQuery {
        all_refs: true,
        ..Default::default()
    }
}

#[test]
fn pages_through_commits_across_all_refs() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);

    let every = repo.log(&[], &all_refs(), &c()).unwrap();
    let page = repo
        .log(
            &[],
            &LogQuery {
                max_count: Some(1),
                skip: 1,
                ..all_refs()
            },
            &c(),
        )
        .unwrap();

    let shas: HashSet<String> = every.iter().map(|c| c.sha.clone()).collect();
    let expected: HashSet<String> = f.shas.values().cloned().collect();
    assert_eq!(shas, expected);
    assert_eq!(every.len(), 3);
    assert_eq!(
        page.iter().map(|c| c.sha.clone()).collect::<Vec<_>>(),
        [every[1].sha.clone()]
    );
}

#[test]
fn all_refs_include_a_detached_head() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    f.git(&["checkout", "--quiet", "--detach"]);
    let loose = f.commit("Detached work", files![("d.txt", "d\n")], None);

    assert!(
        repo.log(&[], &all_refs(), &c())
            .unwrap()
            .iter()
            .any(|commit| commit.sha == loose)
    );
}

#[test]
fn log_searches_message_and_author() {
    let mut f = FixtureRepo::new();
    f.commit_with(
        "Fix bug",
        files![("a", "1\n")],
        CommitOptions::default().with_author(("Alice Smith", "alice@example.com")),
    );
    f.commit_with(
        "Alice's idea",
        files![("a", "2\n")],
        CommitOptions::default().with_author(("Bob", "bob@example.com")),
    );
    f.commit_with(
        "Other [work]",
        files![("a", "3\n")],
        CommitOptions::default().with_author(("Carol", "carol@example.com")),
    );
    let repo = f.open();

    let subjects = |grep: &str, max_count: Option<usize>, skip: usize| -> Vec<String> {
        repo.log(
            &[],
            &LogQuery {
                grep: Some(grep),
                max_count,
                skip,
                ..all_refs()
            },
            &c(),
        )
        .unwrap()
        .into_iter()
        .map(|commit| commit.subject)
        .collect()
    };

    assert_eq!(subjects("alice", None, 0), ["Alice's idea", "Fix bug"]);
    assert_eq!(subjects("BOB@EXAMPLE", None, 0), ["Alice's idea"]);
    // A fixed string, not a pattern
    assert_eq!(subjects("[work]", None, 0), ["Other [work]"]);
    assert_eq!(subjects("alice", Some(1), 1), ["Fix bug"]);
    assert!(subjects("nobody", None, 0).is_empty());
}

#[test]
fn log_on_an_unborn_branch_is_empty() {
    let f = FixtureRepo::new();
    let repo = f.open();

    assert!(
        repo.log(&[], &LogQuery::default(), &c())
            .unwrap()
            .is_empty()
    );
    assert!(repo.log(&[], &all_refs(), &c()).unwrap().is_empty());
}

#[test]
fn commits_in_the_order_given() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    let (base, main, feature) = (f.sha("base"), f.sha("main"), f.sha("feature"));

    let commits = repo
        .commits(&[&base, &main, &feature, &base], None, &c())
        .unwrap();

    assert_eq!(
        commits.iter().map(|c| c.sha.clone()).collect::<Vec<_>>(),
        [base, main, feature]
    );
    assert!(repo.commits(&[], None, &c()).unwrap().is_empty());
    assert_eq!(
        repo.commit("feature", None, &c()).unwrap().subject,
        "Feature work"
    );
    assert_message(
        repo.commit("missing", None, &c()).unwrap_err(),
        "isn't a commit",
    );
}

#[test]
fn parents_and_commits_touching() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    let merge = f.merge("feature", "Merges feature", Some("merge"), None);
    let range = format!("{}..{merge}", f.sha("base"));

    let parents = repo.parents(&[&range], &c()).unwrap();
    assert_eq!(
        parents,
        HashMap::from([
            (merge.clone(), vec![f.sha("main"), f.sha("feature")]),
            (f.sha("main"), vec![f.sha("base")]),
            (f.sha("feature"), vec![f.sha("base")]),
        ])
    );
    assert_eq!(
        repo.commits_touching(&[range.as_str()], &["a.ts"], false, &c())
            .unwrap(),
        [f.sha("feature")]
    );
    assert_eq!(
        repo.commits_touching(&[merge.as_str()], &["b.ts"], false, &c())
            .unwrap(),
        [f.sha("main")]
    );
}

// -- notes --

#[test]
fn writes_notes_and_reads_them_back_only_from_the_requested_ref() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    let (feature, main) = (f.sha("feature"), f.sha("main"));
    repo.write_note(&feature, "Why: first", "ai-lens", &c())
        .unwrap();
    repo.write_note(&feature, "Why: replaced\n\n", "ai-lens", &c())
        .unwrap();
    f.git(&["notes", "add", "-m", "default ref", &main]);

    let read = |sha: &str, notes_ref: &str| repo.read_note(sha, notes_ref, &c()).unwrap();
    assert_eq!(read(&feature, "ai-lens").as_deref(), Some("Why: replaced"));
    assert_eq!(
        read(&feature, "refs/notes/ai-lens").as_deref(),
        Some("Why: replaced")
    );
    assert_eq!(read(&main, "ai-lens"), None);
    assert_eq!(read(&main, "commits").as_deref(), Some("default ref"));

    let with_notes = |notes_ref: &str| -> HashMap<String, GitCommit> {
        repo.log(
            &[],
            &LogQuery {
                notes_ref: Some(notes_ref),
                ..all_refs()
            },
            &c(),
        )
        .unwrap()
        .into_iter()
        .map(|commit| (commit.sha.clone(), commit))
        .collect()
    };
    let short = with_notes("ai-lens");
    assert_eq!(short[&feature].note.as_deref(), Some("Why: replaced"));
    assert_eq!(short[&main].note, None);
    let full = with_notes("refs/notes/ai-lens");
    assert_eq!(full[&feature].note.as_deref(), Some("Why: replaced"));

    assert!(
        repo.log(&[], &all_refs(), &c())
            .unwrap()
            .iter()
            .all(|c| c.note.is_none())
    );
    assert_eq!(
        repo.commit(&feature, Some("ai-lens"), &c())
            .unwrap()
            .note
            .as_deref(),
        Some("Why: replaced")
    );
    assert_eq!(
        repo.commits(&[&feature], Some("ai-lens"), &c()).unwrap()[0]
            .note
            .as_deref(),
        Some("Why: replaced")
    );
}

#[test]
fn notes_keep_unicode() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    let main = f.sha("main");
    repo.write_note(&main, "Warum: ü → 日本", "other", &c())
        .unwrap();

    assert_eq!(
        repo.read_note(&main, "other", &c()).unwrap().as_deref(),
        Some("Warum: ü → 日本")
    );
    assert_eq!(repo.read_note(&main, "ai-lens", &c()).unwrap(), None);
}

// -- patch IDs --

#[test]
fn gives_the_same_patch_id_to_the_same_change_on_different_branches() {
    let mut f = FixtureRepo::new();
    f.commit("Initial", files![("a.ts", "1\n2\n3\n")], Some("base"));
    f.branch("side", "HEAD");
    f.commit("Change", files![("a.ts", "1\n2\nthree\n")], Some("side"));
    f.checkout("main");
    f.commit("Other", files![("b.ts", "b\n")], Some("other"));
    f.commit(
        "Same change",
        files![("a.ts", "1\n2\nthree\n")],
        Some("main"),
    );
    f.merge("side", "Merges side", Some("merge"), None);
    let empty = f.commit("Empty", vec![], None);
    let repo = f.open();
    let (side, main, other, merge) = (f.sha("side"), f.sha("main"), f.sha("other"), f.sha("merge"));

    let ids = repo
        .patch_ids(&[&side, &main, &other, &merge, &empty], &c())
        .unwrap();

    assert_eq!(ids[&side].len(), 40);
    assert_eq!(ids[&main], ids[&side]);
    assert_ne!(ids[&other], ids[&side]);
    assert!(!ids.contains_key(&merge));
    assert!(!ids.contains_key(&empty));
    assert!(repo.patch_ids(&[], &c()).unwrap().is_empty());
}

// -- commit diffs --

#[test]
fn commit_diffs_of_a_root_commit() {
    let mut f = FixtureRepo::new();
    let root = f.commit("Initial", files![("a.txt", "a\n"), ("e.txt", "")], None);
    let repo = f.open();

    let diffs = repo.commit_diffs(&root, &c()).unwrap();

    assert_eq!(
        diffs
            .iter()
            .map(|(change, _)| change.clone())
            .collect::<Vec<_>>(),
        [
            GitFileChange::new(GitFileStatus::Added, "a.txt"),
            GitFileChange::new(GitFileStatus::Added, "e.txt"),
        ]
    );
    assert_eq!(
        non_context(&diffs[0].1),
        [line(DiffLineKind::Add, "a", None, Some(1))]
    );
    let empty = &diffs[1].1;
    assert_eq!(
        (
            empty.old_path.as_deref(),
            empty.new_path.as_deref(),
            empty.hunks.len()
        ),
        (None, Some("e.txt"), 0)
    );
}

#[test]
fn commit_diffs_with_renames_deletes_binary_files_and_mode_changes() {
    let mut f = FixtureRepo::new();
    f.commit(
        "Initial",
        files![
            ("old name.txt", "1\n2\n3\n4\n5\n"),
            ("gone.txt", "g\n"),
            ("img.bin", b"a\0b"),
            ("run.sh", "echo\n"),
        ],
        None,
    );
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            f.path.join("run.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    f.write_files(files![("img.bin", b"a\0c")]);
    let sha = f.commit_with(
        "Changes",
        files![
            ("new “name”.txt", "1\n2\n3\n4\n5\n6\n"),
            ("gone.txt", Content::Delete),
        ],
        CommitOptions::default().with_rename("old name.txt", "new “name”.txt"),
    );
    let repo = f.open();

    let diffs: HashMap<String, (GitFileChange, GitFileDiff)> = repo
        .commit_diffs(&sha, &c())
        .unwrap()
        .into_iter()
        .map(|(change, diff)| (change.path.clone(), (change, diff)))
        .collect();

    let mut paths: Vec<&String> = diffs.keys().collect();
    paths.sort();
    assert_eq!(paths, ["gone.txt", "img.bin", "new “name”.txt", "run.sh"]);
    let (change, diff) = &diffs["new “name”.txt"];
    assert_eq!(
        (change.status, change.original_path.as_deref()),
        (GitFileStatus::Renamed, Some("old name.txt"))
    );
    assert_eq!(
        (diff.old_path.as_deref(), diff.new_path.as_deref()),
        (Some("old name.txt"), Some("new “name”.txt"))
    );
    assert_eq!(
        non_context(diff),
        [line(DiffLineKind::Add, "6", None, Some(6))]
    );
    let (change, diff) = &diffs["gone.txt"];
    assert_eq!(
        (
            change.status,
            diff.old_path.as_deref(),
            diff.new_path.as_deref()
        ),
        (GitFileStatus::Deleted, Some("gone.txt"), None)
    );
    assert!(diffs["img.bin"].1.binary);
    let (change, diff) = &diffs["run.sh"];
    assert_eq!(
        (change.status, diff.new_path.as_deref(), diff.hunks.len()),
        (GitFileStatus::Modified, Some("run.sh"), 0)
    );
}

#[test]
fn commit_diffs_of_a_merge_are_against_its_first_parent() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);
    let merge = f.merge("feature", "Merges feature", None, None);

    let paths: Vec<String> = repo
        .commit_diffs(&merge, &c())
        .unwrap()
        .into_iter()
        .map(|(change, _)| change.path)
        .collect();
    assert_eq!(paths, ["a.ts"]);
}

// -- uncommitted changes and file contents --

#[test]
fn lists_counts_and_reads_staged_unstaged_and_untracked_changes() {
    let mut f = FixtureRepo::new();
    f.commit(
        "Initial",
        files![("a.ts", "1\n2\n"), (".gitignore", "out/\n")],
        Some("a"),
    );
    f.write_files(files![("a.ts", "1\nstaged\n")]);
    f.stage(&[]);
    f.write_files(files![
        ("a.ts", "1\nstaged\nunstaged\n"),
        ("new/u.ts", "u\nlast"),
        ("bin.dat", b"a\0b"),
        ("out/ignored.js", "x\n"),
    ]);
    let repo = f.open();
    let base = vec![f.sha("a")];
    let modified = GitFileChange::new(GitFileStatus::Modified, "a.ts");

    assert_eq!(
        repo.name_status(&base, true, None, &c()).unwrap(),
        std::slice::from_ref(&modified)
    );
    assert_eq!(
        repo.name_status(&base, false, None, &c()).unwrap(),
        [modified]
    );
    assert_eq!(
        repo.untracked_files(None, &c()).unwrap(),
        ["bin.dat", "new/u.ts"]
    );
    assert_eq!(
        repo.numstat(&base, false, None, &c()).unwrap(),
        [GitFileStats::new("a.ts", Some(2), Some(1))]
    );
    assert_eq!(
        repo.untracked_stats("bin.dat", None, &c()).unwrap(),
        GitFileStats::new("bin.dat", None, None)
    );
    assert_eq!(
        repo.untracked_stats("new/u.ts", None, &c()).unwrap(),
        GitFileStats::new("new/u.ts", Some(2), Some(0))
    );

    assert_eq!(
        repo.read_index_file("a.ts", None, &c()).unwrap().as_deref(),
        Some(&b"1\nstaged\n"[..])
    );
    assert_eq!(
        repo.read_worktree_file("a.ts", None, &c())
            .unwrap()
            .as_deref(),
        Some(&b"1\nstaged\nunstaged\n"[..])
    );
    assert_eq!(
        repo.read_blob(&base[0], "a.ts", &c()).unwrap().as_deref(),
        Some(&b"1\n2\n"[..])
    );
    assert_eq!(repo.read_blob(&base[0], "new/u.ts", &c()).unwrap(), None);
    assert_eq!(repo.read_index_file("new/u.ts", None, &c()).unwrap(), None);

    let untracked = repo
        .untracked_diff("new/u.ts", None, &c())
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            untracked.old_path.as_deref(),
            untracked.new_path.as_deref(),
            untracked.binary
        ),
        (None, Some("new/u.ts"), false)
    );
    assert_eq!(
        untracked.hunks[0].lines,
        [
            line(DiffLineKind::Add, "u", None, Some(1)),
            no_newline(line(DiffLineKind::Add, "last", None, Some(2))),
        ]
    );
    assert!(
        repo.untracked_diff("bin.dat", None, &c())
            .unwrap()
            .unwrap()
            .binary
    );

    let staged = repo
        .uncommitted_diffs(UncommittedLayer::Staged, None, None, &c())
        .unwrap();
    assert_eq!(staged.len(), 1);
    assert_eq!(
        non_context(&staged[0]),
        [
            line(DiffLineKind::Delete, "2", Some(2), None),
            line(DiffLineKind::Add, "staged", None, Some(2)),
        ]
    );
    let unstaged = repo
        .uncommitted_diffs(UncommittedLayer::Unstaged, None, None, &c())
        .unwrap();
    assert_eq!(unstaged.len(), 1);
    assert_eq!(
        non_context(&unstaged[0]),
        [line(DiffLineKind::Add, "unstaged", None, Some(3))]
    );

    assert_eq!(
        repo.ignored(&["a.ts", "out/ignored.js", "out/other.js"], None, &c())
            .unwrap(),
        HashSet::from(["out/ignored.js".to_string(), "out/other.js".to_string()])
    );
    assert!(repo.ignored(&["a.ts"], None, &c()).unwrap().is_empty());
    assert!(repo.ignored(&[], None, &c()).unwrap().is_empty());
}

#[test]
fn counts_untracked_lines_like_git() {
    let mut f = FixtureRepo::new();
    f.commit("Initial", files![("a.ts", "1\n")], None);
    let mut many: Vec<(String, Content)> = (0..40)
        .map(|index| {
            (
                format!("many/f{index:03}.txt"),
                Content::from("x\n".repeat(index)),
            )
        })
        .collect();
    many.extend(files![
        // NUL bytes only after the first 8000 bytes don't make a file binary
        ("late-nul.txt", "line\n".repeat(2000) + "\0\n"),
        ("empty.txt", ""),
        ("crlf.txt", "a\r\nb\r\n"),
    ]);
    f.write_files(many);
    std::os::unix::fs::symlink("a.ts", f.path.join("link")).unwrap();
    let repo = f.open();

    let stats: HashMap<String, GitFileStats> = repo
        .untracked_files(None, &c())
        .unwrap()
        .into_iter()
        .map(|path| {
            let stats = repo.untracked_stats(&path, None, &c()).unwrap();
            (path, stats)
        })
        .collect();

    let stat = |path: &str, lines: usize| GitFileStats::new(path, Some(lines), Some(0));
    assert_eq!(stats.len(), 44);
    assert_eq!(stats["many/f000.txt"], stat("many/f000.txt", 0));
    assert_eq!(stats["many/f039.txt"], stat("many/f039.txt", 39));
    assert_eq!(stats["late-nul.txt"], stat("late-nul.txt", 2001));
    assert_eq!(stats["empty.txt"], stat("empty.txt", 0));
    assert_eq!(stats["crlf.txt"], stat("crlf.txt", 2));
    assert_eq!(stats["link"], stat("link", 1));
    // Gone since it was listed
    assert_eq!(
        repo.untracked_stats("vanished.txt", None, &c()).unwrap(),
        stat("vanished.txt", 0)
    );
}

#[test]
fn untracked_diff_of_an_empty_file_and_a_symlink() {
    let mut f = FixtureRepo::new();
    f.commit("Initial", files![("a.ts", "1\n")], None);
    f.write_files(files![("empty.txt", "")]);
    std::os::unix::fs::symlink("/etc/passwd", f.path.join("link")).unwrap();
    let repo = f.open();

    let empty = repo
        .untracked_diff("empty.txt", None, &c())
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            empty.old_path.as_deref(),
            empty.new_path.as_deref(),
            empty.hunks.len()
        ),
        (None, Some("empty.txt"), 0)
    );
    let link = repo.untracked_diff("link", None, &c()).unwrap().unwrap();
    assert_eq!(
        non_context(&link),
        [no_newline(line(
            DiffLineKind::Add,
            "/etc/passwd",
            None,
            Some(1)
        ))]
    );
    assert_eq!(
        repo.untracked_diff("missing.txt", None, &c()).unwrap(),
        None
    );
}

#[test]
fn reads_worktree_files_without_following_links_out() {
    let mut f = FixtureRepo::new();
    f.commit(
        "Initial",
        files![("a.ts", "a\n"), ("dir/b.ts", "b\n")],
        None,
    );
    let outside = f.tmp_path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "secret\n").unwrap();
    std::os::unix::fs::symlink(outside.join("secret.txt"), f.path.join("file-link")).unwrap();
    std::os::unix::fs::symlink(&outside, f.path.join("dir-link")).unwrap();
    std::os::unix::fs::symlink("a.ts", f.path.join("inner-link")).unwrap();
    let repo = f.open();
    let read = |path: &str| repo.read_worktree_file(path, None, &c());

    assert_eq!(read("dir/b.ts").unwrap().as_deref(), Some(&b"b\n"[..]));
    // Links read as their target, as git stores them
    assert_eq!(
        read("file-link").unwrap(),
        Some(
            outside
                .join("secret.txt")
                .to_str()
                .unwrap()
                .as_bytes()
                .to_vec()
        )
    );
    assert_eq!(read("inner-link").unwrap().as_deref(), Some(&b"a.ts"[..]));
    assert_eq!(read("dir-link/secret.txt").unwrap(), None);
    assert_eq!(read("dir").unwrap(), None);
    assert_eq!(read("missing.txt").unwrap(), None);
    assert_eq!(read("a.ts/x").unwrap(), None);
    for bad in ["../outside/secret.txt", "/etc/passwd", "dir/../a.ts", ""] {
        assert!(read(bad).is_err(), "{bad:?} was read");
    }
}

#[test]
fn reads_files_of_another_worktree() {
    let mut f = FixtureRepo::new();
    f.commit("Initial", files![("a.ts", "main\n")], None);
    let other = f.worktree(&f.tmp_path().join("other"), "other", "HEAD");
    f.write_files_in(&other, files![("a.ts", "staged there\n")]);
    f.git_in(&other, &["add", "a.ts"]);
    f.write_files_in(&other, files![("a.ts", "on disk there\n")]);
    let repo = f.open();
    let there: Option<&Path> = Some(&other);

    assert_eq!(
        repo.read_index_file("a.ts", None, &c()).unwrap().as_deref(),
        Some(&b"main\n"[..])
    );
    assert_eq!(
        repo.read_index_file("a.ts", there, &c())
            .unwrap()
            .as_deref(),
        Some(&b"staged there\n"[..])
    );
    assert_eq!(
        repo.read_worktree_file("a.ts", there, &c())
            .unwrap()
            .as_deref(),
        Some(&b"on disk there\n"[..])
    );
    assert_eq!(
        repo.head(there, &c()).unwrap(),
        repo.head(None, &c()).unwrap()
    );
}

#[test]
fn reads_paths_with_newlines_quotes_and_unicode() {
    let names = [
        "new\nline.txt",
        "quo\"te.txt",
        "tab\there.txt",
        "日本/ü.txt",
        "-dash.txt",
    ];
    let mut f = FixtureRepo::new();
    let first = f.commit("Initial", files![("x", "x\n")], None);
    let sha = f.commit(
        "Odd names",
        names
            .iter()
            .enumerate()
            .map(|(index, name)| (name.to_string(), Content::from(format!("{index}\n"))))
            .collect(),
        None,
    );
    let repo = f.open();

    let diffs = repo.commit_diffs(&sha, &c()).unwrap();
    let mut paths: Vec<String> = diffs
        .iter()
        .map(|(change, _)| change.path.clone())
        .collect();
    paths.sort();
    let mut expected: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    expected.sort();
    assert_eq!(paths, expected);
    for (index, name) in names.iter().enumerate() {
        let content = format!("{index}\n").into_bytes();
        assert_eq!(
            repo.read_blob(&sha, name, &c()).unwrap(),
            Some(content.clone())
        );
        assert_eq!(
            repo.read_worktree_file(name, None, &c()).unwrap(),
            Some(content)
        );
    }
    let (_, diff) = diffs
        .iter()
        .find(|(change, _)| change.path == "new\nline.txt")
        .unwrap();
    assert_eq!(diff.new_path.as_deref(), Some("new\nline.txt"));
    let blamed = repo
        .blame(&first, &sha, "new\nline.txt", false, &c())
        .unwrap();
    assert_eq!(blamed.len(), 1);
    assert_eq!(
        (
            blamed[0].sha.as_str(),
            blamed[0].filename.as_str(),
            blamed[0].boundary
        ),
        (sha.as_str(), "new\nline.txt", false)
    );
}

#[test]
fn blame_of_a_path_the_target_lacks_is_empty() {
    let mut f = FixtureRepo::new();
    let repo = build_forked(&mut f);

    assert!(
        repo.blame(&f.sha("base"), &f.sha("feature"), "nope.ts", false, &c())
            .unwrap()
            .is_empty()
    );
}

/// Meld started by `git difftool` or a hook has GIT_DIR etc. set; Python
/// sets them in os.environ, here they go into the environment git is given
#[test]
fn meld_environment_git_variables_do_not_leak() {
    let mut f = FixtureRepo::new();
    f.commit("Adds a", files![("a.txt", "a\n")], None);
    let other = f.tmp_path().join("elsewhere");
    std::fs::create_dir(&other).unwrap();
    let options = options_with(
        &f,
        &[
            ("GIT_DIR", other.to_str().unwrap()),
            ("GIT_WORK_TREE", other.to_str().unwrap()),
        ],
    );
    let repo = GitRepository::new(&f.path, &options);
    assert_eq!(
        repo.head(None, &c()).unwrap(),
        Some(f.git(&["rev-parse", "HEAD"]).trim().to_string())
    );
}
