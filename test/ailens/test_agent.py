import logging

from meld.ailens.agent import detect_agent
from meld.ailens.models import CommitAgent, GitCommit, GitIdentity, GitTrailer


def commit(
    trailers: tuple[GitTrailer, ...] = (),
    author: str = "Jane Doe",
    email: str = "jane@example.com",
) -> GitCommit:
    identity = GitIdentity(author, email, "2024-01-01T00:00:00Z")
    return GitCommit(
        sha="a" * 40,
        parents=(),
        author=identity,
        committer=identity,
        subject="Subject",
        body="",
        trailers=trailers,
    )


def claude(value: str) -> GitTrailer:
    return GitTrailer("Co-Authored-By", value)


def test_reads_the_model_from_the_claude_code_trailer():
    assert detect_agent(
        commit((claude("Claude Opus 5.5 <noreply@anthropic.com>"),))
    ) == CommitAgent("claude-code", "Claude Opus 5.5", "claude-opus-5-5")

    agent = detect_agent(commit((claude("Claude Sonnet 4.5 <noreply@anthropic.com>"),)))
    assert agent is not None
    assert agent.model == "claude-sonnet-4-5"


def test_detects_a_trailer_that_names_no_model():
    trailer = GitTrailer("co-authored-by", "Claude <noreply@anthropic.com>")
    assert detect_agent(commit((trailer,))) == CommitAgent(
        "claude-code", "Claude", None
    )


def test_detects_claude_as_the_author():
    agent = detect_agent(commit(author="Claude", email="noreply@anthropic.com"))
    assert agent is not None
    assert agent.display_name == "Claude"


def test_ignores_human_co_authors_and_look_alikes_from_other_addresses():
    assert detect_agent(commit((claude("Jane Doe <jane@example.com>"),))) is None
    assert detect_agent(commit((claude("Claude Monet <claude@example.com>"),))) is None
    assert (
        detect_agent(commit(author="Claude Monet", email="claude@example.com")) is None
    )


def test_ignores_other_trailers_and_extra_text_after_the_email():
    assert (
        detect_agent(
            commit((GitTrailer("Reviewed-by", "Claude <noreply@anthropic.com>"),))
        )
        is None
    )
    assert (
        detect_agent(commit((claude("Claude <noreply@anthropic.com> extra"),))) is None
    )
    assert detect_agent(commit((claude("Claudette <noreply@anthropic.com>"),))) is None


def test_trailer_matching_is_case_insensitive_and_skips_other_co_authors():
    agent = detect_agent(
        commit(
            (
                claude("Jane Doe <jane@example.com>"),
                claude("claude haiku 4.5   <NoReply@Anthropic.com>"),
            )
        )
    )
    assert agent == CommitAgent("claude-code", "claude haiku 4.5", "claude-haiku-4-5")


def test_author_pattern_recognises_other_agent_commits():
    bot = commit(author="ci-bot", email="bot@example.com")
    assert detect_agent(bot) is None
    assert detect_agent(bot, r"bot@example\.com") == CommitAgent(
        "claude-code", "ci-bot", None
    )
    assert detect_agent(bot, r"^someone-else") is None


def test_author_pattern_reads_a_model_from_a_claude_name():
    agent = detect_agent(
        commit(author="Claude Opus 5.5", email="claude@corp.example"), "corp\\.example"
    )
    assert agent == CommitAgent("claude-code", "Claude Opus 5.5", "claude-opus-5-5")


def test_author_pattern_also_checks_co_authors():
    agent = detect_agent(
        commit((claude("Agent Smith <agent@corp.example>"),)), "agent@corp"
    )
    assert agent == CommitAgent("claude-code", "Agent Smith", None)


def test_the_builtin_detection_comes_before_the_author_pattern():
    agent = detect_agent(
        commit((claude("Claude Opus 5.5 <noreply@anthropic.com>"),), author="ci-bot"),
        "ci-bot",
    )
    assert agent is not None
    assert agent.model == "claude-opus-5-5"


def test_an_invalid_author_pattern_is_ignored(caplog):
    with caplog.at_level(logging.WARNING):
        assert detect_agent(commit(author="ci-bot"), "([unclosed") is None
    assert "invalid agent author pattern" in caplog.text
    # Built-in detection still works
    assert (
        detect_agent(commit(author="Claude", email="noreply@anthropic.com"), "(")
        is not None
    )


def test_an_empty_author_pattern_matches_nothing():
    assert detect_agent(commit(), "") is None


def test_unicode_names():
    assert detect_agent(commit((claude("Clàude <noreply@anthropic.com>"),))) is None
    agent = detect_agent(commit(author="Jörg Agent", email="j@x"), "Jörg")
    assert agent == CommitAgent("claude-code", "Jörg Agent", None)
