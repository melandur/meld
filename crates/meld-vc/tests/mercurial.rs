//! Integration tests of the Mercurial backend against real temporary
//! repositories.
//!
//! Needs `hg` on `PATH`; every test is skipped (passes without checking
//! anything) when it isn't. Last run against Mercurial 7.2.4 (installed
//! with `pip install mercurial`).

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Fixture, run_ok, take, write};
use meld_vc::{
    Action, DiffOptions, DiffRequest, State, StatusFilters, Vc, VcKind, VcRow, choose_vcs, get_vcs,
    prepare_diff, scan_tree,
};

fn hg(dir: &Path, args: &[&str]) -> String {
    common::tool(dir, "hg", args)
}

fn init_repo() -> Fixture {
    let fx = Fixture::new("repo");
    hg(&fx.root, &["init"]);
    fx
}

fn open(root: &Path) -> Box<dyn Vc> {
    let mut vc = VcKind::Mercurial.open(root).unwrap();
    vc.refresh_vc_state(None).unwrap();
    vc
}

/// A repository with one file in every state Mercurial reports.
fn status_repo() -> Fixture {
    let fx = init_repo();
    let root = &fx.root;
    for name in [
        "clean.txt",
        "modified.txt",
        "removed.txt",
        "missing.txt",
        "sub/nested.txt",
        "sub/clean.txt",
    ] {
        write(root, name, &format!("{name}\n"));
    }
    write(root, ".hgignore", "syntax: glob\n*.log\nbuild/\n");
    hg(root, &["add", "-q"]);
    hg(root, &["commit", "-q", "-m", "initial"]);

    write(root, "modified.txt", "changed\n");
    hg(root, &["rm", "-q", "removed.txt"]);
    fs::remove_file(root.join("missing.txt")).unwrap();
    write(root, "new.txt", "new\n");
    hg(root, &["add", "-q", "new.txt"]);
    write(root, "untracked.txt", "untracked\n");
    write(root, "quote\"d ü.txt", "odd name\n");
    write(root, "debug.log", "ignored\n");
    write(root, "build/out.o", "ignored\n");
    write(root, "sub/nested.txt", "nested change\n");
    fx
}

#[test]
fn discovery() {
    require_tool!("hg");
    eprintln!("testing {}", common::version("hg", &["--version"]));
    let fx = status_repo();
    let root = &fx.root;
    assert!(VcKind::Mercurial.is_installed());
    assert!(VcKind::Mercurial.valid_repo(root));
    let sub = root.join("sub");
    let (found_root, location) = VcKind::Mercurial.is_in_repo(&sub.join("nested.txt"));
    assert_eq!(found_root.as_deref(), Some(root.as_path()));
    assert_eq!(location, sub);

    let vcs = get_vcs(&sub);
    let hg_candidate = vcs.iter().find(|c| c.kind == VcKind::Mercurial).unwrap();
    assert_eq!(hg_candidate.root.as_deref(), Some(root.as_path()));

    let choices = choose_vcs(Some(&sub), None);
    assert_eq!(choices.choices[choices.active].kind, VcKind::Mercurial);
    assert_eq!(choices.choices[choices.active].label, "Mercurial");
    let vc = choices.open_active().unwrap();
    assert_eq!(vc.root(), root);
    assert_eq!(vc.location(), sub);

    assert!(!VcKind::Mercurial.valid_repo(fx._tmp.path()));
}

#[test]
fn statuses() {
    require_tool!("hg");
    let fx = status_repo();
    let root = &fx.root;
    let vc = open(root);
    let st = |name: &str| vc.get_entry(&root.join(name)).state;

    assert_eq!(st("clean.txt"), State::Normal);
    assert_eq!(st(".hgignore"), State::Normal);
    assert_eq!(st("modified.txt"), State::Modified);
    assert_eq!(st("removed.txt"), State::Removed);
    assert_eq!(st("missing.txt"), State::Missing);
    assert_eq!(st("new.txt"), State::New);
    assert_eq!(st("untracked.txt"), State::Unversioned);
    assert_eq!(st("quote\"d ü.txt"), State::Unversioned);
    assert_eq!(st("debug.log"), State::Ignored);
    assert_eq!(st("build/out.o"), State::Ignored);
    assert_eq!(st("sub/nested.txt"), State::Modified);
    assert_eq!(st("sub/clean.txt"), State::Normal);

    // Removed and missing entries are listed although they're gone from
    // disk; the .hg folder is not listed.
    let entries = vc.get_entries(root).unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"removed.txt") && names.contains(&"missing.txt"));
    assert!(!names.contains(&".hg"));
    let missing = entries.iter().find(|e| e.name == "missing.txt").unwrap();
    assert!(!missing.is_present());
    assert_eq!(missing.status(), "Missing");

    let tree = scan_tree(vc.as_ref(), root, &StatusFilters::default()).unwrap();
    let flat: Vec<String> = tree
        .children()
        .iter()
        .filter_map(VcRow::entry)
        .map(|e| e.name.clone())
        .collect();
    for n in [
        "modified.txt",
        "removed.txt",
        "missing.txt",
        "new.txt",
        "nested.txt",
    ] {
        assert!(flat.contains(&n.to_string()), "{n} missing from {flat:?}");
    }
    assert!(!flat.contains(&"clean.txt".to_string()));
}

#[test]
fn single_file_refresh_and_subfolder() {
    require_tool!("hg");
    let fx = status_repo();
    let root = &fx.root;
    let mut vc = open(root);
    let file = root.join("modified.txt");
    write(root, "modified.txt", "modified.txt\n");
    vc.refresh_vc_state(Some(&file)).unwrap();
    assert_eq!(vc.get_entry(&file).state, State::Normal);
    write(root, "modified.txt", "again\n");
    vc.refresh_vc_state(Some(&file)).unwrap();
    assert_eq!(vc.get_entry(&file).state, State::Modified);
    assert_eq!(vc.get_entry(&root.join("new.txt")).state, State::New);

    let sub = root.join("sub");
    let mut vc = VcKind::Mercurial.open(&sub).unwrap();
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&sub.join("nested.txt")).state, State::Modified);
    assert_eq!(vc.get_entry(&sub.join("clean.txt")).state, State::Normal);
    // Status is limited to the location
    assert_eq!(vc.get_entry(&file).state, State::Normal);
    let base = vc
        .get_path_for_repo_file(&sub.join("nested.txt"), None)
        .unwrap();
    assert_eq!(take(&base), "sub/nested.txt\n");
}

#[test]
fn base_revision_extraction() {
    require_tool!("hg");
    let fx = status_repo();
    let root = &fx.root;
    let vc = open(root);
    let base = vc
        .get_path_for_repo_file(&root.join("modified.txt"), None)
        .unwrap();
    assert!(base.to_string_lossy().ends_with(".txt"));
    assert_eq!(take(&base), "modified.txt\n");
    let base = vc
        .get_path_for_repo_file(&root.join("removed.txt"), None)
        .unwrap();
    assert_eq!(take(&base), "removed.txt\n");
    // Not in the parent revision: an empty file
    let base = vc
        .get_path_for_repo_file(&root.join("new.txt"), None)
        .unwrap();
    assert_eq!(take(&base), "");
    assert!(
        vc.get_path_for_repo_file(&root.join("modified.txt"), Some("0"))
            .is_err()
    );
    assert!(
        vc.get_path_for_repo_file(Path::new("/definitely/elsewhere.txt"), None)
            .is_err()
    );

    let DiffRequest::Files {
        paths, temp_files, ..
    } = prepare_diff(
        vc.as_ref(),
        &root.join("modified.txt"),
        &DiffOptions::default(),
    )
    .unwrap()
    else {
        panic!("expected a file comparison")
    };
    assert_eq!(fs::read_to_string(&paths[0]).unwrap(), "modified.txt\n");
    assert_eq!(paths[1], root.join("modified.txt"));
    let mut temps = meld_vc::TempFiles::new();
    temps.register(temp_files);
}

#[test]
fn valid_actions() {
    require_tool!("hg");
    let fx = status_repo();
    let root = &fx.root;
    let vc = open(root);
    let v = vc.get_valid_actions(&[(root.join("modified.txt"), State::Modified)]);
    for a in [
        Action::Compare,
        Action::Commit,
        Action::Add,
        Action::Remove,
        Action::Revert,
        Action::Update,
    ] {
        assert!(v.contains(&a), "{a}");
    }
    // Not implemented for Mercurial
    for a in [Action::Push, Action::Resolve, Action::Unstage] {
        assert!(!v.contains(&a), "{a}");
    }
}

#[test]
fn add_commit_remove_revert() {
    require_tool!("hg");
    let fx = status_repo();
    let root = &fx.root;
    let mut vc = open(root);
    let untracked = root.join("untracked.txt");
    let odd = root.join("quote\"d ü.txt");

    run_ok(vc.add(&[untracked.clone(), odd.clone()]).unwrap());
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&untracked).state, State::New);
    assert_eq!(vc.get_entry(&odd).state, State::New);

    // Commit only some files, with a multi-line message
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
    assert_eq!(vc.get_entry(&odd).state, State::New);
    assert_eq!(
        vc.get_entry(&root.join("modified.txt")).state,
        State::Modified
    );
    assert_eq!(
        hg(root, &["log", "-l", "1", "--template", "{desc}"]),
        "Add files\n\nBody"
    );

    run_ok(vc.remove(&[root.join("clean.txt")]).unwrap());
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&root.join("clean.txt")).state, State::Removed);
    assert!(!root.join("clean.txt").exists());

    // Revert a modified, a removed and a missing file
    run_ok(
        vc.revert(&[
            root.join("modified.txt"),
            root.join("clean.txt"),
            root.join("missing.txt"),
        ])
        .unwrap(),
    );
    vc.refresh_vc_state(None).unwrap();
    for name in ["modified.txt", "clean.txt", "missing.txt"] {
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
}

#[test]
fn update_pulls_from_parent() {
    require_tool!("hg");
    let fx = init_repo();
    let upstream = &fx.root;
    write(upstream, "a.txt", "one\n");
    hg(upstream, &["add", "-q"]);
    hg(upstream, &["commit", "-q", "-m", "one"]);
    let clone: PathBuf = fx.sibling("clone");
    hg(
        fx._tmp.path(),
        &["clone", "-q", &upstream.to_string_lossy(), "clone"],
    );
    write(upstream, "a.txt", "two\n");
    hg(upstream, &["commit", "-q", "-m", "two"]);

    let vc = open(&clone);
    let cmds = vc.update().unwrap();
    assert_eq!(cmds[0].argv(), vec!["pull", "-u"]);
    assert_eq!(cmds[0].working_dir, clone);
    run_ok(cmds);
    assert_eq!(fs::read_to_string(clone.join("a.txt")).unwrap(), "two\n");
}
