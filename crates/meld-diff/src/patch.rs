//! Patch generation as done by `meld/patchdialog.py:PatchDialog.update_patch`.

use crate::buffer::splitlines;
use crate::difflib::{UnifiedDiffOptions, unified_diff};
use crate::error::DiffError;

/// Which pair of panes a patch is made from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PatchSide {
    /// Panes 0 and 1 (the only choice in two-way comparisons).
    #[default]
    Left,
    /// Panes 1 and 2.
    Right,
}

/// Split buffer text into lines with terminators, guessing a terminator
/// for a final line that lacks one (from the previous line, else `"\n"`).
pub fn patch_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = splitlines(text, true)
        .into_iter()
        .map(str::to_owned)
        .collect();
    let barelines = splitlines(text, false);
    if let (Some(bare_last), Some(last)) = (barelines.last(), lines.last())
        && bare_last == last
    {
        // Final line lacks a line-break; add in a best guess
        let previous_linebreak = if lines.len() > 1 {
            let prev = &lines[lines.len() - 2];
            prev[barelines[barelines.len() - 2].len()..].to_owned()
        } else {
            "\n".to_owned()
        };
        if let Some(last) = lines.last_mut() {
            last.push_str(&previous_linebreak);
        }
    }
    lines
}

/// `os.path.commonprefix`: the longest common (character-wise) prefix.
fn common_prefix<'s>(names: &[&'s str]) -> &'s str {
    let Some(first) = names.first() else {
        return "";
    };
    let mut len = first.len();
    for name in &names[1..] {
        len = first
            .char_indices()
            .zip(name.chars())
            .take_while(|((_, a), b)| a == b)
            .last()
            .map_or(0, |((i, c), _)| i + c.len_utf8())
            .min(len);
    }
    &first[..len]
}

/// Display names for patch headers: each label with the directory part of
/// the labels' common prefix stripped.
pub fn patch_names<'s>(labels: &[&'s str]) -> Vec<&'s str> {
    let prefix = common_prefix(labels);
    let cut = prefix.rfind('/').map_or(0, |i| i + 1);
    labels.iter().map(|n| &n[cut.min(n.len())..]).collect()
}

/// The unified diff (3 lines of context) between two panes, exactly as
/// Meld's patch dialog shows it.
///
/// `texts` are the full buffer texts and `labels` the buffer labels of all
/// panes. Meld always passes three labels (an unused third pane is
/// labelled `"<unnamed>"`), which influences the common-prefix stripping.
/// With `reverse`, the patch goes from the right pane to the left one.
pub fn make_patch(
    texts: &[&str],
    labels: &[&str],
    side: PatchSide,
    reverse: bool,
) -> Result<String, DiffError> {
    let (mut from, mut to) = match side {
        PatchSide::Left => (0, 1),
        PatchSide::Right => (1, 2),
    };
    if reverse {
        std::mem::swap(&mut from, &mut to);
    }
    let names = patch_names(labels);
    let text = |i: usize| texts.get(i).copied().ok_or(DiffError::InvalidPane(i));
    let name = |i: usize| names.get(i).copied().ok_or(DiffError::InvalidPane(i));
    let a = patch_lines(text(from)?);
    let b = patch_lines(text(to)?);
    let opts = UnifiedDiffOptions {
        fromfile: name(from)?,
        tofile: name(to)?,
        ..Default::default()
    };
    Ok(unified_diff(&a, &b, &opts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_line_break_guess() {
        assert_eq!(patch_lines("a\r\nb"), ["a\r\n", "b\r\n"]);
        assert_eq!(patch_lines("a"), ["a\n"]);
        assert_eq!(patch_lines("a\n"), ["a\n"]);
        assert!(patch_lines("").is_empty());
    }

    #[test]
    fn names_strip_common_directory() {
        assert_eq!(
            patch_names(&[
                "/home/u/proj/a.txt",
                "/home/u/proj/b.txt",
                "/home/u/proj/c.txt"
            ]),
            ["a.txt", "b.txt", "c.txt"]
        );
        assert_eq!(
            patch_names(&["/x/file.txt", "/x/file.txt.orig"]),
            ["file.txt", "file.txt.orig"]
        );
        assert_eq!(
            patch_names(&["/x/a", "/x/b", "<unnamed>"]),
            ["/x/a", "/x/b", "<unnamed>"]
        );
    }

    #[test]
    fn patch() {
        let texts = ["one\ntwo\nthree", "one\n2\nthree\n", ""];
        let labels = ["/p/a", "/p/b", "/p/c"];
        assert_eq!(
            make_patch(&texts, &labels, PatchSide::Left, false).unwrap(),
            "--- a\n+++ b\n@@ -1,3 +1,3 @@\n one\n-two\n+2\n three\n"
        );
        assert_eq!(
            make_patch(&texts, &labels, PatchSide::Left, true).unwrap(),
            "--- b\n+++ a\n@@ -1,3 +1,3 @@\n one\n-2\n+two\n three\n"
        );
        assert_eq!(
            make_patch(&texts[..2], &labels, PatchSide::Right, false),
            Err(DiffError::InvalidPane(2))
        );
    }
}
