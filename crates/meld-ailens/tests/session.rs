//! Port of test/ailens/test_session.py and test_file_tree.py

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};

use common::{CommitOptions, FixtureRepo, co_authored_by_claude};
use meld_ailens::claude::{ClaudeRequest, ClaudeResponse, RunClaude};
use meld_ailens::session::MAX_FILE_BYTES;
use meld_ailens::{
    CancelToken, CardKind, CompareSession, Error, ExplainAction, ExplanationSource, FileEntry,
    GitFileChange, GitFileStatus, PickerSide, RepositoryInfo, Revision, SessionConfig,
    UncommittedLayer, build_file_tree,
};
use regex::Regex;
use serde_json::json;

/// Answers like Claude Code would, and records what it was asked
#[derive(Default)]
struct FakeClaude {
    requests: Mutex<Vec<ClaudeRequest>>,
    fail_for: Vec<String>,
}

impl FakeClaude {
    fn failing_for(text: &str) -> Arc<Self> {
        Arc::new(Self {
            fail_for: vec![text.to_string()],
            ..Default::default()
        })
    }

    fn answer(
        &self,
        request: &ClaudeRequest,
        cancel: &CancelToken,
    ) -> meld_ailens::Result<ClaudeResponse> {
        self.requests.lock().unwrap().push(request.clone());
        cancel.check()?;
        for text in &self.fail_for {
            if request.prompt.contains(text.as_str()) {
                return Err(Error::msg(format!("failed on {text}")));
            }
        }
        let empty = json!({});
        let schema = request.json_schema.as_ref().unwrap_or(&empty);
        let properties = schema.get("properties").cloned().unwrap_or(json!({}));
        let mut numbers: Vec<u64> = Regex::new(r"\[change (\d+)\]")
            .unwrap()
            .captures_iter(&request.prompt)
            .map(|c| c[1].parse().unwrap())
            .collect();
        numbers.sort();
        numbers.dedup();
        let changes: Vec<_> = numbers
            .iter()
            .map(|n| {
                json!({
                    "change": n,
                    "what": format!("Change {n} what"),
                    "why": format!("Change {n} why"),
                    "justification": format!("Change {n} justification"),
                })
            })
            .collect();
        let output = if properties.get("title").is_some() {
            json!({"title": "Fake build title", "summary": "Fake summary.", "decisions": [], "risks": ["Fake risk"]})
        } else if properties.get("what").is_some() {
            json!({
                "what": "Inferred what",
                "why": "Inferred why",
                "justification": "Inferred justification",
                "changes": changes,
            })
        } else {
            json!({"changes": changes})
        };
        Ok(ClaudeResponse::new(output, "", "fake-model"))
    }

    fn runner(self: &Arc<Self>) -> RunClaude {
        let fake = self.clone();
        Arc::new(move |request, cancel| fake.answer(request, cancel))
    }
}

fn config_with(fake: Option<Arc<FakeClaude>>) -> SessionConfig {
    let fake = fake.unwrap_or_default();
    SessionConfig {
        run_claude: Some(fake.runner()),
        generator: "meld-test".into(),
        ..Default::default()
    }
}

fn config() -> SessionConfig {
    config_with(None)
}

fn never() -> CancelToken {
    CancelToken::new()
}

/// The repository, with git isolated from the user's config
fn open_info(repo: &FixtureRepo, path: Option<&Path>) -> Arc<RepositoryInfo> {
    Arc::new(
        RepositoryInfo::open(path.unwrap_or(&repo.path), &repo.git_options(), &never()).unwrap(),
    )
}

fn open_session(
    repo: &FixtureRepo,
    base: Revision,
    target: Revision,
    cfg: Option<SessionConfig>,
) -> CompareSession {
    let info = open_info(repo, None);
    CompareSession::open(info, &base, &target, cfg.unwrap_or_else(config), &never()).unwrap()
}

fn history() -> FixtureRepo {
    let mut repo = FixtureRepo::new();
    repo.commit(
        "Adds the app",
        files![("app.py", "one\ntwo\nthree\n")],
        Some("base"),
    );
    repo.commit(
        "Makes two louder\n\nWhy: Users could not hear it.",
        files![("app.py", "one\nTWO\nthree\n")],
        Some("human"),
    );
    repo.commit_with(
        "Adds four",
        files![
            ("app.py", "one\nTWO\nthree\nfour\n"),
            ("data.bin", b"\0\x01\x02")
        ],
        CommitOptions::label("agent").with_trailer(co_authored_by_claude("Opus 5.5")),
    );
    repo
}

fn base_main(history: &FixtureRepo, cfg: Option<SessionConfig>) -> CompareSession {
    open_session(
        history,
        Revision::commit(history.sha("base")),
        Revision::commit("main"),
        cfg,
    )
}

fn entry_for(session: &CompareSession, path: &str) -> FileEntry {
    session
        .files(&never())
        .unwrap()
        .into_iter()
        .find(|e| e.change.path == path)
        .unwrap()
}

#[test]
fn commit_comparison_files_and_cards() {
    let history = history();
    let session = base_main(&history, None);
    assert_eq!(session.base_label(), &history.sha("base")[..7]);
    assert_eq!(session.target_label(), "main");
    assert!(session.can_build());
    let files = session.files(&never()).unwrap();
    let paths: Vec<&str> = files.iter().map(|e| e.change.path.as_str()).collect();
    assert_eq!(paths, ["app.py", "data.bin"]);
    assert!(files[1].binary());
    assert_eq!((files[0].additions, files[0].deletions), (Some(2), Some(1)));

    let view = session.file_view(&files[0].change, &never()).unwrap();
    assert_eq!(view.base_text.as_deref(), Some(&b"one\ntwo\nthree\n"[..]));
    assert_eq!(
        view.target_text.as_deref(),
        Some(&b"one\nTWO\nthree\nfour\n"[..])
    );
    assert!(view.message.is_empty());
    let human = view
        .cards
        .iter()
        .find(|c| c.sha.as_deref() == Some(&history.sha("human")))
        .unwrap();
    assert_eq!(human.what, "Makes two louder");
    assert_eq!(human.why, "Users could not hear it.");
    assert_eq!(human.source, Some(ExplanationSource::Commit));
    assert!(!human.agent());
    // It has a Why, so only its changes can still be explained
    assert_eq!(human.explain, Some(ExplainAction::Changes));
    let agent = view
        .cards
        .iter()
        .find(|c| c.sha.as_deref() == Some(&history.sha("agent")))
        .unwrap();
    assert_eq!(agent.agent_name, "Claude Opus 5.5");
    assert_eq!(agent.agent_model, "claude-opus-5-5");
    assert_eq!(agent.explain, Some(ExplainAction::Commit));
    assert!(human.color_index == 0 && agent.color_index == 1);
    assert_eq!(agent.added.iter().map(|r| r.start).collect::<Vec<_>>(), [4]);
}

#[test]
fn binary_file_view() {
    let history = history();
    let session = base_main(&history, None);
    let entry = entry_for(&session, "data.bin");
    let view = session.file_view(&entry.change, &never()).unwrap();
    assert!(view.message.contains("binary"));
    assert!(view.cards.is_empty());
    assert!(view.message.contains(&history.sha("agent")[..7]));
}

#[test]
fn explain_commit_saves_note_and_updates_cards() {
    let history = history();
    let session = base_main(&history, None);
    let entry = entry_for(&session, "app.py");
    let view = session.file_view(&entry.change, &never()).unwrap();
    let agent_card = view
        .cards
        .iter()
        .find(|c| c.sha.as_deref() == Some(&history.sha("agent")))
        .unwrap();
    session
        .explain_card(&view, &agent_card.key, &never())
        .unwrap();

    let note = history.git(&["notes", "--ref=ai-lens", "show", &history.sha("agent")]);
    assert!(note.contains("Source: inferred") && note.contains("Model: fake-model"));
    let cards = session.cards(&view, &never()).unwrap();
    let agent = cards
        .iter()
        .find(|c| c.sha.as_deref() == Some(&history.sha("agent")))
        .unwrap();
    assert_eq!(agent.why, "Inferred why");
    assert_eq!(agent.justification, "Inferred justification");
    assert_eq!(agent.source, Some(ExplanationSource::Inferred));
    assert_eq!(agent.inferred_by, "fake-model");
    assert_eq!(agent.change_what, "Change 1 what");
    assert_eq!(agent.change_justification, "Change 1 justification");
    assert_eq!(agent.explain, None);
    assert!(
        note.contains("Why: Inferred why\nJustification: Inferred justification\n")
            && note.contains("Why: Change 1 why\nJustification: Change 1 justification"),
        "{note}"
    );
}

#[test]
fn explain_changes_keeps_the_commit_why() {
    let history = history();
    let session = base_main(&history, None);
    session
        .explain_commit(&history.sha("human"), &never())
        .unwrap();
    let entry = entry_for(&session, "app.py");
    let view = session.file_view(&entry.change, &never()).unwrap();
    let human = view
        .cards
        .iter()
        .find(|c| c.sha.as_deref() == Some(&history.sha("human")))
        .unwrap();
    assert_eq!(human.why, "Users could not hear it.");
    assert_eq!(human.source, Some(ExplanationSource::Commit));
    // The message gives no justification, and an inferred one isn't put
    // under the author's Why
    assert_eq!(human.justification, "");
    assert_eq!(human.change_why, "Change 1 why");
    assert_eq!(human.change_justification, "Change 1 justification");
    assert_eq!(human.explain, None);
}

#[test]
fn explain_all_and_build() {
    let history = history();
    let session = base_main(&history, None);
    assert_eq!(session.explain_all(&never(), None, false).unwrap(), 2);
    assert_eq!(session.explain_all(&never(), None, false).unwrap(), 0);

    let messages = Mutex::new(Vec::<String>::new());
    let progress = |text: &str| messages.lock().unwrap().push(text.to_string());
    let path = session.generate_build(&never(), Some(&progress)).unwrap();
    assert_eq!(
        path.file_name().unwrap().to_str().unwrap(),
        format!(
            "{}_{}_build.md",
            &history.sha("base")[..7],
            &history.sha("agent")[..7]
        )
    );
    assert!(
        messages
            .lock()
            .unwrap()
            .iter()
            .any(|m| m.contains("Writing the summary"))
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("Fake build title"));
    // The model that actually answered is recorded
    assert!(text.contains("build_model: \"fake-model\""));
    let index = std::fs::read_to_string(path.parent().unwrap().join("index.md")).unwrap();
    assert!(index.contains("Fake build title"));
    // Builds never contain diff text
    assert!(!text.contains("TWO"));
    assert!(
        text.contains("- Justification: Inferred justification\n"),
        "{text}"
    );
    assert!(
        text.contains("  - Justification: Change 1 justification\n"),
        "{text}"
    );
    // Cards now link to the build
    let entry = entry_for(&session, "app.py");
    let view = session.file_view(&entry.change, &never()).unwrap();
    let name = path.file_name().unwrap().to_str().unwrap().to_string();
    assert!(view.cards.iter().all(|c| c.builds.contains(&name)));
}

#[test]
fn explain_all_reports_failures_and_continues() {
    let history = history();
    let fake = FakeClaude::failing_for("Makes two louder");
    let session = base_main(&history, Some(config_with(Some(fake))));
    let err = session.explain_all(&never(), None, false).unwrap_err();
    assert!(err.to_string().contains("Explained 1; 1 failed"), "{err}");
}

#[test]
fn missing_claude_stops_explain_all() {
    let history = history();
    let missing: RunClaude =
        Arc::new(|_, _| Err(Error::claude_not_found("Claude Code was not found")));
    let cfg = SessionConfig {
        run_claude: Some(missing),
        ..config()
    };
    let session = base_main(&history, Some(cfg));
    let err = session.explain_all(&never(), None, false).unwrap_err();
    assert!(err.is_claude_not_found());
}

#[test]
fn inference_turned_off() {
    let history = history();
    let cfg = SessionConfig {
        infer_enabled: false,
        ..config()
    };
    let session = base_main(&history, Some(cfg));
    let entry = entry_for(&session, "app.py");
    let view = session.file_view(&entry.change, &never()).unwrap();
    assert!(view.cards.iter().all(|c| c.explain.is_none()));
    let err = session
        .explain_commit(&history.sha("agent"), &never())
        .unwrap_err();
    assert!(err.to_string().contains("turned off"));
}

#[test]
fn working_tree_layers() {
    let history = history();
    history.write_files(files![("app.py", "zero\none\nTWO\nthree\nfour\n")]);
    history.stage(&["app.py"]);
    history.write_files(files![
        ("app.py", "zero\none\nTWO\nthree\nfour\nfive\n"),
        ("new.txt", "n\n")
    ]);
    let session = open_session(
        &history,
        Revision::commit("HEAD"),
        Revision::working_tree(None),
        None,
    );
    assert!(!session.can_build());
    assert!(session.follows_repository_changes());
    let files = session.files(&never()).unwrap();
    let paths: Vec<&str> = files.iter().map(|e| e.change.path.as_str()).collect();
    assert_eq!(paths, ["app.py", "new.txt"]);
    assert!(files[1].change.untracked);

    let view = session.file_view(&files[0].change, &never()).unwrap();
    let mut kinds: Vec<CardKind> = view.cards.iter().map(|c| c.kind).collect();
    kinds.sort_by_key(|k| format!("{k:?}"));
    assert_eq!(kinds, [CardKind::Staged, CardKind::Unstaged]);
    assert_eq!(
        view.target_disk_path.as_deref(),
        Some(history.path.join("app.py").as_path())
    );
    assert!(
        view.cards
            .iter()
            .all(|c| c.explain == Some(ExplainAction::Uncommitted))
    );

    let staged = view
        .cards
        .iter()
        .find(|c| c.kind == CardKind::Staged)
        .unwrap();
    session.explain_card(&view, &staged.key, &never()).unwrap();
    let cards = session.cards(&view, &never()).unwrap();
    let staged = cards.iter().find(|c| c.kind == CardKind::Staged).unwrap();
    let unstaged = cards.iter().find(|c| c.kind == CardKind::Unstaged).unwrap();
    assert_eq!(staged.change_what, "Change 1 what");
    assert_eq!(staged.change_justification, "Change 1 justification");
    assert_eq!(staged.explain, None);
    assert_eq!(unstaged.explain, Some(ExplainAction::Uncommitted));

    // Untracked file: all unstaged; base side absent
    let untracked = session.file_view(&files[1].change, &never()).unwrap();
    assert_eq!(untracked.base_text, None);
    assert_eq!(untracked.target_text.as_deref(), Some(&b"n\n"[..]));
    assert_eq!(
        untracked.cards.iter().map(|c| c.kind).collect::<Vec<_>>(),
        [CardKind::Unstaged]
    );

    // explain_uncommitted directly, for the unstaged layer
    let view = session.file_view(&files[0].change, &never()).unwrap();
    assert_eq!(
        session
            .explain_uncommitted(&view, UncommittedLayer::Unstaged, &never())
            .unwrap(),
        1
    );
}

#[test]
fn explanations_of_uncommitted_hunks_outlive_the_session() {
    let history = history();
    history.write_files(files![("app.py", "one\nTWO\nthree\nfour\nfive\n")]);
    let cfg = config();
    let info = open_info(&history, None);
    let first = CompareSession::open(
        info.clone(),
        &Revision::commit("HEAD"),
        &Revision::working_tree(None),
        cfg.clone(),
        &never(),
    )
    .unwrap();
    let entry = first.files(&never()).unwrap().remove(0);
    let view = first.file_view(&entry.change, &never()).unwrap();
    first
        .explain_card(&view, &view.cards[0].key, &never())
        .unwrap();
    let second = CompareSession::open(
        info,
        &Revision::commit("HEAD"),
        &Revision::working_tree(None),
        cfg,
        &never(),
    )
    .unwrap();
    let again = second.file_view(&entry.change, &never()).unwrap();
    assert_eq!(again.cards[0].change_why, "Change 1 why");
}

#[test]
fn staged_to_working_tree() {
    let history = history();
    history.write_files(files![("app.py", "one\nTWO\nthree\nfour\nstaged\n")]);
    history.stage(&[]);
    history.write_files(files![(
        "app.py",
        "one\nTWO\nthree\nfour\nstaged\nunstaged\n"
    )]);
    let session = open_session(
        &history,
        Revision::index(None),
        Revision::working_tree(None),
        None,
    );
    assert_eq!(session.relation_text(), "Unstaged changes");
    assert_eq!(session.base_label(), "Staged");
    let files = session.files(&never()).unwrap();
    assert_eq!(files.len(), 1);
    let view = session.file_view(&files[0].change, &never()).unwrap();
    assert_eq!(
        view.base_text.as_deref(),
        Some(&b"one\nTWO\nthree\nfour\nstaged\n"[..])
    );
    assert_eq!(
        view.cards.iter().map(|c| c.kind).collect::<Vec<_>>(),
        [CardKind::Unstaged]
    );
    assert_eq!(
        view.cards[0]
            .added
            .iter()
            .map(|r| r.start)
            .collect::<Vec<_>>(),
        [6]
    );
}

#[test]
fn other_worktree() {
    let history = history();
    let other = history.worktree(&history.tmp_path().join("wt"), "feature", "HEAD");
    history.write_files_in(&other, files![("app.py", "changed elsewhere\n")]);
    let session = open_session(
        &history,
        Revision::commit("HEAD"),
        Revision::working_tree(Some(other.clone())),
        None,
    );
    assert_eq!(session.target_label(), "Working Tree (wt)");
    let files = session.files(&never()).unwrap();
    assert_eq!(files.len(), 1);
    let view = session.file_view(&files[0].change, &never()).unwrap();
    assert_eq!(
        view.target_text.as_deref(),
        Some(&b"changed elsewhere\n"[..])
    );
}

#[test]
fn diverged_branches_compare_from_merge_base() {
    let mut history = history();
    history.branch("feature", "base");
    history.commit(
        "Feature work",
        files![("feature.txt", "f\n")],
        Some("feature"),
    );
    let session = open_session(
        &history,
        Revision::commit("main"),
        Revision::commit("feature"),
        None,
    );
    assert!(session.relation_text().contains("merge base"));
    let files = session.files(&never()).unwrap();
    assert_eq!(
        files
            .iter()
            .map(|e| e.change.path.as_str())
            .collect::<Vec<_>>(),
        ["feature.txt"]
    );
    let view = session.file_view(&files[0].change, &never()).unwrap();
    assert!(view.base_label.contains("merge base"));
}

#[test]
fn too_large_file() {
    let mut history = history();
    history.commit(
        "Big",
        files![("big.txt", "x".repeat(MAX_FILE_BYTES + 10))],
        Some("big"),
    );
    let session = open_session(
        &history,
        Revision::commit("HEAD~1"),
        Revision::commit("HEAD"),
        None,
    );
    let files = session.files(&never()).unwrap();
    let view = session.file_view(&files[0].change, &never()).unwrap();
    assert!(view.message.contains("too large"), "{}", view.message);
}

#[test]
fn deleted_and_renamed_files() {
    let mut history = history();
    history.commit_with(
        "Moves and deletes",
        files![
            ("data.bin", None::<&str>),
            ("lib.py", "one\nTWO\nthree\nfour\nmore\n")
        ],
        CommitOptions::default().with_rename("app.py", "lib.py"),
    );
    let session = open_session(
        &history,
        Revision::commit("HEAD~1"),
        Revision::commit("HEAD"),
        None,
    );
    let deleted = entry_for(&session, "data.bin");
    let deleted = session.file_view(&deleted.change, &never()).unwrap();
    assert_eq!(deleted.target_text, None);
    let renamed = entry_for(&session, "lib.py");
    let renamed = session.file_view(&renamed.change, &never()).unwrap();
    assert_eq!(renamed.base_name, "app.py");
    assert_eq!(
        renamed.base_text.as_deref(),
        Some(&b"one\nTWO\nthree\nfour\n"[..])
    );
}

#[test]
fn same_commit() {
    let history = history();
    let session = open_session(
        &history,
        Revision::commit("main"),
        Revision::commit("HEAD"),
        None,
    );
    assert!(session.files(&never()).unwrap().is_empty());
    assert!(!session.can_build());
    assert!(session.relation_text().contains("same commit"));
}

#[test]
fn cancelled_load() {
    let history = history();
    let session = base_main(&history, None);
    let token = CancelToken::new();
    token.cancel();
    assert!(session.files(&token).unwrap_err().is_cancelled());
}

#[test]
fn commit_message() {
    let mut history = history();
    history.note("agent", "Why: A note.", "ai-lens");
    let session = base_main(&history, None);
    let message = session
        .commit_message(&history.sha("agent"), &never())
        .unwrap();
    assert!(message.starts_with(&format!("commit {}", history.sha("agent"))));
    assert!(message.contains("Co-Authored-By: Claude Opus 5.5"));
    assert!(message.contains("A note."));
}

#[test]
fn repository_info() {
    let history = history();
    let other = history.worktree(&history.tmp_path().join("wt"), "feature", "HEAD");
    let info = open_info(&history, Some(&history.path.join("app.py")));
    assert_eq!(info.root, history.path);
    let target = info.picker_entries(PickerSide::Target, &never()).unwrap();
    let labels: Vec<&str> = target.iter().map(|e| e.label.as_str()).collect();
    assert_eq!(labels[0], "HEAD");
    assert!(
        labels.contains(&"Working Tree")
            && labels.contains(&"Unstaged")
            && labels.contains(&"Staged")
    );
    assert!(labels.contains(&"Working Tree (feature)"));
    assert!(labels.contains(&"main") && labels.contains(&"feature"));
    assert!(labels.contains(&"Adds four"));
    let base = info.picker_entries(PickerSide::Base, &never()).unwrap();
    assert!(!base.iter().any(|e| e.label == "Working Tree"));
    assert_eq!(
        info.worktree_heads().get(&Some(other.clone())),
        Some(&history.sha("agent"))
    );

    let found = info.search_commits("louder", &never()).unwrap();
    assert_eq!(
        found.iter().map(|e| e.label.as_str()).collect::<Vec<_>>(),
        ["Makes two louder"]
    );
    assert!(info.search_commits("  ", &never()).unwrap().is_empty());

    let watched = info.watch_paths();
    assert!(watched.iter().any(|p| p.ends_with("HEAD")));
    assert!(watched.iter().any(|p| p.ends_with("index")));
    assert!(
        std::fs::canonicalize(&other)
            .unwrap()
            .starts_with(history.tmp_path())
    );
    assert_eq!(info.display_name(), "repo");
}

#[test]
fn repository_info_errors() {
    let temp = tempfile::tempdir().unwrap();
    let options = meld_ailens::GitOptions::default();
    assert!(RepositoryInfo::open(&temp.path().join("missing"), &options, &never()).is_err());
    let plain = temp.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    let mut env = common::isolated_git_env();
    // Keep git from finding a repository above the temp folder
    env.insert(
        "GIT_CEILING_DIRECTORIES".into(),
        temp.path().display().to_string(),
    );
    let options = meld_ailens::GitOptions {
        env: Some(env),
        ..Default::default()
    };
    assert!(RepositoryInfo::open(&plain, &options, &never()).is_err());
}

#[test]
fn set_up_agents() {
    let history = history();
    let info = open_info(&history, None);
    let changed = info.set_up_agents(&config(), &never()).unwrap();
    assert!(changed.contains(&"CLAUDE.md".to_string()));
    assert!(changed.iter().any(|p| p.ends_with("SKILL.md")));
    assert!(info.set_up_agents(&config(), &never()).unwrap().is_empty());
}

#[test]
fn unborn_repository() {
    let repo = FixtureRepo::new();
    repo.write_files(files![("first.txt", "hello\n")]);
    repo.stage(&[]);
    let session = open_session(
        &repo,
        Revision::index(None),
        Revision::working_tree(None),
        None,
    );
    assert!(session.files(&never()).unwrap().is_empty());
    repo.write_files(files![("first.txt", "hello\nworld\n")]);
    let session = open_session(
        &repo,
        Revision::index(None),
        Revision::working_tree(None),
        None,
    );
    let files = session.files(&never()).unwrap();
    assert_eq!(files.len(), 1);
    let view = session.file_view(&files[0].change, &never()).unwrap();
    assert_eq!(
        view.cards.iter().map(|c| c.kind).collect::<Vec<_>>(),
        [CardKind::Unstaged]
    );
}

#[test]
fn gather_build_facts_folds_cards_into_entries() {
    let history = history();
    let session = base_main(&history, None);
    let facts = session.gather_build_facts(&never(), None).unwrap();
    assert_eq!(facts.stats.files, 2);
    assert_eq!(facts.stats.commits, 2);
    assert_eq!(facts.base.ref_name, None);
    assert_eq!(facts.target.ref_name.as_deref(), Some("main"));
    let app = facts.files.iter().find(|f| f.path == "app.py").unwrap();
    let shas: Vec<Option<String>> = app.entries.iter().map(|e| e.sha.clone()).collect();
    assert_eq!(
        shas,
        [Some(history.sha("human")), Some(history.sha("agent"))]
    );
    let data = facts.files.iter().find(|f| f.path == "data.bin").unwrap();
    assert!(data.binary);
    assert_eq!(data.touched_by, [history.sha("agent")]);
}

// -- the file list as a tree (test_file_tree.py) --

fn tree(paths: &[&str]) -> Vec<meld_ailens::FileRow> {
    let entries: Vec<FileEntry> = paths
        .iter()
        .map(|p| {
            FileEntry::new(
                GitFileChange::new(GitFileStatus::Modified, *p),
                Some(1),
                Some(0),
            )
        })
        .collect();
    build_file_tree(&entries)
}

fn dump(rows: &[meld_ailens::FileRow], depth: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for row in rows {
        let slash = if row.entry.is_none() { "/" } else { "" };
        lines.push(format!("{}{}{slash}", "  ".repeat(depth), row.name));
        lines.extend(dump(&row.children, depth + 1));
    }
    lines
}

#[test]
fn folders_first_and_single_folder_chains_compacted() {
    let rows = tree(&[
        "README.md",
        ".github/workflows/ci.yml",
        "src/a/b/deep.py",
        "src/top.py",
        "agent/index.md",
    ]);
    assert_eq!(
        dump(&rows, 0),
        [
            ".github/workflows/",
            "  ci.yml",
            "agent/",
            "  index.md",
            "src/",
            "  a/b/",
            "    deep.py",
            "  top.py",
            "README.md",
        ]
    );
}

#[test]
fn same_path_listed_twice() {
    let entries = [
        FileEntry::new(
            GitFileChange::new(GitFileStatus::Deleted, "gone.txt"),
            Some(0),
            Some(1),
        ),
        FileEntry::new(GitFileChange::untracked("gone.txt"), Some(1), Some(0)),
    ];
    let mut names: Vec<String> = build_file_tree(&entries)
        .into_iter()
        .map(|r| r.name)
        .collect();
    names.sort();
    assert_eq!(names, ["gone.txt", "gone.txt (untracked)"]);
}
