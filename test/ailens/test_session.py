import os
import re
import threading
from pathlib import Path

import pytest

from meld.ailens.claude import ClaudeNotFound, ClaudeResponse
from meld.ailens.models import AiLensError, Cancelled, Revision
from meld.ailens.process import CancelToken
from meld.ailens.session import (
    MAX_FILE_BYTES,
    CompareSession,
    RepositoryInfo,
    SessionConfig,
)
from test.ailens.fixture_repo import co_authored_by_claude


class FakeClaude:
    """Answers like Claude Code would, and records what it was asked"""

    def __init__(self, fail_for=()):
        self.requests = []
        self.fail_for = fail_for
        self.lock = threading.Lock()

    def __call__(self, request, cancel=None):
        with self.lock:
            self.requests.append(request)
        if cancel is not None:
            cancel.raise_if_cancelled()
        for text in self.fail_for:
            if text in request.prompt:
                raise AiLensError(f"failed on {text}")
        schema = request.json_schema or {}
        properties = schema.get("properties", {})
        numbers = sorted(
            {int(n) for n in re.findall(r"\[change (\d+)\]", request.prompt)}
        )
        changes = [
            {"change": n, "what": f"Change {n} what", "why": f"Change {n} why"}
            for n in numbers
        ]
        if "title" in properties:
            output = {
                "title": "Fake build title",
                "summary": "Fake summary.",
                "decisions": [],
                "risks": ["Fake risk"],
            }
        elif "what" in properties:
            output = {
                "what": "Inferred what",
                "why": "Inferred why",
                "changes": changes,
            }
        else:
            output = {"changes": changes}
        return ClaudeResponse(output=output, text="", model="fake-model")


def config(fake=None, **kwargs):
    return SessionConfig(
        run_claude=fake or FakeClaude(), generator="meld-test", **kwargs
    )


def open_info(repo, cfg=None, path=None):
    """The repository, with git isolated from the user's config"""
    info = RepositoryInfo.open(str(path or repo.path), cfg or config())
    info.repo._env = {**info.repo._env, **repo.env}
    return info


def open_session(repo, base, target, cfg=None):
    info = open_info(repo, cfg)
    return CompareSession.open(info, base, target, cfg or config())


@pytest.fixture
def history(fixture_repo):
    repo = fixture_repo
    repo.commit("Adds the app", {"app.py": "one\ntwo\nthree\n"}, label="base")
    repo.commit(
        "Makes two louder\n\nWhy: Users could not hear it.",
        {"app.py": "one\nTWO\nthree\n"},
        label="human",
    )
    repo.commit(
        "Adds four",
        {"app.py": "one\nTWO\nthree\nfour\n", "data.bin": b"\0\1\2"},
        label="agent",
        trailers=[co_authored_by_claude("Opus 5.5")],
    )
    return repo


def test_commit_comparison_files_and_cards(history):
    session = open_session(
        history, Revision.commit(history.shas["base"]), Revision.commit("main")
    )
    assert session.base_label == history.shas["base"][:7]
    assert session.target_label == "main"
    assert session.can_build
    files = {e.change.path: e for e in session.files()}
    assert set(files) == {"app.py", "data.bin"}
    assert files["data.bin"].binary
    assert (files["app.py"].additions, files["app.py"].deletions) == (2, 1)

    view = session.file_view(files["app.py"].change)
    assert view.base_text == b"one\ntwo\nthree\n"
    assert view.target_text == b"one\nTWO\nthree\nfour\n"
    assert not view.message
    by_sha = {c.sha: c for c in view.cards}
    human = by_sha[history.shas["human"]]
    assert human.what == "Makes two louder"
    assert human.why == "Users could not hear it."
    assert human.source == "commit"
    assert not human.agent
    # It has a Why, so only its changes can still be explained
    assert human.explain == "changes"
    agent = by_sha[history.shas["agent"]]
    assert agent.agent_name == "Claude Opus 5.5"
    assert agent.agent_model == "claude-opus-5-5"
    assert agent.explain == "commit"
    assert human.color_index == 0 and agent.color_index == 1
    assert [r.start for r in agent.added] == [4]


def test_binary_file_view(history):
    session = open_session(
        history, Revision.commit(history.shas["base"]), Revision.commit("main")
    )
    entry = next(e for e in session.files() if e.change.path == "data.bin")
    view = session.file_view(entry.change)
    assert "binary" in view.message
    assert view.cards == []
    assert history.shas["agent"][:7] in view.message


def test_explain_commit_saves_note_and_updates_cards(history):
    fake = FakeClaude()
    cfg = config(fake)
    session = open_session(
        history, Revision.commit(history.shas["base"]), Revision.commit("main"), cfg
    )
    entry = next(e for e in session.files() if e.change.path == "app.py")
    view = session.file_view(entry.change)
    agent_card = next(c for c in view.cards if c.sha == history.shas["agent"])
    session.explain_card(view, agent_card.key)

    note = history.git("notes", "--ref=ai-lens", "show", history.shas["agent"])
    assert "Source: inferred" in note and "Model: fake-model" in note
    cards = {c.sha: c for c in session.cards(view)}
    agent = cards[history.shas["agent"]]
    assert agent.why == "Inferred why"
    assert agent.source == "inferred"
    assert agent.inferred_by == "fake-model"
    assert agent.change_what == "Change 1 what"
    assert agent.explain is None


def test_explain_changes_keeps_the_commit_why(history):
    session = open_session(
        history, Revision.commit(history.shas["base"]), Revision.commit("main")
    )
    session.explain_commit(history.shas["human"])
    entry = next(e for e in session.files() if e.change.path == "app.py")
    view = session.file_view(entry.change)
    human = next(c for c in view.cards if c.sha == history.shas["human"])
    assert human.why == "Users could not hear it."
    assert human.source == "commit"
    assert human.change_why == "Change 1 why"
    assert human.explain is None


def test_explain_all_and_build(history):
    fake = FakeClaude()
    session = open_session(
        history,
        Revision.commit(history.shas["base"]),
        Revision.commit("main"),
        config(fake),
    )
    assert session.explain_all() == 2
    assert session.explain_all() == 0

    path = session.generate_build()
    assert os.path.basename(path) == (
        f"{history.shas['base'][:7]}_{history.shas['agent'][:7]}_build.md"
    )
    text = Path(path).read_text(encoding="utf-8")
    assert "Fake build title" in text
    # The model that actually answered is recorded
    assert 'build_model: "fake-model"' in text
    index = (Path(path).parent / "index.md").read_text(encoding="utf-8")
    assert "Fake build title" in index
    # Builds never contain diff text
    assert "TWO" not in text
    # Cards now link to the build
    entry = next(e for e in session.files() if e.change.path == "app.py")
    view = session.file_view(entry.change)
    assert all(os.path.basename(path) in c.builds for c in view.cards)


def test_explain_all_reports_failures_and_continues(history):
    fake = FakeClaude(fail_for=("Makes two louder",))
    session = open_session(
        history,
        Revision.commit(history.shas["base"]),
        Revision.commit("main"),
        config(fake),
    )
    with pytest.raises(AiLensError, match="Explained 1; 1 failed"):
        session.explain_all()


def test_missing_claude_stops_explain_all(history):
    def missing(request, cancel=None):
        raise ClaudeNotFound("Claude Code was not found")

    session = open_session(
        history,
        Revision.commit(history.shas["base"]),
        Revision.commit("main"),
        config(missing),
    )
    with pytest.raises(ClaudeNotFound):
        session.explain_all()


def test_inference_turned_off(history):
    session = open_session(
        history,
        Revision.commit(history.shas["base"]),
        Revision.commit("main"),
        config(infer_enabled=False),
    )
    entry = next(e for e in session.files() if e.change.path == "app.py")
    view = session.file_view(entry.change)
    assert all(c.explain is None for c in view.cards)
    with pytest.raises(AiLensError, match="turned off"):
        session.explain_commit(history.shas["agent"])


def test_working_tree_layers(history):
    history.write_files({"app.py": "zero\none\nTWO\nthree\nfour\n"})
    history.stage("app.py")
    history.write_files(
        {"app.py": "zero\none\nTWO\nthree\nfour\nfive\n", "new.txt": "n\n"}
    )
    session = open_session(history, Revision.commit("HEAD"), Revision.working_tree())
    assert not session.can_build
    assert session.follows_repository_changes
    paths = {e.change.path: e for e in session.files()}
    assert set(paths) == {"app.py", "new.txt"}
    assert paths["new.txt"].change.untracked

    view = session.file_view(paths["app.py"].change)
    kinds = sorted(c.kind for c in view.cards)
    assert kinds == ["staged", "unstaged"]
    assert view.target_disk_path == os.path.join(str(history.path), "app.py")
    assert all(c.explain == "uncommitted" for c in view.cards)

    staged = next(c for c in view.cards if c.kind == "staged")
    session.explain_card(view, staged.key)
    cards = {c.kind: c for c in session.cards(view)}
    assert cards["staged"].change_what == "Change 1 what"
    assert cards["staged"].explain is None
    assert cards["unstaged"].explain == "uncommitted"

    # Untracked file: all unstaged; base side absent
    untracked = session.file_view(paths["new.txt"].change)
    assert untracked.base_text is None
    assert untracked.target_text == b"n\n"
    assert [c.kind for c in untracked.cards] == ["unstaged"]


def test_explanations_of_uncommitted_hunks_outlive_the_session(history):
    history.write_files({"app.py": "one\nTWO\nthree\nfour\nfive\n"})
    cfg = config()
    info = open_info(history, cfg)
    first = CompareSession.open(
        info, Revision.commit("HEAD"), Revision.working_tree(), cfg
    )
    entry = first.files()[0]
    view = first.file_view(entry.change)
    first.explain_card(view, view.cards[0].key)
    second = CompareSession.open(
        info, Revision.commit("HEAD"), Revision.working_tree(), cfg
    )
    again = second.file_view(entry.change)
    assert again.cards[0].change_why == "Change 1 why"


def test_staged_to_working_tree(history):
    history.write_files({"app.py": "one\nTWO\nthree\nfour\nstaged\n"})
    history.stage()
    history.write_files({"app.py": "one\nTWO\nthree\nfour\nstaged\nunstaged\n"})
    session = open_session(history, Revision.index(), Revision.working_tree())
    assert session.relation_text == "Unstaged changes"
    assert session.base_label == "Staged"
    [entry] = session.files()
    view = session.file_view(entry.change)
    assert view.base_text == b"one\nTWO\nthree\nfour\nstaged\n"
    assert [c.kind for c in view.cards] == ["unstaged"]
    assert [r.start for r in view.cards[0].added] == [6]


def test_other_worktree(history, tmp_path):
    other = history.worktree(tmp_path / "wt", "feature")
    history.write_files({"app.py": "changed elsewhere\n"}, root=other)
    session = open_session(
        history, Revision.commit("HEAD"), Revision.working_tree(str(other))
    )
    assert session.target_label == "Working Tree (wt)"
    [entry] = session.files()
    view = session.file_view(entry.change)
    assert view.target_text == b"changed elsewhere\n"


def test_diverged_branches_compare_from_merge_base(history):
    history.branch("feature", "base")
    history.commit("Feature work", {"feature.txt": "f\n"}, label="feature")
    session = open_session(history, Revision.commit("main"), Revision.commit("feature"))
    assert "merge base" in session.relation_text
    assert [e.change.path for e in session.files()] == ["feature.txt"]
    view = session.file_view(session.files()[0].change)
    assert "merge base" in view.base_label


def test_too_large_file(history):
    history.commit("Big", {"big.txt": "x" * (MAX_FILE_BYTES + 10)}, label="big")
    session = open_session(history, Revision.commit("HEAD~1"), Revision.commit("HEAD"))
    view = session.file_view(session.files()[0].change)
    assert "too large" in view.message


def test_deleted_and_renamed_files(history):
    history.commit(
        "Moves and deletes",
        {"data.bin": None, "lib.py": "one\nTWO\nthree\nfour\nmore\n"},
        renames={"app.py": "lib.py"},
    )
    session = open_session(history, Revision.commit("HEAD~1"), Revision.commit("HEAD"))
    files = {e.change.path: e for e in session.files()}
    deleted = session.file_view(files["data.bin"].change)
    assert deleted.target_text is None
    renamed = session.file_view(files["lib.py"].change)
    assert renamed.base_name == "app.py"
    assert renamed.base_text == b"one\nTWO\nthree\nfour\n"


def test_same_commit(history):
    session = open_session(history, Revision.commit("main"), Revision.commit("HEAD"))
    assert session.files() == []
    assert not session.can_build
    assert "same commit" in session.relation_text


def test_cancelled_load(history):
    session = open_session(
        history, Revision.commit(history.shas["base"]), Revision.commit("main")
    )
    token = CancelToken()
    token.cancel()
    with pytest.raises(Cancelled):
        session.files(token)


def test_commit_message(history):
    history.note("agent", "Why: A note.")
    session = open_session(
        history, Revision.commit(history.shas["base"]), Revision.commit("main")
    )
    message = session.commit_message(history.shas["agent"])
    assert message.startswith(f"commit {history.shas['agent']}")
    assert "Co-Authored-By: Claude Opus 5.5" in message
    assert "A note." in message


def test_repository_info(history, tmp_path):
    other = history.worktree(tmp_path / "wt", "feature")
    info = open_info(history, path=history.path / "app.py")
    assert info.root == str(history.path)
    target = info.picker_entries("target")
    labels = [e.label for e in target]
    assert labels[0] == "HEAD"
    assert "Working Tree" in labels and "Unstaged" in labels and "Staged" in labels
    assert "Working Tree (feature)" in labels
    assert "main" in labels and "feature" in labels
    assert "Adds four" in labels
    base = info.picker_entries("base")
    assert "Working Tree" not in [e.label for e in base]

    found = info.search_commits("louder")
    assert [e.label for e in found] == ["Makes two louder"]
    assert info.search_commits("  ") == []

    watched = info.watch_paths()
    assert any(p.endswith("HEAD") for p in watched)
    assert any(p.endswith("index") for p in watched)
    assert os.path.realpath(other).startswith(str(tmp_path))


def test_repository_info_errors(tmp_path):
    with pytest.raises(AiLensError):
        RepositoryInfo.open(str(tmp_path / "missing"), config())
    plain = tmp_path / "plain"
    plain.mkdir()
    with pytest.raises(AiLensError):
        RepositoryInfo.open(str(plain), config())


def test_set_up_agents(history):
    info = open_info(history)
    changed = info.set_up_agents(config())
    assert "CLAUDE.md" in changed
    assert any(p.endswith("SKILL.md") for p in changed)
    assert info.set_up_agents(config()) == []


def test_unborn_repository(fixture_repo):
    fixture_repo.write_files({"first.txt": "hello\n"})
    fixture_repo.stage()
    session = open_session(fixture_repo, Revision.index(), Revision.working_tree())
    assert session.files() == []
    fixture_repo.write_files({"first.txt": "hello\nworld\n"})
    session = open_session(fixture_repo, Revision.index(), Revision.working_tree())
    [entry] = session.files()
    view = session.file_view(entry.change)
    assert [c.kind for c in view.cards] == ["unstaged"]
