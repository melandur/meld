//! Integration tests of the Bazaar backend against real temporary
//! branches.
//!
//! Needs `bzr` or Breezy's `brz` on `PATH`; every test is skipped (passes
//! without checking anything) when neither is. Last run against Breezy
//! 3.3.23 (`brz`, installed with `pip install breezy`).

mod common;

use std::fs;
use std::path::Path;

use common::{Fixture, run_ok, take, write};
use meld_vc::{
    Action, ConflictSide, DiffOptions, DiffRequest, MergeFileOrder, State, Vc, VcKind, choose_vcs,
    prepare_diff,
};

fn bzr(dir: &Path, args: &[&str]) -> String {
    let cmd = VcKind::Bazaar.cmd().unwrap();
    let mut full = vec!["--no-aliases", "--no-plugins"];
    full.extend_from_slice(args);
    common::tool(dir, cmd, &full)
}

fn init_branch() -> Fixture {
    let fx = Fixture::new("repo");
    bzr(&fx.root, &["init", "-q"]);
    fx
}

fn open(root: &Path) -> Box<dyn Vc> {
    let mut vc = VcKind::Bazaar.open(root).unwrap();
    vc.refresh_vc_state(None).unwrap();
    vc
}

/// A branch with one file in every state Bazaar reports.
fn status_branch() -> Fixture {
    let fx = init_branch();
    let root = &fx.root;
    for name in [
        "clean.txt",
        "modified.txt",
        "removed.txt",
        "missing.txt",
        "renamed.txt",
        "exec.sh",
        "sub/nested.txt",
        "sub/clean.txt",
    ] {
        write(root, name, &format!("{name}\n"));
    }
    bzr(root, &["add", "-q"]);
    bzr(root, &["commit", "-q", "-m", "initial"]);

    write(root, "modified.txt", "changed\n");
    bzr(root, &["rm", "-q", "removed.txt"]);
    fs::remove_file(root.join("missing.txt")).unwrap();
    bzr(root, &["mv", "-q", "renamed.txt", "renamed2.txt"]);
    write(root, "new.txt", "new\n");
    bzr(root, &["add", "-q", "new.txt"]);
    write(root, "untracked.txt", "untracked\n");
    write(root, "quote\"d ü.txt", "odd name\n");
    write(root, "unknown-dir/file.txt", "unknown\n");
    write(root, "sub/nested.txt", "nested change\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.join("exec.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    fx
}

#[test]
fn discovery() {
    require_tool!("bzr", "brz");
    let cmd = VcKind::Bazaar.cmd().unwrap();
    eprintln!("testing {}", common::version(cmd, &["--version"]));
    let fx = status_branch();
    let root = &fx.root;
    assert!(VcKind::Bazaar.is_installed());
    assert!(VcKind::Bazaar.valid_repo(root));
    let sub = root.join("sub");
    assert_eq!(
        VcKind::Bazaar.is_in_repo(&sub).0.as_deref(),
        Some(root.as_path())
    );
    let choices = choose_vcs(Some(&sub), None);
    assert_eq!(choices.choices[choices.active].kind, VcKind::Bazaar);
    assert_eq!(choices.choices[choices.active].label, "Bazaar");
    let vc = choices.open_active().unwrap();
    assert_eq!(vc.root(), root);
    assert_eq!(vc.location(), sub);
}

#[test]
fn statuses() {
    require_tool!("bzr", "brz");
    let fx = status_branch();
    let root = &fx.root;
    let vc = open(root);
    let entry = |name: &str| vc.get_entry(&root.join(name));
    let st = |name: &str| entry(name).state;

    assert_eq!(st("clean.txt"), State::Normal);
    assert_eq!(st("modified.txt"), State::Modified);
    assert_eq!(st("removed.txt"), State::Removed);
    assert_eq!(st("missing.txt"), State::Missing);
    assert_eq!(st("renamed2.txt"), State::Renamed);
    assert_eq!(entry("renamed2.txt").options, "renamed.txt ➡ renamed2.txt");
    assert_eq!(st("renamed.txt"), State::Normal);
    assert_eq!(st("new.txt"), State::New);
    assert_eq!(st("untracked.txt"), State::Unversioned);
    assert_eq!(st("quote\"d ü.txt"), State::Unversioned);
    assert_eq!(st("unknown-dir"), State::Unversioned);
    assert_eq!(st("sub/nested.txt"), State::Modified);
    assert_eq!(st("sub/clean.txt"), State::Normal);
    #[cfg(unix)]
    {
        assert_eq!(st("exec.sh"), State::Modified);
        assert_eq!(entry("exec.sh").options, "(properties changed: -x to +x)");
    }

    let entries = vc.get_entries(root).unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"removed.txt") && names.contains(&"missing.txt"));
    assert!(!names.contains(&".bzr"));
    assert!(!names.contains(&"renamed.txt"));

    // Files to commit for a folder and a file
    assert_eq!(
        vc.get_files_to_commit(&[root.join("sub"), root.join("new.txt")])
            .unwrap(),
        vec!["new.txt", "sub/nested.txt"]
    );
}

#[test]
fn single_file_refresh() {
    require_tool!("bzr", "brz");
    let fx = status_branch();
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
}

#[test]
fn base_revision_extraction() {
    require_tool!("bzr", "brz");
    let fx = status_branch();
    let root = &fx.root;
    let vc = open(root);
    let base = |name: &str, rev: Option<&str>| {
        take(&vc.get_path_for_repo_file(&root.join(name), rev).unwrap())
    };
    assert_eq!(base("modified.txt", None), "modified.txt\n");
    assert_eq!(base("sub/nested.txt", None), "sub/nested.txt\n");
    // Renamed files are looked up by their new name
    assert_eq!(base("renamed2.txt", None), "renamed.txt\n");
    assert_eq!(base("new.txt", None), "");

    bzr(root, &["commit", "-q", "-m", "second"]);
    assert_eq!(base("modified.txt", None), "changed\n");
    assert_eq!(base("modified.txt", Some("1")), "modified.txt\n");
    assert!(
        vc.get_path_for_repo_file(Path::new("/definitely/elsewhere.txt"), None)
            .is_err()
    );
}

/// A branch with a conflicting merge of `conflict.txt` pending.
fn conflict_branch() -> Fixture {
    let fx = init_branch();
    let root = &fx.root;
    write(root, "conflict.txt", "a\nb\nc\n");
    write(root, "other.txt", "other\n");
    bzr(root, &["add", "-q"]);
    bzr(root, &["commit", "-q", "-m", "base"]);
    bzr(fx._tmp.path(), &["branch", "-q", "repo", "other"]);
    let other = fx.sibling("other");
    write(&other, "conflict.txt", "a\nOTHER\nc\n");
    bzr(&other, &["commit", "-q", "-m", "other"]);
    write(root, "conflict.txt", "a\nMAIN\nc\n");
    bzr(root, &["commit", "-q", "-m", "main"]);
    let out = common::run_in(
        root,
        VcKind::Bazaar.cmd().unwrap(),
        &["merge", "-q", "../other"],
        true,
    );
    assert!(!out.status.success(), "merge should conflict");
    fx
}

#[test]
fn conflicts() {
    require_tool!("bzr", "brz");
    let fx = conflict_branch();
    let root = &fx.root;
    let mut vc = open(root);
    let file = root.join("conflict.txt");
    assert_eq!(vc.get_entry(&file).state, State::Conflict);
    assert_eq!(
        vc.get_entry(&root.join("conflict.txt.BASE")).state,
        State::Unversioned
    );
    assert!(
        vc.get_valid_actions(&[(file.clone(), State::Conflict)])
            .contains(&Action::Resolve)
    );

    let side = |side| {
        let (p, is_temp) = vc.get_path_for_conflict(&file, side).unwrap();
        assert!(!is_temp, "bzr owns its conflict files");
        fs::read_to_string(p).unwrap()
    };
    assert_eq!(side(ConflictSide::Base), "a\nb\nc\n");
    assert_eq!(side(ConflictSide::Local), "a\nMAIN\nc\n");
    assert_eq!(side(ConflictSide::Remote), "a\nOTHER\nc\n");
    assert!(side(ConflictSide::Merged).contains("<<<<<<<"));

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
    assert_eq!(fs::read_to_string(&paths[0]).unwrap(), "a\nMAIN\nc\n");
    assert_eq!(paths[1], file);
    assert_eq!(fs::read_to_string(&paths[2]).unwrap(), "a\nOTHER\nc\n");
    assert_eq!(merge_output, Some(file.clone()));
    assert!(temp_files.is_empty());

    // Resolve, then commit: while the merge is pending the whole tree is
    // committed, whatever the selection.
    write(root, "conflict.txt", "a\nRESOLVED\nc\n");
    run_ok(vc.resolve(std::slice::from_ref(&file)).unwrap());
    assert!(!root.join("conflict.txt.BASE").exists());
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&file).state, State::Modified);
    let cmds = vc
        .commit(std::slice::from_ref(&file), "Merge other")
        .unwrap();
    assert!(!cmds[0].argv().contains(&"conflict.txt".to_string()));
    run_ok(cmds);
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&file).state, State::Normal);
}

#[test]
fn add_commit_remove_revert() {
    require_tool!("bzr", "brz");
    let fx = status_branch();
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

    // Only the selected files are committed
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
    let log = bzr(root, &["log", "-l", "1"]);
    assert!(log.contains("  Add files\n  \n  Body"), "{log}");

    run_ok(vc.remove(&[root.join("clean.txt")]).unwrap());
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&root.join("clean.txt")).state, State::Removed);
    assert!(!root.join("clean.txt").exists());

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

    let v = vc.get_valid_actions(&[(root.join("modified.txt"), State::Modified)]);
    assert!(v.contains(&Action::Push) && v.contains(&Action::Update));
    assert!(!v.contains(&Action::Unstage));
}

#[test]
fn update_and_push() {
    require_tool!("bzr", "brz");
    let fx = init_branch();
    let upstream = &fx.root;
    write(upstream, "a.txt", "one\n");
    bzr(upstream, &["add", "-q"]);
    bzr(upstream, &["commit", "-q", "-m", "one"]);
    bzr(fx._tmp.path(), &["branch", "-q", "repo", "clone"]);
    let clone = fx.sibling("clone");
    write(upstream, "a.txt", "two\n");
    bzr(upstream, &["commit", "-q", "-m", "two"]);

    let vc = open(&clone);
    run_ok(vc.update().unwrap());
    assert_eq!(fs::read_to_string(clone.join("a.txt")).unwrap(), "two\n");

    bzr(&clone, &["push", "-q", "--remember", "../pushed"]);
    write(&clone, "a.txt", "three\n");
    bzr(&clone, &["commit", "-q", "-m", "three"]);
    run_ok(vc.push().unwrap());
    let pushed = fx.sibling("pushed");
    assert!(bzr(&pushed, &["log", "-l", "1"]).contains("three"));
}
