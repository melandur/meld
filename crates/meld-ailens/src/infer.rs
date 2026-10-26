//! Asks Claude Code why a commit, or an uncommitted change, was made (port
//! of `infer.py`)
//!
//! Nothing here runs git: callers pass the commit and its own diff (against
//! its first parent), already parsed. Ignored and generated files are never
//! sent, and the diff is capped to keep the prompt a reasonable size.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

use serde_json::{Value, json};

use crate::claude::{ClaudeRequest, ClaudeResponse, DEFAULT_TIMEOUT, RunClaude, default_runner};
use crate::error::{ClaudeErrorReason, Error, Result};
use crate::explanation::{MessageFormat, compose_note, explain_commit, format_note, parse_note};
use crate::ignore::{create_ignore_rules, is_ignored};
use crate::models::{
    ChangeExplanation, ChangeGuide, ChangeKind, ChangeStep, CommitExplanation, CommitHunk,
    DiffLineKind, ExplanationSource, GitCommit, GitDiffHunk, GitFileChange, GitFileDiff,
    GitFileStatus, Side, StepLines, UncommittedLayer,
};
use crate::process::CancelToken;

pub const MAX_PATCH_CHARS_PER_FILE: usize = 12_000;
pub const MAX_PATCH_CHARS: usize = 60_000;

/// What the prompts ask of every explained change, in the commit and the
/// uncommitted prompts alike
macro_rules! change_fields_prompt {
    () => {
        r#"  - "kind": what kind of change it is, so the reader knows how closely to read it: "behavior" (changes what the
    code does), "fix" (fixes a bug), "refactor" (same behavior, new structure), "rename" (renames or moves, nothing
    else), "mechanical" (formatting, imports, generated or other changes that need no thought) or "test" (tests only).
  - "order": where to read the change to understand all the changes fastest, 1 first: definitions and the core of
    the change before their callers, mechanical changes last. Every change gets its own number.
  - "what": one plain sentence saying what the changed code does, as a statement a reader can take in at a glance
    before reading the lines.
  - "steps": the changed lines read line by line, top to bottom: one entry per line, or per few lines that do one
    thing together, with "lines" naming them by their tags ("n12", or "n12-n14" for a run; "o7" for deleted lines)
    and "says" telling in a few plain words what those lines do. Cover every changed line that does something; skip
    blank lines, lone closing brackets and unchanged lines. A deleted line gets a step only when its removal matters.
  - "was": what the code did before the change, in a few words; empty when the change adds something new.
  - "now": what the code does after the change, in a few words, set against "was".
  - "why": one or two plain sentences on why that change is needed: its part in the whole, specific to it.
    Don't repeat the overall reason.
  - "check": one plain sentence on what a reviewer should verify about the change: an edge case, a caller, an
    assumption it makes; empty for rename, mechanical and test changes that need no checking.
  - "justification": one to three plain sentences on why that change is the right one: the evidence for it in the
    diff and the code around it, and which alternatives or simpler fixes it rules out.
A justification uses only the evidence you are given; if that evidence is thin, say so plainly instead of inventing
support.
Write names from the code (functions, types, variables, fields, files) in backticks, exactly as the code spells
them. If a reason is not evident, give the most likely one and say that it is a guess. No other markdown, no
headings, no lists."#
    };
}

pub const SYSTEM_PROMPT: &str = concat!(
    r#"You explain git commits to engineers who want to understand the changed code as fast as possible.
The diff's hunks are numbered: each hunk header starts with "[change N]".
Every line of a hunk starts with a tag: "nN" for line N of the new version (added or unchanged lines), "oN" for
line N of the old version (deleted lines).
Reply with JSON matching the schema:
- "what": one plain sentence saying what the commit changed.
- "why": one to three plain sentences on why: the problem it solves or the goal it serves.
- "justification": one to three plain sentences on why this is the right change: the evidence for it in the diff,
  the commit message and the code around it, and which alternatives or simpler fixes it rules out.
- "changes": one entry for every numbered change in the diff, with its number as "change", and
"#,
    change_fields_prompt!(),
    "\nBase the answer only on the commit message and diff you are given."
);

pub const UNCOMMITTED_SYSTEM_PROMPT: &str = concat!(
    r#"You explain uncommitted changes to engineers who want to understand the changed code as fast as possible.
The changes are not committed yet, so there is no commit message: base the answer on the diff alone.
The diff's hunks are numbered: each hunk header starts with "[change N]".
Every line of a hunk starts with a tag: "nN" for line N of the new version (added or unchanged lines), "oN" for
line N of the old version (deleted lines).
Reply with JSON matching the schema: "changes" has one entry for every numbered change in the diff, with its
number as "change", and
"#,
    change_fields_prompt!()
);

fn changes_schema() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "change": {"type": "integer"},
                "kind": {"type": "string", "enum": ChangeKind::ALL.map(ChangeKind::as_str)},
                "order": {"type": "integer"},
                "what": {"type": "string"},
                "steps": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "lines": {"type": "string"},
                            "says": {"type": "string"},
                        },
                        "required": ["lines", "says"],
                        "additionalProperties": false,
                    },
                },
                "was": {"type": "string"},
                "now": {"type": "string"},
                "why": {"type": "string"},
                "check": {"type": "string"},
                "justification": {"type": "string"},
            },
            "required": [
                "change", "kind", "order", "what", "steps", "was", "now", "why", "check",
                "justification"
            ],
            "additionalProperties": false,
        },
    })
}

/// The reply schema of [`infer_explanation`]
pub static SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {
            "what": {"type": "string"},
            "why": {"type": "string"},
            "justification": {"type": "string"},
            "changes": changes_schema(),
        },
        "required": ["what", "why", "justification", "changes"],
        "additionalProperties": false,
    })
});

/// The reply schema of [`infer_uncommitted_explanation`]
pub static UNCOMMITTED_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    json!({
        "type": "object",
        "properties": {"changes": changes_schema()},
        "required": ["changes"],
        "additionalProperties": false,
    })
});

/// One changed file of a commit, with its diff
pub type FileDiff = (GitFileChange, Option<GitFileDiff>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferredExplanation {
    /// The commit's own explanation; `source` is `Inferred`
    pub explanation: CommitExplanation,
    /// The model that answered
    pub model: String,
    /// One per hunk of the commit's diff the model explained, in diff order
    pub changes: Vec<ChangeExplanation>,
}

/// Explanations of a file's uncommitted hunks in one layer
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferredChanges {
    pub model: String,
    /// In diff order; hunks the model didn't explain are left out
    pub changes: Vec<ChangeExplanation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub text: String,
    /// The hunks in the order they are numbered in it: `[change 1]` is the
    /// first
    pub hunks: Vec<CommitHunk>,
}

/// How to reach Claude Code
#[derive(Clone)]
pub struct InferOptions<'a> {
    pub model: &'a str,
    /// The text of the repo's `.ailensignore`
    pub ignore_rules: Option<&'a str>,
    pub claude_path: &'a str,
    pub timeout: Duration,
    /// Replaces [`crate::claude::run_claude`], e.g. in tests
    pub run: Option<RunClaude>,
}

impl<'a> InferOptions<'a> {
    pub fn new(model: &'a str) -> Self {
        Self {
            model,
            ignore_rules: None,
            claude_path: "claude",
            timeout: DEFAULT_TIMEOUT,
            run: None,
        }
    }

    fn call(&self, request: &ClaudeRequest, cancel: &CancelToken) -> Result<ClaudeResponse> {
        let run = self.run.clone().unwrap_or_else(default_runner);
        run(request, cancel)
    }
}

fn no_explanation() -> Error {
    Error::claude(
        "Claude Code's answer had no explanation in it.",
        ClaudeErrorReason::InvalidOutput,
    )
}

/// Ask Claude Code what a commit did and why, as a whole and hunk by hunk
///
/// `files` is every file the commit changed with its diff against the first
/// parent. Blocks until Claude Code answers.
pub fn infer_explanation(
    commit: &GitCommit,
    files: &[FileDiff],
    options: &InferOptions<'_>,
    cancel: &CancelToken,
) -> Result<InferredExplanation> {
    let prompt = build_prompt(commit, files, options.ignore_rules);
    let request = ClaudeRequest {
        prompt: prompt.text.clone(),
        model: options.model.to_string(),
        system_prompt: Some(SYSTEM_PROMPT.to_string()),
        json_schema: Some(SCHEMA.clone()),
        claude_path: options.claude_path.to_string(),
        cwd: None,
        timeout: options.timeout,
        env: None,
    };
    let response = options.call(&request, cancel)?;

    let output = response.output.as_object();
    let what = output.and_then(|o| o.get("what")).and_then(Value::as_str);
    let why = output.and_then(|o| o.get("why")).and_then(Value::as_str);
    let (Some(what), Some(why)) = (what, why) else {
        return Err(no_explanation());
    };
    if why.trim().is_empty() {
        return Err(no_explanation());
    }
    let what = what.trim();
    // Optional: an answer without one still explains the commit
    let justification = output
        .and_then(|o| o.get("justification"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|j| !j.is_empty())
        .map(str::to_string);

    Ok(InferredExplanation {
        explanation: CommitExplanation {
            what: if what.is_empty() {
                commit.subject.clone()
            } else {
                what.to_string()
            },
            why: Some(why.trim().to_string()),
            justification,
            source: ExplanationSource::Inferred,
            model: Some(response.model.clone()),
        },
        model: response.model.clone(),
        changes: parse_changes(output.and_then(|o| o.get("changes")), &prompt.hunks),
    })
}

/// The note to save on `commit` after inferring its explanation
///
/// A commit that already has a Why (from its message or a hand-written
/// note) keeps it, and only gets the explanations of its changes.
pub fn compose_inferred_note(
    commit: &GitCommit,
    inferred: &InferredExplanation,
    format: &MessageFormat,
) -> String {
    let current = explain_commit(commit, format);
    let note = commit.note.as_deref().map(parse_note);
    let head = if current.why.is_none() || current.source == ExplanationSource::Inferred {
        let why = inferred.explanation.why.clone().unwrap_or_default();
        format_note(
            &inferred.explanation.what,
            &why,
            inferred.explanation.justification.as_deref(),
            ExplanationSource::Inferred,
            Some(&inferred.model),
        )
    } else {
        match &note {
            Some(note) if note.source == ExplanationSource::Note => note.head.clone(),
            _ => String::new(),
        }
    };
    compose_note(&head, &inferred.model, &inferred.changes)
}

/// Ask Claude Code what each hunk of a file's staged or unstaged diff does
/// and why, from the diff alone
///
/// Fails with [`Error::IgnoredFile`] for generated and ignored files, which
/// are never sent, and [`Error::NothingToExplain`] for a diff without
/// hunks.
pub fn infer_uncommitted_explanation(
    layer: UncommittedLayer,
    diff: &GitFileDiff,
    options: &InferOptions<'_>,
    cancel: &CancelToken,
) -> Result<InferredChanges> {
    let Some(path) = diff.new_path.as_deref().or(diff.old_path.as_deref()) else {
        return Err(Error::NothingToExplain("The change has no path.".into()));
    };
    let original_path = diff.old_path.as_deref().filter(|old| *old != path);
    if is_ignored(
        &create_ignore_rules(options.ignore_rules),
        path,
        original_path,
    ) {
        return Err(Error::IgnoredFile(format!(
            "{path} is generated or in .ailensignore, so it isn't sent to Claude Code."
        )));
    }

    let hunks = hunks_of(diff);
    if hunks.is_empty() {
        return Err(Error::NothingToExplain(format!(
            "{path} has no {} changes to explain.",
            layer.as_str()
        )));
    }

    let mut numbering = 1..;
    let patch = render_file(diff, None, &mut numbering);
    let (shown, cut) = cap_patch(&[(patch, diff.old_path.as_deref().unwrap_or(path).to_string())]);
    let name = match original_path {
        Some(original) => format!("{original} -> {path}"),
        None => path.to_string(),
    };
    let title = match layer {
        UncommittedLayer::Staged => "Staged",
        UncommittedLayer::Unstaged => "Unstaged",
    };
    let mut lines = vec![format!("{title} changes to {name}, not committed yet")];
    if !cut.is_empty() {
        lines.extend([String::new(), "The diff is cut short.".into()]);
    }
    lines.extend([String::new(), "Diff:".into(), shown]);

    let request = ClaudeRequest {
        prompt: lines.join("\n"),
        model: options.model.to_string(),
        system_prompt: Some(UNCOMMITTED_SYSTEM_PROMPT.to_string()),
        json_schema: Some(UNCOMMITTED_SCHEMA.clone()),
        claude_path: options.claude_path.to_string(),
        cwd: None,
        timeout: options.timeout,
        env: None,
    };
    let response = options.call(&request, cancel)?;

    let changes = parse_changes(
        response.output.as_object().and_then(|o| o.get("changes")),
        &hunks,
    );
    if changes.is_empty() {
        return Err(no_explanation());
    }
    Ok(InferredChanges {
        model: response.model,
        changes,
    })
}

/// The answer's explanations of numbered hunks, in diff order
///
/// Unknown numbers, repeated numbers and empty explanations are dropped. A
/// missing or empty justification or guide field is None; the change is
/// still kept.
pub fn parse_changes(answer: Option<&Value>, hunks: &[CommitHunk]) -> Vec<ChangeExplanation> {
    let Some(Value::Array(items)) = answer else {
        return Vec::new();
    };
    type Parsed<'a> = (
        String,
        String,
        Option<String>,
        ChangeGuide,
        Option<&'a Value>,
    );
    let mut by_hunk: HashMap<i64, Parsed<'_>> = HashMap::new();
    for item in items {
        let Some(item) = item.as_object() else {
            continue;
        };
        let (Some(Value::Number(number)), Some(Value::String(what)), Some(Value::String(why))) =
            (item.get("change"), item.get("what"), item.get("why"))
        else {
            continue;
        };
        let Some(number) = integer(number) else {
            continue;
        };
        if why.trim().is_empty() || by_hunk.contains_key(&number) {
            continue;
        }
        let text = |key: &str| {
            item.get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
        };
        let guide = ChangeGuide {
            kind: item
                .get("kind")
                .and_then(Value::as_str)
                .and_then(ChangeKind::parse),
            order: match item.get("order") {
                Some(Value::Number(order)) => integer(order)
                    .and_then(|order| u32::try_from(order).ok())
                    .filter(|&order| order > 0),
                _ => None,
            },
            was: text("was"),
            now: text("now"),
            check: text("check"),
            steps: Vec::new(),
        };
        by_hunk.insert(
            number,
            (
                what.trim().to_string(),
                why.trim().to_string(),
                text("justification"),
                guide,
                item.get("steps"),
            ),
        );
    }

    hunks
        .iter()
        .enumerate()
        .filter_map(|(index, hunk)| {
            let (what, why, justification, mut guide, steps) =
                by_hunk.remove(&(index as i64 + 1))?;
            guide.steps = parse_steps(steps, hunk);
            Some(ChangeExplanation {
                hunk: hunk.clone(),
                what,
                why,
                justification,
                guide,
            })
        })
        .collect()
}

/// The answer's steps of one change, with their lines numbered among the
/// change's lines (see [`StepLines`])
///
/// Steps without text are dropped; a step whose tags are not lines of the
/// change keeps its text without lines.
fn parse_steps(steps: Option<&Value>, hunk: &CommitHunk) -> Vec<ChangeStep> {
    let Some(Value::Array(steps)) = steps else {
        return Vec::new();
    };
    steps
        .iter()
        .filter_map(|step| {
            let text = step.get("says")?.as_str()?.trim();
            if text.is_empty() {
                return None;
            }
            let lines = step
                .get("lines")
                .and_then(Value::as_str)
                .and_then(|tags| step_lines(tags, hunk));
            Some(ChangeStep {
                lines,
                text: text.to_string(),
            })
        })
        .collect()
}

/// Tags such as `n12`, `n12-n14` or `o7-9` as lines of the change at `hunk`
fn step_lines(tags: &str, hunk: &CommitHunk) -> Option<StepLines> {
    let tag = |tag: &str| -> Option<(Option<Side>, usize)> {
        let tag = tag.trim();
        let side = match tag.chars().next()? {
            'n' | 'N' => Some(Side::New),
            'o' | 'O' => Some(Side::Old),
            _ => None,
        };
        let digits = if side.is_some() { &tag[1..] } else { tag };
        Some((side, digits.trim().parse().ok()?))
    };
    let (first, last) = match tags.split_once('-') {
        Some((first, last)) => (tag(first)?, tag(last)?),
        None => {
            let line = tag(tags)?;
            (line, line)
        }
    };
    let side = first.0.or(last.0)?;
    let (start, count) = match side {
        Side::Old => (hunk.old_start, hunk.old_count),
        Side::New => (hunk.new_start, hunk.new_count),
    };
    let (first, last) = (first.1.min(last.1), first.1.max(last.1));
    if first < start || first >= start + count {
        return None;
    }
    Some(StepLines {
        side,
        first: first - start + 1,
        last: last.min(start + count - 1) - start + 1,
    })
}

/// A JSON number as an integer; JSON has no integers of its own, so 2.0 is 2
fn integer(number: &serde_json::Number) -> Option<i64> {
    match number.as_i64() {
        Some(number) => Some(number),
        None => match number.as_f64() {
            Some(value) if value.fract() == 0.0 && value.is_finite() => Some(value as i64),
            _ => None,
        },
    }
}

/// The prompt for [`infer_explanation`], and the hunks it numbers
pub fn build_prompt(commit: &GitCommit, files: &[FileDiff], ignore_rules: Option<&str>) -> Prompt {
    let rules = create_ignore_rules(ignore_rules);
    let included: Vec<&FileDiff> = files
        .iter()
        .filter(|(change, _)| !is_ignored(&rules, &change.path, change.original_path.as_deref()))
        .collect();
    let left_out = files.len() - included.len();

    let mut numbering = 1..;
    let mut hunks: Vec<CommitHunk> = Vec::new();
    let mut rendered: Vec<(String, String)> = Vec::new();
    for (change, diff) in included {
        let Some(diff) = diff else { continue };
        hunks.extend(hunks_of(diff));
        let name = diff
            .old_path
            .clone()
            .or_else(|| diff.new_path.clone())
            .unwrap_or_else(|| change.path.clone());
        rendered.push((render_file(diff, Some(change), &mut numbering), name));
    }
    let (shown, cut) = cap_patch(&rendered);

    let message = [commit.subject.as_str(), commit.body.as_str()]
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut lines = vec![
        format!("Commit {} by {}", commit.sha, commit.author.name),
        String::new(),
        "Message:".into(),
        message,
        String::new(),
        "Changed files:".into(),
    ];
    for (change, _) in files {
        let name = match &change.original_path {
            Some(original) => format!("{original} -> {}", change.path),
            None => change.path.clone(),
        };
        lines.push(format!("{} {name}", change.status));
    }
    if left_out > 0 {
        lines.extend([
            String::new(),
            format!("{left_out} of these files are left out of the diff (ignored or generated)."),
        ]);
    }
    if !cut.is_empty() {
        lines.extend([
            String::new(),
            format!("The diff is cut short for: {}.", cut.join(", ")),
        ]);
    }
    lines.extend([
        String::new(),
        "Diff:".into(),
        if shown.is_empty() {
            "(no textual changes to show)".into()
        } else {
            shown
        },
    ]);
    Prompt {
        text: lines.join("\n"),
        hunks,
    }
}

/// The diff's changes as they are numbered: one per change block, so each
/// gets its own explanation even where git joined it with a neighbour
fn hunks_of(diff: &GitFileDiff) -> Vec<CommitHunk> {
    let Some(path) = diff.new_path.as_deref().or(diff.old_path.as_deref()) else {
        return Vec::new();
    };
    let original_path = match (&diff.new_path, &diff.old_path) {
        (Some(_), Some(old)) if old != path => Some(old.clone()),
        _ => None,
    };
    diff.hunks
        .iter()
        .flat_map(GitDiffHunk::blocks)
        .map(|hunk| CommitHunk {
            path: path.to_string(),
            old_start: hunk.old_start,
            old_count: hunk.old_count,
            new_start: hunk.new_start,
            new_count: hunk.new_count,
            original_path: original_path.clone(),
        })
        .collect()
}

fn range(start: usize, count: usize) -> String {
    if count == 1 {
        start.to_string()
    } else {
        format!("{start},{count}")
    }
}

fn hunk_header(hunk: &GitDiffHunk) -> String {
    let header = format!(
        "@@ -{} +{} @@",
        range(hunk.old_start, hunk.old_count),
        range(hunk.new_start, hunk.new_count)
    );
    if hunk.section.is_empty() {
        header
    } else {
        format!("{header} {}", hunk.section)
    }
}

/// One file's diff as `git diff` prints it, split into change blocks, with
/// each block's hunk header prefixed by `[change N]` (N from `numbering`)
fn render_file(
    diff: &GitFileDiff,
    change: Option<&GitFileChange>,
    numbering: &mut impl Iterator<Item = usize>,
) -> String {
    let (old, new) = (diff.old_path.as_deref(), diff.new_path.as_deref());
    let mut lines = vec![format!(
        "diff --git a/{} b/{}",
        old.or(new).unwrap_or(""),
        new.or(old).unwrap_or("")
    )];
    if let (Some(old), Some(new)) = (old, new)
        && old != new
    {
        let kind = if change.is_some_and(|c| c.status == GitFileStatus::Copied) {
            "copy"
        } else {
            "rename"
        };
        if let Some(similarity) = change.and_then(|c| c.similarity) {
            lines.push(format!("similarity index {similarity}%"));
        }
        lines.push(format!("{kind} from {old}"));
        lines.push(format!("{kind} to {new}"));
    }

    let old_name = old.map_or("/dev/null".to_string(), |old| format!("a/{old}"));
    let new_name = new.map_or("/dev/null".to_string(), |new| format!("b/{new}"));
    if diff.binary {
        lines.push(format!("Binary files {old_name} and {new_name} differ"));
    } else if !diff.hunks.is_empty() {
        lines.push(format!("--- {old_name}"));
        lines.push(format!("+++ {new_name}"));
        for hunk in diff.hunks.iter().flat_map(GitDiffHunk::blocks) {
            let number = numbering.next().unwrap_or_default();
            lines.push(format!("[change {number}] {}", hunk_header(&hunk)));
            for line in &hunk.lines {
                let tag = match (line.kind, line.old_line, line.new_line) {
                    (DiffLineKind::Delete, Some(old), _) => format!("o{old}"),
                    (_, _, Some(new)) => format!("n{new}"),
                    _ => String::new(),
                };
                lines.push(format!("{tag:<6}{}{}", line.kind.prefix(), line.content));
                if line.no_newline_at_end {
                    lines.push("\\ No newline at end of file".into());
                }
            }
        }
    }
    lines.join("\n") + "\n"
}

/// Keep each file's patch and the whole patch under the caps
///
/// `files` holds each file's patch and name. Returns the patch and the
/// names of the files that were cut. Lengths count characters.
fn cap_patch(files: &[(String, String)]) -> (String, Vec<String>) {
    let mut cut: Vec<String> = Vec::new();
    let mut total = 0;
    let mut kept = String::new();
    for (text, name) in files {
        let mut text = text.clone();
        let mut length = text.chars().count();
        if length > MAX_PATCH_CHARS_PER_FILE {
            text = format!(
                "{}\n[… rest of {name} left out]\n",
                crate::claude::prefix(&text, MAX_PATCH_CHARS_PER_FILE)
            );
            length = text.chars().count();
            cut.push(name.clone());
        }
        if total + length > MAX_PATCH_CHARS {
            cut.push(name.clone());
            continue;
        }
        kept.push_str(&text);
        total += length;
    }
    (kept, cut)
}
