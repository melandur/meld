//! Integration tests of the Git backend against real temporary repositories.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use meld_vc::{
    Action, ConflictSide, DiffOptions, DiffRequest, DiffSide, MergeFileOrder, State, StatusFilters,
    Vc, VcKind, VcRow, choose_vcs, get_vcs, prepare_diff, run_all, scan_tree,
};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git runs");
    assert!(
        out.status.success() || args[0] == "merge",
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn configure(dir: &Path) {
    for (k, v) in [
        ("user.name", "Meld Test"),
        ("user.email", "meld@example.com"),
        ("commit.gpgsign", "false"),
        ("tag.gpgsign", "false"),
        ("rerere.enabled", "false"),
        ("core.autocrlf", "false"),
        ("merge.conflictStyle", "merge"),
        ("pull.rebase", "false"),
    ] {
        git(dir, &["config", k, v]);
    }
}

fn init_repo() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    configure(&root);
    (tmp, root)
}

fn write(root: &Path, name: &str, content: &str) {
    let p = root.join(name);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(p, content).unwrap();
}

fn open(root: &Path) -> Box<dyn Vc> {
    let mut vc = VcKind::Git.open(root).unwrap();
    vc.refresh_vc_state(None).unwrap();
    vc
}

/// A repository with one file in every interesting state.
fn status_repo() -> (tempfile::TempDir, PathBuf) {
    let (tmp, root) = init_repo();
    for name in [
        "clean.txt",
        "modified.txt",
        "deleted.txt",
        "staged.txt",
        "partial.txt",
        "renamed.txt",
        "exec.sh",
        "sub/nested.txt",
        "sub/clean.txt",
    ] {
        write(&root, name, &format!("{name}\n"));
    }
    write(&root, ".gitignore", "*.log\nbuild/\n");
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "initial"]);

    write(&root, "modified.txt", "changed\n");
    fs::remove_file(root.join("deleted.txt")).unwrap();
    write(&root, "staged.txt", "staged change\n");
    git(&root, &["add", "staged.txt"]);
    write(&root, "partial.txt", "first change\n");
    git(&root, &["add", "partial.txt"]);
    write(&root, "partial.txt", "second change\n");
    git(&root, &["mv", "renamed.txt", "renamed2.txt"]);
    write(&root, "new.txt", "new\n");
    git(&root, &["add", "new.txt"]);
    write(&root, "untracked.txt", "untracked\n");
    write(&root, "debug.log", "ignored\n");
    write(&root, "build/out.o", "ignored\n");
    write(&root, "sub/nested.txt", "nested change\n");
    write(&root, "quote\"d ü.txt", "odd name\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.join("exec.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    (tmp, root)
}

#[test]
fn discovery() {
    let (_tmp, root) = status_repo();
    assert!(VcKind::Git.is_installed());
    assert!(VcKind::Git.valid_repo(&root));
    let sub = root.join("sub");
    let (found_root, location) = VcKind::Git.is_in_repo(&sub.join("nested.txt"));
    assert_eq!(found_root.as_deref(), Some(root.as_path()));
    assert_eq!(location, sub);

    let vcs = get_vcs(&sub);
    assert_eq!(vcs[0].kind, VcKind::Git);
    assert_eq!(vcs[0].root.as_deref(), Some(root.as_path()));

    let choices = choose_vcs(Some(&sub.join("does-not-exist.txt")), None);
    assert_eq!(choices.location, sub);
    assert_eq!(choices.choices[choices.active].kind, VcKind::Git);
    assert_eq!(choices.choices[choices.active].label, "Git");
    let vc = choices.open_active().unwrap();
    assert_eq!(vc.root(), root);
    assert_eq!(vc.location(), sub);

    let tmp = tempfile::tempdir().unwrap();
    assert!(
        VcKind::Git.open(tmp.path()).is_err() || VcKind::Git.is_in_repo(tmp.path()).0.is_some()
    );
}

#[test]
fn statuses() {
    let (_tmp, root) = status_repo();
    let vc = open(&root);
    let entry = |name: &str| vc.get_entry(&root.join(name));
    let st = |name: &str| entry(name).state;

    assert_eq!(st("clean.txt"), State::Normal);
    assert_eq!(st("modified.txt"), State::Modified);
    assert_eq!(entry("modified.txt").options, "");
    assert_eq!(st("deleted.txt"), State::Removed);
    assert_eq!(st("staged.txt"), State::Modified);
    assert_eq!(entry("staged.txt").options, "Staged");
    assert_eq!(st("partial.txt"), State::Modified);
    assert_eq!(entry("partial.txt").options, "Partially staged");
    // Git has no rename detection here: old name removed, new name added
    assert_eq!(st("renamed.txt"), State::Removed);
    assert_eq!(st("renamed2.txt"), State::New);
    assert_eq!(entry("renamed2.txt").options, "Staged");
    assert_eq!(st("new.txt"), State::New);
    assert_eq!(st("untracked.txt"), State::Unversioned);
    assert_eq!(st("quote\"d ü.txt"), State::Unversioned);
    assert_eq!(st("debug.log"), State::Ignored);
    assert_eq!(st("build"), State::Ignored);
    assert!(entry("build").isdir);
    assert_eq!(st("sub/nested.txt"), State::Modified);
    assert_eq!(st("sub/clean.txt"), State::Normal);
    #[cfg(unix)]
    {
        assert_eq!(st("exec.sh"), State::Modified);
        assert_eq!(
            entry("exec.sh").options,
            "Mode changed from 100644 to 100755"
        );
    }

    // Removed entries are listed although they're gone from disk; the
    // .git folder is not listed.
    let entries = vc.get_entries(&root).unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"deleted.txt"));
    assert!(names.contains(&"renamed.txt"));
    assert!(!names.contains(&".git"));
    let deleted = entries.iter().find(|e| e.name == "deleted.txt").unwrap();
    assert!(!deleted.is_present());
    assert_eq!(deleted.status(), "Removed");

    // A missing path gives its full path as name
    let gone = vc.get_entry(&root.join("nowhere.txt"));
    assert_eq!(gone.name, root.join("nowhere.txt").to_string_lossy());
    assert_eq!(gone.state, State::Normal);
}

fn names(rows: &[VcRow]) -> Vec<String> {
    rows.iter()
        .map(|r| match r {
            VcRow::Entry { entry, .. } => entry.name.clone(),
            VcRow::Empty(s) | VcRow::Error(s) => s.clone(),
        })
        .collect()
}

#[test]
fn tree_scanning() {
    let (_tmp, root) = status_repo();
    let vc = open(&root);

    // Default filters: flattened, modified only
    let tree = scan_tree(vc.as_ref(), &root, &StatusFilters::default()).unwrap();
    let VcRow::Entry {
        entry,
        children,
        expanded,
    } = &tree
    else {
        panic!()
    };
    assert_eq!(entry.path, root);
    assert!(expanded);
    let flat = names(children);
    for n in [
        "modified.txt",
        "deleted.txt",
        "new.txt",
        "renamed2.txt",
        "nested.txt",
    ] {
        assert!(flat.contains(&n.to_string()), "{n} missing from {flat:?}");
    }
    assert!(!flat.contains(&"clean.txt".to_string()));
    assert!(!flat.contains(&"untracked.txt".to_string()));
    // Folders aren't rows in flattened mode unless their own state changed
    assert!(!flat.contains(&"sub".to_string()));
    // Root files come before files of sub-folders (path order)
    let pos = |n: &str| flat.iter().position(|x| x == n).unwrap();
    assert!(pos("modified.txt") < pos("nested.txt"));

    // Nested, everything visible
    let all = StatusFilters::from_keys(&["modified", "normal", "unknown", "ignored"]);
    let tree = scan_tree(vc.as_ref(), &root, &all).unwrap();
    let top = names(tree.children());
    // Folders first, then files by name
    assert_eq!(&top[..2], &["build".to_string(), "sub".to_string()]);
    assert!(top.contains(&"clean.txt".to_string()));
    assert!(top.contains(&"untracked.txt".to_string()));
    assert!(top.contains(&"debug.log".to_string()));
    let sub = tree
        .children()
        .iter()
        .find(|r| r.entry().is_some_and(|e| e.name == "sub"))
        .unwrap();
    assert_eq!(names(sub.children()), vec!["clean.txt", "nested.txt"]);
    assert!(matches!(sub, VcRow::Entry { expanded: true, .. }));
    // Ignored folders are not descended into
    let build = tree
        .children()
        .iter()
        .find(|r| r.entry().is_some_and(|e| e.name == "build"))
        .unwrap();
    assert!(build.children().is_empty());

    // Folder without visible entries gets the "(Empty)" placeholder
    let only_unknown = StatusFilters::from_keys(&["unknown"]);
    let tree = scan_tree(vc.as_ref(), &root, &only_unknown).unwrap();
    let sub = tree
        .children()
        .iter()
        .find(|r| r.entry().is_some_and(|e| e.name == "sub"))
        .unwrap();
    assert_eq!(sub.children(), &[VcRow::Empty("(Empty)".into())]);

    let auto = meld_vc::auto_compare_paths(
        &scan_tree(vc.as_ref(), &root, &StatusFilters::default()).unwrap(),
    );
    assert!(auto.contains(&root.join("modified.txt")));
    assert!(!auto.contains(&root.join("deleted.txt")));
}

#[test]
fn valid_actions() {
    let (_tmp, root) = status_repo();
    let vc = open(&root);
    let sel = |items: &[(&str, State)]| -> Vec<(PathBuf, State)> {
        items.iter().map(|(n, s)| (root.join(n), *s)).collect()
    };
    let v = vc.get_valid_actions(&sel(&[
        ("modified.txt", State::Modified),
        ("new.txt", State::New),
    ]));
    for a in [
        Action::Compare,
        Action::Commit,
        Action::Add,
        Action::Revert,
        Action::Unstage,
        Action::Remove,
        Action::Push,
        Action::Update,
    ] {
        assert!(v.contains(&a), "{a}");
    }
    assert!(!v.contains(&Action::Resolve));
    let v = vc.get_valid_actions(&sel(&[("untracked.txt", State::Unversioned)]));
    assert!(
        v.contains(&Action::Add) && !v.contains(&Action::Commit) && !v.contains(&Action::Unstage)
    );
    let v = vc.get_valid_actions(&[(root.clone(), State::Normal)]);
    assert!(!v.contains(&Action::Remove));
    let s = meld_vc::action_sensitivity(Some(vc.as_ref()), &[(root.clone(), State::Normal)]);
    assert!(!s.delete_locally && s.open_external && s.commit);
}

#[test]
fn base_revision_extraction() {
    let (_tmp, root) = status_repo();
    let vc = open(&root);
    let base = vc
        .get_path_for_repo_file(&root.join("modified.txt"), None)
        .unwrap();
    assert_eq!(fs::read_to_string(&base).unwrap(), "modified.txt\n");
    assert!(
        base.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("meld-tmp")
    );
    assert!(base.to_string_lossy().ends_with(".txt"));
    fs::remove_file(base).unwrap();

    // Files in sub-folders and files that don't exist in HEAD (empty)
    let base = vc
        .get_path_for_repo_file(&root.join("sub/nested.txt"), None)
        .unwrap();
    assert_eq!(fs::read_to_string(&base).unwrap(), "sub/nested.txt\n");
    fs::remove_file(base).unwrap();
    let base = vc
        .get_path_for_repo_file(&root.join("new.txt"), None)
        .unwrap();
    assert_eq!(fs::read_to_string(&base).unwrap(), "");
    fs::remove_file(base).unwrap();

    // An explicit commit
    git(&root, &["commit", "-q", "-a", "-m", "second"]);
    let old = vc
        .get_path_for_repo_file(&root.join("modified.txt"), Some("HEAD~1"))
        .unwrap();
    assert_eq!(fs::read_to_string(&old).unwrap(), "modified.txt\n");
    fs::remove_file(old).unwrap();

    assert!(
        vc.get_path_for_repo_file(Path::new("/definitely/elsewhere.txt"), None)
            .is_err()
    );
}

#[test]
fn compare_to_base_requests() {
    let (_tmp, root) = status_repo();
    let vc = open(&root);
    let file = root.join("modified.txt");

    let mut temps = meld_vc::TempFiles::new();
    let req = prepare_diff(vc.as_ref(), &file, &DiffOptions::default()).unwrap();
    let DiffRequest::Files {
        paths,
        labels,
        tab_label,
        temp_files,
        diff_sides,
        merge_output,
        ..
    } = req
    else {
        panic!("expected a file comparison")
    };
    assert_eq!(paths[1], file);
    assert_eq!(paths[0], temp_files[0]);
    assert_eq!(
        labels,
        vec![Some("modified.txt — repository".to_string()), None]
    );
    assert_eq!(tab_label, "modified.txt (repository, working)");
    assert_eq!(diff_sides, None);
    assert_eq!(merge_output, None);
    assert!(
        fs::metadata(&temp_files[0])
            .unwrap()
            .permissions()
            .readonly()
    );
    temps.register(temp_files);

    let opts = DiffOptions {
        left_is_local: true,
        ..DiffOptions::default()
    };
    let DiffRequest::Files {
        paths,
        labels,
        tab_label,
        temp_files,
        diff_sides,
        ..
    } = prepare_diff(vc.as_ref(), &file, &opts).unwrap()
    else {
        panic!()
    };
    assert_eq!(paths[0], file);
    assert_eq!(
        labels,
        vec![None, Some("modified.txt — repository".to_string())]
    );
    assert_eq!(tab_label, "modified.txt (working, repository)");
    assert_eq!(diff_sides, Some([DiffSide::New, DiffSide::Old]));
    temps.register(temp_files);

    assert_eq!(
        prepare_diff(vc.as_ref(), &root.join("sub"), &opts).unwrap(),
        DiffRequest::Folder(root.join("sub"))
    );

    let registered = temps.files().to_vec();
    assert!(temps.cleanup().is_empty());
    assert!(registered.iter().all(|p| !p.exists()));
}

fn conflict_repo() -> (tempfile::TempDir, PathBuf) {
    let (tmp, root) = init_repo();
    write(&root, "conflict.txt", "a\nb\nc\n");
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "base"]);
    git(&root, &["checkout", "-q", "-b", "other"]);
    write(&root, "conflict.txt", "a\nOTHER\nc\n");
    git(&root, &["commit", "-q", "-a", "-m", "other"]);
    git(&root, &["checkout", "-q", "main"]);
    write(&root, "conflict.txt", "a\nMAIN\nc\n");
    git(&root, &["commit", "-q", "-a", "-m", "main"]);
    git(&root, &["merge", "other"]);
    (tmp, root)
}

#[test]
fn conflict_extraction() {
    let (_tmp, root) = conflict_repo();
    let vc = open(&root);
    let file = root.join("conflict.txt");
    assert_eq!(vc.get_entry(&file).state, State::Conflict);
    assert!(
        vc.get_valid_actions(&[(file.clone(), State::Conflict)])
            .contains(&Action::Resolve)
    );

    let read = |side| {
        let (p, is_temp) = vc.get_path_for_conflict(&file, side).unwrap();
        assert!(is_temp);
        let content = fs::read_to_string(&p).unwrap();
        fs::remove_file(&p).unwrap();
        (p, content)
    };
    let (base_path, base) = read(ConflictSide::Base);
    assert_eq!(base, "a\nb\nc\n");
    assert!(
        base_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("meld-tmp-Base")
    );
    assert_eq!(read(ConflictSide::Local).1, "a\nMAIN\nc\n");
    assert_eq!(read(ConflictSide::Remote).1, "a\nOTHER\nc\n");
    let (merged_path, merged) = read(ConflictSide::Merged);
    assert_eq!(merged, "a\n==== BASE ====\nb\n==== BASE ====\nc\n");
    assert!(
        merged_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("meld-tmp-0-")
    );

    let prefill = vc.get_commit_message_prefill().unwrap();
    assert!(prefill.starts_with("Merge branch 'other'"), "{prefill:?}");
    assert!(!prefill.contains('#'));

    // 3-way request, both file orders
    let opts = DiffOptions {
        merge_file_order: MergeFileOrder::LocalMergeRemote,
        ..Default::default()
    };
    let DiffRequest::Files {
        paths,
        labels,
        tab_label,
        merge_output,
        prompt_resolve,
        auto_merge,
        temp_files,
        ..
    } = prepare_diff(vc.as_ref(), &file, &opts).unwrap()
    else {
        panic!()
    };
    assert_eq!(paths.len(), 3);
    assert_eq!(fs::read_to_string(&paths[0]).unwrap(), "a\nMAIN\nc\n");
    assert_eq!(fs::read_to_string(&paths[2]).unwrap(), "a\nOTHER\nc\n");
    assert_eq!(labels[0].as_deref(), Some("conflict.txt — local"));
    assert_eq!(labels[1], None);
    assert_eq!(tab_label, "conflict.txt (local, merge, remote)");
    assert_eq!(merge_output, Some(file.clone()));
    assert!(prompt_resolve && !auto_merge);
    assert_eq!(temp_files.len(), 3);
    let mut temps = meld_vc::TempFiles::new();
    temps.register(temp_files);
    drop(temps);

    let DiffRequest::Files {
        paths,
        tab_label,
        temp_files,
        ..
    } = prepare_diff(vc.as_ref(), &file, &DiffOptions::default()).unwrap()
    else {
        panic!()
    };
    assert_eq!(fs::read_to_string(&paths[0]).unwrap(), "a\nOTHER\nc\n");
    assert_eq!(tab_label, "conflict.txt (remote, merge, local)");
    let mut temps = meld_vc::TempFiles::new();
    temps.register(temp_files);
    drop(temps);

    // Resolve marks the conflict as handled
    write(&root, "conflict.txt", "a\nRESOLVED\nc\n");
    let cmds = vc.resolve(std::slice::from_ref(&file)).unwrap();
    assert_eq!(
        cmds[0].console_line(),
        format!("git add conflict.txt (in {})\n", root.display())
    );
    for (_, out) in run_all(&cmds).unwrap() {
        assert!(out.success(), "{}", out.stderr);
    }
    let mut vc = vc;
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&file).state, State::Modified);
    assert_eq!(vc.get_entry(&file).options, "Staged");
}

#[test]
fn add_commit_remove_revert_unstage() {
    let (_tmp, root) = status_repo();
    let mut vc = open(&root);
    let run = |cmds: Vec<meld_vc::VcCommand>| {
        for (cmd, out) in run_all(&cmds).unwrap() {
            assert!(
                out.success(),
                "{} failed: {}",
                cmd.console_line(),
                out.stderr
            );
        }
    };

    // add
    let untracked = root.join("untracked.txt");
    run(vc.add(std::slice::from_ref(&untracked)).unwrap());
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&untracked).state, State::New);

    // unstage
    run(vc.unstage(std::slice::from_ref(&untracked)).unwrap());
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&untracked).state, State::Unversioned);
    run(vc.add(std::slice::from_ref(&untracked)).unwrap());

    // files to commit for a folder and a file
    let files = vc
        .get_files_to_commit(&[root.join("sub"), root.join("new.txt")])
        .unwrap();
    assert_eq!(files, vec!["new.txt", "sub/nested.txt"]);

    // commit only some files, with a multi-word message
    let cmds = vc
        .commit(
            &[root.join("new.txt"), untracked.clone()],
            "Add new files\n\nBody",
        )
        .unwrap();
    assert!(
        cmds[0]
            .console_line()
            .starts_with("git commit -m \"Add new files\n\nBody\" new.txt untracked.txt")
    );
    run(cmds);
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&root.join("new.txt")).state, State::Normal);
    assert_eq!(vc.get_entry(&untracked).state, State::Normal);
    assert_eq!(
        vc.get_entry(&root.join("modified.txt")).state,
        State::Modified
    );
    assert_eq!(
        git(&root, &["log", "-1", "--format=%B"]).trim(),
        "Add new files\n\nBody"
    );

    // remove
    run(vc.remove(&[root.join("clean.txt")]).unwrap());
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&root.join("clean.txt")).state, State::Removed);
    assert!(!root.join("clean.txt").exists());

    // revert: an existing modified file and a deleted file (checkout HEAD)
    let cmds = vc
        .revert(&[root.join("modified.txt"), root.join("deleted.txt")])
        .unwrap();
    assert_eq!(cmds.len(), 2);
    assert_eq!(cmds[0].argv(), vec!["checkout", "modified.txt"]);
    assert_eq!(cmds[1].argv(), vec!["checkout", "HEAD", "deleted.txt"]);
    run(cmds);
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(
        vc.get_entry(&root.join("modified.txt")).state,
        State::Normal
    );
    assert_eq!(vc.get_entry(&root.join("deleted.txt")).state, State::Normal);
    assert_eq!(
        fs::read_to_string(root.join("deleted.txt")).unwrap(),
        "deleted.txt\n"
    );

    let display = meld_vc::files_to_commit_display(vc.as_ref(), &[root.join("clean.txt")]).unwrap();
    assert_eq!(display, vec!["clean.txt"]);
}

#[test]
fn single_file_refresh() {
    let (_tmp, root) = status_repo();
    let mut vc = open(&root);
    let file = root.join("modified.txt");
    assert_eq!(vc.get_entry(&file).state, State::Modified);
    write(&root, "modified.txt", "modified.txt\n");
    vc.refresh_vc_state(Some(&file)).unwrap();
    assert_eq!(vc.get_entry(&file).state, State::Normal);
    write(&root, "modified.txt", "again\n");
    vc.refresh_vc_state(Some(&file)).unwrap();
    assert_eq!(vc.get_entry(&file).state, State::Modified);
    // Other entries are kept
    assert_eq!(vc.get_entry(&root.join("new.txt")).state, State::New);
}

#[test]
fn subfolder_location() {
    let (_tmp, root) = status_repo();
    let sub = root.join("sub");
    let mut vc = VcKind::Git.open(&sub).unwrap();
    vc.refresh_vc_state(None).unwrap();
    assert_eq!(vc.get_entry(&sub.join("nested.txt")).state, State::Modified);
    // Status is limited to the location
    assert_eq!(
        vc.get_entry(&root.join("modified.txt")).state,
        State::Normal
    );
    let base = vc
        .get_path_for_repo_file(&sub.join("nested.txt"), None)
        .unwrap();
    assert_eq!(fs::read_to_string(&base).unwrap(), "sub/nested.txt\n");
    fs::remove_file(base).unwrap();
}

#[test]
fn push_summary() {
    let tmp = tempfile::tempdir().unwrap();
    let remote = tmp.path().join("remote.git");
    fs::create_dir(&remote).unwrap();
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    let (_t, root) = init_repo();
    write(&root, "a.txt", "a\n");
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "a"]);
    git(
        &root,
        &["remote", "add", "origin", &remote.to_string_lossy()],
    );
    git(&root, &["push", "-q", "-u", "origin", "main"]);

    let vc = open(&root);
    assert_eq!(vc.get_commits_to_push_summary().unwrap(), "");
    write(&root, "a.txt", "b\n");
    git(&root, &["commit", "-q", "-a", "-m", "b"]);
    assert_eq!(
        vc.get_commits_to_push_summary().unwrap(),
        "1 unpushed commit"
    );

    git(
        &root,
        &["checkout", "-q", "-b", "feature", "--track", "origin/main"],
    );
    write(&root, "f.txt", "f\n");
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "f1"]);
    write(&root, "f.txt", "g\n");
    git(&root, &["commit", "-q", "-a", "-m", "f2"]);
    assert_eq!(
        vc.get_commits_to_push_summary().unwrap(),
        "3 unpushed commits in 2 branches"
    );

    let cmds = vc.push().unwrap();
    assert_eq!(
        cmds[0].console_line(),
        format!("git push (in {})\n", root.display())
    );
    assert_eq!(vc.update().unwrap()[0].argv(), vec!["pull"]);
}
