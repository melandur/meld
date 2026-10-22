//! Map line numbers between the two versions of a file diff (port of
//! `linemap.py`)
//!
//! Lines outside the hunks move by the lines added and deleted above them.
//! Line numbers are 1-based, as in the diff.

use crate::models::{DiffLineKind, GitFileDiff};

/// Where line `line` of the new version was in the old one
///
/// None when the diff added it.
pub fn new_to_old(diff: &GitFileDiff, line: usize) -> Option<usize> {
    let mut shift: i64 = 0;
    for hunk in &diff.hunks {
        // A hunk without new lines sits after line new_start
        let start = if hunk.new_count == 0 {
            hunk.new_start + 1
        } else {
            hunk.new_start
        };
        if line < start {
            break;
        }
        if line < start + hunk.new_count {
            for candidate in &hunk.lines {
                if candidate.kind != DiffLineKind::Delete && candidate.new_line == Some(line) {
                    return if candidate.kind == DiffLineKind::Context {
                        candidate.old_line
                    } else {
                        None
                    };
                }
            }
            return None;
        }
        shift += hunk.old_count as i64 - hunk.new_count as i64;
    }
    usize::try_from(line as i64 + shift).ok()
}

/// Where line `line` of the old version is in the new one
///
/// None when the diff deleted it.
pub fn old_to_new(diff: &GitFileDiff, line: usize) -> Option<usize> {
    let mut shift: i64 = 0;
    for hunk in &diff.hunks {
        // A hunk without old lines sits after line old_start
        let start = if hunk.old_count == 0 {
            hunk.old_start + 1
        } else {
            hunk.old_start
        };
        if line < start {
            break;
        }
        if line < start + hunk.old_count {
            for candidate in &hunk.lines {
                if candidate.kind != DiffLineKind::Add && candidate.old_line == Some(line) {
                    return if candidate.kind == DiffLineKind::Context {
                        candidate.new_line
                    } else {
                        None
                    };
                }
            }
            return None;
        }
        shift += hunk.new_count as i64 - hunk.old_count as i64;
    }
    usize::try_from(line as i64 + shift).ok()
}
