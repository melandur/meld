//! Port of test/ailens/test_uncommitted.py

use std::sync::Arc;

use meld_ailens::explanation::find_change_explanations;
use meld_ailens::uncommitted::{
    HunkExplanation, MAX_ENTRIES, PlacedExplanation, UncommittedExplanations,
};
use meld_ailens::{
    ChangeExplanation, CommitHunk, DiffLineKind, GitDiffHunk, GitDiffLine, GitFileDiff, LineOrigin,
    Side, UncommittedLayer,
};

use UncommittedLayer::{Staged, Unstaged};

fn hunk(old_start: usize, new_start: usize, lines: &[&str]) -> GitDiffHunk {
    let parsed = lines
        .iter()
        .map(|line| {
            let kind = match &line[..1] {
                "+" => DiffLineKind::Add,
                "-" => DiffLineKind::Delete,
                _ => DiffLineKind::Context,
            };
            GitDiffLine::new(kind, &line[1..], None, None)
        })
        .collect();
    GitDiffHunk {
        old_start,
        old_count: lines.iter().filter(|l| !l.starts_with('+')).count(),
        new_start,
        new_count: lines.iter().filter(|l| !l.starts_with('-')).count(),
        section: String::new(),
        lines: parsed,
    }
}

fn file(
    old: Option<&str>,
    new: Option<&str>,
    binary: bool,
    hunks: Vec<GitDiffHunk>,
) -> GitFileDiff {
    GitFileDiff {
        old_path: old.map(str::to_string),
        new_path: new.map(str::to_string),
        binary,
        hunks,
    }
}

fn top() -> GitDiffHunk {
    hunk(1, 1, &["-one", "+two"])
}

fn bottom() -> GitDiffHunk {
    hunk(9, 9, &["-nine", "+NINE"])
}

fn diff() -> GitFileDiff {
    file(Some("a.ts"), Some("a.ts"), false, vec![top(), bottom()])
}

fn explained() -> HunkExplanation {
    HunkExplanation {
        what: "Shouts nine.".into(),
        why: "Louder.".into(),
        justification: None,
        model: "claude-sonnet-5-5".into(),
    }
}

fn commit_hunk(
    path: &str,
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
    original: Option<&str>,
) -> CommitHunk {
    CommitHunk {
        path: path.into(),
        old_start,
        old_count,
        new_start,
        new_count,
        original_path: original.map(str::to_string),
    }
}

#[test]
fn keeps_an_explanation_per_layer_file_and_hunk() {
    let cache = UncommittedExplanations::default();
    cache.set(Staged, &diff(), &bottom(), explained());

    assert_eq!(cache.get(Staged, &diff(), &bottom()), Some(explained()));
    assert_eq!(cache.get(Staged, &diff(), &top()), None);
    assert_eq!(cache.get(Unstaged, &diff(), &bottom()), None);
    let other = file(Some("b.ts"), Some("b.ts"), false, vec![bottom()]);
    assert_eq!(cache.get(Staged, &other, &bottom()), None);
}

#[test]
fn lasts_while_the_hunk_moves_and_ends_when_it_changes() {
    let cache = UncommittedExplanations::default();
    cache.set(Staged, &diff(), &bottom(), explained());

    let moved = hunk(20, 22, &["-nine", "+NINE"]);
    assert_eq!(cache.get(Staged, &diff(), &moved), Some(explained()));
    let changed = hunk(9, 9, &["-nine", "+NINE!"]);
    assert_eq!(cache.get(Staged, &diff(), &changed), None);
    // The kind of a line counts, not just its text
    let context = hunk(9, 9, &[" nine", "+NINE"]);
    assert_eq!(cache.get(Staged, &diff(), &context), None);
}

#[test]
fn a_rename_is_a_different_file() {
    let cache = UncommittedExplanations::default();
    let renamed = file(Some("old.ts"), Some("a.ts"), false, vec![top()]);
    cache.set(Staged, &renamed, &top(), explained());
    assert_eq!(cache.get(Staged, &diff(), &top()), None);
    assert_eq!(cache.get(Staged, &renamed, &top()), Some(explained()));
}

#[test]
fn place_puts_explanations_where_the_hunks_are_now() {
    let cache = UncommittedExplanations::default();
    cache.set(Unstaged, &diff(), &bottom(), explained());

    let moved = hunk(19, 21, &["-nine", "+NINE"]);
    let now = file(
        Some("old.ts"),
        Some("a.ts"),
        false,
        vec![top(), moved.clone()],
    );
    cache.set(Unstaged, &now, &moved, explained());
    let placed = cache.place(Unstaged, std::slice::from_ref(&now));
    assert_eq!(
        placed,
        [PlacedExplanation {
            hunk: commit_hunk("a.ts", 19, 1, 21, 1, Some("old.ts")),
            what: "Shouts nine.".into(),
            why: "Louder.".into(),
            justification: None,
            model: "claude-sonnet-5-5".into(),
        }]
    );
    // Ready for find_change_explanations, like a commit's changes
    let found: Vec<PlacedExplanation> =
        find_change_explanations(&[LineOrigin::new("a.ts", Side::New, 21)], &placed)
            .into_iter()
            .cloned()
            .collect();
    assert_eq!(found, placed);
    let found: Vec<PlacedExplanation> =
        find_change_explanations(&[LineOrigin::new("old.ts", Side::Old, 19)], &placed)
            .into_iter()
            .cloned()
            .collect();
    assert_eq!(found, placed);
}

#[test]
fn place_handles_additions_deletions_and_nothing_explained() {
    let cache = UncommittedExplanations::default();
    let added = file(None, Some("new.ts"), false, vec![hunk(0, 1, &["+hi"])]);
    let deleted = file(Some("gone.ts"), None, false, vec![hunk(1, 0, &["-bye"])]);
    cache.set(Unstaged, &added, &added.hunks[0], explained());
    cache.set(Unstaged, &deleted, &deleted.hunks[0], explained());

    let placed = cache.place(
        Unstaged,
        &[
            added.clone(),
            deleted,
            diff(),
            file(None, None, true, vec![]),
        ],
    );
    let hunks: Vec<CommitHunk> = placed.into_iter().map(|p| p.hunk).collect();
    assert_eq!(
        hunks,
        [
            commit_hunk("new.ts", 0, 0, 1, 1, None),
            commit_hunk("gone.ts", 1, 1, 0, 0, None),
        ]
    );
    assert_eq!(cache.place(Staged, &[added]), []);
}

#[test]
fn remember_keeps_a_models_answers_by_hunk_position() {
    let cache = UncommittedExplanations::default();
    let changes = [
        ChangeExplanation {
            hunk: commit_hunk("a.ts", 9, 1, 9, 1, None),
            what: "Shouts nine.".into(),
            why: "Louder.".into(),
            justification: None,
        },
        // Matches no hunk of the diff
        ChangeExplanation {
            hunk: commit_hunk("a.ts", 50, 1, 50, 1, None),
            what: "Gone".into(),
            why: "Stale.".into(),
            justification: None,
        },
    ];
    assert_eq!(cache.remember(Staged, &diff(), &changes, "claude-x"), 1);
    assert_eq!(
        cache.get(Staged, &diff(), &bottom()),
        Some(HunkExplanation {
            what: "Shouts nine.".into(),
            why: "Louder.".into(),
            justification: None,
            model: "claude-x".into(),
        })
    );
    assert!(!cache.is_explained(Staged, &diff()));

    cache.remember(
        Staged,
        &diff(),
        &[ChangeExplanation {
            hunk: commit_hunk("a.ts", 1, 1, 1, 1, None),
            what: "w".into(),
            why: "y".into(),
            justification: None,
        }],
        "claude-x",
    );
    assert!(cache.is_explained(Staged, &diff()));
    assert!(!cache.is_explained(Unstaged, &diff()));
}

#[test]
fn setting_again_replaces_and_refreshes() {
    let cache = UncommittedExplanations::new(2);
    let first = HunkExplanation {
        what: "1".into(),
        why: "1".into(),
        justification: None,
        model: "m".into(),
    };
    cache.set(Staged, &diff(), &top(), first.clone());
    cache.set(Staged, &diff(), &bottom(), first.clone());
    // Refreshing TOP makes BOTTOM the oldest
    cache.set(Staged, &diff(), &top(), explained());
    cache.set(Unstaged, &diff(), &top(), first);

    assert_eq!(cache.len(), 2);
    assert_eq!(cache.get(Staged, &diff(), &top()), Some(explained()));
    assert_eq!(cache.get(Staged, &diff(), &bottom()), None);
}

#[test]
fn keeps_at_most_max_entries_dropping_the_oldest() {
    let cache = UncommittedExplanations::default();
    let hunks: Vec<GitDiffHunk> = (0..MAX_ENTRIES + 5)
        .map(|i| hunk(i, i, &[&format!("+line {i}")]))
        .collect();
    for h in &hunks {
        cache.set(Unstaged, &diff(), h, explained());
    }

    assert_eq!(cache.len(), MAX_ENTRIES);
    assert_eq!(cache.get(Unstaged, &diff(), &hunks[0]), None);
    assert_eq!(cache.get(Unstaged, &diff(), &hunks[4]), None);
    assert_eq!(cache.get(Unstaged, &diff(), &hunks[5]), Some(explained()));
    assert_eq!(
        cache.get(Unstaged, &diff(), hunks.last().unwrap()),
        Some(explained())
    );
}

#[test]
fn clear() {
    let cache = UncommittedExplanations::default();
    cache.set(Staged, &diff(), &top(), explained());
    cache.clear();
    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());
    assert_eq!(cache.get(Staged, &diff(), &top()), None);
}

#[test]
fn unicode_content_is_keyed_by_its_text() {
    let cache = UncommittedExplanations::default();
    let snow = hunk(1, 1, &["-☃", "+雪"]);
    cache.set(Staged, &diff(), &snow, explained());
    assert_eq!(
        cache.get(Staged, &diff(), &hunk(3, 3, &["-☃", "+雪"])),
        Some(explained())
    );
    assert_eq!(
        cache.get(Staged, &diff(), &hunk(1, 1, &["-☃", "+雨"])),
        None
    );
}

#[test]
fn is_safe_to_share_between_threads() {
    let cache = Arc::new(UncommittedExplanations::new(500));
    let hunks: Arc<Vec<GitDiffHunk>> =
        Arc::new((0..200).map(|i| hunk(i, i, &[&format!("+{i}")])).collect());

    let threads: Vec<_> = [Staged, Unstaged]
        .repeat(4)
        .into_iter()
        .map(|layer| {
            let cache = cache.clone();
            let hunks = hunks.clone();
            std::thread::spawn(move || {
                let whole = file(Some("a.ts"), Some("a.ts"), false, hunks.to_vec());
                for _ in 0..5 {
                    for h in hunks.iter() {
                        cache.set(layer, &diff(), h, explained());
                        cache.get(layer, &diff(), h);
                    }
                    cache.place(layer, std::slice::from_ref(&whole));
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().expect("no panics");
    }

    assert_eq!(cache.len(), 400);
}

#[test]
fn justifications_are_kept_and_placed() {
    let cache = UncommittedExplanations::default();
    let changes = [ChangeExplanation {
        hunk: commit_hunk("a.ts", 9, 1, 9, 1, None),
        what: "Shouts nine.".into(),
        why: "Louder.".into(),
        justification: Some("Nothing else is loud enough.".into()),
    }];
    assert_eq!(cache.remember(Staged, &diff(), &changes, "claude-x"), 1);
    assert_eq!(
        cache
            .get(Staged, &diff(), &bottom())
            .unwrap()
            .justification
            .as_deref(),
        Some("Nothing else is loud enough.")
    );
    let placed = cache.place(Staged, &[diff()]);
    assert_eq!(placed.len(), 1);
    assert_eq!(
        placed[0].justification.as_deref(),
        Some("Nothing else is loud enough.")
    );
    assert_eq!(placed[0].as_change(), changes[0]);
}
