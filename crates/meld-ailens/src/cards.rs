//! What the Why column shows: one card per change group (port of
//! `cards.py`)

use crate::models::{LineRange, Side};

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
    pub what: String,
    /// What this change (not the whole commit) does and why; always
    /// inferred
    pub change_what: String,
    pub change_why: String,
    /// Why this change is the right one; empty when not given
    pub change_justification: String,
    /// How much the change needs checking, 1 to 5: the highest of the
    /// changes the card shows; None in notes from before risks
    pub change_risk: Option<u8>,
    /// The change read line by line; empty in notes written before steps
    /// were asked for
    pub change_steps: Vec<CardStep>,
    /// The explain button, if any
    pub explain: Option<ExplainAction>,
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
