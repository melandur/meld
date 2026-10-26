//! What the Why column shows: one card per change group (port of
//! `cards.py`)

use std::sync::LazyLock;

use regex::Regex;

use crate::models::{ChangeKind, ExplanationSource, LineRange, Side};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CardKind {
    Commit,
    Staged,
    Unstaged,
    #[default]
    Unknown,
}

/// What the card's explain button explains
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExplainAction {
    /// The commit has no Why yet: explain it and its changes
    Commit,
    /// The commit has a Why: explain its changes one by one
    Changes,
    /// Explain the file's staged or unstaged hunks
    Uncommitted,
    /// The commit's changes were explained, but not this card's (left out
    /// of the prompt for its size): explain only its hunks
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Card {
    /// Stable across reloads of the same file, e.g. to keep focus
    pub key: String,
    pub kind: CardKind,
    /// Lines in the target version (1-based, inclusive)
    pub added: Vec<LineRange>,
    /// Lines in the base version (1-based, inclusive)
    pub deleted: Vec<LineRange>,
    pub sha: Option<String>,
    pub short_sha: String,
    pub subject: String,
    /// The whole commit message
    pub message: String,
    pub author: String,
    /// Author date, strict ISO 8601
    pub date: String,
    /// Committer date, strict ISO 8601: when the commit was made as it is,
    /// which a rebase or cherry-pick updates, unlike the author date
    pub committed: String,
    /// e.g. "Claude Opus 5.5"; empty for people
    pub agent_name: String,
    pub agent_model: String,
    pub what: String,
    pub why: String,
    /// Why the commit is the right change: its evidence and the
    /// alternatives it rules out; empty when not given. From the same
    /// source as `why`
    pub justification: String,
    pub source: Option<ExplanationSource>,
    /// Model that inferred `why`, for `Inferred`
    pub inferred_by: String,
    /// What this change (not the whole commit) does and why; always
    /// inferred
    pub change_what: String,
    pub change_why: String,
    /// Why this change is the right one; empty when not given
    pub change_justification: String,
    /// The change's kind, reading order, before and after, and what to
    /// check; empty in notes written before they were asked for
    pub change_kind: Option<ChangeKind>,
    /// Where to read the change among its commit's (or layer's) changes;
    /// 1 first
    pub change_order: Option<u32>,
    pub change_was: String,
    pub change_now: String,
    pub change_check: String,
    /// How much the change needs checking, 1 to 5: the highest of the
    /// changes the card shows; None in notes from before risks
    pub change_risk: Option<u8>,
    /// The change read line by line; empty in notes written before steps
    /// were asked for
    pub change_steps: Vec<CardStep>,
    pub change_model: String,
    /// The explain button, if any
    pub explain: Option<ExplainAction>,
    /// Index into the comparison's commits, oldest first, for its color; -1
    /// for uncommitted and unknown changes
    pub color_index: i32,
    /// Build files (names in the build folder) covering this commit
    pub builds: Vec<String>,
}

/// One step of reading a change line by line
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardStep {
    /// Its lines in the comparison: in the target version (`Side::New`) or
    /// the base version (`Side::Old`), 1-based, inclusive; None when they
    /// are not lines of this card
    pub lines: Option<(Side, LineRange)>,
    pub text: String,
}

impl Card {
    /// Where the card is level with: its first added line (in the target
    /// version, `Side::New`), else its first deleted line (in the base
    /// version, `Side::Old`); 1-based
    pub fn anchor(&self) -> (Side, usize) {
        if let Some(start) = self.added.iter().map(|r| r.start).min() {
            return (Side::New, start);
        }
        if let Some(start) = self.deleted.iter().map(|r| r.start).min() {
            return (Side::Old, start);
        }
        (Side::New, 1)
    }

    pub fn agent(&self) -> bool {
        !self.agent_name.is_empty()
    }
}

/// Positions for cards wanting to be at `tops`, in order
///
/// Cards are margin notes: each stays level with its change, pushed down
/// only as far as needed to clear the card above it.
pub fn layout_cards(tops: &[f32], heights: &[f32], gap: f32) -> Vec<f32> {
    let mut placed = Vec::with_capacity(tops.len());
    let mut bottom = f32::NEG_INFINITY;
    for (&top, &height) in tops.iter().zip(heights) {
        let y = if placed.is_empty() {
            top
        } else {
            top.max(bottom + gap)
        };
        placed.push(y);
        bottom = y + height;
    }
    placed
}

/// The default gap of [`layout_cards`]
pub const CARD_GAP: f32 = 4.0;

static LIST_ITEM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:[-*+•]|[0-9]+[.)])\s+").expect("valid regex"));
static PARAGRAPHS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n\s*\n").expect("valid regex"));

/// Join hard-wrapped lines so labels can wrap them to fit
///
/// Paragraphs (blank lines) and list items stay on lines of their own.
pub fn reflow(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n");
    let mut result: Vec<String> = Vec::new();
    for paragraph in PARAGRAPHS.split(normalized.trim()) {
        let mut lines: Vec<String> = Vec::new();
        for line in paragraph.split('\n') {
            let stripped = line.trim();
            if stripped.is_empty() {
                continue;
            }
            let item = LIST_ITEM.is_match(line);
            match lines.last_mut() {
                Some(last) if !item => {
                    last.push(' ');
                    last.push_str(stripped);
                }
                _ => lines.push(if item {
                    line.trim_end().to_string()
                } else {
                    stripped.to_string()
                }),
            }
        }
        result.push(lines.join("\n"));
    }
    result
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}
