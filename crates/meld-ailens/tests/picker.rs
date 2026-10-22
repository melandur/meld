//! Port of test/ailens/test_picker.py
//!
//! Labels are English only: the Rust port has no gettext.

use std::collections::HashMap;
use std::path::PathBuf;

use meld_ailens::picker::{
    EntryKind, PickerEntry, PickerSide, PickerSources, apply_pick, build_entries, filter_entries,
    relative_date, revisions_from_args, typed_entry,
};
use meld_ailens::time::timestamp_utc;
use meld_ailens::{
    GitCommit, GitIdentity, GitRef, GitRefType, GitWorktree, Revision, RevisionKind,
};

fn now() -> f64 {
    timestamp_utc(2026, 10, 21, 12, 0, 0.0)
}

fn ref_(name: &str, kind: GitRefType, date: &str) -> GitRef {
    let full = match kind {
        GitRefType::Branch => format!("refs/heads/{name}"),
        GitRefType::Remote => format!("refs/remotes/{name}"),
        GitRefType::Tag => format!("refs/tags/{name}"),
    };
    GitRef {
        kind,
        name: name.into(),
        full_name: full,
        sha: "a".repeat(40),
        subject: "Subj".into(),
        date: date.into(),
        current: false,
        worktree: None,
    }
}

fn branch(name: &str) -> GitRef {
    ref_(name, GitRefType::Branch, "2026-10-20T12:00:00+00:00")
}

fn commit(sha: &str, subject: &str) -> GitCommit {
    let who = GitIdentity {
        name: "Jane Doe".into(),
        email: "jane@example.com".into(),
        date: "2026-10-19T12:00:00+00:00".into(),
    };
    GitCommit {
        sha: sha.into(),
        author: who.clone(),
        committer: who,
        subject: subject.into(),
        ..Default::default()
    }
}

fn worktree(path: &str, head: Option<&str>, branch: Option<&str>, main: bool) -> GitWorktree {
    GitWorktree {
        path: path.into(),
        head: head.map(str::to_string),
        branch: branch.map(str::to_string),
        main,
        ..Default::default()
    }
}

fn wt(path: &str) -> Option<PathBuf> {
    Some(PathBuf::from(path))
}

#[test]
fn relative_dates() {
    let cases: [(f64, &str); 7] = [
        (-10.0, "now"),
        (-60.0, "1 minute ago"),
        (-5.0 * 3600.0, "5 hours ago"),
        (-86400.0, "1 day ago"),
        (-14.0 * 86400.0, "2 weeks ago"),
        (-400.0 * 86400.0, "1 year ago"),
        (2.0 * 3600.0, "in 2 hours"),
    ];
    for (delta, expected) in cases {
        assert_eq!(relative_date(now() + delta, now()), expected, "{delta}");
    }
}

#[test]
fn entries_order_and_sections() {
    let mut main = ref_("main", GitRefType::Branch, "2025-01-01T00:00:00+00:00");
    main.current = true;
    let refs = [
        ref_("old", GitRefType::Branch, "2026-01-01T00:00:00+00:00"),
        main,
        branch("new"),
        ref_(
            "origin/main",
            GitRefType::Remote,
            "2026-10-20T12:00:00+00:00",
        ),
        ref_("v1", GitRefType::Tag, "2026-10-20T12:00:00+00:00"),
    ];
    let mut bare = worktree("/bare", None, None, false);
    bare.bare = true;
    let worktrees = [
        worktree("/repo", Some(&"a".repeat(40)), Some("main"), true),
        worktree("/wt/feature", Some(&"b".repeat(40)), Some("feature"), false),
        bare,
    ];
    let commits = [commit(&"c".repeat(40), "Fixes it")];
    let head = commit(&"a".repeat(40), "Head subject");
    let entries = build_entries(
        &PickerSources {
            refs: &refs,
            worktrees: &worktrees,
            commits: &commits,
            current_worktree: Some("/repo".as_ref()),
            head: Some(&head),
            head_branch: Some("main"),
            now: Some(now()),
        },
        PickerSide::Target,
    );
    let labels: Vec<&str> = entries.iter().map(|e| e.label.as_str()).collect();
    assert_eq!(labels[0], "HEAD");
    // This worktree's uncommitted versions, then the other worktree's
    assert_eq!(labels[1..4], ["Working Tree", "Unstaged", "Staged"]);
    assert_eq!(
        labels[4..7],
        [
            "Working Tree (feature)",
            "Unstaged (feature)",
            "Staged (feature)"
        ]
    );
    // Current branch first, then newest first
    assert_eq!(labels[7..10], ["main", "new", "old"]);
    assert_eq!(labels[10..], ["origin/main", "v1", "Fixes it"]);

    assert_eq!(entries[1].revision, Revision::working_tree(None));
    assert_eq!(entries[2].paired_base, Some(Revision::index(None)));
    assert_eq!(
        entries[4].revision,
        Revision::working_tree(wt("/wt/feature"))
    );
    let last = entries.last().unwrap();
    assert_eq!(last.revision, Revision::commit("c".repeat(40)));
    assert_eq!(last.short_label, "ccccccc");
    assert!(
        last.description.contains("2 days ago"),
        "{}",
        last.description
    );
}

#[test]
fn base_side_offers_no_working_tree() {
    let worktrees = [worktree("/repo", Some(&"a".repeat(40)), Some("main"), true)];
    let entries = build_entries(
        &PickerSources {
            worktrees: &worktrees,
            current_worktree: Some("/repo".as_ref()),
            now: Some(now()),
            ..Default::default()
        },
        PickerSide::Base,
    );
    let labels: Vec<&str> = entries.iter().map(|e| e.label.as_str()).collect();
    assert_eq!(labels, ["Staged"]);
    assert_eq!(entries[0].revision.kind, RevisionKind::Index);
}

#[test]
fn branch_in_other_worktree_is_marked() {
    let mut feature = branch("feature");
    feature.worktree = wt("/wt/feature");
    let refs = [feature];
    let entries = build_entries(
        &PickerSources {
            refs: &refs,
            current_worktree: Some("/repo".as_ref()),
            now: Some(now()),
            ..Default::default()
        },
        PickerSide::Target,
    );
    assert!(entries[0].description.contains("in worktree /wt/feature"));
}

#[test]
fn unborn_worktree() {
    let worktrees = [worktree("/repo", None, Some("main"), true)];
    let entries = build_entries(
        &PickerSources {
            worktrees: &worktrees,
            current_worktree: Some("/repo".as_ref()),
            now: Some(now()),
            ..Default::default()
        },
        PickerSide::Target,
    );
    assert!(!entries.is_empty());
    assert!(
        entries
            .iter()
            .all(|e| e.description.contains("unborn branch"))
    );
}

#[test]
fn typed_entry_accepts_refs() {
    for text in [
        "main",
        "origin/main",
        "HEAD~3",
        "HEAD^2",
        "abc1234",
        "v1.0",
        "a/b-c_d",
    ] {
        let entry = typed_entry(text).unwrap_or_else(|| panic!("{text}"));
        assert_eq!(entry.revision, Revision::commit(text));
    }
}

#[test]
fn typed_entry_rejects_non_refs() {
    for text in [
        "",
        "   ",
        "two words",
        "-rf",
        "a:b",
        "x*",
        "a[1]",
        "back\\slash",
    ] {
        assert!(typed_entry(text).is_none(), "{text}");
    }
}

#[test]
fn filters_entries() {
    let mut main = branch("main");
    main.current = true;
    let refs = [main, branch("feature/login")];
    let commits = [commit(&"d".repeat(40), "Adds login form")];
    let entries = build_entries(
        &PickerSources {
            refs: &refs,
            commits: &commits,
            now: Some(now()),
            ..Default::default()
        },
        PickerSide::Target,
    );
    assert_eq!(filter_entries(&entries, ""), entries);
    let matched = filter_entries(&entries, "login");
    // "login" could be a ref too, so it's offered first
    let kinds: Vec<EntryKind> = matched.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        [EntryKind::Typed, EntryKind::Branch, EntryKind::Commit]
    );
    let labels: Vec<String> = filter_entries(&entries, "ADDS form")
        .into_iter()
        .map(|e| e.label)
        .collect();
    assert_eq!(labels, ["Adds login form"]);
    // An exact name match isn't offered again as typed text
    let kinds: Vec<EntryKind> = filter_entries(&entries, "main")
        .iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, [EntryKind::Branch]);
}

#[test]
fn revisions_from_args_parses() {
    let cases = [
        (
            None,
            None,
            (Revision::commit("HEAD"), Revision::working_tree(None)),
        ),
        (
            Some("main"),
            Some("staged"),
            (Revision::commit("main"), Revision::index(None)),
        ),
        (
            Some("v1"),
            Some("WORKING-TREE"),
            (Revision::commit("v1"), Revision::working_tree(None)),
        ),
        (
            None,
            Some("unstaged"),
            (Revision::index(None), Revision::working_tree(None)),
        ),
        (
            Some("staged"),
            Some("unstaged"),
            (Revision::index(None), Revision::working_tree(None)),
        ),
        (
            Some("a1b2c3d"),
            Some("main"),
            (Revision::commit("a1b2c3d"), Revision::commit("main")),
        ),
    ];
    for (base, target, expected) in cases {
        assert_eq!(
            revisions_from_args(base, target).unwrap(),
            expected,
            "{base:?} {target:?}"
        );
    }
}

#[test]
fn revisions_from_args_errors() {
    for (base, target) in [
        (Some("unstaged"), None),
        (Some("main"), Some("unstaged")),
        (Some("two words"), None),
        (None, Some("-x")),
    ] {
        assert!(
            revisions_from_args(base, target).is_err(),
            "{base:?} {target:?}"
        );
    }
}

fn pick(revision: Revision, paired_base: Option<Revision>) -> PickerEntry {
    PickerEntry {
        kind: EntryKind::Uncommitted,
        section: String::new(),
        label: String::new(),
        description: String::new(),
        icon_name: String::new(),
        revision,
        short_label: String::new(),
        paired_base,
        search_text: String::new(),
    }
}

#[test]
fn apply_picks() {
    use PickerSide::{Base, Target};
    let c = Revision::commit;
    let cases = [
        // Plain picks
        (c("a"), c("b"), c("c"), Target, (c("a"), c("c"))),
        (
            c("a"),
            Revision::working_tree(None),
            c("c"),
            Base,
            (c("c"), Revision::working_tree(None)),
        ),
        // Staged as base makes the target this worktree's files
        (
            c("a"),
            c("b"),
            Revision::index(None),
            Base,
            (Revision::index(None), Revision::working_tree(None)),
        ),
        (
            c("a"),
            c("b"),
            Revision::index(wt("/wt")),
            Base,
            (
                Revision::index(wt("/wt")),
                Revision::working_tree(wt("/wt")),
            ),
        ),
        // Leaving Staged → Working Tree for another target resets the base
        (
            Revision::index(None),
            Revision::working_tree(None),
            Revision::working_tree(wt("/wt")),
            Target,
            (c("wthead"), Revision::working_tree(wt("/wt"))),
        ),
        (
            Revision::index(None),
            Revision::working_tree(None),
            c("main"),
            Target,
            (c("head"), c("main")),
        ),
        (
            Revision::index(None),
            Revision::working_tree(None),
            Revision::working_tree(None),
            Target,
            (Revision::index(None), Revision::working_tree(None)),
        ),
        // A working tree picked as base moves to the target
        (
            c("a"),
            c("b"),
            Revision::working_tree(None),
            Base,
            (c("head"), Revision::working_tree(None)),
        ),
    ];
    let heads: HashMap<Option<PathBuf>, String> = [
        (None, "head".to_string()),
        (wt("/wt"), "wthead".to_string()),
    ]
    .into();
    for (base, target, picked, side, expected) in cases {
        let entry = pick(picked.clone(), None);
        assert_eq!(
            apply_pick(&base, &target, &entry, side, &heads),
            expected,
            "{picked:?} {side:?}"
        );
    }
}

#[test]
fn apply_pick_paired_base() {
    let entry = pick(
        Revision::working_tree(wt("/wt")),
        Some(Revision::index(wt("/wt"))),
    );
    assert_eq!(
        apply_pick(
            &Revision::commit("a"),
            &Revision::commit("b"),
            &entry,
            PickerSide::Target,
            &HashMap::new()
        ),
        (
            Revision::index(wt("/wt")),
            Revision::working_tree(wt("/wt"))
        )
    );
}
