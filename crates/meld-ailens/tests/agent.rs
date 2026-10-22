//! Port of test/ailens/test_agent.py

use meld_ailens::agent::detect_agent;
use meld_ailens::{CommitAgent, GitCommit, GitIdentity, GitTrailer};

fn commit_by(trailers: Vec<GitTrailer>, author: &str, email: &str) -> GitCommit {
    let identity = GitIdentity {
        name: author.into(),
        email: email.into(),
        date: "2024-01-01T00:00:00Z".into(),
    };
    GitCommit {
        sha: "a".repeat(40),
        parents: Vec::new(),
        author: identity.clone(),
        committer: identity,
        subject: "Subject".into(),
        body: String::new(),
        trailers,
        note: None,
    }
}

fn commit(trailers: Vec<GitTrailer>) -> GitCommit {
    commit_by(trailers, "Jane Doe", "jane@example.com")
}

fn author(name: &str, email: &str) -> GitCommit {
    commit_by(Vec::new(), name, email)
}

fn claude(value: &str) -> GitTrailer {
    GitTrailer::new("Co-Authored-By", value)
}

fn agent(display_name: &str, model: Option<&str>) -> Option<CommitAgent> {
    Some(CommitAgent {
        kind: "claude-code".into(),
        display_name: display_name.into(),
        model: model.map(str::to_string),
    })
}

#[test]
fn reads_the_model_from_the_claude_code_trailer() {
    assert_eq!(
        detect_agent(
            &commit(vec![claude("Claude Opus 5.5 <noreply@anthropic.com>")]),
            None
        ),
        agent("Claude Opus 5.5", Some("claude-opus-5-5"))
    );

    let found = detect_agent(
        &commit(vec![claude("Claude Sonnet 4.5 <noreply@anthropic.com>")]),
        None,
    )
    .expect("an agent");
    assert_eq!(found.model.as_deref(), Some("claude-sonnet-4-5"));
}

#[test]
fn detects_a_trailer_that_names_no_model() {
    let trailer = GitTrailer::new("co-authored-by", "Claude <noreply@anthropic.com>");
    assert_eq!(
        detect_agent(&commit(vec![trailer]), None),
        agent("Claude", None)
    );
}

#[test]
fn detects_claude_as_the_author() {
    let found = detect_agent(&author("Claude", "noreply@anthropic.com"), None).expect("an agent");
    assert_eq!(found.display_name, "Claude");
}

#[test]
fn ignores_human_co_authors_and_look_alikes_from_other_addresses() {
    assert_eq!(
        detect_agent(&commit(vec![claude("Jane Doe <jane@example.com>")]), None),
        None
    );
    assert_eq!(
        detect_agent(
            &commit(vec![claude("Claude Monet <claude@example.com>")]),
            None
        ),
        None
    );
    assert_eq!(
        detect_agent(&author("Claude Monet", "claude@example.com"), None),
        None
    );
}

#[test]
fn ignores_other_trailers_and_extra_text_after_the_email() {
    assert_eq!(
        detect_agent(
            &commit(vec![GitTrailer::new(
                "Reviewed-by",
                "Claude <noreply@anthropic.com>"
            )]),
            None
        ),
        None
    );
    assert_eq!(
        detect_agent(
            &commit(vec![claude("Claude <noreply@anthropic.com> extra")]),
            None
        ),
        None
    );
    assert_eq!(
        detect_agent(
            &commit(vec![claude("Claudette <noreply@anthropic.com>")]),
            None
        ),
        None
    );
}

#[test]
fn trailer_matching_is_case_insensitive_and_skips_other_co_authors() {
    let found = detect_agent(
        &commit(vec![
            claude("Jane Doe <jane@example.com>"),
            claude("claude haiku 4.5   <NoReply@Anthropic.com>"),
        ]),
        None,
    );
    assert_eq!(found, agent("claude haiku 4.5", Some("claude-haiku-4-5")));
}

#[test]
fn author_pattern_recognises_other_agent_commits() {
    let bot = author("ci-bot", "bot@example.com");
    assert_eq!(detect_agent(&bot, None), None);
    assert_eq!(
        detect_agent(&bot, Some(r"bot@example\.com")),
        agent("ci-bot", None)
    );
    assert_eq!(detect_agent(&bot, Some(r"^someone-else")), None);
}

#[test]
fn author_pattern_reads_a_model_from_a_claude_name() {
    let found = detect_agent(
        &author("Claude Opus 5.5", "claude@corp.example"),
        Some("corp\\.example"),
    );
    assert_eq!(found, agent("Claude Opus 5.5", Some("claude-opus-5-5")));
}

#[test]
fn author_pattern_also_checks_co_authors() {
    let found = detect_agent(
        &commit(vec![claude("Agent Smith <agent@corp.example>")]),
        Some("agent@corp"),
    );
    assert_eq!(found, agent("Agent Smith", None));
}

#[test]
fn the_builtin_detection_comes_before_the_author_pattern() {
    let found = detect_agent(
        &commit_by(
            vec![claude("Claude Opus 5.5 <noreply@anthropic.com>")],
            "ci-bot",
            "jane@example.com",
        ),
        Some("ci-bot"),
    )
    .expect("an agent");
    assert_eq!(found.model.as_deref(), Some("claude-opus-5-5"));
}

#[test]
fn an_invalid_author_pattern_is_ignored() {
    // Python also checks the warning that is logged; the Rust port logs it
    // through the `log` crate, which this test doesn't capture
    assert_eq!(
        detect_agent(&author("ci-bot", "jane@example.com"), Some("([unclosed")),
        None
    );
    // Built-in detection still works
    assert!(detect_agent(&author("Claude", "noreply@anthropic.com"), Some("(")).is_some());
}

#[test]
fn an_empty_author_pattern_matches_nothing() {
    assert_eq!(detect_agent(&commit(Vec::new()), Some("")), None);
}

#[test]
fn unicode_names() {
    assert_eq!(
        detect_agent(
            &commit(vec![claude("Clàude <noreply@anthropic.com>")]),
            None
        ),
        None
    );
    let found = detect_agent(&author("Jörg Agent", "j@x"), Some("Jörg"));
    assert_eq!(found, agent("Jörg Agent", None));
}
