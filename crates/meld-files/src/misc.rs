//! GUI-free helpers from `meld/misc.py`
//!
//! `copy2()` and `copytree()` live in [`crate::fileops`].

use thiserror::Error;

use crate::filters::{ByteRegex, TextRegex};

/// Return true if all elements are equal (true for empty input)
pub fn all_same<T: PartialEq>(items: &[T]) -> bool {
    items.windows(2).all(|w| w[0] == w[1])
}

/// Path syntax used by [`shorten_names_with_style`]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathStyle {
    /// `/`-separated paths
    Posix,
    /// `\`- or `/`-separated paths with optional drive letters
    Windows,
}

impl PathStyle {
    /// The style of the platform we're running on
    pub fn native() -> PathStyle {
        if cfg!(windows) {
            PathStyle::Windows
        } else {
            PathStyle::Posix
        }
    }
}

/// A lexical path split like Python's `PurePath`: an anchor (root or
/// drive) plus components
#[derive(Clone, Debug)]
struct PureParts {
    anchor: String,
    parts: Vec<String>,
    style: PathStyle,
}

impl PureParts {
    fn parse(path: &str, style: PathStyle) -> PureParts {
        let (anchor, rest) = match style {
            PathStyle::Posix => {
                if let Some(rest) = path.strip_prefix("//").filter(|r| !r.starts_with('/')) {
                    // POSIX allows a distinct "//" root, as does PurePosixPath
                    ("//".to_string(), rest)
                } else if path.starts_with('/') {
                    ("/".to_string(), path.trim_start_matches('/'))
                } else {
                    (String::new(), path)
                }
            }
            PathStyle::Windows => {
                let bytes = path.as_bytes();
                let mut anchor = String::new();
                let mut rest = path;
                if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
                    anchor.push_str(&path[..2]);
                    rest = &path[2..];
                }
                if rest.starts_with(['\\', '/']) {
                    anchor.push('\\');
                    rest = rest.trim_start_matches(['\\', '/']);
                }
                (anchor, rest)
            }
        };
        let seps: &[char] = match style {
            PathStyle::Posix => &['/'],
            PathStyle::Windows => &['/', '\\'],
        };
        let parts = rest
            .split(seps)
            .filter(|p| !p.is_empty() && *p != ".")
            .map(str::to_string)
            .collect();
        PureParts {
            anchor,
            parts,
            style,
        }
    }

    fn key(&self) -> (String, Vec<String>) {
        match self.style {
            PathStyle::Posix => (self.anchor.clone(), self.parts.clone()),
            PathStyle::Windows => (
                self.anchor.to_lowercase(),
                self.parts.iter().map(|p| p.to_lowercase()).collect(),
            ),
        }
    }

    /// Python's `PurePath.parents`, nearest first
    fn parents(&self) -> Vec<PureParts> {
        (0..self.parts.len())
            .rev()
            .map(|n| PureParts {
                anchor: self.anchor.clone(),
                parts: self.parts[..n].to_vec(),
                style: self.style,
            })
            .collect()
    }

    fn name(&self) -> &str {
        self.parts.last().map(String::as_str).unwrap_or("")
    }
}

/// Remove common parts of a list of paths
///
/// For example, `["/tmp/foo1", "/tmp/foo2"]` is summarised as
/// `["foo1", "foo2"]`. Paths that share a basename are distinguished by
/// prepending an indicator, e.g., `["/a/b/c", "/a/d/c"]` gives
/// `["[b] c", "[d] c"]`.
pub fn shorten_names<S: AsRef<str>>(names: &[S]) -> Vec<String> {
    shorten_names_with_style(names, PathStyle::native())
}

/// [`shorten_names`] for a given path syntax
pub fn shorten_names_with_style<S: AsRef<str>>(names: &[S], style: PathStyle) -> Vec<String> {
    let originals: Vec<String> = names.iter().map(|n| n.as_ref().to_string()).collect();
    if originals.is_empty() {
        return originals;
    }
    let paths: Vec<PureParts> = originals
        .iter()
        .map(|n| PureParts::parse(n, style))
        .collect();

    // Identify the longest common parent among the paths
    let mut common: Vec<PureParts> = paths[0].parents();
    for path in &paths[1..] {
        let keys: Vec<_> = path.parents().iter().map(PureParts::key).collect();
        common.retain(|p| keys.contains(&p.key()));
    }
    let Some(common_parent) = common.into_iter().max_by_key(|p| p.parts.len()) else {
        return originals;
    };

    let n = common_parent.parts.len();
    let relative: Vec<Vec<String>> = paths.iter().map(|p| p.parts[n..].to_vec()).collect();
    let basenames: Vec<String> = paths
        .iter()
        .zip(&relative)
        .map(|(p, rel)| {
            if rel.is_empty() {
                String::new()
            } else {
                p.name().to_string()
            }
        })
        .collect();

    if all_same(&basenames) {
        return relative
            .iter()
            .zip(&basenames)
            .map(|(rel, name)| {
                if rel.len() > 1 && !rel[0].is_empty() {
                    format!("[{}] {}", rel[0], name)
                } else {
                    name.clone()
                }
            })
            .collect();
    }

    basenames
        .into_iter()
        .map(|name| {
            if name.is_empty() {
                "[None]".to_string()
            } else {
                name
            }
        })
        .collect()
}

/// Try to guess whether we're being X11 forwarded
pub fn guess_if_remote_x11() -> bool {
    let set = |var: &str| std::env::var_os(var).is_some_and(|v| !v.is_empty());
    set("DISPLAY") && set("SSH_CONNECTION")
}

/// Merge a list of half-open intervals
///
/// Returns the intervals sorted, with all overlapping (or touching)
/// intervals merged.
pub fn merge_intervals(intervals: &[(usize, usize)]) -> Vec<(usize, usize)> {
    if intervals.len() < 2 {
        return intervals.to_vec();
    }
    let mut sorted = intervals.to_vec();
    sorted.sort();
    let mut merged = vec![sorted[0]];
    for &(start, end) in &sorted[1..] {
        let last = merged.last_mut().expect("merged is never empty");
        if last.1 >= end {
            continue;
        }
        if last.1 < start {
            merged.push((start, end));
        } else {
            last.1 = end;
        }
    }
    merged
}

/// A text filter matched across a line break
///
/// Meld's filters work within lines; Python raises an `AssertionError`
/// in this case, which callers treat as "filters can't be applied".
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("text filter matched across lines at {start}..{end}")]
pub struct MultilineFilterError {
    /// Start byte offset of the offending range
    pub start: usize,
    /// End byte offset of the offending range
    pub end: usize,
}

/// The merged byte ranges of `text` removed by the filters
pub fn text_filter_ranges<'a>(
    text: &str,
    regexes: impl IntoIterator<Item = &'a TextRegex>,
) -> Vec<(usize, usize)> {
    let spans: Vec<(usize, usize)> = regexes.into_iter().flat_map(|r| r.spans(text)).collect();
    merge_intervals(&spans)
}

/// Remove the text matched by the filters; `apply_text_filters()`
///
/// See [`TextRegex::spans`] for which parts of a match are removed.
pub fn apply_text_filters<'a>(
    text: &str,
    regexes: impl IntoIterator<Item = &'a TextRegex>,
) -> Result<String, MultilineFilterError> {
    let ranges = text_filter_ranges(text, regexes);
    let mut result = String::with_capacity(text.len());
    let mut offset = 0;
    for (start, end) in ranges {
        if text[start..end].contains('\n') {
            return Err(MultilineFilterError { start, end });
        }
        result.push_str(&text[offset..start]);
        offset = end;
    }
    result.push_str(&text[offset..]);
    Ok(result)
}

/// [`apply_text_filters`] for raw bytes
pub fn apply_byte_filters<'a>(
    data: &[u8],
    regexes: impl IntoIterator<Item = &'a ByteRegex>,
) -> Result<Vec<u8>, MultilineFilterError> {
    let spans: Vec<(usize, usize)> = regexes.into_iter().flat_map(|r| r.spans(data)).collect();
    let mut result = Vec::with_capacity(data.len());
    let mut offset = 0;
    for (start, end) in merge_intervals(&spans) {
        if data[start..end].contains(&b'\n') {
            return Err(MultilineFilterError { start, end });
        }
        result.extend_from_slice(&data[offset..start]);
        offset = end;
    }
    result.extend_from_slice(&data[offset..]);
    Ok(result)
}

/// Calculate a cross-pane scrolling synchronisation point
///
/// Takes the values of a `GtkAdjustment`: `value` is clamped to
/// `[lower, upper - page_size]` as GTK does. The normal syncpoint is
/// the middle of the screen; within the first half screen of a document
/// it scales linearly back to 0.0 (top), and in the last half screen
/// to 1.0 (bottom), so that the ends of documents with different
/// lengths scroll into view.
pub fn calc_syncpoint(value: f64, page_size: f64, lower: f64, upper: f64) -> f64 {
    let current = value.min(upper - page_size).max(lower);
    let half_a_screen = page_size / 2.0;

    let mut syncpoint = 0.0;
    if half_a_screen == 0.0 {
        return syncpoint;
    }

    let first_scale = (current - lower) / half_a_screen;
    syncpoint += 0.5 * first_scale.min(1.0);
    let bottom_val = upper - 1.5 * page_size;
    let last_scale = (current - bottom_val) / half_a_screen;
    syncpoint += 0.5 * last_scale.max(0.0);
    syncpoint
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_intervals_cases() {
        type Intervals = &'static [(usize, usize)];
        let cases: &[(Intervals, Intervals)] = &[
            (&[(1, 5), (5, 9), (10, 11), (0, 20)], &[(0, 20)]),
            (&[(1, 5), (6, 9), (10, 11)], &[(1, 5), (6, 9), (10, 11)]),
            (&[(1, 5), (5, 9), (10, 12), (11, 20)], &[(1, 9), (10, 20)]),
            (&[(5, 9), (1, 5), (11, 20), (10, 12)], &[(1, 9), (10, 20)]),
            (&[(1, 5), (7, 8), (1, 5)], &[(1, 5), (7, 8)]),
            (&[(1, 5), (4, 10), (9, 15)], &[(1, 15)]),
        ];
        for (intervals, expected) in cases {
            assert_eq!(merge_intervals(intervals), expected.to_vec());
        }
    }

    #[test]
    fn calc_syncpoint_cases() {
        let cases = [
            (0.0, 100.0, 0.0, 1000.0, 0.0),
            (0.0, 100.0, 100.0, 1000.0, 0.0),
            (25.0, 100.0, 0.0, 1000.0, 0.25),
            (50.0, 100.0, 0.0, 1000.0, 0.5),
            (25.0, 100.0, 25.0, 1000.0, 0.0),
            (50.0, 100.0, 25.0, 1000.0, 0.25),
            (500.0, 100.0, 0.0, 1000.0, 0.5),
            (850.0, 100.0, 0.0, 1000.0, 0.5),
            (875.0, 100.0, 0.0, 1000.0, 0.75),
            (900.0, 100.0, 0.0, 1000.0, 1.0),
            (1100.0, 100.0, 0.0, 1000.0, 1.0),
        ];
        for (value, page, lower, upper, expected) in cases {
            assert_eq!(
                calc_syncpoint(value, page, lower, upper),
                expected,
                "{value} {lower}"
            );
        }
    }

    #[test]
    fn all_same_cases() {
        let empty: [i32; 0] = [];
        assert!(all_same(&empty));
        let cases: &[(&[i32], bool)] = &[
            (&[0], true),
            (&[1], true),
            (&[0, 0], true),
            (&[0, 1], false),
            (&[1, 0], false),
            (&[1, 1], true),
            (&[0, 0, 0], true),
            (&[0, 0, 1], false),
            (&[0, 1, 0], false),
            (&[0, 1, 1], false),
            (&[1, 0, 0], false),
            (&[1, 0, 1], false),
            (&[1, 1, 0], false),
            (&[1, 1, 1], true),
        ];
        for (items, expected) in cases {
            assert_eq!(all_same(items), *expected, "{items:?}");
        }
    }

    #[test]
    fn shorten_names_cases() {
        use PathStyle::*;
        let cases: &[(PathStyle, &[&str], &[&str])] = &[
            (Posix, &["/tmp/foo1", "/tmp/foo2"], &["foo1", "foo2"]),
            (
                Posix,
                &["/tmp/foo1", "/tmp/foo2", "/tmp/foo3"],
                &["foo1", "foo2", "foo3"],
            ),
            (
                Posix,
                &["/tmp/bar/foo1", "/tmp/woo/foo2"],
                &["foo1", "foo2"],
            ),
            (
                Posix,
                &["/tmp/bar/foo1", "/tmp/woo/foo1"],
                &["[bar] foo1", "[woo] foo1"],
            ),
            (
                Posix,
                &["/tmp/bar/foo1", "/tmp/woo/foo1", "/tmp/ree/foo1"],
                &["[bar] foo1", "[woo] foo1", "[ree] foo1"],
            ),
            (
                Posix,
                &["/tmp/bar/deep/deep", "/tmp/bar/shallow"],
                &["deep", "shallow"],
            ),
            (
                Posix,
                &["/tmp/bar/deep/deep/foo1", "/tmp/bar/shallow/foo1"],
                &["[deep] foo1", "[shallow] foo1"],
            ),
            (
                Posix,
                &["/tmp/bar/subdir/subsub", "/tmp/bar/"],
                &["subsub", "bar"],
            ),
            (
                Windows,
                &["C:\\Users\\hmm\\bar", "C:\\Users\\hmm\\foo"],
                &["bar", "foo"],
            ),
            (
                Windows,
                &["C:\\Users\\bar\\hmm", "C:\\Users\\foo\\hmm"],
                &["[bar] hmm", "[foo] hmm"],
            ),
            (Posix, &["nothing in", "common"], &["nothing in", "common"]),
            (
                Posix,
                &["<unnamed>", "/tmp/real/path"],
                &["<unnamed>", "/tmp/real/path"],
            ),
        ];
        for (style, paths, expected) in cases {
            assert_eq!(
                shorten_names_with_style(paths, *style),
                expected.to_vec(),
                "{paths:?}"
            );
        }
    }

    #[test]
    fn text_filters_apply() {
        let comment = TextRegex::new("#.*").unwrap();
        let ws = TextRegex::new(r"[ \t]+$").unwrap();
        assert_eq!(
            apply_text_filters("a # c  \nb  ", [&comment, &ws]).unwrap(),
            "a \nb"
        );
        assert_eq!(text_filter_ranges("a # c", [&comment]), vec![(2, 5)]);
        let all = TextRegex::new(r"\s+").unwrap();
        assert!(apply_text_filters("a\n b", [&all]).is_err());

        let bytes = ByteRegex::new("#.*").unwrap();
        assert_eq!(apply_byte_filters(b"x#y\nz", [&bytes]).unwrap(), b"x\nz");
    }
}
