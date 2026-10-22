//! Integration tests of the CVS backend against real temporary local
//! repositories and checkouts.
//!
//! Needs `cvs` (or `cvsnt`) on `PATH`; every test is skipped (passes
//! without checking anything) when it isn't. Last run against CVS 1.12.13
//! (Debian's `cvs_1.12.13+real-30` binary).

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Fixture, run_ok, take, write};
use meld_vc::{
    Action, ConflictSide, DiffOptions, DiffRequest, MergeFileOrder, State, Vc, VcKind, choose_vcs,
    prepare_diff,
};

fn cvs_cmd() -> &'static str {
    VcKind::Cvs.cmd().unwrap()
}

fn cvs(dir: &Path, args: &[&str]) -> String {
    common::tool(dir, cvs_cmd(), args)
}

/// A repository (`cvsroot`, module `proj`) with a checkout at `root`
/// (`wc`). The module starts with `files`, each containing its own name.
fn checkout(files: &[&str]) -> Fixture {
    let fx = Fixture::new("import");
    let cvsroot = fx.sibling("cvsroot");
    let d = cvsroot.to_string_lossy().into_owned();
    cvs(fx._tmp.path(), &["-d", &d, "-Q", "init"]);
    for name in files {
        write(&fx.root, name, &format!("{name}\n"));
    }
    cvs(
        &fx.root,
        &[
            "-d", &d, "-Q", "import", "-m", "initial", "proj", "vendor", "start",
        ],
    );
    cvs(
        fx._tmp.path(),
        &["-d", &d, "-Q", "checkout", "-d", "wc", "proj"],
    );
    Fixture {
        root: fx.sibling("wc"),
        _tmp: fx._tmp,
    }
}

/// Another checkout of the module at `name` next to the first one.
fn second_checkout(fx: &Fixture, name: &str) -> PathBuf {
    let d = fx.sibling("cvsroot").to_string_lossy().into_owned();
    cvs(
        fx._tmp.path(),
        &["-d", &d, "-Q", "checkout", "-d", name, "proj"],
    );
    fx.sibling(name)
}

fn open(root: &Path) -> Box<dyn Vc> {
    let mut vc = VcKind::Cvs.open(root).unwrap();
    vc.refresh_vc_state(None).unwrap();
    vc
}

/// A checkout with one file in every state CVS reports.
fn status_wc() -> Fixture {
    let fx = checkout(&[
        "clean.txt",
        "modified.txt",
        "removed.txt",
        "missing.txt",
        "stale.txt",
        "conflict.txt",
        "sub/nested.txt",
        "sub/clean.txt",
    ]);
    let root = &fx.root;
    write(root, "conflict.txt", "a\nb\nc\n");
    cvs(root, &["-Q", "commit", "-m", "base", "conflict.txt"]);

    let other = second_checkout(&fx, "other");
    write(&other, "stale.txt", "newer\n");
    write(&other, "conflict.txt", "a\nOTHER\nc\n");
    cvs(&other, &["-Q", "commit", "-m", "other"]);
    write(root, "conflict.txt", "a\nMAIN\nc\n");
    cvs(root, &["-Q", "update", "conflict.txt"]);

    write(root, "modified.txt", "changed\n");
    fs::remove_file(root.join("removed.txt")).unwrap();
    cvs(root, &["-Q", "remove", "removed.txt"]);
    fs::remove_file(root.join("missing.txt")).unwrap();
    write(root, "new.txt", "new\n");
    cvs(root, &["-Q", "add", "new.txt"]);
    write(root, "untracked.txt", "untracked\n");
    write(root, "quote\"d ü.txt", "odd name\n");
    write(root, "unknown-dir/file.txt", "unknown\n");
    write(root, "sub/nested.txt", "nested change\n");
    fx
}

#[test]
fn discovery() {
    require_tool!("cvs", "cvsnt");
    eprintln!("testing {}", common::version(cvs_cmd(), &["--version"]));
    let fx = status_wc();
    let root = &fx.root;
    assert!(VcKind::Cvs.is_installed());
    assert!(VcKind::Cvs.valid_repo(root));
    // CVS has metadata in every folder and doesn't walk up to a root
    let sub = root.join("sub");
    assert_eq!(
        VcKind::Cvs.is_in_repo(&sub).0.as_deref(),
        Some(sub.as_path())
    );
    assert_eq!(VcKind::Cvs.is_in_repo(&root.join("unknown-dir")).0, None);
    let choices = choose_vcs(Some(root), None);
    assert_eq!(choices.choices[choices.active].kind, VcKind::Cvs);
    assert_eq!(choices.choices[choices.active].label, "CVS");
}

#[test]
fn statuses() {
    require_tool!("cvs", "cvsnt");
    let fx = status_wc();
    let root = &fx.root;
    let vc = open(root);
    let st = |name: &str| vc.get_entry(&root.join(name)).state;

    assert_eq!(st("clean.txt"), State::Normal);
    assert_eq!(st("modified.txt"), State::Modified);
    assert_eq!(st("removed.txt"), State::Removed);
    assert_eq!(st("missing.txt"), State::Missing);
    assert_eq!(st("stale.txt"), State::Normal);
    assert_eq!(st("conflict.txt"), State::Conflict);
    assert_eq!(st("new.txt"), State::New);
    assert_eq!(st("untracked.txt"), State::Unversioned);
    assert_eq!(st("quote\"d ü.txt"), State::Unversioned);
    assert_eq!(st("unknown-dir"), State::Unversioned);
    assert_eq!(st("unknown-dir/file.txt"), State::Unversioned);
    assert_eq!(st("sub/nested.txt"), State::Modified);
    assert_eq!(st("sub/clean.txt"), State::Normal);

    let entries = vc.get_entries(root).unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"removed.txt") && names.contains(&"missing.txt"));
    assert!(!names.contains(&"CVS"));

    // Single-file refresh
    let mut vc = vc;
    let file = root.join("modified.txt");
    write(root, "modified.txt", "modified.txt\n");
    vc.refresh_vc_state(Some(&file)).unwrap();
    assert_eq!(vc.get_entry(&file).state, State::Normal);
    write(root, "modified.txt", "again\n");
    // CVS compares one-second timestamps before contents, and the status
    // call above has just re-stamped the file's entry.
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
        .unwrap();
    vc.refresh_vc_state(Some(&file)).unwrap();
    assert_eq!(vc.get_entry(&file).state, State::Modified);

    // The conflict backup `cvs update` made is ignored by CVS's defaults
    let backup = root.join(".#conflict.txt.1.2");
    assert!(backup.exists());
    assert_eq!(vc.get_entry(&backup).state, State::Ignored);
}

#[test]
fn conflicts() {
    require_tool!("cvs", "cvsnt");
    let fx = status_wc();
    let root = &fx.root;
    let mut vc = open(root);
    let file = root.join("conflict.txt");
    assert_eq!(vc.get_entry(&file).state, State::Conflict);
    let v = vc.get_valid_actions(&[(file.clone(), State::Conflict)]);
    assert!(v.contains(&Action::Resolve));

    let side = |side| vc.get_path_for_conflict(&file, side).unwrap();
    let (local, is_temp) = side(ConflictSide::Local);
    assert_eq!(local, root.join(".#conflict.txt.1.2"));
    assert!(!is_temp, "CVS owns its backup");
    assert_eq!(fs::read_to_string(&local).unwrap(), "a\nMAIN\nc\n");
    let (base, is_temp) = side(ConflictSide::Base);
    assert!(is_temp);
    assert_eq!(take(&base), "a\nb\nc\n");
    let (remote, is_temp) = side(ConflictSide::Remote);
    assert!(is_temp);
    assert_eq!(take(&remote), "a\nOTHER\nc\n");
    assert_eq!(side(ConflictSide::Merged), (file.clone(), false));

    let opts = DiffOptions {
        merge_file_order: MergeFileOrder::RemoteMergeLocal,
        ..Default::default()
    };
    let DiffRequest::Files {
        paths,
        merge_output,
        temp_files,
        prompt_resolve,
        ..
    } = prepare_diff(vc.as_ref(), &file, &opts).unwrap()
    else {
        panic!("expected a file comparison")
    };
    assert_eq!(fs::read_to_string(&paths[0]).unwrap(), "a\nOTHER\nc\n");
    assert_eq!(paths[1], file);
    assert_eq!(paths[2], local);
    assert_eq!(merge_output, Some(file.clone()));
    assert!(prompt_resolve);
    assert_eq!(temp_files.len(), 1);
    let mut temps = meld_vc::TempFiles::new();
    temps.register(temp_files);

    // Resolving is refused while the markers are there
    let err = vc.resolve(std::slice::from_ref(&file)).unwrap_err();
    assert!(err.to_string().contains("conflict markers"), "{err}");
    // Once they're gone the file is modified, and resolve only re-checks
    write(root, "conflict.txt", "a\nRESOLVED\nc\n");
    let cmds = vc.resolve(std::slice::from_ref(&file)).unwrap();
    assert_eq!(cmds[0].argv(), vec!["-Q", "status", "conflict.txt"]);
    run_ok(cmds);
    vc.refresh_vc_state(Some(&file)).unwrap();
    assert_eq!(vc.get_entry(&file).state, State::Modified);
    run_ok(vc.commit(std::slice::from_ref(&file), "Resolved").unwrap());
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&file).state, State::Normal);

    // Without the backup, conflicts are compared with the base revision
    let fx = status_wc();
    let root = &fx.root;
    fs::remove_file(root.join(".#conflict.txt.1.2")).unwrap();
    let vc = open(root);
    let file = root.join("conflict.txt");
    let DiffRequest::Files {
        paths, temp_files, ..
    } = prepare_diff(vc.as_ref(), &file, &DiffOptions::default()).unwrap()
    else {
        panic!("expected a file comparison")
    };
    assert_eq!(paths.len(), 2);
    assert_eq!(fs::read_to_string(&paths[0]).unwrap(), "a\nOTHER\nc\n");
    temps.register(temp_files);
}

#[test]
fn ignored_files() {
    require_tool!("cvs", "cvsnt");
    let fx = checkout(&["a.txt", "kept.o", "sub/b.txt"]);
    let root = &fx.root;
    // The repository's CVSROOT/cvsignore applies everywhere
    let cvsroot = fx.sibling("cvsroot");
    write(&cvsroot, "CVSROOT/cvsignore", "*.repo\n");
    write(root, ".cvsignore", "*.log build\n");
    cvs(root, &["-Q", "add", ".cvsignore"]);
    write(root, "sub/.cvsignore", "local.txt\n");
    for name in [
        "x.o",
        "debug.log",
        "data.repo",
        "build/out.txt",
        "sub/local.txt",
        "sub/nested.log",
        "sub/core",
        "core.c",
        "notes.txt",
    ] {
        write(root, name, "x\n");
    }
    let mut vc = open(root);
    let st = |vc: &dyn Vc, name: &str| vc.get_entry(&root.join(name)).state;
    for name in [
        "x.o",
        "debug.log",
        "data.repo",
        "build",
        "sub/local.txt",
        "sub/core",
    ] {
        assert_eq!(st(vc.as_ref(), name), State::Ignored, "{name}");
    }
    // .cvsignore applies to its own folder only
    assert_eq!(st(vc.as_ref(), "sub/nested.log"), State::Unversioned);
    assert_eq!(st(vc.as_ref(), "sub/.cvsignore"), State::Unversioned);
    assert_eq!(st(vc.as_ref(), "core.c"), State::Unversioned);
    assert_eq!(st(vc.as_ref(), "notes.txt"), State::Unversioned);
    // Versioned files are never ignored
    assert_eq!(st(vc.as_ref(), "kept.o"), State::Normal);
    assert_eq!(st(vc.as_ref(), ".cvsignore"), State::New);
    // CVS agrees on what is unknown
    let unknown = cvs(root, &["-n", "-q", "update"]);
    let mut listed: Vec<&str> = unknown
        .lines()
        .filter_map(|l| l.strip_prefix("? "))
        .collect();
    listed.sort();
    assert_eq!(
        listed,
        vec!["core.c", "notes.txt", "sub/.cvsignore", "sub/nested.log"]
    );

    // ~/.cvsignore, with a single-file refresh (the test HOME is private;
    // no other test has files with this extension)
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    write(&home, ".cvsignore", "*.meld-home-ignored\n");
    write(root, "sub/x.meld-home-ignored", "x\n");
    let file = root.join("sub/x.meld-home-ignored");
    vc.refresh_vc_state(Some(&file)).unwrap();
    assert_eq!(vc.get_entry(&file).state, State::Ignored);
    assert_eq!(st(vc.as_ref(), "notes.txt"), State::Unversioned);
}

#[test]
fn base_revision_extraction() {
    require_tool!("cvs", "cvsnt");
    let fx = status_wc();
    let root = &fx.root;
    let vc = open(root);
    let base = |name: &str| take(&vc.get_path_for_repo_file(&root.join(name), None).unwrap());
    assert_eq!(base("modified.txt"), "modified.txt\n");
    assert_eq!(base("sub/nested.txt"), "sub/nested.txt\n");
    assert_eq!(base("removed.txt"), "removed.txt\n");
    assert_eq!(base("missing.txt"), "missing.txt\n");
    // The checked-out revision, not the newer one in the repository
    assert_eq!(base("stale.txt"), "stale.txt\n");
    assert_eq!(base("new.txt"), "");
    assert!(
        vc.get_path_for_repo_file(&root.join("modified.txt"), Some("1.1"))
            .is_err()
    );
    assert!(
        vc.get_path_for_repo_file(Path::new("/definitely/elsewhere.txt"), None)
            .is_err()
    );
}

#[test]
fn add_commit_remove_revert_update() {
    require_tool!("cvs", "cvsnt");
    let fx = status_wc();
    let root = &fx.root;
    let mut vc = open(root);
    let untracked = root.join("untracked.txt");
    let nested_new = root.join("unknown-dir/file.txt");

    // Unversioned parent folders are added with the file
    let cmds = vc.add(&[untracked.clone(), nested_new.clone()]).unwrap();
    assert_eq!(
        cmds[0].argv(),
        vec![
            "add",
            "untracked.txt",
            "unknown-dir",
            "unknown-dir/file.txt"
        ]
    );
    run_ok(cmds);
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&untracked).state, State::New);
    assert_eq!(vc.get_entry(&nested_new).state, State::New);
    assert_eq!(vc.get_entry(&root.join("unknown-dir")).state, State::Normal);

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
    assert_eq!(vc.get_entry(&nested_new).state, State::New);
    assert_eq!(
        vc.get_entry(&root.join("modified.txt")).state,
        State::Modified
    );
    let log = cvs(root, &["log", "-r1.1", "new.txt"]);
    assert!(log.contains("Add files\n\nBody"), "{log}");

    run_ok(vc.remove(&[root.join("clean.txt")]).unwrap());
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&root.join("clean.txt")).state, State::Removed);
    assert!(!root.join("clean.txt").exists());

    // Removed files are resurrected with `cvs add`
    let cmds = vc
        .revert(&[
            root.join("modified.txt"),
            root.join("clean.txt"),
            root.join("missing.txt"),
        ])
        .unwrap();
    assert_eq!(cmds[0].argv(), vec!["add", "clean.txt"]);
    assert_eq!(
        cmds[1].argv(),
        vec!["update", "-C", "modified.txt", "missing.txt"]
    );
    run_ok(cmds);
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

    // Update brings in the newer revision of the stale file
    let cmds = vc.update().unwrap();
    assert_eq!(cmds[0].argv(), vec!["update"]);
    run_in_ok(root, &cmds);
    assert_eq!(
        fs::read_to_string(root.join("stale.txt")).unwrap(),
        "newer\n"
    );
}

/// Run update commands; CVS exits with 1 when conflicts remain.
fn run_in_ok(root: &Path, cmds: &[meld_vc::VcCommand]) {
    for (cmd, out) in meld_vc::run_all(cmds).unwrap() {
        assert!(
            out.status.is_some(),
            "{} in {} was killed: {}",
            cmd.console_line(),
            root.display(),
            out.stderr
        );
    }
}
