//! Lexical path helpers mirroring the `os.path` functions the Python
//! backends rely on (`abspath`, `normpath`, `relpath`, `splitext`,
//! `commonprefix`).

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// Lexically normalise a path like `os.path.normpath`: drops `.`
/// components, resolves `..` against preceding components and removes
/// trailing separators. Symlinks are not resolved.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out: Vec<Component<'_>> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                _ => out.push(comp),
            },
            _ => out.push(comp),
        }
    }
    if out.is_empty() {
        return PathBuf::from(".");
    }
    out.iter().collect()
}

/// `os.path.abspath(os.path.join(base, path))`.
pub fn absolutize(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize(path)
    } else {
        normalize(&base.join(path))
    }
}

/// `os.path.abspath(path)` relative to the process working directory.
pub fn abspath(path: &Path) -> PathBuf {
    match std::env::current_dir() {
        Ok(cwd) => absolutize(&cwd, path),
        Err(_) => normalize(path),
    }
}

/// The folders version control systems keep their own data in.
const ADMIN_DIRS: [&str; 5] = [".git", ".hg", ".bzr", ".svn", "_darcs"];

/// `path` moved out of a version control system's own folder: a `.git`
/// folder, or anything inside one, gives the work tree it belongs to (the
/// folder holding `.git`). Other paths are returned unchanged. Lexical.
pub fn outside_admin_dir(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        if let Component::Normal(name) = comp
            && ADMIN_DIRS.iter().any(|d| name == *d)
            && !out.as_os_str().is_empty()
        {
            return out;
        }
        out.push(comp);
    }
    path.to_path_buf()
}

/// `os.path.relpath(path, start)` for two absolute (or two relative)
/// paths. Both are normalised lexically first.
pub fn relpath(path: &Path, start: &Path) -> PathBuf {
    let path = abspath(path);
    let start = abspath(start);
    let pc: Vec<_> = path.components().collect();
    let sc: Vec<_> = start.components().collect();
    let common = pc.iter().zip(sc.iter()).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..sc.len() {
        out.push("..");
    }
    for comp in &pc[common..] {
        out.push(comp.as_os_str());
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

/// `os.path.splitext(path)[1]`: the extension including its dot, or an
/// empty string. Leading dots of the file name do not start an extension.
pub fn suffix(path: &Path) -> String {
    match path.extension() {
        Some(ext) => format!(".{}", ext.to_string_lossy()),
        None => String::new(),
    }
}

/// The parent directory of `path`, or `path` itself when it has none
/// (`os.path.dirname` semantics for the root).
pub fn parent_or_self(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        Some(_) => PathBuf::from("."),
        None => path.to_path_buf(),
    }
}

/// `os.path.commonprefix`: the longest common *string* prefix of the given
/// paths (not necessarily a path boundary).
pub fn common_string_prefix(paths: &[PathBuf]) -> String {
    let strings: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let Some(first) = strings.first() else {
        return String::new();
    };
    let mut end = first.len();
    for s in &strings[1..] {
        end = first
            .char_indices()
            .zip(s.chars())
            .take_while(|((_, a), b)| a == b)
            .map(|((i, a), _)| i + a.len_utf8())
            .last()
            .unwrap_or(0)
            .min(end);
    }
    first[..end].to_string()
}

/// `os.path.dirname(os.path.commonprefix(paths))`.
pub fn common_dir(paths: &[PathBuf]) -> PathBuf {
    let prefix = common_string_prefix(paths);
    match prefix.rfind('/') {
        Some(0) => PathBuf::from("/"),
        Some(i) => PathBuf::from(&prefix[..i]),
        None => PathBuf::new(),
    }
}

/// Find an executable on `PATH` (`shutil.which`).
pub fn which(program: &str) -> Option<PathBuf> {
    let path: OsString = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file() || path.with_extension("exe").is_file()
}

/// Lossy string form of a path, used for display and sorting.
pub fn lossy(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_dirs_map_to_work_tree() {
        assert_eq!(outside_admin_dir(Path::new("/r/.git")), PathBuf::from("/r"));
        assert_eq!(
            outside_admin_dir(Path::new("/r/.git/refs/heads")),
            PathBuf::from("/r")
        );
        assert_eq!(
            outside_admin_dir(Path::new("/r/sub/.hg")),
            PathBuf::from("/r/sub")
        );
        assert_eq!(
            outside_admin_dir(Path::new("/r/src")),
            PathBuf::from("/r/src")
        );
        assert_eq!(
            outside_admin_dir(Path::new("/r/.github")),
            PathBuf::from("/r/.github")
        );
    }

    #[test]
    fn normalize_like_normpath() {
        assert_eq!(normalize(Path::new("/a/./b/../c/")), PathBuf::from("/a/c"));
        assert_eq!(normalize(Path::new("./")), PathBuf::from("."));
        assert_eq!(normalize(Path::new("/../a")), PathBuf::from("/a"));
        assert_eq!(normalize(Path::new("../a")), PathBuf::from("../a"));
    }

    #[test]
    fn relpath_like_python() {
        assert_eq!(
            relpath(Path::new("/r/a/b.txt"), Path::new("/r")),
            PathBuf::from("a/b.txt")
        );
        assert_eq!(
            relpath(Path::new("/r/x"), Path::new("/r/a")),
            PathBuf::from("../x")
        );
        assert_eq!(
            relpath(Path::new("/r"), Path::new("/r")),
            PathBuf::from(".")
        );
    }

    #[test]
    fn suffix_like_splitext() {
        assert_eq!(suffix(Path::new("a/b.tar.gz")), ".gz");
        assert_eq!(suffix(Path::new(".bashrc")), "");
        assert_eq!(suffix(Path::new("Makefile")), "");
    }

    #[test]
    fn common_dir_like_python() {
        let paths = vec![PathBuf::from("/r/abc/x"), PathBuf::from("/r/abd/y")];
        assert_eq!(common_string_prefix(&paths), "/r/ab");
        assert_eq!(common_dir(&paths), PathBuf::from("/r"));
        assert_eq!(
            common_dir(&[PathBuf::from("/r/a.txt")]),
            PathBuf::from("/r")
        );
    }
}
