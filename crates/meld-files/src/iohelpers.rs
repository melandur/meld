//! Path helpers from `meld/iohelpers.py`
//!
//! Paths are handled lexically, like `GFile`s created with
//! `g_file_new_for_path()`: trailing separators and `.` components are
//! dropped and `..` is resolved without touching the file system.
//! Trashing lives in [`crate::trash`].

use std::path::{Component, Path, PathBuf};

use thiserror::Error;

/// Errors from path formatting
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PathError {
    /// The path is a root and has no parent
    #[error("Path {0} has no parent")]
    NoParent(String),
}

/// Lexically canonicalise a path, as GIO does for local paths
pub fn canonicalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() && !out.has_root() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `g_file_get_relative_path()`: the path of `descendant` relative to
/// `parent`, or `None` if it isn't a strict descendant
pub fn relative_path(parent: &Path, descendant: &Path) -> Option<PathBuf> {
    let parent = canonicalize_lexically(parent);
    let descendant = canonicalize_lexically(descendant);
    match descendant.strip_prefix(&parent) {
        Ok(rel) if !rel.as_os_str().is_empty() => Some(rel.to_path_buf()),
        _ => None,
    }
}

fn parent_of(path: &Path) -> Option<PathBuf> {
    canonicalize_lexically(path).parent().map(Path::to_path_buf)
}

/// Find the closest common parent folder of the given paths
///
/// A single path gives its own parent; no paths, or any missing path,
/// gives `None`.
pub fn find_shared_parent_path<P: AsRef<Path>>(paths: &[Option<P>]) -> Option<PathBuf> {
    let first = paths.first()?.as_ref()?;
    if paths.iter().any(Option::is_none) {
        return None;
    }
    let mut current = parent_of(first.as_ref());
    if paths.len() == 1 {
        return current;
    }
    while let Some(parent) = current {
        let valid = paths
            .iter()
            .flatten()
            .all(|p| relative_path(&parent, p.as_ref()).is_some());
        if valid {
            return Some(parent);
        }
        current = parent.parent().map(Path::to_path_buf);
    }
    None
}

/// The user's home folder
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// Format a path relative to the home folder as `~/…` if possible
pub fn format_home_relative_path(path: &Path) -> String {
    format_home_relative_path_with(path, home_dir().as_deref())
}

/// [`format_home_relative_path`] with an explicit home folder
pub fn format_home_relative_path_with(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| relative_path(h, path)) {
        Some(rel) => Path::new("~").join(rel).to_string_lossy().into_owned(),
        None => canonicalize_lexically(path).to_string_lossy().into_owned(),
    }
}

fn basename(path: &Path) -> String {
    let path = canonicalize_lexically(path);
    match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => path.to_string_lossy().into_owned(),
    }
}

/// Format a shortened path for `descendant` using a common `parent`
///
/// The label keeps the first path segment below `parent`, the
/// immediate parent folder (plus the one above it, if the immediate
/// parent's name is short, like `src`) and the file name, eliding the
/// rest with `…`. For example, with a parent of `/home/hey`,
/// `/home/hey/project/package/subpackage/foo.txt` is formatted as
/// `…/project/…/subpackage/foo.txt`.
pub fn format_parent_relative_path(parent: &Path, descendant: &Path) -> Result<String, PathError> {
    format_parent_relative_path_with(parent, descendant, home_dir().as_deref())
}

/// [`format_parent_relative_path`] with an explicit home folder
pub fn format_parent_relative_path_with(
    parent: &Path,
    descendant: &Path,
    home: Option<&Path>,
) -> Result<String, PathError> {
    let parent = canonicalize_lexically(parent);
    let descendant = canonicalize_lexically(descendant);
    let descendant_parent = descendant
        .parent()
        .ok_or_else(|| PathError::NoParent(descendant.to_string_lossy().into_owned()))?;

    let mut base_path: Option<String> = None;
    let mut immediate_parents: Vec<String> = Vec::new();
    let mut has_elided_path = false;

    if let Some(rel) = relative_path(&parent, descendant_parent) {
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        // We always try to leave the first and last path segments
        base_path = parts.first().cloned();
        if parts.len() > 1 {
            immediate_parents.push(parts[parts.len() - 1].clone());
        }
        // Include the second-last segment too if the last is short,
        // to handle src, dist, pkg, etc.
        if parts.len() > 2 && immediate_parents[0].chars().count() < 5 {
            immediate_parents.insert(0, parts[parts.len() - 2].clone());
        }
        has_elided_path = parts.len() > immediate_parents.len() + 1;
    }

    let show_parent = parent.parent().is_none();
    // A single component looks odd, so use the parent's name if we
    // have no base path; it's just for appearances.
    if !show_parent && base_path.is_none() {
        base_path = Some(basename(&parent));
    }

    let mut segments: Vec<String> = Vec::new();
    if !show_parent {
        segments.push("…".to_string());
    }
    segments.extend(base_path);
    if has_elided_path {
        segments.push("…".to_string());
    }
    segments.extend(immediate_parents);
    segments.push(basename(&descendant));
    let segments: Vec<String> = segments.into_iter().filter(|s| !s.is_empty()).collect();

    let mut label = if show_parent {
        format_home_relative_path_with(&parent, home)
    } else {
        String::new()
    };
    label.push_str(&segments.join(std::path::MAIN_SEPARATOR_STR));
    Ok(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    // test/test_iohelpers.py::test_find_shared_parent_path
    #[test]
    fn shared_parent_path() {
        let cases: &[(&[Option<&str>], Option<&str>)] = &[
            (&[], None),
            (&[Some("/foo/a/b/c")], Some("/foo/a/b")),
            (&[Some("/foo/a"), Some("/foo/b")], Some("/foo")),
            (
                &[Some("/foo/a"), Some("/foo/b"), Some("/foo/c")],
                Some("/foo"),
            ),
            (&[Some("/foo/a/asd/asd"), Some("/foo/b")], Some("/foo")),
            (&[Some("/foo/a/"), Some("/foo/b/asd/asd")], Some("/foo")),
            (&[Some("/foo/a/"), Some("/bar/b/")], Some("/")),
            (&[Some("/foo/a"), None], None),
            (&[Some("/foo/a"), None, Some("/foo/c")], None),
        ];
        for (paths, expected) in cases {
            let parent = find_shared_parent_path(paths);
            assert_eq!(parent, expected.map(PathBuf::from), "{paths:?}");
        }
    }

    // test/test_iohelpers.py::test_format_home_relative_path
    #[test]
    fn home_relative_path() {
        let home = Path::new("/home/hey/");
        assert_eq!(
            format_home_relative_path_with(Path::new("/home/hey/foo"), Some(home)),
            "~/foo"
        );
        assert_eq!(
            format_home_relative_path_with(Path::new("/home/hmph/foo"), Some(home)),
            "/home/hmph/foo"
        );
    }

    // test/test_iohelpers.py::test_format_parent_relative_path
    #[test]
    fn parent_relative_path() {
        let cases = [
            ("/home/hey/", "/home/hey/foo.txt", "…/hey/foo.txt"),
            ("/", "/foo.txt", "/foo.txt"),
            (
                "/home/hey/",
                "/home/hey/project/foo.txt",
                "…/project/foo.txt",
            ),
            (
                "/home/hey/",
                "/home/hey/project/package/foo.txt",
                "…/project/package/foo.txt",
            ),
            (
                "/home/hey/",
                "/home/hey/project/package/subpackage/foo.txt",
                "…/project/…/subpackage/foo.txt",
            ),
            (
                "/home/hey/",
                "/home/hey/project/package/src/foo.txt",
                "…/project/package/src/foo.txt",
            ),
        ];
        for (parent, child, expected) in cases {
            let label = format_parent_relative_path_with(Path::new(parent), Path::new(child), None)
                .unwrap();
            assert_eq!(label, expected, "{child}");
        }
    }

    #[test]
    fn parent_relative_path_no_parent() {
        let err = format_parent_relative_path(Path::new("/"), Path::new("/")).unwrap_err();
        assert!(err.to_string().contains("has no parent"));
    }

    #[test]
    fn lexical_canonicalisation() {
        assert_eq!(
            canonicalize_lexically(Path::new("/a/./b/../c/")),
            PathBuf::from("/a/c")
        );
        assert_eq!(relative_path(Path::new("/a"), Path::new("/a")), None);
        assert_eq!(
            relative_path(Path::new("/a"), Path::new("/a/b/c")),
            Some(PathBuf::from("b/c"))
        );
        assert_eq!(relative_path(Path::new("/a"), Path::new("/ab")), None);
    }
}
