//! Which commits an agent wrote, and with which model (port of `agent.py`)

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use regex::Regex;

use crate::models::{CommitAgent, GitCommit};

static ANTHROPIC_EMAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<noreply@anthropic\.com>\z").expect("valid regex"));
static CLAUDE_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\A(Claude(?:\s+[^<]*?)?)\s*<[^>]*>\z").expect("valid regex"));
static NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A\s*([^<]*?)\s*(?:<[^>]*>)?\s*\z").expect("valid regex"));

/// `claude\b` at the start, case-insensitive, with an ASCII word boundary
fn starts_with_claude_word(text: &str) -> bool {
    let Some(head) = text.get(..6) else {
        return false;
    };
    if !head.eq_ignore_ascii_case("claude") {
        return false;
    }
    !text[6..]
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The Claude Code agent behind a commit
///
/// Found from its `Co-Authored-By: Claude …` trailer, or a Claude author.
/// `author_pattern` is an extra regular expression (the user's setting)
/// searched in the author's and co-authors' `Name <email>`; a pattern that
/// doesn't compile is ignored.
pub fn detect_agent(commit: &GitCommit, author_pattern: Option<&str>) -> Option<CommitAgent> {
    let co_authors: Vec<&str> = commit
        .trailers
        .iter()
        .filter(|trailer| trailer.key.to_lowercase() == "co-authored-by")
        .map(|trailer| trailer.value.as_str())
        .collect();
    for value in &co_authors {
        let Some(captures) = CLAUDE_NAME.captures(value) else {
            continue;
        };
        if !ANTHROPIC_EMAIL.is_match(value) {
            continue;
        }
        return Some(to_agent(captures[1].trim()));
    }

    let author = &commit.author;
    if starts_with_claude_word(&author.name)
        && ANTHROPIC_EMAIL.is_match(&format!("<{}>", author.email))
    {
        return Some(to_agent(author.name.trim()));
    }

    let pattern = compile(author_pattern.filter(|p| !p.is_empty())?)?;
    let identity = format!("{} <{}>", author.name, author.email);
    if pattern.is_match(&identity) {
        let name = author.name.trim();
        return Some(to_agent(if name.is_empty() { &identity } else { name }));
    }
    for value in &co_authors {
        if pattern.is_match(value) {
            let name = NAME
                .captures(value)
                .and_then(|c| c.get(1))
                .map_or("", |m| m.as_str());
            return Some(to_agent(if name.is_empty() { value.trim() } else { name }));
        }
    }
    None
}

fn compile(pattern: &str) -> Option<Regex> {
    static CACHE: LazyLock<Mutex<HashMap<String, Option<Regex>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(found) = cache.get(pattern) {
        return found.clone();
    }
    let compiled = match Regex::new(pattern) {
        Ok(regex) => Some(regex),
        Err(err) => {
            log::warn!("Ignoring invalid agent author pattern {pattern:?}: {err}");
            None
        }
    };
    if cache.len() >= 16 {
        cache.clear();
    }
    cache.insert(pattern.to_string(), compiled.clone());
    compiled
}

fn to_agent(display_name: &str) -> CommitAgent {
    // Only Claude's own names spell a model: "Claude Opus 5.5" → claude-opus-5-5
    let words: Vec<&str> = if starts_with_claude_word(display_name) {
        display_name.split_whitespace().skip(1).collect()
    } else {
        Vec::new()
    };
    let model = (!words.is_empty()).then(|| {
        std::iter::once("claude")
            .chain(words)
            .collect::<Vec<_>>()
            .join("-")
            .to_lowercase()
            .replace('.', "-")
    });
    CommitAgent {
        kind: "claude-code".into(),
        display_name: display_name.to_string(),
        model,
    }
}
