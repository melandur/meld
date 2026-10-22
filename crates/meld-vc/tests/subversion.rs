//! Integration tests of the Subversion backend against real temporary
//! repositories (`file://` URLs) and working copies.
//!
//! Needs `svn` and `svnadmin` on `PATH`; every test is skipped (passes
//! without checking anything) when they aren't. Last run against
//! Subversion 1.15.0 (conda-forge `subversion` package).
//!
//! File names are ASCII: the conda-forge build fails on non-ASCII names
//! in every locale (distribution builds don't), which says nothing about
//! the backend.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Fixture, run_ok, take, write};
use meld_vc::{
    Action, ConflictSide, DiffOptions, DiffRequest, MergeFileOrder, State, Vc, VcKind, choose_vcs,
    prepare_diff,
};

fn svn(dir: &Path, args: &[&str]) -> String {
    common::tool(dir, "svn", args)
}

/// A repository (`svnrepo`) with a working copy at `root` (`wc`).
fn checkout() -> Fixture {
    let fx = Fixture::new("wc");
    let repo = fx.sibling("svnrepo");
    common::tool(fx._tmp.path(), "svnadmin", &["create", "svnrepo"]);
    svn(
        fx._tmp.path(),
        &["checkout", "-q", &url(&repo), "--force", "wc"],
    );
    fx
}

fn url(path: &Path) -> String {
    format!("file://{}", path.display())
}

/// Check out the same repository again at `name` next to the first one.
fn second_checkout(fx: &Fixture, name: &str) -> PathBuf {
    let repo = fx.sibling("svnrepo");
    svn(fx._tmp.path(), &["checkout", "-q", &url(&repo), name]);
    fx.sibling(name)
}

fn open(root: &Path) -> Box<dyn Vc> {
    let mut vc = VcKind::Subversion.open(root).unwrap();
    vc.refresh_vc_state(None).unwrap();
    vc
}

/// A working copy with one file in every state Subversion reports.
fn status_wc() -> Fixture {
    let fx = checkout();
    let root = &fx.root;
    for name in [
        "clean.txt",
        "modified.txt",
        "deleted.txt",
        "missing.txt",
        "sub/nested.txt",
        "sub/clean.txt",
    ] {
        write(root, name, &format!("{name}\n"));
    }
    svn(
        root,
        &[
            "add",
            "-q",
            "clean.txt",
            "modified.txt",
            "deleted.txt",
            "missing.txt",
            "sub",
        ],
    );
    svn(
        root,
        &["propset", "-q", "svn:ignore", "*.log\nbuild\n", "."],
    );
    svn(root, &["commit", "-q", "-m", "initial"]);
    svn(root, &["update", "-q"]);

    write(root, "modified.txt", "changed\n");
    svn(root, &["rm", "-q", "deleted.txt"]);
    fs::remove_file(root.join("missing.txt")).unwrap();
    write(root, "new.txt", "new\n");
    svn(root, &["add", "-q", "new.txt"]);
    write(root, "untracked.txt", "untracked\n");
    write(root, "quote\"d name.txt", "odd name\n");
    write(root, "unknown-dir/file.txt", "unknown\n");
    write(root, "debug.log", "ignored\n");
    write(root, "build/out.o", "ignored\n");
    write(root, "sub/nested.txt", "nested change\n");
    fx
}

#[test]
fn discovery() {
    require_tool!("svn");
    eprintln!("testing {}", common::version("svn", &["--version"]));
    let fx = status_wc();
    let root = &fx.root;
    assert!(VcKind::Subversion.is_installed());
    assert!(VcKind::Subversion.valid_repo(root));
    let sub = root.join("sub");
    assert_eq!(
        VcKind::Subversion
            .is_in_repo(&sub.join("nested.txt"))
            .0
            .as_deref(),
        Some(root.as_path())
    );
    let choices = choose_vcs(Some(&sub), None);
    assert_eq!(choices.choices[choices.active].kind, VcKind::Subversion);
    assert_eq!(choices.choices[choices.active].label, "Subversion");
    let vc = choices.open_active().unwrap();
    assert_eq!(vc.root(), root);
    assert_eq!(vc.location(), sub);
}

#[test]
fn statuses() {
    require_tool!("svn");
    let fx = status_wc();
    let root = &fx.root;
    let vc = open(root);
    let entry = |name: &str| vc.get_entry(&root.join(name));
    let st = |name: &str| entry(name).state;

    assert_eq!(st("clean.txt"), State::Normal);
    assert_eq!(entry("clean.txt").options, "Rev 1");
    assert_eq!(st("modified.txt"), State::Modified);
    assert_eq!(st("deleted.txt"), State::Removed);
    assert_eq!(st("missing.txt"), State::Missing);
    assert_eq!(st("new.txt"), State::New);
    assert_eq!(st("untracked.txt"), State::Unversioned);
    assert_eq!(st("quote\"d name.txt"), State::Unversioned);
    assert_eq!(st("unknown-dir"), State::Unversioned);
    assert_eq!(st("debug.log"), State::Ignored);
    assert_eq!(st("build"), State::Ignored);
    assert_eq!(st("sub"), State::Normal);
    assert_eq!(st("sub/nested.txt"), State::Modified);
    assert_eq!(st("sub/clean.txt"), State::Normal);

    let entries = vc.get_entries(root).unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"missing.txt"));
    assert!(!names.contains(&".svn"));

    // A location below the root only reports its own folder
    let sub = root.join("sub");
    let mut vc = VcKind::Subversion.open(&sub).unwrap();
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&sub.join("nested.txt")).state, State::Modified);
    assert_eq!(
        vc.get_entry(&root.join("modified.txt")).state,
        State::Normal
    );

    // Single-file refresh
    let file = sub.join("nested.txt");
    write(root, "sub/nested.txt", "sub/nested.txt\n");
    vc.refresh_vc_state(Some(&file)).unwrap();
    assert_eq!(vc.get_entry(&file).state, State::Normal);
}

#[test]
fn base_revision_extraction() {
    require_tool!("svn");
    let fx = status_wc();
    let root = &fx.root;
    let vc = open(root);
    let base = |name: &str| take(&vc.get_path_for_repo_file(&root.join(name), None).unwrap());
    assert_eq!(base("modified.txt"), "modified.txt\n");
    assert_eq!(base("deleted.txt"), "deleted.txt\n");
    assert_eq!(base("missing.txt"), "missing.txt\n");
    assert_eq!(base("sub/nested.txt"), "sub/nested.txt\n");
    assert_eq!(base("new.txt"), "");
    assert!(
        vc.get_path_for_repo_file(&root.join("modified.txt"), Some("1"))
            .is_err()
    );
    assert!(
        vc.get_path_for_repo_file(Path::new("/definitely/elsewhere.txt"), None)
            .is_err()
    );
}

/// Check the conflict files of `file` and resolve the conflict.
fn check_conflict(root: &Path, file: &Path) {
    let mut vc = open(root);
    assert_eq!(vc.get_entry(file).state, State::Conflict);
    assert!(
        vc.get_valid_actions(&[(file.to_path_buf(), State::Conflict)])
            .contains(&Action::Resolve)
    );
    let side = |side| {
        let (p, is_temp) = vc.get_path_for_conflict(file, side).unwrap();
        assert!(!is_temp, "svn owns its conflict files");
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
        tab_label,
        ..
    } = prepare_diff(vc.as_ref(), file, &opts).unwrap()
    else {
        panic!("expected a file comparison")
    };
    assert_eq!(fs::read_to_string(&paths[0]).unwrap(), "a\nMAIN\nc\n");
    assert_eq!(fs::read_to_string(&paths[2]).unwrap(), "a\nOTHER\nc\n");
    assert_eq!(merge_output.as_deref(), Some(file));
    assert_eq!(tab_label, "conflict.txt (local, merge, remote)");

    write(root, "conflict.txt", "a\nRESOLVED\nc\n");
    run_ok(vc.resolve(&[file.to_path_buf()]).unwrap());
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(file).state, State::Modified);
    let leftovers: Vec<String> = fs::read_dir(root)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("conflict.txt."))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn update_conflict() {
    require_tool!("svn");
    let fx = checkout();
    let root = &fx.root;
    write(root, "conflict.txt", "a\nb\nc\n");
    svn(root, &["add", "-q", "conflict.txt"]);
    svn(root, &["commit", "-q", "-m", "base"]);
    let other = second_checkout(&fx, "other");
    write(&other, "conflict.txt", "a\nOTHER\nc\n");
    svn(&other, &["commit", "-q", "-m", "other"]);
    write(root, "conflict.txt", "a\nMAIN\nc\n");

    // Update runs at the root and leaves the conflict for Meld
    let vc = open(root);
    let cmds = vc.update().unwrap();
    assert_eq!(cmds[0].argv(), vec!["update"]);
    let out = common::run_in(root, "svn", &["update", "--accept", "postpone"], false);
    assert!(String::from_utf8_lossy(&out.stdout).contains("C    conflict.txt"));
    assert!(root.join("conflict.txt.mine").exists());
    check_conflict(root, &root.join("conflict.txt"));
}

#[test]
fn merge_conflict() {
    require_tool!("svn");
    let fx = checkout();
    let tmp = fx._tmp.path();
    let repo = url(&fx.sibling("svnrepo"));
    svn(
        tmp,
        &[
            "mkdir",
            "-q",
            "-m",
            "layout",
            &format!("{repo}/trunk"),
            &format!("{repo}/branches"),
        ],
    );
    svn(tmp, &["checkout", "-q", &format!("{repo}/trunk"), "trunk"]);
    let trunk = fx.sibling("trunk");
    write(&trunk, "conflict.txt", "a\nb\nc\n");
    svn(&trunk, &["add", "-q", "conflict.txt"]);
    svn(&trunk, &["commit", "-q", "-m", "base"]);
    svn(
        tmp,
        &[
            "copy",
            "-q",
            "-m",
            "branch",
            &format!("{repo}/trunk"),
            &format!("{repo}/branches/other"),
        ],
    );
    svn(
        tmp,
        &["checkout", "-q", &format!("{repo}/branches/other"), "other"],
    );
    let other = fx.sibling("other");
    write(&other, "conflict.txt", "a\nOTHER\nc\n");
    svn(&other, &["commit", "-q", "-m", "other"]);
    write(&trunk, "conflict.txt", "a\nMAIN\nc\n");
    svn(&trunk, &["commit", "-q", "-m", "main"]);
    svn(&trunk, &["update", "-q"]);
    svn(
        &trunk,
        &["merge", "-q", "--accept", "postpone", "^/branches/other"],
    );
    assert!(trunk.join("conflict.txt.working").exists());
    check_conflict(&trunk, &trunk.join("conflict.txt"));
}

#[test]
fn add_commit_remove_revert() {
    require_tool!("svn");
    let fx = status_wc();
    let root = &fx.root;
    let mut vc = open(root);
    let untracked = root.join("untracked.txt");

    // A folder gets its own command, run from its parent
    let cmds = vc
        .add(&[root.join("unknown-dir"), untracked.clone()])
        .unwrap();
    assert_eq!(cmds.len(), 2);
    assert_eq!(cmds[0].argv(), vec!["add", "unknown-dir"]);
    run_ok(cmds);
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&untracked).state, State::New);
    assert_eq!(vc.get_entry(&root.join("unknown-dir")).state, State::New);
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
    assert_eq!(vc.get_entry(&root.join("unknown-dir")).state, State::New);
    let log = svn(root, &["log", "-l", "1", &url(&fx.sibling("svnrepo"))]);
    assert!(log.contains("Add files\n\nBody"), "{log}");

    // Remove a modified file (--force)
    run_ok(vc.remove(&[root.join("modified.txt")]).unwrap());
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(
        vc.get_entry(&root.join("modified.txt")).state,
        State::Removed
    );
    assert!(!root.join("modified.txt").exists());

    run_ok(
        vc.revert(&[
            root.join("modified.txt"),
            root.join("deleted.txt"),
            root.join("missing.txt"),
        ])
        .unwrap(),
    );
    vc.refresh_vc_state(None).unwrap();
    for name in ["modified.txt", "deleted.txt", "missing.txt"] {
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
    assert!(v.contains(&Action::Update) && v.contains(&Action::Commit));
    assert!(!v.contains(&Action::Push) && !v.contains(&Action::Unstage));
}

#[test]
fn update_brings_in_changes() {
    require_tool!("svn");
    let fx = checkout();
    let root = &fx.root;
    write(root, "a.txt", "one\n");
    svn(root, &["add", "-q", "a.txt"]);
    svn(root, &["commit", "-q", "-m", "one"]);
    let other = second_checkout(&fx, "other");
    write(&other, "a.txt", "two\n");
    svn(&other, &["commit", "-q", "-m", "two"]);

    let vc = open(root);
    run_ok(vc.update().unwrap());
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "two\n");
}
