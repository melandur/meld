//! Integration tests of the Darcs backend against real temporary
//! repositories.
//!
//! Needs `darcs` >= 2.10.3 on `PATH`; every test is skipped (passes
//! without checking anything) when it isn't. Last run against Darcs 2.18.4
//! (Debian's `darcs_2.18.4-1` binary).
//!
//! `darcs whatsnew` doesn't report conflicts (darcs issue2138), so
//! conflicted files show as modified and are compared with the recorded
//! version; boring (ignored) files aren't reported either.

mod common;

use std::fs;
use std::path::Path;

use common::{Fixture, run_ok, take, write};
use meld_vc::{Action, State, StatusFilters, Vc, VcKind, VcRow, choose_vcs, scan_tree};

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
