import threading

from meld.ailens.explanation import find_change_explanations
from meld.ailens.models import (
    ChangeExplanation,
    CommitHunk,
    GitDiffHunk,
    GitDiffLine,
    GitFileDiff,
    LineOrigin,
)
from meld.ailens.uncommitted import (
    MAX_ENTRIES,
    HunkExplanation,
    PlacedExplanation,
    UncommittedExplanations,
)


def hunk(old_start: int, new_start: int, *lines: str) -> GitDiffHunk:
    parsed = []
    for line in lines:
        kind = {"+": "add", "-": "delete", " ": "context"}[line[0]]
        parsed.append(GitDiffLine(kind, line[1:], None, None))
    old_count = sum(1 for line in lines if line[0] != "+")
    new_count = sum(1 for line in lines if line[0] != "-")
    return GitDiffHunk(old_start, old_count, new_start, new_count, "", tuple(parsed))


TOP = hunk(1, 1, "-one", "+two")
BOTTOM = hunk(9, 9, "-nine", "+NINE")
DIFF = GitFileDiff("a.ts", "a.ts", False, (TOP, BOTTOM))
EXPLAINED = HunkExplanation("Shouts nine.", "Louder.", "claude-sonnet-5-5")


def test_keeps_an_explanation_per_layer_file_and_hunk():
    cache = UncommittedExplanations()
    cache.set("staged", DIFF, BOTTOM, EXPLAINED)

    assert cache.get("staged", DIFF, BOTTOM) == EXPLAINED
    assert cache.get("staged", DIFF, TOP) is None
    assert cache.get("unstaged", DIFF, BOTTOM) is None
    other = GitFileDiff("b.ts", "b.ts", False, (BOTTOM,))
    assert cache.get("staged", other, BOTTOM) is None


def test_lasts_while_the_hunk_moves_and_ends_when_it_changes():
    cache = UncommittedExplanations()
    cache.set("staged", DIFF, BOTTOM, EXPLAINED)

    moved = hunk(20, 22, "-nine", "+NINE")
    assert cache.get("staged", DIFF, moved) == EXPLAINED
    changed = hunk(9, 9, "-nine", "+NINE!")
    assert cache.get("staged", DIFF, changed) is None
    # The kind of a line counts, not just its text
    context = hunk(9, 9, " nine", "+NINE")
    assert cache.get("staged", DIFF, context) is None


def test_a_rename_is_a_different_file():
    cache = UncommittedExplanations()
    renamed = GitFileDiff("old.ts", "a.ts", False, (TOP,))
    cache.set("staged", renamed, TOP, EXPLAINED)
    assert cache.get("staged", DIFF, TOP) is None
    assert cache.get("staged", renamed, TOP) == EXPLAINED


def test_place_puts_explanations_where_the_hunks_are_now():
    cache = UncommittedExplanations()
    cache.set("unstaged", DIFF, BOTTOM, EXPLAINED)

    moved = hunk(19, 21, "-nine", "+NINE")
    now = GitFileDiff("old.ts", "a.ts", False, (TOP, moved))
    cache.set("unstaged", now, moved, EXPLAINED)
    placed = cache.place("unstaged", [now])
    assert placed == [
        PlacedExplanation(
            CommitHunk("a.ts", 19, 1, 21, 1, original_path="old.ts"),
            "Shouts nine.",
            "Louder.",
            "claude-sonnet-5-5",
        )
    ]
    # Ready for find_change_explanations, like a commit's changes
    assert find_change_explanations([LineOrigin("a.ts", "new", 21)], placed) == placed
    assert find_change_explanations([LineOrigin("old.ts", "old", 19)], placed) == placed


def test_place_handles_additions_deletions_and_nothing_explained():
    cache = UncommittedExplanations()
    added = GitFileDiff(None, "new.ts", False, (hunk(0, 1, "+hi"),))
    deleted = GitFileDiff("gone.ts", None, False, (hunk(1, 0, "-bye"),))
    cache.set("unstaged", added, added.hunks[0], EXPLAINED)
    cache.set("unstaged", deleted, deleted.hunks[0], EXPLAINED)

    placed = cache.place(
        "unstaged", [added, deleted, DIFF, GitFileDiff(None, None, True, ())]
    )
    assert [p.hunk for p in placed] == [
        CommitHunk("new.ts", 0, 0, 1, 1),
        CommitHunk("gone.ts", 1, 1, 0, 0),
    ]
    assert cache.place("staged", [added]) == []


def test_remember_keeps_a_models_answers_by_hunk_position():
    cache = UncommittedExplanations()
    changes = [
        ChangeExplanation(CommitHunk("a.ts", 9, 1, 9, 1), "Shouts nine.", "Louder."),
        # Matches no hunk of the diff
        ChangeExplanation(CommitHunk("a.ts", 50, 1, 50, 1), "Gone", "Stale."),
    ]
    assert cache.remember("staged", DIFF, changes, "claude-x") == 1
    assert cache.get("staged", DIFF, BOTTOM) == HunkExplanation(
        "Shouts nine.", "Louder.", "claude-x"
    )
    assert not cache.is_explained("staged", DIFF)

    cache.remember(
        "staged",
        DIFF,
        [ChangeExplanation(CommitHunk("a.ts", 1, 1, 1, 1), "w", "y")],
        "claude-x",
    )
    assert cache.is_explained("staged", DIFF)
    assert not cache.is_explained("unstaged", DIFF)


def test_setting_again_replaces_and_refreshes():
    cache = UncommittedExplanations(max_entries=2)
    first = HunkExplanation("1", "1", "m")
    cache.set("staged", DIFF, TOP, first)
    cache.set("staged", DIFF, BOTTOM, first)
    # Refreshing TOP makes BOTTOM the oldest
    cache.set("staged", DIFF, TOP, EXPLAINED)
    cache.set("unstaged", DIFF, TOP, first)

    assert len(cache) == 2
    assert cache.get("staged", DIFF, TOP) == EXPLAINED
    assert cache.get("staged", DIFF, BOTTOM) is None


def test_keeps_at_most_max_entries_dropping_the_oldest():
    cache = UncommittedExplanations()
    hunks = [hunk(i, i, f"+line {i}") for i in range(MAX_ENTRIES + 5)]
    for h in hunks:
        cache.set("unstaged", DIFF, h, EXPLAINED)

    assert len(cache) == MAX_ENTRIES
    assert cache.get("unstaged", DIFF, hunks[0]) is None
    assert cache.get("unstaged", DIFF, hunks[4]) is None
    assert cache.get("unstaged", DIFF, hunks[5]) == EXPLAINED
    assert cache.get("unstaged", DIFF, hunks[-1]) == EXPLAINED


def test_clear():
    cache = UncommittedExplanations()
    cache.set("staged", DIFF, TOP, EXPLAINED)
    cache.clear()
    assert len(cache) == 0
    assert cache.get("staged", DIFF, TOP) is None


def test_unicode_content_is_keyed_by_its_text():
    cache = UncommittedExplanations()
    snow = hunk(1, 1, "-☃", "+雪")
    cache.set("staged", DIFF, snow, EXPLAINED)
    assert cache.get("staged", DIFF, hunk(3, 3, "-☃", "+雪")) == EXPLAINED
    assert cache.get("staged", DIFF, hunk(1, 1, "-☃", "+雨")) is None


def test_is_safe_to_share_between_threads():
    cache = UncommittedExplanations(max_entries=500)
    hunks = [hunk(i, i, f"+{i}") for i in range(200)]
    errors: list[BaseException] = []

    def work(layer: str) -> None:
        try:
            for _ in range(5):
                for h in hunks:
                    cache.set(layer, DIFF, h, EXPLAINED)
                    cache.get(layer, DIFF, h)
                cache.place(layer, [GitFileDiff("a.ts", "a.ts", False, tuple(hunks))])
        except BaseException as err:
            errors.append(err)

    threads = [
        threading.Thread(target=work, args=(layer,))
        for layer in ("staged", "unstaged") * 4
    ]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()

    assert errors == []
    assert len(cache) == 400
