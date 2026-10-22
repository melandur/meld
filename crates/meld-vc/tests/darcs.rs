//! Integration tests of the Darcs backend against real temporary
//! repositories.
//!
//! Needs `darcs` >= 2.10.3 on `PATH`; every test is skipped (passes
//! without checking anything) when it isn't. Last run against Darcs 2.18.4
//! (Debian's `darcs_2.18.4-1` binary).
//!
//! `darcs whatsnew` reports neither conflicts (darcs issue2138) nor boring
//! (ignored) files; the backend finds conflicts by their markup and boring
//! files by the boring file's rules, checked here against what Darcs does.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Fixture, run_ok, take, write};
use meld_vc::{
    Action, ConflictSide, DiffOptions, DiffRequest, MergeFileOrder, State, StatusFilters, Vc,
    VcKind, VcRow, choose_vcs, prepare_diff, scan_tree,
};

fn darcs(dir: &Path, args: &[&str]) -> String {
    common::tool(dir, "darcs", args)
}

fn init_repo() -> Fixture {
    let fx = Fixture::new("repo");
    darcs(&fx.root, &["init", "-q"]);
    fx
}

fn open(root: &Path) -> Box<dyn Vc> {
    let mut vc = VcKind::Darcs.open(root).unwrap();
    vc.refresh_vc_state(None).unwrap();
    vc
}

/// A repository with one file in every state Darcs reports.
fn status_repo() -> Fixture {
    let fx = init_repo();
    let root = &fx.root;
    for name in [
        "clean.txt",
        "modified.txt",
        "removed.txt",
        "deleted.txt",
        "renamed.txt",
        "sub/nested.txt",
        "sub/clean.txt",
    ] {
        write(root, name, &format!("{name}\n"));
    }
    darcs(root, &["add", "-q", "-r", "."]);
    darcs(root, &["record", "-q", "-a", "-m", "initial"]);

    write(root, "modified.txt", "changed\n");
    // `darcs remove` keeps the file on disk
    darcs(root, &["remove", "-q", "removed.txt"]);
    fs::remove_file(root.join("deleted.txt")).unwrap();
    darcs(root, &["move", "-q", "renamed.txt", "renamed2.txt"]);
    write(root, "new.txt", "new\n");
    darcs(root, &["add", "-q", "new.txt"]);
    write(root, "untracked.txt", "untracked\n");
    write(root, "quote\"d ü.txt", "odd name\n");
    write(root, "unknown-dir/file.txt", "unknown\n");
    write(root, "sub/nested.txt", "nested change\n");
    fx
}

#[test]
fn discovery() {
    require_tool!("darcs");
    eprintln!("testing darcs {}", common::version("darcs", &["--version"]));
    let fx = status_repo();
    let root = &fx.root;
    assert!(VcKind::Darcs.is_installed());
    assert!(VcKind::Darcs.valid_repo(root));
    let sub = root.join("sub");
    assert_eq!(
        VcKind::Darcs.is_in_repo(&sub).0.as_deref(),
        Some(root.as_path())
    );
    let choices = choose_vcs(Some(&sub), None);
    assert_eq!(choices.choices[choices.active].kind, VcKind::Darcs);
    assert_eq!(choices.choices[choices.active].label, "Darcs");
    let vc = choices.open_active().unwrap();
    assert_eq!(vc.root(), root);
    assert_eq!(vc.location(), sub);
}

#[test]
fn statuses() {
    require_tool!("darcs");
    let fx = status_repo();
    let root = &fx.root;
    let vc = open(root);
    let entry = |name: &str| vc.get_entry(&root.join(name));
    let st = |name: &str| entry(name).state;

    assert_eq!(st("clean.txt"), State::Normal);
    assert_eq!(st("modified.txt"), State::Modified);
    assert_eq!(st("removed.txt"), State::Removed);
    assert_eq!(st("deleted.txt"), State::Removed);
    assert_eq!(st("renamed.txt"), State::NonExist);
    assert_eq!(st("renamed2.txt"), State::Renamed);
    assert_eq!(entry("renamed2.txt").options, "renamed.txt ➡ renamed2.txt");
    assert_eq!(st("new.txt"), State::New);
    assert_eq!(st("untracked.txt"), State::Unversioned);
    assert_eq!(st("quote\"d ü.txt"), State::Unversioned);
    assert_eq!(st("unknown-dir"), State::Unversioned);
    assert_eq!(st("sub/nested.txt"), State::Modified);
    assert_eq!(st("sub/clean.txt"), State::Normal);
    // Nothing else is reported (no paths joined to the wrong folder)
    assert_eq!(vc.core().cache.states.len(), 11, "{:?}", vc.core().cache);

    // Each name is listed once, whether or not it's still on disk
    let entries = vc.get_entries(root).unwrap();
    let mut names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    names.sort();
    let len = names.len();
    names.dedup();
    assert_eq!(names.len(), len, "{names:?}");
    assert!(names.contains(&"removed.txt") && names.contains(&"deleted.txt"));
    assert!(!names.contains(&"_darcs"));

    // A sub-folder location sees the same (repository-relative) states
    let sub = root.join("sub");
    let mut vc = VcKind::Darcs.open(&sub).unwrap();
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&sub.join("nested.txt")).state, State::Modified);
    assert_eq!(vc.get_entry(&sub.join("clean.txt")).state, State::Normal);
    let tree = scan_tree(vc.as_ref(), &sub, &StatusFilters::default()).unwrap();
    let flat: Vec<String> = tree
        .children()
        .iter()
        .filter_map(VcRow::entry)
        .map(|e| e.name.clone())
        .collect();
    assert_eq!(flat, vec!["nested.txt"]);
}

#[test]
fn single_file_refresh() {
    require_tool!("darcs");
    let fx = init_repo();
    let root = &fx.root;
    write(root, "a.txt", "a\n");
    darcs(root, &["add", "-q", "a.txt"]);
    darcs(root, &["record", "-q", "-a", "-m", "a"]);
    write(root, "a.txt", "changed\n");
    let mut vc = open(root);
    let file = root.join("a.txt");
    assert_eq!(vc.get_entry(&file).state, State::Modified);
    // Back to the recorded content: darcs prints "No changes!"
    write(root, "a.txt", "a\n");
    vc.refresh_vc_state(Some(&file)).unwrap();
    assert_eq!(vc.get_entry(&file).state, State::Normal);
    assert!(vc.core().cache.states.is_empty(), "{:?}", vc.core().cache);
}

#[test]
fn base_revision_extraction() {
    require_tool!("darcs");
    let fx = status_repo();
    let root = &fx.root;
    let vc = open(root);
    let base = |name: &str| take(&vc.get_path_for_repo_file(&root.join(name), None).unwrap());
    assert_eq!(base("modified.txt"), "modified.txt\n");
    assert_eq!(base("sub/nested.txt"), "sub/nested.txt\n");
    assert_eq!(base("deleted.txt"), "deleted.txt\n");
    // Moved files are looked up by their recorded name
    assert_eq!(base("renamed2.txt"), "renamed.txt\n");
    assert_eq!(base("new.txt"), "");

    let sub = root.join("sub");
    let mut vc = VcKind::Darcs.open(&sub).unwrap();
    vc.refresh_vc_state(None).unwrap();
    let p = vc
        .get_path_for_repo_file(&sub.join("nested.txt"), None)
        .unwrap();
    assert_eq!(take(&p), "sub/nested.txt\n");
    assert!(
        vc.get_path_for_repo_file(&sub.join("nested.txt"), Some("x"))
            .is_err()
    );
    assert!(
        vc.get_path_for_repo_file(Path::new("/definitely/elsewhere.txt"), None)
            .is_err()
    );
}

#[test]
fn add_commit_remove_revert() {
    require_tool!("darcs");
    let fx = status_repo();
    let root = &fx.root;
    let mut vc = open(root);
    let untracked = root.join("untracked.txt");

    run_ok(
        vc.add(&[untracked.clone(), root.join("unknown-dir")])
            .unwrap(),
    );
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&untracked).state, State::New);
    assert_eq!(
        vc.get_entry(&root.join("unknown-dir/file.txt")).state,
        State::New
    );

    // Only the selected files are recorded; a multi-line message becomes
    // the patch name and long comment.
    run_ok(
        vc.commit(
            &[root.join("new.txt"), untracked.clone()],
            "Add files\n\nBody",
        )
        .unwrap(),
    );
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&root.join("new.txt")).state, State::Normal);
    assert_eq!(vc.get_entry(&untracked).state, State::Normal);
    assert_eq!(
        vc.get_entry(&root.join("modified.txt")).state,
        State::Modified
    );
    assert_eq!(
        vc.get_entry(&root.join("unknown-dir/file.txt")).state,
        State::New
    );
    let log = darcs(root, &["log", "--last", "1"]);
    assert!(log.contains("* Add files\n"), "{log}");
    assert!(log.contains("  Body"), "{log}");

    run_ok(vc.remove(&[root.join("clean.txt")]).unwrap());
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&root.join("clean.txt")).state, State::Removed);

    run_ok(
        vc.revert(&[
            root.join("modified.txt"),
            root.join("clean.txt"),
            root.join("deleted.txt"),
        ])
        .unwrap(),
    );
    vc.refresh_vc_state(None).unwrap();
    for name in ["modified.txt", "clean.txt", "deleted.txt"] {
        assert_eq!(
            vc.get_entry(&root.join(name)).state,
            State::Normal,
            "{name}"
        );
        assert_eq!(
            fs::read_to_string(root.join(name)).unwrap(),
            format!("{name}\n")
        );
    }

    // Committing the repository root records everything that's left
    run_ok(vc.commit(std::slice::from_ref(root), "Everything").unwrap());
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(
        vc.get_entry(&root.join("sub/nested.txt")).state,
        State::Normal
    );
    assert_eq!(
        vc.get_entry(&root.join("renamed2.txt")).state,
        State::Normal
    );

    let v = vc.get_valid_actions(&[(root.join("modified.txt"), State::Modified)]);
    assert!(v.contains(&Action::Push) && v.contains(&Action::Update));
    assert!(!v.contains(&Action::Resolve) && !v.contains(&Action::Unstage));
}

/// Two clones whose `conflict.txt` and `sub/nested.txt` changes conflict;
/// `merged.txt` changes on one side only. Returns the fixture with
/// clones at `root` ("main") and `other`.
fn conflicting_clones() -> (Fixture, PathBuf) {
    let fx = init_repo();
    let root = &fx.root;
    write(root, "conflict.txt", "a\nb\nc\nd\ne\n");
    write(root, "sub/nested.txt", "1\n2\n3\n");
    write(root, "merged.txt", "x\n");
    darcs(root, &["add", "-q", "-r", "."]);
    darcs(root, &["record", "-q", "-a", "-m", "base"]);
    darcs(fx._tmp.path(), &["clone", "-q", "repo", "other"]);
    let other = fx.sibling("other");
    write(&other, "conflict.txt", "a\nOTHER\nc\nd\nE\n");
    write(&other, "sub/nested.txt", "1\nOTHER\n3\n");
    write(&other, "merged.txt", "y\n");
    darcs(&other, &["record", "-q", "-a", "-m", "other"]);
    write(root, "conflict.txt", "a\nMAIN\nc\nd\ne\n");
    write(root, "sub/nested.txt", "1\nMAIN\n3\n");
    darcs(root, &["record", "-q", "-a", "-m", "main"]);
    (fx, other)
}

fn conflict_side(vc: &dyn Vc, file: &Path, side: ConflictSide) -> String {
    let (p, is_temp) = vc.get_path_for_conflict(file, side).unwrap();
    assert!(is_temp);
    take(&p)
}

#[test]
fn conflicts() {
    require_tool!("darcs");
    let (fx, other) = conflicting_clones();
    let root = &fx.root;
    let other_s = other.to_string_lossy().into_owned();
    darcs(root, &["pull", "-q", "-a", &other_s]);
    let mut vc = open(root);
    let file = root.join("conflict.txt");
    assert!(fs::read_to_string(&file).unwrap().contains("v v v v v v v"));
    assert_eq!(vc.get_entry(&file).state, State::Conflict);
    assert_eq!(
        vc.get_entry(&root.join("sub/nested.txt")).state,
        State::Conflict
    );
    assert_eq!(vc.get_entry(&root.join("merged.txt")).state, State::Normal);
    // Darcs' backup of the file is boring
    assert_eq!(
        vc.get_entry(&root.join("conflict.txt.~0~")).state,
        State::Ignored
    );
    assert!(
        vc.get_valid_actions(&[(file.clone(), State::Conflict)])
            .contains(&Action::Resolve)
    );

    assert_eq!(
        conflict_side(vc.as_ref(), &file, ConflictSide::Base),
        "a\nb\nc\nd\nE\n"
    );
    assert_eq!(
        conflict_side(vc.as_ref(), &file, ConflictSide::Local),
        "a\nMAIN\nc\nd\nE\n"
    );
    assert_eq!(
        conflict_side(vc.as_ref(), &file, ConflictSide::Remote),
        "a\nOTHER\nc\nd\nE\n"
    );
    assert_eq!(
        vc.get_path_for_conflict(&file, ConflictSide::Merged)
            .unwrap(),
        (file.clone(), false)
    );

    let opts = DiffOptions {
        merge_file_order: MergeFileOrder::LocalMergeRemote,
        ..Default::default()
    };
    let DiffRequest::Files {
        paths,
        merge_output,
        temp_files,
        ..
    } = prepare_diff(vc.as_ref(), &file, &opts).unwrap()
    else {
        panic!("expected a file comparison")
    };
    assert_eq!(fs::read_to_string(&paths[0]).unwrap(), "a\nMAIN\nc\nd\nE\n");
    assert_eq!(paths[1], file);
    assert_eq!(
        fs::read_to_string(&paths[2]).unwrap(),
        "a\nOTHER\nc\nd\nE\n"
    );
    assert_eq!(merge_output, Some(file.clone()));
    let mut temps = meld_vc::TempFiles::new();
    temps.register(temp_files);

    // Resolve refuses files with markup; once it's gone the file is
    // modified, and recording it records the resolution.
    let err = vc.resolve(std::slice::from_ref(&file)).unwrap_err();
    assert!(err.to_string().contains("conflict markers"), "{err}");
    write(root, "conflict.txt", "a\nRESOLVED\nc\nd\nE\n");
    run_ok(vc.resolve(std::slice::from_ref(&file)).unwrap());
    vc.refresh_vc_state(Some(&file)).unwrap();
    assert_eq!(vc.get_entry(&file).state, State::Modified);
    write(root, "sub/nested.txt", "1\nRESOLVED\n3\n");
    run_ok(vc.commit(std::slice::from_ref(root), "Resolve").unwrap());
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&file).state, State::Normal);
    assert_eq!(fs::read_to_string(&file).unwrap(), "a\nRESOLVED\nc\nd\nE\n");
}

/// Darcs writes the conflicting versions in its own order, not local
/// first; the local one is recognised with Darcs' backup of the file.
#[test]
fn conflicts_pulled_the_other_way() {
    require_tool!("darcs");
    let (fx, other) = conflicting_clones();
    let root_s = fx.root.to_string_lossy().into_owned();
    darcs(&other, &["pull", "-q", "-a", &root_s]);
    let vc = open(&other);
    let file = other.join("conflict.txt");
    assert_eq!(vc.get_entry(&file).state, State::Conflict);
    let markup = fs::read_to_string(&file).unwrap();
    assert!(
        markup.find("MAIN") < markup.find("OTHER"),
        "Darcs no longer writes the versions in the same order: {markup}"
    );
    assert_eq!(
        conflict_side(vc.as_ref(), &file, ConflictSide::Local),
        "a\nOTHER\nc\nd\nE\n"
    );
    assert_eq!(
        conflict_side(vc.as_ref(), &file, ConflictSide::Remote),
        "a\nMAIN\nc\nd\nE\n"
    );
    let nested = other.join("sub/nested.txt");
    assert_eq!(
        conflict_side(vc.as_ref(), &nested, ConflictSide::Local),
        "1\nOTHER\n3\n"
    );
}

#[test]
fn boring_files() {
    require_tool!("darcs");
    let fx = init_repo();
    let root = &fx.root;
    write(root, "a.txt", "a\n");
    write(root, "kept.o", "recorded although boring\n");
    darcs(root, &["add", "-q", "a.txt"]);
    darcs(root, &["add", "-q", "--boring", "kept.o"]);
    darcs(root, &["record", "-q", "-a", "-m", "initial"]);
    let mut boring = fs::read_to_string(root.join("_darcs/prefs/boring")).unwrap();
    boring.push_str("^build(/|$)\n\\.log$\n");
    fs::write(root.join("_darcs/prefs/boring"), boring).unwrap();
    for name in [
        "x.o",
        "debug.log",
        "sub/nested.log",
        "sub/notes.txt",
        "build/out.txt",
        "backup.txt~",
        ".git/HEAD",
        "unknown-dir/q.o",
        "unknown-dir/q.txt",
    ] {
        write(root, name, "x\n");
    }
    write(root, "kept.o", "changed\n");

    let vc = open(root);
    let st = |name: &str| vc.get_entry(&root.join(name)).state;
    for name in [
        "x.o",
        "debug.log",
        "sub/nested.log",
        "build",
        "backup.txt~",
        ".git",
        "unknown-dir/q.o",
    ] {
        assert_eq!(st(name), State::Ignored, "{name}");
    }
    // Boring folders aren't entered
    assert_eq!(st("build/out.txt"), State::Normal);
    assert_eq!(st("sub/notes.txt"), State::Unversioned);
    assert_eq!(st("unknown-dir/q.txt"), State::Unversioned);
    assert_eq!(st("kept.o"), State::Modified);
    assert_eq!(st("a.txt"), State::Normal);

    // Darcs agrees on what isn't boring
    let whatsnew = darcs(root, &["whatsnew", "-sl", "--machine-readable"]);
    let mut listed: Vec<&str> = whatsnew
        .lines()
        .filter_map(|l| l.strip_prefix("a ./"))
        .collect();
    listed.sort();
    assert_eq!(
        listed,
        vec!["sub/", "sub/notes.txt", "unknown-dir/", "unknown-dir/q.txt"]
    );

    // A sub-folder location
    let sub = root.join("sub");
    let mut vc = VcKind::Darcs.open(&sub).unwrap();
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&sub.join("nested.log")).state, State::Ignored);
}

#[test]
fn update_and_push() {
    require_tool!("darcs");
    let fx = init_repo();
    let upstream = &fx.root;
    write(upstream, "a.txt", "one\n");
    darcs(upstream, &["add", "-q", "a.txt"]);
    darcs(upstream, &["record", "-q", "-a", "-m", "one"]);
    darcs(fx._tmp.path(), &["clone", "-q", "repo", "clone"]);
    let clone = fx.sibling("clone");
    write(upstream, "a.txt", "two\n");
    darcs(upstream, &["record", "-q", "-a", "-m", "two"]);

    let vc = open(&clone);
    run_ok(vc.update().unwrap());
    assert_eq!(fs::read_to_string(clone.join("a.txt")).unwrap(), "two\n");

    write(&clone, "b.txt", "b\n");
    darcs(&clone, &["add", "-q", "b.txt"]);
    darcs(&clone, &["record", "-q", "-a", "-m", "three"]);
    run_ok(vc.push().unwrap());
    assert_eq!(fs::read_to_string(upstream.join("b.txt")).unwrap(), "b\n");
}
