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
    Action, ConflictSide, DiffOptions, DiffRequest, MergeFileOrder, State, StatusFilters, Vc,
    VcKind, VcRow, choose_vcs, get_vcs, prepare_diff, scan_tree,
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
    // Not implemented for Mercurial, or (resolve) only for conflicts
    for a in [Action::Push, Action::Resolve, Action::Unstage] {
        assert!(!v.contains(&a), "{a}");
    }
}

/// Run `hg args` in `dir`, which is expected to fail (exit 1) because of
/// merge conflicts. Merges use Mercurial's internal merge with conflict
/// markers, whatever `HGMERGE` says.
fn hg_conflicting(dir: &Path, args: &[&str]) {
    let mut full = args.to_vec();
    full.extend(["--tool", "internal:merge"]);
    let out = common::run_in(dir, "hg", &full, true);
    assert_eq!(
        out.status.code(),
        Some(1),
        "hg {args:?} should leave conflicts: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository where merging `other` into `main` conflicts in
/// `conflict.txt` (and, in `sub/`, in `sub/nested.txt`) and merges
/// `clean-merge.txt` cleanly.
fn merge_conflict_repo() -> Fixture {
    let fx = init_repo();
    let root = &fx.root;
    write(root, "conflict.txt", "a\nb\nc\n");
    write(root, "sub/nested.txt", "1\n2\n3\n");
    write(root, "clean-merge.txt", "x\n");
    hg(root, &["add", "-q"]);
    hg(root, &["commit", "-q", "-m", "base"]);
    write(root, "conflict.txt", "a\nMAIN\nc\n");
    write(root, "sub/nested.txt", "1\nMAIN\n3\n");
    hg(root, &["commit", "-q", "-m", "main"]);
    hg(root, &["update", "-q", "0"]);
    write(root, "conflict.txt", "a\nOTHER\nc\n");
    write(root, "sub/nested.txt", "1\nOTHER\n3\n");
    write(root, "clean-merge.txt", "y\n");
    hg(root, &["commit", "-q", "-m", "other"]);
    hg(root, &["update", "-q", "1"]);
    hg_conflicting(root, &["merge", "2"]);
    fx
}

fn conflict_side(vc: &dyn Vc, file: &Path, side: ConflictSide) -> String {
    let (p, is_temp) = vc.get_path_for_conflict(file, side).unwrap();
    if side == ConflictSide::Merged {
        assert!(!is_temp, "the merged file is the working file");
        assert_eq!(p, file);
        fs::read_to_string(p).unwrap()
    } else {
        assert!(is_temp);
        take(&p)
    }
}

#[test]
fn merge_conflicts() {
    require_tool!("hg");
    let fx = merge_conflict_repo();
    let root = &fx.root;
    let mut vc = open(root);
    let file = root.join("conflict.txt");
    assert_eq!(vc.get_entry(&file).state, State::Conflict);
    assert_eq!(
        vc.get_entry(&root.join("sub/nested.txt")).state,
        State::Conflict
    );
    assert_eq!(
        vc.get_entry(&root.join("clean-merge.txt")).state,
        State::Modified
    );
    assert_eq!(
        vc.get_entry(&root.join("conflict.txt.orig")).state,
        State::Unversioned
    );
    assert!(
        vc.get_valid_actions(&[(file.clone(), State::Conflict)])
            .contains(&Action::Resolve)
    );

    assert_eq!(
        conflict_side(vc.as_ref(), &file, ConflictSide::Base),
        "a\nb\nc\n"
    );
    assert_eq!(
        conflict_side(vc.as_ref(), &file, ConflictSide::Local),
        "a\nMAIN\nc\n"
    );
    assert_eq!(
        conflict_side(vc.as_ref(), &file, ConflictSide::Remote),
        "a\nOTHER\nc\n"
    );
    assert!(conflict_side(vc.as_ref(), &file, ConflictSide::Merged).contains("<<<<<<<"));

    // A sub-folder location sees the conflict below it
    let sub = root.join("sub");
    let mut subvc = VcKind::Mercurial.open(&sub).unwrap();
    subvc.refresh_vc_state(None).unwrap();
    let nested = sub.join("nested.txt");
    assert_eq!(subvc.get_entry(&nested).state, State::Conflict);
    assert_eq!(
        conflict_side(subvc.as_ref(), &nested, ConflictSide::Remote),
        "1\nOTHER\n3\n"
    );

    let opts = DiffOptions {
        merge_file_order: MergeFileOrder::LocalMergeRemote,
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
    assert_eq!(fs::read_to_string(&paths[0]).unwrap(), "a\nMAIN\nc\n");
    assert_eq!(paths[1], file);
    assert_eq!(fs::read_to_string(&paths[2]).unwrap(), "a\nOTHER\nc\n");
    assert_eq!(merge_output, Some(file.clone()));
    assert!(prompt_resolve);
    assert_eq!(temp_files.len(), 2);
    let mut temps = meld_vc::TempFiles::new();
    temps.register(temp_files);

    // Resolve one file: it's modified again; a single-file refresh keeps
    // the other conflict.
    write(root, "conflict.txt", "a\nRESOLVED\nc\n");
    run_ok(vc.resolve(std::slice::from_ref(&file)).unwrap());
    vc.refresh_vc_state(Some(&file)).unwrap();
    assert_eq!(vc.get_entry(&file).state, State::Modified);
    assert_eq!(
        vc.get_entry(&root.join("sub/nested.txt")).state,
        State::Conflict
    );
    write(root, "sub/nested.txt", "1\nRESOLVED\n3\n");
    run_ok(vc.resolve(&[root.join("sub/nested.txt")]).unwrap());

    // Committing a merge commits everything, whatever the selection
    vc.refresh_vc_state(None).unwrap();
    let cmds = vc.commit(std::slice::from_ref(&file), "Merge").unwrap();
    assert_eq!(cmds[0].argv(), vec!["commit", "-m", "Merge"]);
    run_ok(cmds);
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&file).state, State::Normal);
    assert_eq!(
        vc.get_entry(&root.join("clean-merge.txt")).state,
        State::Normal
    );
    assert_eq!(hg(root, &["log", "-r", "p2()", "-T", "{desc}"]), "");
    assert_eq!(hg(root, &["log", "-r", ".", "-T", "{desc}"]), "Merge");
    // Back to normal commits of the selection
    write(root, "conflict.txt", "after\n");
    write(root, "clean-merge.txt", "after\n");
    run_ok(vc.commit(std::slice::from_ref(&file), "One").unwrap());
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(
        vc.get_entry(&root.join("clean-merge.txt")).state,
        State::Modified
    );
}

/// Grafting has no second parent, and its base is the grafted changeset's
/// parent rather than a common ancestor.
#[test]
fn graft_conflicts() {
    require_tool!("hg");
    let fx = init_repo();
    let root = &fx.root;
    write(root, "f.txt", "a\nb\nc\n");
    hg(root, &["add", "-q"]);
    hg(root, &["commit", "-q", "-m", "base"]);
    write(root, "f.txt", "a\nB1\nc\n");
    hg(root, &["commit", "-q", "-m", "first"]);
    write(root, "f.txt", "a\nB2\nc\n");
    hg(root, &["commit", "-q", "-m", "second"]);
    hg(root, &["update", "-q", "0"]);
    write(root, "f.txt", "a\nMAIN\nc\n");
    hg(root, &["commit", "-q", "-m", "main"]);
    hg_conflicting(root, &["graft", "-r", "2"]);

    let vc = open(root);
    let file = root.join("f.txt");
    assert_eq!(vc.get_entry(&file).state, State::Conflict);
    assert_eq!(
        conflict_side(vc.as_ref(), &file, ConflictSide::Base),
        "a\nB1\nc\n"
    );
    assert_eq!(
        conflict_side(vc.as_ref(), &file, ConflictSide::Local),
        "a\nMAIN\nc\n"
    );
    assert_eq!(
        conflict_side(vc.as_ref(), &file, ConflictSide::Remote),
        "a\nB2\nc\n"
    );
}

/// A file deleted on one side and changed on the other has an empty side.
#[test]
fn delete_modify_conflict() {
    require_tool!("hg");
    let fx = init_repo();
    let root = &fx.root;
    write(root, "f.txt", "a\n");
    hg(root, &["add", "-q"]);
    hg(root, &["commit", "-q", "-m", "base"]);
    write(root, "f.txt", "changed\n");
    hg(root, &["commit", "-q", "-m", "changed"]);
    hg(root, &["update", "-q", "0"]);
    hg(root, &["rm", "-q", "f.txt"]);
    hg(root, &["commit", "-q", "-m", "deleted"]);
    hg_conflicting(root, &["merge", "1"]);

    let vc = open(root);
    let file = root.join("f.txt");
    assert_eq!(vc.get_entry(&file).state, State::Conflict);
    assert_eq!(conflict_side(vc.as_ref(), &file, ConflictSide::Local), "");
    assert_eq!(conflict_side(vc.as_ref(), &file, ConflictSide::Base), "a\n");
    assert_eq!(
        conflict_side(vc.as_ref(), &file, ConflictSide::Remote),
        "changed\n"
    );
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
