//! Text filters applied to buffer lines before comparing (port of
//! `misc.apply_text_filters` / `misc.merge_intervals` as used by
//! `FileDiff._filter_text`).
//!
//! Filtering removes the matched text but never changes the number of
//! lines, so chunk line numbers still apply to the unfiltered buffers.

use regex::Regex;

use crate::error::DiffError;

/// Merge overlapping or touching intervals (`merge_intervals`).
///
/// Lists with fewer than two intervals are returned unchanged.
pub fn merge_intervals(mut intervals: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    if intervals.len() < 2 {
        return intervals;
    }
    intervals.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(intervals.len());
    for (start, end) in intervals {
        match merged.last_mut() {
            Some(last) if last.1 >= end => {}
            // Intervals overlap (or touch); extend the current one
            Some(last) if last.1 >= start => last.1 = end,
            _ => merged.push((start, end)),
        }
    }
    merged
}

/// Text after filtering, plus the byte ranges of the input that were
/// filtered out (Meld dims these in the view).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilteredText {
    pub text: String,
    pub ranges: Vec<(usize, usize)>,
}

/// Apply text filters to `txt` (`apply_text_filters`).
///
/// For regexes without capture groups the whole match is removed; with
/// groups, every participating non-empty group is removed instead. Empty
/// matches are ignored. Ranges are byte offsets into `txt` (Python used
/// code point offsets). Fails if a removed range contains a newline.
pub fn apply_text_filters(txt: &str, regexes: &[Regex]) -> Result<FilteredText, DiffError> {
    let mut ranges = Vec::new();
    for r in regexes {
        let groups = r.captures_len() - 1;
        if groups == 0 {
            for m in r.find_iter(txt) {
                if m.start() != m.end() {
                    ranges.push((m.start(), m.end()));
                }
            }
            continue;
        }
        for caps in r.captures_iter(txt) {
            for g in caps.iter().skip(1).flatten() {
                if g.start() != g.end() {
                    ranges.push((g.start(), g.end()));
                }
            }
        }
    }
    let ranges = merge_intervals(ranges);

    let mut text = String::with_capacity(txt.len());
    let mut offset = 0;
    for &(start, end) in &ranges {
        if txt[start..end].contains('\n') {
            return Err(DiffError::FilterChangedLineCount);
        }
        text.push_str(&txt[offset..start]);
        offset = end;
    }
    text.push_str(&txt[offset..]);
    Ok(FilteredText { text, ranges })
}

/// Filter one buffer line the way `FileDiff._filter_text` does: on failure
/// the line is compared unfiltered.
pub fn filter_line(line: &str, regexes: &[Regex]) -> String {
    if regexes.is_empty() {
        return line.to_owned();
    }
    apply_text_filters(line, regexes).map_or_else(|_| line.to_owned(), |f| f.text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn re(s: &str) -> Regex {
        Regex::new(s).unwrap()
    }

    #[test]
    fn merge_intervals_cases() {
        // Cases from test/test_misc.py
        type Intervals = Vec<(usize, usize)>;
        let cases: Vec<(Intervals, Intervals)> = vec![
            // Dominated by a single range
            (vec![(1, 5), (5, 9), (10, 11), (0, 20)], vec![(0, 20)]),
            // No overlap
            (
                vec![(1, 5), (6, 9), (10, 11)],
                vec![(1, 5), (6, 9), (10, 11)],
            ),
            // Two overlap points between ranges
            (
                vec![(1, 5), (5, 9), (10, 12), (11, 20)],
                vec![(1, 9), (10, 20)],
            ),
            // Two overlap points between ranges, out of order
            (
                vec![(5, 9), (1, 5), (11, 20), (10, 12)],
                vec![(1, 9), (10, 20)],
            ),
            // Two equal ranges
            (vec![(1, 5), (7, 8), (1, 5)], vec![(1, 5), (7, 8)]),
            // Three ranges overlap
            (vec![(1, 5), (4, 10), (9, 15)], vec![(1, 15)]),
            // Fewer than two intervals are returned as they are
            (vec![], vec![]),
            (vec![(3, 4)], vec![(3, 4)]),
        ];
        for (input, expected) in cases {
            assert_eq!(merge_intervals(input.clone()), expected, "{input:?}");
        }
    }

    #[test]
    fn filters_whole_matches_and_groups() {
        let f = apply_text_filters("int x = 1; // note", &[re(r"//.*")]).unwrap();
        assert_eq!(f.text, "int x = 1; ");
        assert_eq!(f.ranges, vec![(11, 18)]);

        // Meld's default "CVS/SVN Keywords" filter only removes the group
        let f = apply_text_filters("x $Id: foo 123 $ y", &[re(r"\$\w+(:[^\n$]+)?\$")]).unwrap();
        assert_eq!(f.text, "x $Id$ y");
        assert_eq!(f.ranges, vec![(5, 15)]);
        // ...and nothing when the group does not participate
        let f = apply_text_filters("$Id$", &[re(r"\$\w+(:[^\n$]+)?\$")]).unwrap();
        assert_eq!(f.text, "$Id$");

        // Overlapping filters merge; empty matches are ignored
        let f = apply_text_filters("aaabbb", &[re("a+b"), re("ab+"), re("x*")]).unwrap();
        assert_eq!(f.text, "");
        assert_eq!(f.ranges, vec![(0, 6)]);

        // Whitespace filter, as in Meld's defaults
        assert_eq!(filter_line("  a  b ", &[re(r"[ \t\r\f\v]*")]), "ab");

        assert_eq!(
            apply_text_filters("a\nb", &[re(r"a\nb")]),
            Err(DiffError::FilterChangedLineCount)
        );
        assert_eq!(filter_line("a\nb", &[re(r"a\nb")]), "a\nb");
    }
}
