import pytest

from meld.ailens.claude import ClaudeError, ClaudeRequest, ClaudeResponse
from meld.ailens.explanation import compose_note, format_note, parse_note
from meld.ailens.infer import (
    MAX_PATCH_CHARS,
    MAX_PATCH_CHARS_PER_FILE,
    SCHEMA,
    SYSTEM_PROMPT,
    UNCOMMITTED_SCHEMA,
    IgnoredFile,
    NothingToExplain,
    build_prompt,
    compose_inferred_note,
    infer_explanation,
    infer_uncommitted_explanation,
)
from meld.ailens.models import (
    ChangeExplanation,
    CommitExplanation,
    CommitHunk,
    GitCommit,
    GitDiffHunk,
    GitDiffLine,
    GitFileChange,
    GitFileDiff,
    GitIdentity,
)
from meld.ailens.process import CancelToken


def hunk(old_start: int, new_start: int, *lines: str, section: str = "") -> GitDiffHunk:
    """A hunk from ``git diff`` body lines (`` ``, ``+`` or ``-`` first)"""
    parsed = []
    old, new = old_start, new_start
    for line in lines:
        kind, content = line[0], line[1:]
        if kind == "+":
            parsed.append(GitDiffLine("add", content, None, new))
            new += 1
        elif kind == "-":
            parsed.append(GitDiffLine("delete", content, old, None))
            old += 1
        else:
            parsed.append(GitDiffLine("context", content, old, new))
            old += 1
            new += 1
    old_count = sum(1 for line in lines if line[0] != "+")
    new_count = sum(1 for line in lines if line[0] != "-")
    return GitDiffHunk(
        old_start, old_count, new_start, new_count, section, tuple(parsed)
    )


def modified(path: str, *hunks: GitDiffHunk) -> tuple[GitFileChange, GitFileDiff]:
    return GitFileChange("M", path), GitFileDiff(path, path, False, hunks)


def added(path: str, *lines: str) -> tuple[GitFileChange, GitFileDiff]:
    diff = GitFileDiff(
        None, path, False, (hunk(0, 1, *(f"+{line}" for line in lines)),)
    )
    return GitFileChange("A", path), diff


def commit(subject: str = "Changes a", body: str = "Some body.") -> GitCommit:
    identity = GitIdentity("Jane Doe", "jane@example.com", "2024-01-01T00:00:00Z")
    return GitCommit(
        sha="c" * 40,
        parents=("b" * 40,),
        author=identity,
        committer=identity,
        subject=subject,
        body=body,
    )


class FakeClaude:
    """Records the request and answers with ``output``"""

    def __init__(self, output: object, model: str = "claude-sonnet-5-5") -> None:
        self.output = output
        self.model = model
        self.requests: list[ClaudeRequest] = []
        self.cancels: list[CancelToken | None] = []

    def __call__(
        self, request: ClaudeRequest, cancel: CancelToken | None
    ) -> ClaudeResponse:
        self.requests.append(request)
        self.cancels.append(cancel)
        return ClaudeResponse(output=self.output, text="", model=self.model, cost_usd=0)

    @property
    def prompt(self) -> str:
        return self.requests[-1].prompt


def never_called(request: ClaudeRequest, cancel: CancelToken | None) -> ClaudeResponse:
    raise AssertionError("Claude Code must not be called")


CHANGE_FILES = [
    modified("src/a.ts", hunk(1, 1, "-one", "+two")),
    added("pnpm-lock.yaml", "lock"),
    modified("secrets/key.txt", hunk(1, 1, "-old", "+SECRET-VALUE")),
]


# -- infer_explanation --


def test_sends_the_message_and_diff_leaving_out_ignored_and_generated_files():
    run = FakeClaude(
        {
            "what": " Changes a. ",
            "why": " Because. ",
            "changes": [
                {"change": 1, "what": " Swaps one for two. ", "why": " Two is right. "},
                {"change": 1, "what": "Duplicate", "why": "Ignored."},
                {"change": 7, "what": "Unknown", "why": "No such change."},
            ],
        }
    )
    cancel = CancelToken()
    explanation = infer_explanation(
        commit(),
        CHANGE_FILES,
        model="claude-sonnet-5-5",
        ignore_rules="secrets/\n",
        claude_path="/opt/claude",
        cancel=cancel,
        run=run,
    )

    assert explanation.explanation == CommitExplanation(
        "Changes a.", "Because.", "inferred", "claude-sonnet-5-5"
    )
    assert explanation.model == "claude-sonnet-5-5"
    assert explanation.changes == (
        ChangeExplanation(
            CommitHunk("src/a.ts", 1, 1, 1, 1), "Swaps one for two.", "Two is right."
        ),
    )

    sent = run.requests[0]
    assert sent.model == "claude-sonnet-5-5"
    assert sent.claude_path == "/opt/claude"
    assert sent.system_prompt == SYSTEM_PROMPT
    assert sent.json_schema == SCHEMA
    assert run.cancels == [cancel]
    assert "[change 1] @@ -1 +1 @@\n-one\n+two" in sent.prompt
    assert "[change 2]" not in sent.prompt
    assert "Changes a\n\nSome body." in sent.prompt
    assert "2 of these files are left out of the diff" in sent.prompt
    assert "SECRET-VALUE" not in sent.prompt
    assert "+lock" not in sent.prompt


def test_rejects_an_answer_without_a_why():
    run = FakeClaude({"what": "x", "why": ""}, model="m")
    with pytest.raises(ClaudeError, match="no explanation"):
        infer_explanation(commit(), [added("a.ts", "a")], model="m", run=run)


@pytest.mark.parametrize(
    "output",
    [
        None,
        "text",
        [],
        {"what": 1, "why": "y"},
        {"what": "w"},
        {"what": "w", "why": "  "},
    ],
)
def test_rejects_malformed_answers(output):
    with pytest.raises(ClaudeError, match="no explanation"):
        infer_explanation(
            commit(), [added("a.ts", "a")], model="m", run=FakeClaude(output)
        )


def test_uses_the_subject_for_an_empty_what_and_the_model_that_answered():
    run = FakeClaude(
        {"what": "  ", "why": "Y", "changes": "not a list"}, model="claude-x"
    )
    explanation = infer_explanation(commit(subject="Subj"), [], model="m", run=run)
    assert explanation.explanation == CommitExplanation(
        "Subj", "Y", "inferred", "claude-x"
    )
    assert explanation.model == "claude-x"
    assert explanation.changes == ()


def test_change_answers_are_validated():
    files = [
        modified(
            "a.py", hunk(1, 1, "-a", "+b"), hunk(10, 10, "-c", "+d"), hunk(20, 20, "+e")
        )
    ]
    run = FakeClaude(
        {
            "what": "W",
            "why": "Y",
            "changes": [
                "not an object",
                {"change": "1", "what": "string number", "why": "dropped"},
                {"change": True, "what": "bool", "why": "dropped"},
                {"change": 1.5, "what": "fraction", "why": "dropped"},
                {"change": 1, "what": "no why", "why": "  "},
                {"change": 1, "what": "first", "why": "kept"},
                {"change": 3.0, "what": "float", "why": "kept"},
                {"change": 2, "what": None, "why": "dropped"},
                {"change": 0, "what": "zero", "why": "dropped"},
                {"change": -1, "what": "negative", "why": "dropped"},
            ],
        }
    )
    explanation = infer_explanation(commit(), files, model="m", run=run)
    assert [(c.hunk.old_start, c.what) for c in explanation.changes] == [
        (1, "first"),
        (20, "float"),
    ]


def test_hunks_are_numbered_across_files_in_diff_order():
    files = [
        modified("a.py", hunk(1, 1, "-a", "+b"), hunk(9, 9, "-c", "+d")),
        modified("b.py", hunk(5, 5, "-x", "+y")),
    ]
    run = FakeClaude(
        {
            "what": "W",
            "why": "Y",
            "changes": [
                {"change": 3, "what": "third", "why": "c"},
                {"change": 1, "what": "first", "why": "a"},
            ],
        }
    )
    explanation = infer_explanation(commit(), files, model="m", run=run)
    assert [(c.hunk.path, c.what) for c in explanation.changes] == [
        ("a.py", "first"),
        ("b.py", "third"),
    ]
    assert "[change 1] @@ -1 +1 @@" in run.prompt
    assert "[change 2] @@ -9 +9 @@" in run.prompt
    assert "[change 3] @@ -5 +5 @@" in run.prompt


def test_answers_for_hunks_that_match_nothing_are_dropped():
    run = FakeClaude(
        {"what": "W", "why": "Y", "changes": [{"change": 1, "what": "w", "why": "y"}]}
    )
    explanation = infer_explanation(
        commit(), [(GitFileChange("M", "mode.sh"), None)], model="m", run=run
    )
    assert explanation.changes == ()
    assert "(no textual changes to show)" in run.prompt


# -- build_prompt --


def test_build_prompt_layout():
    prompt = build_prompt(
        commit(),
        [modified("a.py", hunk(3, 3, " ctx", "-old", "+new", section="def f():"))],
    )
    assert prompt.text == "\n".join(
        [
            f"Commit {'c' * 40} by Jane Doe",
            "",
            "Message:",
            "Changes a\n\nSome body.",
            "",
            "Changed files:",
            "M a.py",
            "",
            "Diff:",
            "diff --git a/a.py b/a.py",
            "--- a/a.py",
            "+++ b/a.py",
            "[change 1] @@ -3,2 +3,2 @@ def f():",
            " ctx",
            "-old",
            "+new",
            "",
        ]
    )
    assert prompt.hunks == (CommitHunk("a.py", 3, 2, 3, 2),)


def test_build_prompt_with_an_empty_body_and_no_files():
    prompt = build_prompt(commit(body="  \n"), [])
    assert (
        "Message:\nChanges a\n\nChanged files:\n\nDiff:\n(no textual changes to show)"
        in prompt.text
    )
    assert prompt.hunks == ()


def test_build_prompt_renders_renames_additions_deletions_and_binaries():
    renamed = (
        GitFileChange("R", "new.py", "old.py", similarity=90),
        GitFileDiff("old.py", "new.py", False, (hunk(1, 1, "-a", "+b"),)),
    )
    deleted = (
        GitFileChange("D", "gone.py"),
        GitFileDiff("gone.py", None, False, (hunk(1, 0, "-bye"),)),
    )
    binary = (
        GitFileChange("M", "logo.png"),
        GitFileDiff("logo.png", "logo.png", True, ()),
    )
    no_newline = (
        GitFileChange("M", "end.txt"),
        GitFileDiff(
            "end.txt",
            "end.txt",
            False,
            (
                GitDiffHunk(
                    1,
                    1,
                    1,
                    1,
                    "",
                    (
                        GitDiffLine("delete", "x", 1, None, no_newline_at_end=True),
                        GitDiffLine("add", "y", None, 1, no_newline_at_end=True),
                    ),
                ),
            ),
        ),
    )
    prompt = build_prompt(
        commit(), [renamed, added("new.txt", "hi"), deleted, binary, no_newline]
    )

    assert (
        "R old.py -> new.py\nA new.txt\nD gone.py\nM logo.png\nM end.txt\n"
        in prompt.text
    )
    assert (
        "diff --git a/old.py b/new.py\nsimilarity index 90%\nrename from old.py\n"
        "rename to new.py\n--- a/old.py\n+++ b/new.py\n[change 1] @@ -1 +1 @@\n-a\n+b\n"
    ) in prompt.text
    assert (
        "diff --git a/new.txt b/new.txt\n--- /dev/null\n+++ b/new.txt\n"
        "[change 2] @@ -0,0 +1 @@\n+hi\n"
    ) in prompt.text
    assert (
        "diff --git a/gone.py b/gone.py\n--- a/gone.py\n+++ /dev/null\n"
        "[change 3] @@ -1 +0,0 @@\n-bye\n"
    ) in prompt.text
    assert (
        "diff --git a/logo.png b/logo.png\nBinary files a/logo.png and b/logo.png differ\n"
    ) in prompt.text
    assert (
        "[change 4] @@ -1 +1 @@\n-x\n\\ No newline at end of file\n+y\n"
        "\\ No newline at end of file\n"
    ) in prompt.text
    assert prompt.hunks == (
        CommitHunk("new.py", 1, 1, 1, 1, original_path="old.py"),
        CommitHunk("new.txt", 0, 0, 1, 1),
        CommitHunk("gone.py", 1, 1, 0, 0),
        CommitHunk("end.txt", 1, 1, 1, 1),
    )


def test_a_rename_is_left_out_when_either_path_is_ignored():
    renamed = (
        GitFileChange("R", "public/key.txt", "secrets/key.txt"),
        GitFileDiff(
            "secrets/key.txt", "public/key.txt", False, (hunk(1, 1, "-s", "+t"),)
        ),
    )
    prompt = build_prompt(commit(), [renamed], "secrets/")
    assert "1 of these files are left out" in prompt.text
    assert "R secrets/key.txt -> public/key.txt" in prompt.text
    assert "+t" not in prompt.text
    assert prompt.hunks == ()


def test_unicode_paths_and_content():
    prompt = build_prompt(
        commit(subject="Grüße", body=""),
        [modified("données/é.py", hunk(1, 1, "-☃", "+雪"))],
    )
    assert "Message:\nGrüße\n" in prompt.text
    assert "diff --git a/données/é.py b/données/é.py" in prompt.text
    assert "-☃\n+雪" in prompt.text


def test_a_huge_file_is_cut_and_listed():
    big = modified("big.txt", hunk(1, 1, *(f"+line {i:05}" for i in range(2000))))
    small = modified("small.txt", hunk(1, 1, "-a", "+b"))
    prompt = build_prompt(commit(), [big, small])

    assert "The diff is cut short for: big.txt." in prompt.text
    assert "[… rest of big.txt left out]" in prompt.text
    assert "+line 01999" not in prompt.text
    # The next file is still shown, and every hunk keeps its number
    assert "[change 2] @@ -1 +1 @@\n-a\n+b" in prompt.text
    assert len(prompt.hunks) == 2
    diff = prompt.text.split("\nDiff:\n", 1)[1]
    big_part = diff.split("diff --git a/small.txt", 1)[0]
    assert len(big_part) == MAX_PATCH_CHARS_PER_FILE + len(
        "\n[… rest of big.txt left out]\n"
    )


def test_files_past_the_total_budget_are_left_out():
    files = [
        modified(f"f{i}.txt", hunk(1, 1, *(f"+{i}-{n:04}" for n in range(1500))))
        for i in range(8)
    ]
    prompt = build_prompt(commit(), files)
    diff = prompt.text.split("\nDiff:\n", 1)[1]

    assert len(diff) <= MAX_PATCH_CHARS
    # Each file is cut to its cap, so four fit; the rest are left out
    assert diff.count("diff --git ") == 4
    assert (
        "The diff is cut short for: f0.txt, f1.txt, f2.txt, f3.txt, f4.txt, f4.txt, "
        in prompt.text
    )
    assert "f7.txt, f7.txt." in prompt.text
    # Hunks left out of the prompt are still numbered, so answers line up
    assert len(prompt.hunks) == 8


# -- compose_inferred_note --


def inferred(changes=()):
    run = FakeClaude(
        {"what": "Adds X", "why": "Because.", "changes": list(changes)}, "claude-x"
    )
    return infer_explanation(
        commit(), [modified("a.py", hunk(1, 1, "-a", "+b"))], model="m", run=run
    )


def test_inferred_note_for_a_commit_without_a_why():
    explanation = inferred([{"change": 1, "what": "Swaps", "why": "Needed."}])
    note = compose_inferred_note(commit(body=""), explanation)
    assert note == compose_note(
        format_note("Adds X", "Because.", "inferred", "claude-x"),
        "claude-x",
        explanation.changes,
    )
    parsed = parse_note(note)
    assert (parsed.why, parsed.source, parsed.change_model) == (
        "Because.",
        "inferred",
        "claude-x",
    )
    assert len(parsed.changes) == 1


def test_inferred_note_keeps_a_why_from_the_message():
    explanation = inferred()
    note = compose_inferred_note(commit(body="Real reason."), explanation)
    assert note == "Change-Model: claude-x\n"


def test_inferred_note_keeps_a_hand_written_note():
    explanation = inferred()
    with_note = GitCommit(
        **{**commit(body="").__dict__, "note": "By hand.\n\nChange-Model: old\n"}
    )
    assert (
        compose_inferred_note(with_note, explanation)
        == "By hand.\n\nChange-Model: claude-x\n"
    )


def test_inferred_note_replaces_an_older_inferred_explanation():
    explanation = inferred()
    old = format_note("Old", "Old why.", "inferred", "claude-old")
    with_note = GitCommit(**{**commit(body="").__dict__, "note": old})
    note = compose_inferred_note(with_note, explanation)
    assert parse_note(note).why == "Because."
    assert parse_note(note).model == "claude-x"


# -- infer_uncommitted_explanation --

PATCH = GitFileDiff(
    "a.ts", "a.ts", False, (hunk(1, 1, "-one", "+two"), hunk(9, 9, "-nine", "+NINE"))
)


def test_explains_each_hunk_from_the_patch_alone():
    run = FakeClaude(
        {"changes": [{"change": 2, "what": " Shouts nine. ", "why": " Louder. "}]}
    )
    explained = infer_uncommitted_explanation(
        "staged", PATCH, model="claude-sonnet-5-5", run=run
    )

    assert "Staged changes to a.ts, not committed yet" in run.prompt
    assert "[change 1] @@ -1 +1 @@" in run.prompt
    assert "[change 2] @@ -9 +9 @@" in run.prompt
    assert run.requests[0].json_schema == UNCOMMITTED_SCHEMA
    assert run.requests[0].json_schema["required"] == ["changes"]
    assert explained.model == "claude-sonnet-5-5"
    assert explained.changes == (
        ChangeExplanation(CommitHunk("a.ts", 9, 1, 9, 1), "Shouts nine.", "Louder."),
    )


def test_uncommitted_prompt_layout_for_a_rename():
    diff = GitFileDiff("old.ts", "new.ts", False, (hunk(1, 1, "-a", "+b"),))
    run = FakeClaude({"changes": [{"change": 1, "what": "w", "why": "y"}]})
    explained = infer_uncommitted_explanation("unstaged", diff, model="m", run=run)
    expected = [
        "Unstaged changes to old.ts -> new.ts, not committed yet",
        "",
        "Diff:",
        "diff --git a/old.ts b/new.ts",
        "rename from old.ts",
        "rename to new.ts",
        "--- a/old.ts",
        "+++ b/new.ts",
        "[change 1] @@ -1 +1 @@",
        "-a",
        "+b",
        "",
    ]
    assert run.prompt == "\n".join(expected)
    assert explained.changes[0].hunk.original_path == "old.ts"


def test_a_huge_uncommitted_diff_is_cut():
    diff = GitFileDiff(
        "a", "a", False, (hunk(1, 1, *(f"+{n:06}" for n in range(3000))),)
    )
    run = FakeClaude({"changes": [{"change": 1, "what": "w", "why": "y"}]})
    infer_uncommitted_explanation("unstaged", diff, model="m", run=run)
    assert "\n\nThe diff is cut short.\n\nDiff:\n" in run.prompt
    assert "[… rest of a left out]" in run.prompt


def test_never_sends_ignored_or_generated_files():
    secret = GitFileDiff("secrets/key.txt", "secrets/key.txt", False, PATCH.hunks)
    with pytest.raises(IgnoredFile, match="isn't sent"):
        infer_uncommitted_explanation(
            "unstaged", secret, model="m", ignore_rules="secrets/\n", run=never_called
        )
    lock = GitFileDiff("pnpm-lock.yaml", "pnpm-lock.yaml", False, PATCH.hunks)
    with pytest.raises(IgnoredFile, match="isn't sent"):
        infer_uncommitted_explanation("unstaged", lock, model="m", run=never_called)
    # By the old path of a rename too
    moved = GitFileDiff("secrets/key.txt", "public/key.txt", False, PATCH.hunks)
    with pytest.raises(IgnoredFile):
        infer_uncommitted_explanation(
            "staged", moved, model="m", ignore_rules="secrets/", run=never_called
        )


def test_a_diff_without_hunks_is_not_sent():
    empty = GitFileDiff("a.ts", "a.ts", True, ())
    with pytest.raises(NothingToExplain, match="a.ts has no staged changes"):
        infer_uncommitted_explanation("staged", empty, model="m", run=never_called)


def test_an_answer_that_explains_no_hunk_is_an_error():
    run = FakeClaude({"changes": [{"change": 5, "what": "w", "why": "y"}]})
    with pytest.raises(ClaudeError, match="no explanation"):
        infer_uncommitted_explanation("staged", PATCH, model="m", run=run)
    with pytest.raises(ClaudeError, match="no explanation"):
        infer_uncommitted_explanation(
            "staged", PATCH, model="m", run=FakeClaude("text")
        )


def test_an_untracked_file_is_an_addition():
    diff = GitFileDiff(None, "new.txt", False, (hunk(0, 1, "+hello"),))
    run = FakeClaude({"changes": [{"change": 1, "what": "w", "why": "y"}]})
    explained = infer_uncommitted_explanation("unstaged", diff, model="m", run=run)
    assert (
        "--- /dev/null\n+++ b/new.txt\n[change 1] @@ -0,0 +1 @@\n+hello" in run.prompt
    )
    assert explained.changes[0].hunk == CommitHunk("new.txt", 0, 0, 1, 1)
