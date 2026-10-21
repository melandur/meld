from meld.ailens.explanation import (
    DEFAULT_MESSAGE_FORMAT,
    MessageFormat,
    ParsedNote,
    compose_note,
    explain_commit,
    find_change_explanations,
    format_note,
    parse_note,
)
from meld.ailens.models import (
    ChangeExplanation,
    CommitExplanation,
    CommitHunk,
    GitCommit,
    GitIdentity,
    GitTrailer,
    LineOrigin,
)


def commit(
    subject: str = "Subject",
    body: str = "",
    note: str | None = None,
    trailers: tuple[GitTrailer, ...] = (),
) -> GitCommit:
    identity = GitIdentity("Jane Doe", "jane@example.com", "2024-01-01T00:00:00Z")
    return GitCommit(
        sha="a" * 40,
        parents=(),
        author=identity,
        committer=identity,
        subject=subject,
        body=body,
        trailers=trailers,
        note=note,
    )


def claude(value: str) -> GitTrailer:
    return GitTrailer("Co-Authored-By", value)


def change(
    path: str = "a.ts",
    old_start: int = 1,
    old_count: int = 0,
    new_start: int = 1,
    new_count: int = 0,
    what: str = "w",
    why: str = "y",
    original_path: str | None = None,
) -> ChangeExplanation:
    hunk = CommitHunk(path, old_start, old_count, new_start, new_count, original_path)
    return ChangeExplanation(hunk, what, why)


# -- explain_commit --


def test_uses_the_subject_as_what_and_the_body_without_trailers_as_why():
    explanation = explain_commit(
        commit(
            subject="Makes load async",
            body="The cache now reads from disk.\n\nThat needs I/O.\n\n"
            "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>",
            trailers=(claude("Claude Opus 5.5 <noreply@anthropic.com>"),),
        )
    )
    assert explanation == CommitExplanation(
        "Makes load async",
        "The cache now reads from disk.\n\nThat needs I/O.",
        "commit",
    )


def test_has_no_why_when_the_body_is_only_trailers_or_empty():
    assert (
        explain_commit(
            commit(
                body="Signed-off-by: Jane <j@x>",
                trailers=(GitTrailer("Signed-off-by", "Jane <j@x>"),),
            )
        ).why
        is None
    )
    assert explain_commit(commit()).why is None


def test_keeps_a_last_paragraph_that_only_looks_like_trailers_when_git_found_none():
    assert (
        explain_commit(commit(body="Note: this is prose.")).why
        == "Note: this is prose."
    )


def test_trailer_block_with_continuation_lines_is_stripped():
    explanation = explain_commit(
        commit(
            body="Reason.\n\nSigned-off-by: Jane\n  continued\nAcked-by: Bob",
            trailers=(
                GitTrailer("Signed-off-by", "Jane continued"),
                GitTrailer("Acked-by", "Bob"),
            ),
        )
    )
    assert explanation.why == "Reason."


def test_a_last_paragraph_with_prose_is_kept_even_with_trailers():
    explanation = explain_commit(
        commit(
            body="Reason.\n\nSigned-off-by: Jane\nthis is prose",
            trailers=(GitTrailer("Signed-off-by", "Jane"),),
        )
    )
    assert explanation.why == "Reason.\n\nSigned-off-by: Jane\nthis is prose"


def test_crlf_messages():
    explanation = explain_commit(
        commit(
            body="First line.\r\nSecond line.\r\n\r\nCo-Authored-By: Claude <noreply@anthropic.com>\r\n",
            trailers=(claude("Claude <noreply@anthropic.com>"),),
        )
    )
    assert explanation.why == "First line.\nSecond line."

    labeled = explain_commit(
        commit(body="What: Adds X\r\n\r\nWhy: Because\r\nof Y.\r\n")
    )
    assert labeled == CommitExplanation("Adds X", "Because\nof Y.", "commit")


def test_whitespace_only_body_has_no_why():
    assert explain_commit(commit(body="  \n\n \t\n")).why is None


def test_unicode_messages():
    explanation = explain_commit(commit(subject="Ändert ☃", body="Weil 雪 fällt."))
    assert explanation == CommitExplanation("Ändert ☃", "Weil 雪 fällt.", "commit")


# -- explanation sources --


def test_prefers_what_why_labeled_in_the_message_as_trailers_or_body_paragraphs():
    assert explain_commit(
        commit(
            trailers=(GitTrailer("Agent-Why", "Users asked for it"),),
            note="Why: from a note",
        )
    ) == CommitExplanation("Subject", "Users asked for it", "commit")

    assert explain_commit(
        commit(
            body="What: Adds backoff\n\nWhy: Retries hit the\nrate limit.\n\nOther text."
        )
    ) == CommitExplanation("Adds backoff", "Retries hit the\nrate limit.", "commit")


def test_uses_labels_from_the_configured_format():
    format = MessageFormat(what_keys=("Change",), why_keys=("Reason",))
    assert (
        explain_commit(commit(trailers=(GitTrailer("reason", "Because"),)), format).why
        == "Because"
    )
    assert (
        explain_commit(commit(trailers=(GitTrailer("Why", "Ignored"),)), format).why
        is None
    )


def test_default_format():
    assert DEFAULT_MESSAGE_FORMAT == MessageFormat(
        ("What", "Agent-What"), ("Why", "Agent-Why")
    )


def test_the_first_label_wins_and_trailers_beat_paragraphs():
    explanation = explain_commit(
        commit(
            body="Why: from the body\n\nWhy: second\n\nWhy: from a trailer",
            trailers=(GitTrailer("Why", "from a trailer"),),
        )
    )
    assert explanation.why == "from a trailer"
    assert explain_commit(commit(body="Why: first\n\nWhy: second")).why == "first"


def test_a_labeled_what_without_a_why_falls_through_to_the_body():
    explanation = explain_commit(commit(body="What: Adds X\n\nPlain reason."))
    assert explanation == CommitExplanation(
        "Adds X", "What: Adds X\n\nPlain reason.", "commit"
    )


def test_an_empty_labeled_why_counts_as_missing():
    explanation = explain_commit(commit(body="Why:\n\nPlain."))
    assert explanation.why == "Why:\n\nPlain."
    assert explanation.source == "commit"


def test_uses_the_note_before_the_plain_body():
    assert explain_commit(
        commit(
            body="Plain body.",
            note="Source: inferred\nModel: claude-sonnet-5-5\nWhat: Adds X\nWhy: Because Y.",
        )
    ) == CommitExplanation("Adds X", "Because Y.", "inferred", "claude-sonnet-5-5")

    assert explain_commit(
        commit(subject="S", note="Written by hand\nover two lines")
    ) == CommitExplanation("S", "Written by hand\nover two lines", "note")


def test_falls_back_to_the_body_and_has_no_why_when_there_is_none():
    assert explain_commit(commit(body="Plain body.")) == CommitExplanation(
        "Subject", "Plain body.", "commit"
    )
    assert explain_commit(
        commit(note="Source: inferred\nModel: m")
    ) == CommitExplanation("Subject", None, "commit")


def test_an_empty_note_is_ignored():
    assert explain_commit(commit(body="Body.", note="")).why == "Body."
    assert explain_commit(commit(body="Body.", note="\n  \n")).why == "Body."


# -- notes --


def test_round_trips_through_format_note_and_parse_note():
    text = format_note(
        what="Adds X\nsecond line",
        why="Because Y.\n\nAnd Z.",
        source="inferred",
        model="claude-sonnet-5-5",
    )

    assert (
        text == "Source: inferred\nModel: claude-sonnet-5-5\nWhat: Adds X second line\n"
        "Why: Because Y.\n\nAnd Z.\n"
    )
    assert parse_note(text) == ParsedNote(
        what="Adds X second line",
        why="Because Y.\n\nAnd Z.",
        source="inferred",
        model="claude-sonnet-5-5",
        change_model=None,
        changes=[],
        head=text.strip(),
    )


def test_format_note_without_a_model():
    assert format_note("W", "Y", "note") == "Source: note\nWhat: W\nWhy: Y\n"


def test_keeps_the_explanations_of_single_changes_after_the_commits_own():
    changes = [
        change(
            path="src/a b.ts",
            old_start=3,
            old_count=2,
            new_start=3,
            new_count=4,
            what="Awaits the load",
            why="The cache reads from disk.\n\nChange: -1,1 +1,1 x looks like a header.",
        ),
        change(
            path="src/new.ts",
            original_path="src/old.ts",
            old_start=0,
            old_count=0,
            new_start=1,
            new_count=2,
            what="Adds the file",
            why="Room for the helper.",
        ),
    ]
    text = compose_note("Plain note by hand.", "claude-sonnet-5-5", changes)

    expected = [
        "Plain note by hand.",
        "",
        "Change-Model: claude-sonnet-5-5",
        "",
        "Change: -3,2 +3,4 src/a b.ts",
        "What: Awaits the load",
        "Why: The cache reads from disk.",
        "",
        " Change: -1,1 +1,1 x looks like a header.",
        "",
        "Change: -0,0 +1,2 src/new.ts",
        "Change-From: src/old.ts",
        "What: Adds the file",
        "Why: Room for the helper.",
        "",
    ]
    assert text == "\n".join(expected)
    assert parse_note(text) == ParsedNote(
        what=None,
        why="Plain note by hand.",
        source="note",
        model=None,
        change_model="claude-sonnet-5-5",
        changes=[
            ChangeExplanation(
                changes[0].hunk,
                "Awaits the load",
                "The cache reads from disk.\n\n Change: -1,1 +1,1 x looks like a header.",
            ),
            changes[1],
        ],
        head="Plain note by hand.",
    )

    # Only changes: the commit's own Why stays with its message
    only_changes = parse_note(compose_note("", "m", []))
    assert only_changes.why is None
    assert only_changes.change_model == "m"
    assert only_changes.changes == []
    assert only_changes.head == ""
    assert explain_commit(
        commit(body="Body why.", note="Change-Model: m\n")
    ) == CommitExplanation("Subject", "Body why.", "commit")


def test_compose_note_flattens_what_and_skips_a_same_path_change_from():
    text = compose_note(
        "",
        "m",
        [change(path="a", original_path="a", what="two\nlines", why="  padded  ")],
    )
    assert (
        text == "Change-Model: m\n\nChange: -1,0 +1,0 a\nWhat: two lines\nWhy: padded\n"
    )


def test_round_trips_an_inferred_head_with_changes():
    head = format_note("Adds X", "Because.", "inferred", "claude-opus-5-5")
    changes = [
        change(old_start=10, old_count=3, new_start=11, new_count=5, why="Why A.")
    ]
    note = parse_note(compose_note(head, "claude-haiku-4-5", changes))
    assert note.what == "Adds X"
    assert note.why == "Because."
    assert note.source == "inferred"
    assert note.model == "claude-opus-5-5"
    assert note.change_model == "claude-haiku-4-5"
    assert note.changes == changes
    assert note.head == head.strip()


def test_parse_note_with_only_why():
    assert parse_note("Why: Because.\nSecond line.") == ParsedNote(
        what=None,
        why="Because.\nSecond line.",
        source="note",
        model=None,
        head="Why: Because.\nSecond line.",
    )


def test_parse_note_head_edge_cases():
    # Only What: no Why, so the commit's own body is used
    assert parse_note("What: Adds X").why is None
    # Labels stop at the first unknown line; that is not Why either
    assert parse_note("What: Adds X\nfree text").why is None
    assert parse_note("Source: inferred\nfree text").why is None
    # An unknown first label makes the whole note Why
    assert parse_note("Note: by hand").why == "Note: by hand"
    # Labels are case-insensitive; an unknown Source is a note
    note = parse_note("SOURCE: Inferred\nmodel: m\nwhat: W\nWHY: Y")
    assert (note.source, note.model, note.what, note.why) == ("inferred", "m", "W", "Y")
    assert parse_note("Source: robot\nWhy: Y").source == "note"
    # Empty values count as missing
    note = parse_note("Model:\nWhat:\nWhy:   ")
    assert (note.model, note.what, note.why) == (None, None, None)


def test_parse_note_crlf():
    text = (
        "Source: inferred\r\nModel: m\r\nWhat: W\r\nWhy: Y\r\n\r\n"
        "Change-Model: c\r\n\r\nChange: -1,2 +1,3 a.py\r\nWhat: A\r\nWhy: B\r\n"
    )
    note = parse_note(text)
    assert (note.what, note.why, note.model, note.change_model) == ("W", "Y", "m", "c")
    assert note.changes == [
        change(path="a.py", old_count=2, new_count=3, what="A", why="B")
    ]


def test_parse_note_unicode():
    note = parse_note(
        "Why: Für ☃.\n\nChange-Model: m\n\nChange: -1 +1 données/é.py\nWhat: ✓\nWhy: 雪"
    )
    assert note.why == "Für ☃."
    assert note.changes == [
        change(path="données/é.py", old_count=1, new_count=1, what="✓", why="雪")
    ]


def test_parse_note_change_ranges_default_to_one_line():
    note = parse_note("Change: -5 +7,0 a.py\nWhat: W\nWhy: Y")
    assert note.changes[0].hunk == CommitHunk("a.py", 5, 1, 7, 0)
    # Changes without a Change-Model line before them still count
    assert note.change_model is None
    assert note.head == ""


def test_parse_note_drops_changes_without_a_why():
    note = parse_note(
        "Change-Model: m\n\n"
        "Change: -1,1 +1,1 a.py\nWhat: no reason\n\n"
        "Change: -2,1 +2,1 b.py\nWhat: empty reason\nWhy:   \n\n"
        "Change: -3,1 +3,1 c.py\nWhy: kept without What"
    )
    assert note.changes == [
        change(
            path="c.py",
            old_start=3,
            old_count=1,
            new_start=3,
            new_count=1,
            what="",
            why="kept without What",
        )
    ]


def test_parse_note_skips_malformed_change_lines():
    note = parse_note(
        "Change-Model: m\n\n"
        "Change: 1,1 +1,1 missing-dash.py\nWhat: W\nWhy: lost\n\n"
        "Change: -a,1 +1,1 letters.py\nWhy: lost\n\n"
        "Change: -1,1 +1,1\nWhy: no path\n\n"
        "Change: -4,1 +4,1 good.py\nWhat: W\nWhy: Y"
    )
    assert note.changes == [
        change(
            path="good.py",
            old_start=4,
            old_count=1,
            new_start=4,
            new_count=1,
            what="W",
            why="Y",
        )
    ]


def test_parse_note_why_runs_to_the_next_change_and_keeps_labels_in_it():
    note = parse_note(
        "Change: -1 +1 a.py\nWhat: W\nWhy: line one\nWhat: still why\nChange-Model: also why\n"
        "Change: -2 +2 b.py\nWhy: B"
    )
    assert [c.why for c in note.changes] == [
        "line one\nWhat: still why\nChange-Model: also why",
        "B",
    ]
    assert note.change_model is None


def test_parse_note_ignores_unknown_lines_before_why():
    note = parse_note("Change: -1 +1 a.py\nnoise\nWhat: W\nOther: x\nWhy: Y")
    assert note.changes == [
        change(path="a.py", old_count=1, new_count=1, what="W", why="Y")
    ]


def test_a_head_line_shaped_like_a_change_starts_the_tail():
    note = parse_note("Why: hand note\nChange: -1 +1 a.py\nWhy: Y")
    assert note.why == "hand note"
    assert len(note.changes) == 1


# -- find_change_explanations --

TOP = change(old_start=1, old_count=4, new_start=1, new_count=5, what="top")
BOTTOM = change(old_start=20, old_count=3, new_start=21, new_count=2, what="bottom")
RENAMED = change(
    path="b.ts",
    original_path="old.ts",
    old_start=1,
    old_count=3,
    new_start=1,
    new_count=3,
    what="renamed",
)


def test_finds_the_hunks_the_lines_fall_in_by_side_and_path_in_the_order_the_lines_reach_them():
    found = find_change_explanations(
        [
            LineOrigin("a.ts", "old", 22),
            LineOrigin("a.ts", "new", 5),
            LineOrigin("a.ts", "new", 1),
        ],
        [TOP, BOTTOM, RENAMED],
    )
    assert [c.what for c in found] == ["bottom", "top"]


def test_matches_old_lines_of_a_renamed_file_by_its_old_path():
    changes = [TOP, BOTTOM, RENAMED]
    assert find_change_explanations([LineOrigin("old.ts", "old", 2)], changes) == [
        RENAMED
    ]
    assert find_change_explanations([LineOrigin("b.ts", "old", 2)], changes) == []
    assert find_change_explanations([LineOrigin("b.ts", "new", 3)], changes) == [
        RENAMED
    ]


def test_finds_nothing_outside_every_hunk():
    assert find_change_explanations([LineOrigin("a.ts", "new", 6)], [TOP, BOTTOM]) == []
    assert (
        find_change_explanations([LineOrigin("a.ts", "new", 23)], [TOP, BOTTOM]) == []
    )
    # Hunk edges: the start is in, start + count is out
    assert find_change_explanations([LineOrigin("a.ts", "new", 21)], [BOTTOM]) == [
        BOTTOM
    ]
    assert find_change_explanations([LineOrigin("a.ts", "old", 23)], [BOTTOM]) == []


def test_finds_nothing_for_empty_input_or_empty_hunks():
    assert find_change_explanations([], [TOP]) == []
    assert find_change_explanations([LineOrigin("a.ts", "new", 1)], []) == []
    # A pure deletion has no new lines
    empty = change(old_start=5, old_count=2, new_start=4, new_count=0)
    assert find_change_explanations([LineOrigin("a.ts", "new", 4)], [empty]) == []
    assert find_change_explanations([LineOrigin("a.ts", "old", 6)], [empty]) == [empty]


def test_finds_each_change_once():
    found = find_change_explanations(
        [LineOrigin("a.ts", "new", 1), LineOrigin("a.ts", "new", 2)], [TOP]
    )
    assert found == [TOP]
