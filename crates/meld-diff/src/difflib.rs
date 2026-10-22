//! Python's `difflib.SequenceMatcher` and `difflib.unified_diff`.
//!
//! Patches must be byte-identical to what Meld's patch dialog produced, and
//! that used difflib's own matcher (not Myers), including its "autojunk"
//! popularity heuristic, so it is ported here exactly.

use std::collections::HashMap;
use std::hash::Hash;

use crate::chunk::{Chunk, MatchingBlock, Tag, opcodes_from_blocks};

/// `difflib.SequenceMatcher(None, a, b)` with `autojunk=True`.
#[derive(Clone, Debug)]
pub struct SequenceMatcher<'s, T> {
    a: &'s [T],
    b: &'s [T],
    b2j: HashMap<&'s T, Vec<usize>>,
}

impl<'s, T: Eq + Hash> SequenceMatcher<'s, T> {
    pub fn new(a: &'s [T], b: &'s [T]) -> Self {
        let mut b2j: HashMap<&T, Vec<usize>> = HashMap::new();
        for (i, elt) in b.iter().enumerate() {
            b2j.entry(elt).or_default().push(i);
        }
        // Purge popular elements (the autojunk heuristic). There is no
        // `isjunk`, so popular elements are dropped from b2j but never treated
        // as junk when extending matches.
        let n = b.len();
        if n >= 200 {
            let ntest = n / 100 + 1;
            b2j.retain(|_, idxs| idxs.len() <= ntest);
        }
        SequenceMatcher { a, b, b2j }
    }

    /// `find_longest_match(alo, ahi, blo, bhi)`.
    pub fn find_longest_match(
        &self,
        alo: usize,
        ahi: usize,
        blo: usize,
        bhi: usize,
    ) -> MatchingBlock {
        let (a, b) = (self.a, self.b);
        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0);
        let mut j2len: HashMap<usize, usize> = HashMap::new();
        for (i, elt) in a.iter().enumerate().take(ahi).skip(alo) {
            let mut newj2len = HashMap::new();
            if let Some(indices) = self.b2j.get(elt) {
                for &j in indices {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let k = j
                        .checked_sub(1)
                        .and_then(|p| j2len.get(&p))
                        .copied()
                        .unwrap_or(0)
                        + 1;
                    newj2len.insert(j, k);
                    if k > bestsize {
                        besti = i + 1 - k;
                        bestj = j + 1 - k;
                        bestsize = k;
                    }
                }
            }
            j2len = newj2len;
        }
        while besti > alo && bestj > blo && a[besti - 1] == b[bestj - 1] {
            besti -= 1;
            bestj -= 1;
            bestsize += 1;
        }
        while besti + bestsize < ahi
            && bestj + bestsize < bhi
            && a[besti + bestsize] == b[bestj + bestsize]
        {
            bestsize += 1;
        }
        MatchingBlock::new(besti, bestj, bestsize)
    }

    /// `get_matching_blocks()`, terminated by the `(len(a), len(b), 0)` sentinel.
    pub fn matching_blocks(&self) -> Vec<MatchingBlock> {
        let (la, lb) = (self.a.len(), self.b.len());
        let mut queue = vec![(0, la, 0, lb)];
        let mut blocks = Vec::new();
        while let Some((alo, ahi, blo, bhi)) = queue.pop() {
            let x = self.find_longest_match(alo, ahi, blo, bhi);
            let (i, j, k) = (x.a, x.b, x.size);
            if k > 0 {
                blocks.push(x);
                if alo < i && blo < j {
                    queue.push((alo, i, blo, j));
                }
                if i + k < ahi && j + k < bhi {
                    queue.push((i + k, ahi, j + k, bhi));
                }
            }
        }
        blocks.sort();
        let (mut i1, mut j1, mut k1) = (0, 0, 0);
        let mut non_adjacent = Vec::new();
        for m in blocks {
            if i1 + k1 == m.a && j1 + k1 == m.b {
                k1 += m.size;
            } else {
                if k1 > 0 {
                    non_adjacent.push(MatchingBlock::new(i1, j1, k1));
                }
                (i1, j1, k1) = (m.a, m.b, m.size);
            }
        }
        if k1 > 0 {
            non_adjacent.push(MatchingBlock::new(i1, j1, k1));
        }
        non_adjacent.push(MatchingBlock::new(la, lb, 0));
        non_adjacent
    }

    /// `get_opcodes()`.
    pub fn opcodes(&self) -> Vec<Chunk> {
        opcodes_from_blocks(&self.matching_blocks())
    }

    /// `get_grouped_opcodes(n)`: change clusters with up to `n` lines of context.
    pub fn grouped_opcodes(&self, n: usize) -> Vec<Vec<Chunk>> {
        let mut codes = self.opcodes();
        if codes.is_empty() {
            codes.push(Chunk::new(Tag::Equal, 0, 1, 0, 1));
        }
        // Fixup leading and trailing groups if they show no changes.
        if let Some(first) = codes.first_mut().filter(|c| c.tag == Tag::Equal) {
            first.start_a = first.start_a.max(first.end_a.saturating_sub(n));
            first.start_b = first.start_b.max(first.end_b.saturating_sub(n));
        }
        if let Some(last) = codes.last_mut().filter(|c| c.tag == Tag::Equal) {
            last.end_a = last.end_a.min(last.start_a + n);
            last.end_b = last.end_b.min(last.start_b + n);
        }

        let nn = n + n;
        let mut groups = Vec::new();
        let mut group = Vec::new();
        for c in codes {
            let (mut i1, mut j1) = (c.start_a, c.start_b);
            // End the current group and start a new one whenever there is a
            // large range with no changes.
            if c.tag == Tag::Equal && c.end_a - c.start_a > nn {
                group.push(Chunk::new(
                    c.tag,
                    i1,
                    c.end_a.min(i1 + n),
                    j1,
                    c.end_b.min(j1 + n),
                ));
                groups.push(std::mem::take(&mut group));
                i1 = i1.max(c.end_a.saturating_sub(n));
                j1 = j1.max(c.end_b.saturating_sub(n));
            }
            group.push(Chunk::new(c.tag, i1, c.end_a, j1, c.end_b));
        }
        if !group.is_empty() && !(group.len() == 1 && group[0].tag == Tag::Equal) {
            groups.push(group);
        }
        groups
    }
}

/// `difflib._format_range_unified`.
fn format_range_unified(start: usize, stop: usize) -> String {
    let mut beginning = start + 1;
    let length = stop - start;
    if length == 1 {
        return beginning.to_string();
    }
    if length == 0 {
        beginning -= 1;
    }
    format!("{beginning},{length}")
}

/// Header and context options for [`unified_diff`] (difflib's keyword args).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnifiedDiffOptions<'s> {
    pub fromfile: &'s str,
    pub tofile: &'s str,
    pub fromfiledate: &'s str,
    pub tofiledate: &'s str,
    /// Lines of context (difflib default: 3).
    pub n: usize,
    /// Terminator of the `---`, `+++` and `@@` lines (default `"\n"`).
    pub lineterm: &'s str,
}

impl Default for UnifiedDiffOptions<'_> {
    fn default() -> Self {
        UnifiedDiffOptions {
            fromfile: "",
            tofile: "",
            fromfiledate: "",
            tofiledate: "",
            n: 3,
            lineterm: "\n",
        }
    }
}

/// `difflib.unified_diff(a, b, ...)` as the list of yielded strings.
///
/// Like difflib, content lines are emitted as given, so `a` and `b` should
/// keep their line terminators.
pub fn unified_diff_lines<S: AsRef<str> + Eq + Hash>(
    a: &[S],
    b: &[S],
    opts: &UnifiedDiffOptions<'_>,
) -> Vec<String> {
    let mut out = Vec::new();
    let matcher = SequenceMatcher::new(a, b);
    for (index, group) in matcher.grouped_opcodes(opts.n).iter().enumerate() {
        if index == 0 {
            let fromdate = if opts.fromfiledate.is_empty() {
                String::new()
            } else {
                format!("\t{}", opts.fromfiledate)
            };
            let todate = if opts.tofiledate.is_empty() {
                String::new()
            } else {
                format!("\t{}", opts.tofiledate)
            };
            out.push(format!(
                "--- {}{}{}",
                opts.fromfile, fromdate, opts.lineterm
            ));
            out.push(format!("+++ {}{}{}", opts.tofile, todate, opts.lineterm));
        }
        let (first, last) = (group[0], group[group.len() - 1]);
        let file1_range = format_range_unified(first.start_a, last.end_a);
        let file2_range = format_range_unified(first.start_b, last.end_b);
        out.push(format!(
            "@@ -{file1_range} +{file2_range} @@{}",
            opts.lineterm
        ));

        for c in group {
            let side_a = &a[c.start_a..c.end_a];
            let side_b = &b[c.start_b..c.end_b];
            if c.tag == Tag::Equal {
                out.extend(side_a.iter().map(|l| format!(" {}", l.as_ref())));
                continue;
            }
            if matches!(c.tag, Tag::Replace | Tag::Delete) {
                out.extend(side_a.iter().map(|l| format!("-{}", l.as_ref())));
            }
            if matches!(c.tag, Tag::Replace | Tag::Insert) {
                out.extend(side_b.iter().map(|l| format!("+{}", l.as_ref())));
            }
        }
    }
    out
}

/// `"".join(difflib.unified_diff(a, b, ...))`.
pub fn unified_diff<S: AsRef<str> + Eq + Hash>(
    a: &[S],
    b: &[S],
    opts: &UnifiedDiffOptions<'_>,
) -> String {
    unified_diff_lines(a, b, opts).concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn difflib_doc_examples() {
        let s = SequenceMatcher::new(b" abcd".as_slice(), b"abcd abcd".as_slice());
        assert_eq!(
            s.find_longest_match(0, 5, 0, 9),
            MatchingBlock::new(0, 4, 5)
        );
        let s = SequenceMatcher::new(b"abxcd".as_slice(), b"abcd".as_slice());
        assert_eq!(
            s.matching_blocks(),
            vec![
                MatchingBlock::new(0, 0, 2),
                MatchingBlock::new(3, 2, 2),
                MatchingBlock::new(5, 4, 0)
            ]
        );
    }

    #[test]
    fn grouped_opcodes_doc_example() {
        let a: Vec<String> = (1..40).map(|i| i.to_string()).collect();
        let mut b = a.clone();
        b.insert(8, "i".into());
        b[20].push('x');
        b.drain(23..28);
        b[30].push('y');
        let groups = SequenceMatcher::new(&a, &b).grouped_opcodes(3);
        let c = |t, a1, a2, b1, b2| Chunk::new(t, a1, a2, b1, b2);
        use Tag::*;
        assert_eq!(
            groups,
            vec![
                vec![
                    c(Equal, 5, 8, 5, 8),
                    c(Insert, 8, 8, 8, 9),
                    c(Equal, 8, 11, 9, 12)
                ],
                vec![
                    c(Equal, 16, 19, 17, 20),
                    c(Replace, 19, 20, 20, 21),
                    c(Equal, 20, 22, 21, 23),
                    c(Delete, 22, 27, 23, 23),
                    c(Equal, 27, 30, 23, 26)
                ],
                vec![
                    c(Equal, 31, 34, 27, 30),
                    c(Replace, 34, 35, 30, 31),
                    c(Equal, 35, 38, 31, 34)
                ],
            ]
        );
    }

    #[test]
    fn unified_diff_doc_example() {
        let a = ["one", "two", "three", "four"];
        let b = ["zero", "one", "tree", "four"];
        let opts = UnifiedDiffOptions {
            fromfile: "Original",
            tofile: "Current",
            fromfiledate: "2005-01-26 23:30:50",
            tofiledate: "2010-04-02 10:20:52",
            lineterm: "",
            ..Default::default()
        };
        assert_eq!(
            unified_diff_lines(&a, &b, &opts),
            vec![
                "--- Original\t2005-01-26 23:30:50",
                "+++ Current\t2010-04-02 10:20:52",
                "@@ -1,4 +1,4 @@",
                "+zero",
                " one",
                "-two",
                "-three",
                "+tree",
                " four",
            ]
        );
    }

    #[test]
    fn identical_inputs_give_empty_diff() {
        let a = ["x\n", "y\n"];
        assert_eq!(unified_diff(&a, &a, &UnifiedDiffOptions::default()), "");
        let e: [&str; 0] = [];
        assert_eq!(unified_diff(&e, &e, &UnifiedDiffOptions::default()), "");
        assert_eq!(
            unified_diff(&e, &["a\n"], &UnifiedDiffOptions::default()),
            "--- \n+++ \n@@ -0,0 +1 @@\n+a\n"
        );
    }
}
