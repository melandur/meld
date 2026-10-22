//! Why a commit was made: from its message or its AI Lens note (port of
//! `explanation.py`)
//!
//! The note format is shared with the AI Lens VS Code extension, so notes
//! written by either are read by the other; keep it byte-for-byte
//! compatible.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use crate::models::{
    ChangeExplanation, CommitExplanation, CommitHunk, ExplanationSource, GitCommit, LineOrigin,
    Side,
};
use crate::uncommitted::HunkChange;

fn regex(source: &str) -> Regex {
    Regex::new(source).expect("valid regex")
}

static LABEL: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?s)\A([A-Za-z][A-Za-z0-9_-]*):[ \t]*(.*)\z"));
static HEAD_LINE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\A([A-Za-z][A-Za-z0-9_-]*):[ \t]*(.*)\z"));
static CHANGE_MODEL_LINE: LazyLock<Regex> = LazyLock::new(|| regex(r"\AChange-Model:[ \t]*(.*)\z"));
static CHANGE_LINE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\AChange: -([0-9]+)(?:,([0-9]+))? \+([0-9]+)(?:,([0-9]+))? (.+)\z"));
static CHANGE_FIELD: LazyLock<Regex> =
    LazyLock::new(|| regex(r"\A(Change-From|What|Why):[ \t]*(.*)\z"));
static TRAILER_LINE: LazyLock<Regex> = LazyLock::new(|| regex(r"\A[A-Za-z0-9][A-Za-z0-9_-]*:\s"));
static PARAGRAPH_BREAK: LazyLock<Regex> = LazyLock::new(|| regex(r"\n\s*\n"));

/// Which labels mark What and Why in a commit message
///
/// Either as trailers (`Why: …`) or as body paragraphs starting with them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageFormat {
    pub what_keys: Vec<String>,
    pub why_keys: Vec<String>,
}

impl Default for MessageFormat {
    fn default() -> Self {
        Self {
            what_keys: vec!["What".into(), "Agent-What".into()],
            why_keys: vec!["Why".into(), "Agent-Why".into()],
        }
    }
}

fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
}

fn some_if_nonempty(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

/// The commit's explanation, best source first
///
/// 1. What/Why labeled in the commit message
/// 2. its note in the AI Lens notes ref (added later by hand, or inferred
///    and saved)
/// 3. the plain message: subject as What, body (without trailers) as Why
///
/// A result without a Why can be inferred.
pub fn explain_commit(commit: &GitCommit, format: &MessageFormat) -> CommitExplanation {
    let (what, why) = parse_labeled_message(commit, format);
    if let Some(why) = why {
        return CommitExplanation {
            what: what.unwrap_or_else(|| commit.subject.clone()),
            why: Some(why),
            source: ExplanationSource::Commit,
            model: None,
        };
    }

    if let Some(note) = &commit.note {
        let note = parse_note(note);
        if note.why.is_some() {
            return CommitExplanation {
                what: note.what.unwrap_or_else(|| commit.subject.clone()),
                why: note.why,
                source: note.source,
                model: note.model,
            };
        }
    }

    let body = strip_trailer_block(&normalize(&commit.body), !commit.trailers.is_empty());
    CommitExplanation {
        what: what.unwrap_or_else(|| commit.subject.clone()),
        why: some_if_nonempty(body.trim().to_string()),
        source: ExplanationSource::Commit,
        model: None,
    }
}

/// What/Why from trailers with the format's keys, or from body paragraphs
/// that start with `<key>:`; empty values count as missing
fn parse_labeled_message(
    commit: &GitCommit,
    format: &MessageFormat,
) -> (Option<String>, Option<String>) {
    let what_keys: HashSet<String> = format.what_keys.iter().map(|k| k.to_lowercase()).collect();
    let why_keys: HashSet<String> = format.why_keys.iter().map(|k| k.to_lowercase()).collect();

    let mut what: Option<String> = None;
    let mut why: Option<String> = None;
    for trailer in &commit.trailers {
        let key = trailer.key.to_lowercase();
        if what.is_none() && what_keys.contains(&key) {
            what = Some(trailer.value.clone());
        } else if why.is_none() && why_keys.contains(&key) {
            why = Some(trailer.value.clone());
        }
    }

    let body = normalize(&commit.body);
    for paragraph in PARAGRAPH_BREAK.split(&body) {
        let Some(captures) = LABEL.captures(paragraph.trim()) else {
            continue;
        };
        let key = captures[1].to_lowercase();
        if what.is_none() && what_keys.contains(&key) {
            what = Some(captures[2].trim().to_string());
        } else if why.is_none() && why_keys.contains(&key) {
            why = Some(captures[2].trim().to_string());
        }
    }

    (
        what.filter(|w| !w.is_empty()),
        why.filter(|w| !w.is_empty()),
    )
}

fn strip_trailer_block(body: &str, has_trailers: bool) -> String {
    if !has_trailers {
        return body.to_string();
    }
    let trimmed = body.trim_end();
    let mut paragraphs: Vec<&str> = PARAGRAPH_BREAK.split(trimmed).collect();
    if let Some(last) = paragraphs.last() {
        let all_trailers = last.split('\n').all(|line| {
            TRAILER_LINE.is_match(line) || line.chars().next().is_some_and(char::is_whitespace)
        });
        if all_trailers {
            paragraphs.pop();
        }
    }
    paragraphs.join("\n\n")
}

/// A parsed AI Lens note
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedNote {
    pub what: Option<String>,
    pub why: Option<String>,
    /// `Note` or `Inferred`
    pub source: ExplanationSource,
    pub model: Option<String>,
    /// Model that explained the commit's changes one by one; set once they
    /// were explained
    pub change_model: Option<String>,
    /// In the order of the commit's diff
    pub changes: Vec<ChangeExplanation>,
    /// The note's text before its changes: the commit's own explanation, as
    /// written
    pub head: String,
}

impl ParsedNote {
    /// Whether the commit's changes were explained one by one
    pub fn changes_explained(&self) -> bool {
        self.change_model.is_some() || !self.changes.is_empty()
    }
}

/// Parse an AI Lens note
///
/// Notes are `Key: value` lines (`Source`, `Model`, `What`) and then
/// `Why:`, which runs to the end. A note without `What:` or `Why:` is all
/// Why, so a plain `git notes add -m "…"` works too.
///
/// Explanations of single changes come after, from a `Change-Model:` line
/// on: each starts with `Change: -<old> +<new> <path>` (the hunk's ranges in
/// the commit's own diff), then `Change-From:` for a rename, `What:`, and
/// `Why:` up to the next `Change:`.
pub fn parse_note(text: &str) -> ParsedNote {
    let normalized = normalize(text);
    let all_lines: Vec<&str> = normalized.trim().split('\n').collect();
    let tail_start = all_lines
        .iter()
        .position(|line| CHANGE_MODEL_LINE.is_match(line) || CHANGE_LINE.is_match(line))
        .unwrap_or(all_lines.len());
    let head = all_lines[..tail_start].join("\n").trim().to_string();
    let mut note = parse_note_head(&head);

    // The change being read: its header fields, What, and Why lines so far
    let mut hunk: Option<CommitHunk> = None;
    let mut what = String::new();
    let mut why: Option<Vec<&str>> = None;

    fn finish(
        note: &mut ParsedNote,
        hunk: &Option<CommitHunk>,
        what: &str,
        why: &Option<Vec<&str>>,
    ) {
        if let Some(hunk) = hunk {
            let text = why
                .as_ref()
                .map(|lines| lines.join("\n").trim().to_string())
                .unwrap_or_default();
            if !text.is_empty() {
                note.changes.push(ChangeExplanation {
                    hunk: hunk.clone(),
                    what: what.to_string(),
                    why: text,
                });
            }
        }
    }

    for &line in &all_lines[tail_start..] {
        if hunk.is_none()
            && let Some(captures) = CHANGE_MODEL_LINE.captures(line)
        {
            note.change_model = some_if_nonempty(captures[1].trim().to_string());
            continue;
        }

        if let Some(captures) = CHANGE_LINE.captures(line) {
            finish(&mut note, &hunk, &what, &why);
            let number = |index: usize| -> Option<usize> {
                captures.get(index).map(|m| m.as_str().parse().unwrap_or(0))
            };
            hunk = Some(CommitHunk {
                path: captures[5].to_string(),
                old_start: number(1).unwrap_or(0),
                old_count: number(2).unwrap_or(1),
                new_start: number(3).unwrap_or(0),
                new_count: number(4).unwrap_or(1),
                original_path: None,
            });
            what.clear();
            why = None;
            continue;
        }
        let Some(current) = hunk.as_mut() else {
            continue;
        };

        if let Some(lines) = why.as_mut() {
            lines.push(line);
            continue;
        }

        let Some(captures) = CHANGE_FIELD.captures(line) else {
            continue;
        };
        let value = captures.get(2).map_or("", |m| m.as_str());
        match &captures[1] {
            "Change-From" => current.original_path = Some(value.to_string()),
            "What" => what = value.trim().to_string(),
            _ => why = Some(vec![value]),
        }
    }
    finish(&mut note, &hunk, &what, &why);
    note
}

fn parse_note_head(text: &str) -> ParsedNote {
    let mut note = ParsedNote {
        what: None,
        why: None,
        source: ExplanationSource::Note,
        model: None,
        change_model: None,
        changes: Vec::new(),
        head: text.to_string(),
    };
    let lines: Vec<&str> = text.split('\n').collect();

    let mut i = 0;
    while i < lines.len() {
        let Some(captures) = HEAD_LINE.captures(lines[i]) else {
            break;
        };
        let key = captures[1].to_lowercase();
        let value = captures[2].trim().to_string();
        match key.as_str() {
            "why" => {
                let mut parts = vec![value.as_str()];
                parts.extend(&lines[i + 1..]);
                note.why = some_if_nonempty(parts.join("\n").trim().to_string());
                return note;
            }
            "what" => note.what = some_if_nonempty(value),
            "source" => {
                note.source = if value.to_lowercase() == "inferred" {
                    ExplanationSource::Inferred
                } else {
                    ExplanationSource::Note
                };
            }
            "model" => note.model = some_if_nonempty(value),
            _ => break,
        }
        i += 1;
    }

    if note.what.is_none() && i == 0 {
        note.why = some_if_nonempty(text.trim().to_string());
    }
    note
}

/// A note holding a commit's own explanation
///
/// `source` is `Note` or `Inferred`.
pub fn format_note(
    what: &str,
    why: &str,
    source: ExplanationSource,
    model: Option<&str>,
) -> String {
    let mut lines = vec![format!("Source: {source}")];
    if let Some(model) = model {
        lines.push(format!("Model: {model}"));
    }
    lines.push(format!("What: {}", what.replace('\n', " ")));
    lines.push(format!("Why: {why}"));
    lines.push(String::new());
    lines.join("\n")
}

/// A line that looks like the next change's header would end a Why early:
/// indent each such line by a space
fn escape_change_headers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let header = line
            .strip_prefix("Change: -")
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()));
        if header {
            out.push(' ');
        }
        out.push_str(line);
    }
    out
}

/// A note from its head (the commit's own explanation, may be empty) and
/// the explanations of its changes
pub fn compose_note(head: &str, model: &str, changes: &[ChangeExplanation]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !head.trim().is_empty() {
        parts.push(head.trim().to_string());
    }
    parts.push(format!("Change-Model: {model}"));
    for change in changes {
        let hunk = &change.hunk;
        let mut lines = vec![format!(
            "Change: -{},{} +{},{} {}",
            hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count, hunk.path
        )];
        if let Some(original) = hunk.original_path.as_ref().filter(|o| **o != hunk.path) {
            lines.push(format!("Change-From: {original}"));
        }
        lines.push(format!("What: {}", change.what.replace('\n', " ")));
        lines.push(format!("Why: {}", escape_change_headers(change.why.trim())));
        parts.push(lines.join("\n"));
    }
    parts.join("\n\n") + "\n"
}

/// The explanations of the commit's own hunks that a change group's lines
/// fall in, in the order the group's lines first reach them
///
/// Empty when none do (the commit's changes were not explained, or the
/// hunk was left out of the prompt).
pub fn find_change_explanations<'a, C: HunkChange>(
    origins: &[LineOrigin],
    changes: &'a [C],
) -> Vec<&'a C> {
    let mut found: Vec<usize> = Vec::new();
    for origin in origins {
        for (index, change) in changes.iter().enumerate() {
            if !found.contains(&index) && covers(change.hunk(), origin) {
                found.push(index);
            }
        }
    }
    found.into_iter().map(|index| &changes[index]).collect()
}

fn covers(hunk: &CommitHunk, origin: &LineOrigin) -> bool {
    let (path, start, count) = match origin.side {
        Side::Old => (
            hunk.original_path.as_deref().unwrap_or(&hunk.path),
            hunk.old_start,
            hunk.old_count,
        ),
        Side::New => (hunk.path.as_str(), hunk.new_start, hunk.new_count),
    };
    path == origin.path && start <= origin.line && origin.line < start + count
}
