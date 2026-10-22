//! The written part of a build: the one `claude -p` call (port of
//! `build_narrative.py`)
//!
//! The model sees each commit's own explanation and the list of changed
//! files, never the code.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use crate::build_facts::{BuildAgent, BuildCommit, BuildFacts, BuildRevision};
use crate::claude::{ClaudeRequest, RunClaude, default_runner};
use crate::error::{Error, Result};
use crate::models::{ComparisonRelation, ExplanationSource};
use crate::process::CancelToken;

pub const BUILD_PROMPT_VERSION: u32 = 1;

pub const MAX_COMMITS: usize = 400;
pub const MAX_FILES: usize = 400;
pub const MAX_WHY_CHARS: usize = 600;

pub const SYSTEM_PROMPT: &str = r#"You write the summary of a range of code changes, for engineers and AI coding agents who later
need to know why the code is the way it is. You get each commit's own explanation and the changed files; you
never see the code. Reply with JSON matching the schema:
- "title": the range's main change in at most 10 words.
- "summary": 2 to 5 plain sentences: what the range does as a whole and why.
- "decisions": design choices the commits made, each with its reason and the short SHAs (from the list) of the
  commits it came from. Only choices the explanations support; an empty list is fine.
- "risks": anything unfinished, risky or unexplained, such as commits whose reason is only inferred or missing.
Use only what you are given. Plain text, no markdown."#;

/// The reply schema of [`write_narrative`]
pub static SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "title": {"type": "string"},
            "summary": {"type": "string"},
            "decisions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "text": {"type": "string"},
                        "commits": {"type": "array", "items": {"type": "string"}},
                    },
                    "required": ["text", "commits"],
                    "additionalProperties": false,
                },
            },
            "risks": {"type": "array", "items": {"type": "string"}},
        },
        "required": ["title", "summary", "decisions", "risks"],
        "additionalProperties": false,
    })
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildDecision {
    pub text: String,
    /// Short SHAs of the commits it came from; only commits in the range
    pub commits: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildNarrative {
    /// At most a short line; also the build's row in `index.md`
    pub title: String,
    pub summary: String,
    pub decisions: Vec<BuildDecision>,
    pub risks: Vec<String>,
    /// The model that wrote it
    pub model: String,
}

// -- JavaScript string helpers, so text matches what AI Lens writes --

/// JavaScript's `\s` (what `String.prototype.trim` strips)
pub const JS_SPACE: &[char] = &[
    '\t', '\n', '\u{0b}', '\u{0c}', '\r', ' ', '\u{a0}', '\u{1680}', '\u{2000}', '\u{2001}',
    '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}', '\u{2007}', '\u{2008}', '\u{2009}',
    '\u{200a}', '\u{2028}', '\u{2029}', '\u{202f}', '\u{205f}', '\u{3000}', '\u{feff}',
];

/// [`JS_SPACE`] as a regex class
pub(crate) const JS_SPACE_CLASS: &str = r"[\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]";

pub fn is_js_space(c: char) -> bool {
    JS_SPACE.contains(&c)
}

static ONE_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!("{JS_SPACE_CLASS}*\n{JS_SPACE_CLASS}*")).expect("valid regex")
});

/// `String.prototype.trim`
pub fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_space)
}

/// `String.prototype.trimEnd`
pub fn js_trim_end(text: &str) -> &str {
    text.trim_end_matches(is_js_space)
}

/// Joins hard-wrapped lines into one
pub fn one_line(text: &str) -> String {
    js_trim(&ONE_LINE_RE.replace_all(text, " ")).to_string()
}

/// Cuts `text` to `max_length` UTF-16 code units, ending in `…`
///
/// Lengths count UTF-16 code units, as JavaScript's do, so prompts match AI
/// Lens'. A surrogate pair the cut would split is dropped whole.
pub fn truncate(text: &str, max_length: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() <= max_length {
        return text.to_string();
    }
    let mut kept = &units[..max_length.saturating_sub(1)];
    // Drop a trailing high surrogate left without its low half
    if let Some(&last) = kept.last()
        && (0xD800..=0xDBFF).contains(&last)
    {
        kept = &kept[..kept.len() - 1];
    }
    String::from_utf16_lossy(kept) + "…"
}

// -- the prompt --

pub fn build_narrative_prompt(facts: &BuildFacts) -> String {
    let commits = &facts.commits[..facts.commits.len().min(MAX_COMMITS)];
    let files = &facts.files[..facts.files.len().min(MAX_FILES)];

    let mut range_line = format!(
        "Range: {} → {}",
        describe_revision(&facts.base),
        describe_revision(&facts.target)
    );
    if facts.relation == ComparisonRelation::Diverged {
        range_line.push_str(" (diverged branches; changes since their merge base)");
    }
    let stats = &facts.stats;
    let mut lines = vec![
        range_line,
        format!(
            "{} commits, {} files, +{} \u{2212}{} lines",
            stats.commits, stats.files, stats.insertions, stats.deletions
        ),
        String::new(),
        "Commits, oldest first:".into(),
    ];
    lines.extend(commits.iter().map(describe_commit));
    if facts.commits.len() > commits.len() {
        lines.push(format!(
            "({} more commits left out)",
            facts.commits.len() - commits.len()
        ));
    }
    lines.extend([String::new(), "Changed files:".into()]);
    for file in files {
        // Unique, in order of first appearance
        let mut seen = HashSet::new();
        let by: Vec<String> = file
            .entries
            .iter()
            .filter_map(|entry| entry.sha.as_ref())
            .map(|sha| {
                facts
                    .commits
                    .iter()
                    .find(|c| c.sha == *sha)
                    .map_or_else(|| sha.clone(), |c| c.short_sha.clone())
            })
            .filter(|short| seen.insert(short.clone()))
            .collect();
        let original = file
            .original_path
            .as_ref()
            .map_or_else(String::new, |original| format!("{original} -> "));
        let size = if file.ignored {
            "left out (generated or ignored)".to_string()
        } else if file.binary {
            "binary".to_string()
        } else {
            format!(
                "+{} \u{2212}{}",
                file.additions.unwrap_or(0),
                file.deletions.unwrap_or(0)
            )
        };
        let mut parts = vec![format!("{} {original}{}", file.status, file.path), size];
        if !by.is_empty() {
            parts.push(format!("by {}", by.join(", ")));
        }
        lines.push(parts.join(" · "));
    }
    if facts.files.len() > files.len() {
        lines.push(format!(
            "({} more files left out)",
            facts.files.len() - files.len()
        ));
    }
    lines.join("\n")
}

fn describe_revision(revision: &BuildRevision) -> String {
    match &revision.ref_name {
        Some(ref_name) => format!("{ref_name} ({})", revision.short_sha),
        None => revision.short_sha.clone(),
    }
}

fn describe_commit(commit: &BuildCommit) -> String {
    let who = if commit.agent == BuildAgent::ClaudeCode {
        let name = commit.agent_name.as_deref().unwrap_or("Claude");
        format!("{name} with {}", commit.author)
    } else {
        commit.author.clone()
    };
    let why = match &commit.why {
        None => "Why: not recorded".to_string(),
        Some(why) => {
            let label = if commit.why_source == ExplanationSource::Inferred {
                "Why (inferred, not the author’s words)"
            } else {
                "Why"
            };
            format!("{label}: {}", truncate(&one_line(why), MAX_WHY_CHARS))
        }
    };
    format!(
        "- {} · {who} · What: {} · {why}",
        commit.short_sha,
        one_line(&commit.what)
    )
}

// -- the answer --

/// Validates the model's answer; decisions may only cite commits in the
/// range. `model` is copied into the result as is.
pub fn parse_narrative(output: &Value, facts: &BuildFacts, model: &str) -> Result<BuildNarrative> {
    let get = |name: &str| output.as_object().and_then(|o| o.get(name));
    let (
        Some(Value::String(title)),
        Some(Value::String(summary)),
        Some(Value::Array(decisions)),
        Some(Value::Array(risks)),
    ) = (get("title"), get("summary"), get("decisions"), get("risks"))
    else {
        return Err(Error::msg("Claude Code's build summary was missing parts."));
    };

    let known: HashSet<&str> = facts.commits.iter().map(|c| c.short_sha.as_str()).collect();
    let strings = |values: &[Value]| -> Vec<String> {
        values
            .iter()
            .filter_map(Value::as_str)
            .filter(|value| !js_trim(value).is_empty())
            .map(one_line)
            .collect()
    };

    let mut parsed_decisions = Vec::new();
    for decision in decisions {
        let text = decision.get("text").and_then(Value::as_str);
        let Some(text) = text.filter(|text| !js_trim(text).is_empty()) else {
            continue;
        };
        let commits = match decision.get("commits") {
            Some(Value::Array(cited)) => strings(cited)
                .into_iter()
                .filter(|sha| known.contains(sha.as_str()))
                .collect(),
            _ => Vec::new(),
        };
        parsed_decisions.push(BuildDecision {
            text: one_line(text),
            commits,
        });
    }

    let title = one_line(title);
    Ok(BuildNarrative {
        title: if title.is_empty() {
            "Changes".into()
        } else {
            title
        },
        summary: js_trim(summary).to_string(),
        decisions: parsed_decisions,
        risks: strings(risks),
        model: model.to_string(),
    })
}

/// Asks Claude Code for the build's title, summary, decisions and risks
///
/// `run` replaces [`crate::claude::run_claude`], e.g. in tests. Blocks until
/// Claude answers, so call it off the UI thread.
pub fn write_narrative(
    facts: &BuildFacts,
    model: &str,
    claude_path: Option<&str>,
    run: Option<RunClaude>,
    cancel: &CancelToken,
) -> Result<BuildNarrative> {
    let mut request = ClaudeRequest::new(build_narrative_prompt(facts), model);
    request.system_prompt = Some(SYSTEM_PROMPT.to_string());
    request.json_schema = Some(SCHEMA.clone());
    // Leave the client's default (`claude` on PATH) alone otherwise
    if let Some(claude_path) = claude_path {
        request.claude_path = claude_path.to_string();
    }
    let run = run.unwrap_or_else(default_runner);
    let response = run(&request, cancel)?;
    parse_narrative(&response.output, facts, &response.model)
}
