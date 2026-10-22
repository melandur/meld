//! Producing merged text from a [`Differ`] (port of `Merger` in
//! `meld/matchers/merge.py`).
//!
//! The differ is computed on the (possibly filtered) comparison lines, while
//! the merge copies the real buffer lines (`texts`), exactly as Meld does.

use crate::chunk::{Chunk, Tag};
use crate::differ::Differ;
use crate::error::DiffError;

/// Prefix of lines with an unresolved conflict in an automatic merge.
pub const CONFLICT_MARKER: &str = "(??)";

/// The result of a merge.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Merged {
    /// The merged lines, without terminators.
    pub lines: Vec<String>,
    /// Indices into `lines` of unresolved conflict marker lines.
    pub unresolved: Vec<usize>,
}

impl Merged {
    /// The merged text as Python produced it: `"\n".join(lines)`.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
}

fn line_of<S: AsRef<[String]>>(texts: &[S], pane: usize, i: usize) -> Option<&String> {
    texts.get(pane).and_then(|t| t.as_ref().get(i))
}

fn push_range<S: AsRef<[String]>>(
    out: &mut Vec<String>,
    texts: &[S],
    pane: usize,
    lo: usize,
    hi: usize,
) {
    out.extend((lo..hi).filter_map(|i| line_of(texts, pane, i)).cloned());
}

/// `_apply_change`: copy the `b` side of `change` from `texts[pane]`;
/// returns how many base (`a` side) lines it consumes.
fn apply_change<S: AsRef<[String]>>(
    texts: &[S],
    pane: usize,
    change: &Chunk,
    out: &mut Vec<String>,
) -> usize {
    match change.tag {
        Tag::Insert => {
            push_range(out, texts, pane, change.start_b, change.end_b);
            0
        }
        Tag::Replace => {
            push_range(out, texts, pane, change.start_b, change.end_b);
            change.end_a - change.start_a
        }
        _ => change.end_a - change.start_a,
    }
}

/// Merge the changes of panes 0 and 2 into pane 1 (`merge_3_files`).
///
/// Non-conflicting changes are applied. Conflicting lines are kept from the
/// base and, with `mark_conflicts`, prefixed with [`CONFLICT_MARKER`] (an
/// empty conflict becomes a lone marker line) and recorded as unresolved;
/// without it they are left as they are ("merge all non-conflicting").
pub fn merge_3_files<S: AsRef<[String]>>(
    differ: &Differ,
    texts: &[S],
    mark_conflicts: bool,
) -> Merged {
    let mut unresolved = Vec::new();
    let mut lastline = 0;
    let mut mergedline = 0;
    let mut merged = Vec::new();
    for change in differ.all_changes() {
        let mut low_mark = lastline;
        if let Some(c) = change[0] {
            low_mark = c.start_a;
        }
        if let Some(c) = change[1] {
            low_mark = low_mark.max(c.start_a);
        }
        push_range(&mut merged, texts, 1, lastline, low_mark);
        mergedline += low_mark.saturating_sub(lastline);
        lastline = low_mark;
        match *change {
            [Some(c0), Some(c1)] if c0.tag == Tag::Conflict => {
                let high_mark = c0.end_a.max(c1.end_a);
                if mark_conflicts {
                    if low_mark < high_mark {
                        for i in low_mark..high_mark {
                            let base = line_of(texts, 1, i).map_or("", |s| s.as_str());
                            merged.push(format!("{CONFLICT_MARKER}{base}"));
                            unresolved.push(mergedline);
                            mergedline += 1;
                        }
                    } else {
                        merged.push(CONFLICT_MARKER.to_string());
                        unresolved.push(mergedline);
                        mergedline += 1;
                    }
                    lastline = high_mark;
                }
            }
            [Some(c0), _] => {
                lastline += apply_change(texts, 0, &c0, &mut merged);
                mergedline += c0.end_b - c0.start_b;
            }
            [None, Some(c1)] => {
                lastline += apply_change(texts, 2, &c1, &mut merged);
                mergedline += c1.end_b - c1.start_b;
            }
            [None, None] => {}
        }
    }
    let baselen = texts.get(1).map_or(0, |t| t.as_ref().len());
    push_range(&mut merged, texts, 1, lastline, baselen);
    Merged {
        lines: merged,
        unresolved,
    }
}

/// Apply all non-conflicting changes from `fromindex` onto `toindex`
/// (`merge_2_files`, used by "pull all non-conflicting changes").
pub fn merge_2_files<S: AsRef<[String]>>(
    differ: &Differ,
    texts: &[S],
    fromindex: usize,
    toindex: usize,
) -> Merged {
    let mut lastline = 0;
    let mut merged = Vec::new();
    for change in differ.pair_changes(toindex, fromindex, None) {
        let low_mark = if change.tag == Tag::Conflict {
            change.end_a
        } else {
            change.start_a
        };
        push_range(&mut merged, texts, toindex, lastline, low_mark);
        lastline = low_mark;
        if change.tag != Tag::Conflict {
            lastline += apply_change(texts, fromindex, &change, &mut merged);
        }
    }
    let baselen = texts.get(toindex).map_or(0, |t| t.as_ref().len());
    push_range(&mut merged, texts, toindex, lastline, baselen);
    Merged {
        lines: merged,
        unresolved: Vec::new(),
    }
}

/// The initial automatic merge of Meld's auto-merge mode: compare
/// `sequences` (the filtered lines of the three panes) with an
/// auto-merging differ and merge `texts` (the real lines) with conflict
/// markers (`Merger.initialize` + `merge_3_files(True)`).
///
/// The caller replaces pane 1 with the result, re-runs its comparison and
/// hands `unresolved` to its own differ with [`Differ::set_unresolved`].
pub fn auto_merge<S: AsRef<[String]>, T: AsRef<[String]>>(
    sequences: &[S],
    texts: &[T],
) -> Result<Merged, DiffError> {
    auto_merge_cancellable(sequences, texts, &|| false)
}

/// [`auto_merge`] with a cancellation callback.
pub fn auto_merge_cancellable<S: AsRef<[String]>, T: AsRef<[String]>>(
    sequences: &[S],
    texts: &[T],
    cancel: &dyn Fn() -> bool,
) -> Result<Merged, DiffError> {
    let mut differ = Differ::new();
    differ.auto_merge = true;
    differ.set_sequences_cancellable(sequences, cancel)?;
    Ok(merge_3_files(&differ, texts, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn merges_non_conflicting_and_marks_conflicts() {
        let left = lines(&["a", "L", "c", "x", "e"]);
        let base = lines(&["a", "b", "c", "d", "e"]);
        let right = lines(&["a", "b", "c", "y", "e", "R"]);
        let texts = [left, base, right];
        let merged = auto_merge(&texts, &texts).unwrap();
        assert_eq!(merged.lines, lines(&["a", "L", "c", "(??)d", "e", "R"]));
        assert_eq!(merged.unresolved, vec![3]);
        assert_eq!(merged.text(), "a\nL\nc\n(??)d\ne\nR");

        let mut differ = Differ::new();
        differ.set_sequences(&texts).unwrap();
        let merged = merge_3_files(&differ, &texts, false);
        assert_eq!(merged.lines, lines(&["a", "L", "c", "d", "e", "R"]));
        assert!(merged.unresolved.is_empty());
    }

    #[test]
    fn two_way_pull() {
        let texts = [lines(&["a", "b", "c"]), lines(&["a", "x", "c", "d"])];
        let mut differ = Differ::new();
        differ.set_sequences(&texts).unwrap();
        assert_eq!(merge_2_files(&differ, &texts, 0, 1).lines, texts[0]);
        assert_eq!(merge_2_files(&differ, &texts, 1, 0).lines, texts[1]);
    }
}
