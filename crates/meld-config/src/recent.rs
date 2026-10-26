//! Recently used comparisons, replacing `meld/recent.py`.
//!
//! Meld stored each comparison as a `.meldcmp` key file registered with
//! `GtkRecentManager`. Here the list is a TOML file at
//! `$XDG_DATA_HOME/meld/recent.toml`, most recent first. `.meldcmp` files
//! can still be read, since Meld can be launched with one.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::ConfigError;
use crate::error::atomic_write;

/// Most comparisons kept; the GTK recent chooser Meld used showed 50.
pub const MAX_RECENT: usize = 50;

/// Kind of a comparison (`meld.const.RecentType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComparisonType {
    File,
    Folder,
    VersionControl,
    Merge,
    /// Git Compare; Meld's Python version doesn't record these.
    GitCompare,
}

impl ComparisonType {
    /// The value used in `.meldcmp` files.
    pub fn meldcmp_name(self) -> &'static str {
        match self {
            Self::File => "File",
            Self::Folder => "Folder",
            Self::VersionControl => "Version control",
            Self::Merge => "Merge",
            Self::GitCompare => "Git compare",
        }
    }

    pub fn from_meldcmp_name(name: &str) -> Option<ComparisonType> {
        [
            Self::File,
            Self::Folder,
            Self::VersionControl,
            Self::Merge,
            Self::GitCompare,
        ]
        .into_iter()
        .find(|t| t.meldcmp_name() == name)
    }
}

/// One recent comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentComparison {
    #[serde(rename = "type")]
    pub kind: ComparisonType,
    pub paths: Vec<PathBuf>,
    /// Extra options for re-opening the comparison (free-form, e.g. Git
    /// Compare revisions).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<String>,
    /// When the comparison was last used, in seconds since the Unix epoch.
    #[serde(default)]
    pub timestamp: u64,
}

impl RecentComparison {
    pub fn new(kind: ComparisonType, paths: Vec<PathBuf>) -> Self {
        RecentComparison {
            kind,
            paths,
            flags: Vec::new(),
            timestamp: now(),
        }
    }

    /// Label for the recent comparisons list, as `RecentFiles.add` makes
    /// it: shortened names joined by " : ", or "Version control: ~/path"
    /// for single-path comparisons.
    pub fn display_name(&self) -> String {
        let names: Vec<String> = self.paths.iter().map(|p| p.display().to_string()).collect();
        if names.len() > 1 {
            return shorten_names(&names).join(" : ");
        }
        let mut display = names.first().cloned().unwrap_or_default();
        if let Some(home) = dirs::home_dir() {
            let home = home.display().to_string();
            if !home.is_empty() && display.starts_with(&home) {
                display = format!("~{}", &display[home.len()..]);
            }
        }
        let label = match self.kind {
            ComparisonType::GitCompare => "Git Compare:",
            _ => "Version control:",
        };
        format!("{label} {display}")
    }

    /// Whether every compared path exists.
    pub fn exists(&self) -> bool {
        self.paths.iter().all(|p| p.exists())
    }

    fn same_comparison(&self, other: &RecentComparison) -> bool {
        self.kind == other.kind && self.paths == other.paths && self.flags == other.flags
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RecentFile {
    #[serde(default, rename = "comparison")]
    comparisons: Vec<RecentComparison>,
}

/// Path of the recent comparisons file, `$XDG_DATA_HOME/meld/recent.toml`.
pub fn recent_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("meld").join("recent.toml"))
}

/// The recent comparisons list (`RecentFiles`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecentComparisons {
    entries: Vec<RecentComparison>,
    path: Option<PathBuf>,
}

impl RecentComparisons {
    /// Load from [`recent_path`], dropping entries whose paths no longer
    /// exist (like `_clean_recent_files`).
    pub fn load() -> RecentComparisons {
        let mut recent = match recent_path() {
            Some(path) => Self::load_from(path),
            None => RecentComparisons::default(),
        };
        recent.prune_missing();
        recent
    }

    /// Load from a file. Missing files give an empty list; corrupt files
    /// are logged and give an empty list. Saving goes back to `path`.
    pub fn load_from(path: impl AsRef<Path>) -> RecentComparisons {
        let path = path.as_ref();
        let mut entries = match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<RecentFile>(&text) {
                Ok(file) => file.comparisons,
                Err(e) => {
                    log::warn!(
                        "Could not parse {}: {e}; ignoring recent comparisons",
                        path.display()
                    );
                    Vec::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                log::warn!("Could not read {}: {e}", path.display());
                Vec::new()
            }
        };
        entries.sort_by_key(|e| std::cmp::Reverse(e.timestamp));
        let mut recent = RecentComparisons {
            entries: Vec::new(),
            path: Some(path.to_path_buf()),
        };
        // Re-adding oldest first de-duplicates while keeping the order
        for entry in entries.into_iter().rev() {
            recent.push(entry);
        }
        recent
    }

    /// Save to the path this list was loaded from (or [`recent_path`]).
    pub fn save(&self) -> Result<(), ConfigError> {
        let path = self
            .path
            .clone()
            .or_else(recent_path)
            .ok_or(ConfigError::NoConfigDir)?;
        self.save_to(path)
    }

    /// Atomically write the list to `path`.
    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<(), ConfigError> {
        let file = RecentFile {
            comparisons: self.entries.clone(),
        };
        let text = toml::to_string_pretty(&file)?;
        atomic_write(path.as_ref(), text.as_bytes())
    }

    /// Entries, most recently used first.
    pub fn entries(&self) -> &[RecentComparison] {
        &self.entries
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Record a comparison as just used (`RecentFiles.add`).
    ///
    /// Comparisons without paths, with empty paths or including files on
    /// a tmpfs (temporary files) are not recorded; this returns false for
    /// them. Relative paths are made absolute. A comparison already in
    /// the list (same type, paths and flags) moves to the front. The list
    /// is capped at [`MAX_RECENT`] entries.
    pub fn add(&mut self, kind: ComparisonType, paths: &[PathBuf], flags: &[String]) -> bool {
        if paths.is_empty() || paths.iter().any(|p| p.as_os_str().is_empty()) {
            return false;
        }
        if paths.iter().any(|p| is_on_tmpfs(p)) {
            log::debug!("Not adding comparison because it includes tmpfs path");
            return false;
        }
        let paths: Vec<PathBuf> = paths
            .iter()
            .map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone()))
            .collect();
        let mut entry = RecentComparison::new(kind, paths);
        entry.flags = flags.to_vec();
        self.push(entry);
        true
    }

    /// Put an entry at the front, replacing a duplicate.
    fn push(&mut self, entry: RecentComparison) {
        self.entries.retain(|e| !e.same_comparison(&entry));
        self.entries.insert(0, entry);
        self.entries.truncate(MAX_RECENT);
    }

    /// Drop entries with paths that no longer exist; returns how many.
    pub fn prune_missing(&mut self) -> usize {
        let before = self.entries.len();
        self.entries.retain(RecentComparison::exists);
        before - self.entries.len()
    }
}

/// Whether a path is on a tmpfs, from `/proc/self/mounts`. Always false
/// where that isn't available.
pub fn is_on_tmpfs(path: &Path) -> bool {
    let Ok(mounts) = std::fs::read_to_string("/proc/self/mounts") else {
        return false;
    };
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let path = path.canonicalize().unwrap_or(path);
    mounts
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let _device = fields.next()?;
            let mount_point = unescape_mount(fields.next()?);
            let fs_type = fields.next()?;
            Some((PathBuf::from(mount_point), fs_type.to_string()))
        })
        .filter(|(mount_point, _)| path.starts_with(mount_point))
        .max_by_key(|(mount_point, _)| mount_point.components().count())
        .is_some_and(|(_, fs_type)| fs_type == "tmpfs")
}

/// Undo the octal escapes (`\040` for space) of `/proc/mounts`.
fn unescape_mount(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && let Some(digits) = field.get(i + 1..i + 4)
            && digits.bytes().all(|b| (b'0'..=b'7').contains(&b))
            && let Ok(v) = u8::from_str_radix(digits, 8)
        {
            out.push(v);
            i += 4;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Read a `.meldcmp` comparison file (`RecentFiles.read`).
///
/// The file is an INI-style key file with a `[Comparison]` section
/// holding `type` (a [`ComparisonType::meldcmp_name`]) and `uris`, a
/// `;`-separated list of URIs. Only `file://` URIs (or plain paths) are
/// supported.
pub fn read_meldcmp(path: impl AsRef<Path>) -> Result<RecentComparison, ConfigError> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).map_err(|e| ConfigError::io(path, e))?;
    parse_meldcmp(&text)
}

/// Parse the contents of a `.meldcmp` file; see [`read_meldcmp`].
pub fn parse_meldcmp(text: &str) -> Result<RecentComparison, ConfigError> {
    let invalid = |msg: &str| ConfigError::InvalidComparisonFile(msg.to_string());
    let section =
        parse_ini_section(text, "Comparison").ok_or_else(|| invalid("no [Comparison] section"))?;
    let get = |key: &str| {
        section
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    let kind = get("type").ok_or_else(|| invalid("no comparison type"))?;
    let uris = get("uris").ok_or_else(|| invalid("no comparison URIs"))?;
    let kind = ComparisonType::from_meldcmp_name(kind)
        .ok_or_else(|| invalid(&format!("unknown comparison type '{kind}'")))?;
    let paths = uris
        .split(';')
        .map(|uri| {
            uri_to_path(uri.trim()).ok_or_else(|| invalid(&format!("unsupported URI '{uri}'")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RecentComparison {
        kind,
        paths,
        flags: Vec::new(),
        timestamp: 0,
    })
}

/// Keys (lowercased) and values of one section of an INI file, parsed
/// like Python's `RawConfigParser`: `key = value` or `key: value`, `#`
/// and `;` comment lines, and indented continuation lines.
fn parse_ini_section(text: &str, wanted: &str) -> Option<Vec<(String, String)>> {
    let mut found = false;
    let mut in_section = false;
    let mut entries: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if line.starts_with([' ', '\t']) {
            if in_section && let Some((_, value)) = entries.last_mut() {
                value.push('\n');
                value.push_str(trimmed);
            }
            continue;
        }
        if let Some(name) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_section = name == wanted;
            found |= in_section;
            continue;
        }
        if !in_section {
            continue;
        }
        let split = trimmed.find(['=', ':'])?;
        let key = trimmed[..split].trim().to_lowercase();
        let value = trimmed[split + 1..].trim().to_string();
        entries.retain(|(k, _)| *k != key);
        entries.push((key, value));
    }
    found.then_some(entries)
}

/// Path of a `file://` URI (or a plain absolute path). Other schemes
/// give `None`.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = match uri.strip_prefix("file://") {
        Some(rest) => rest,
        None if uri.starts_with('/') => return Some(PathBuf::from(uri)),
        None => return None,
    };
    // Skip a "localhost" (or other) authority
    let rest = &rest[rest.find('/')?..];
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(digits) = rest.get(i + 1..i + 3)
            && digits.bytes().all(|b| b.is_ascii_hexdigit())
            && let Ok(v) = u8::from_str_radix(digits, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
    }
    #[cfg(not(unix))]
    {
        Some(PathBuf::from(String::from_utf8_lossy(&out).into_owned()))
    }
}

/// Remove common parts of a list of paths (`meld.misc.shorten_names`).
///
/// `('/tmp/foo1', '/tmp/foo2')` becomes `('foo1', 'foo2')`; paths that
/// share a basename get their first differing directory prepended, so
/// `('/a/b/c', '/a/d/c')` becomes `['[b] c', '[d] c']`.
fn shorten_names(names: &[String]) -> Vec<String> {
    let paths: Vec<&Path> = names.iter().map(Path::new).collect();
    let parents =
        |p: &Path| -> Vec<PathBuf> { p.ancestors().skip(1).map(Path::to_path_buf).collect() };
    let Some(first) = paths.first() else {
        return Vec::new();
    };
    let common = parents(first)
        .into_iter()
        .filter(|c| paths.iter().all(|p| parents(p).contains(c)))
        .max_by_key(|c| c.components().count());
    let Some(common) = common else {
        return names.to_vec();
    };
    let relative: Vec<PathBuf> = paths
        .iter()
        .map(|p| {
            p.strip_prefix(&common)
                .map(Path::to_path_buf)
                .unwrap_or_else(|_| p.to_path_buf())
        })
        .collect();
    let basenames: Vec<String> = relative
        .iter()
        .map(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .collect();
    if basenames.windows(2).all(|w| w[0] == w[1]) {
        return relative
            .iter()
            .zip(&basenames)
            .map(|(p, name)| {
                let mut components = p.components();
                let first = components.next();
                match (first, p.components().count() > 1) {
                    (Some(first), true) => {
                        format!("[{}] {name}", first.as_os_str().to_string_lossy())
                    }
                    _ => name.clone(),
                }
            })
            .collect();
    }
    basenames
        .into_iter()
        .map(|n| {
            if n.is_empty() {
                "[None]".to_string()
            } else {
                n
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Temporary directory off tmpfs (/tmp often is one), since
    /// comparisons of tmpfs files are not recorded.
    fn tempdir() -> tempfile::TempDir {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        std::fs::create_dir_all(&target).unwrap();
        let dir = tempfile::tempdir_in(&target).unwrap();
        assert!(!is_on_tmpfs(dir.path()), "test directory is on a tmpfs");
        dir
    }

    fn touch(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, "").unwrap();
        p
    }

    #[test]
    fn meldcmp_parsing() {
        let parsed = parse_meldcmp(
            "# saved\n[Other]\ntype = Folder\n[Comparison]\nTYPE: File\nuris = file:///a/b;file://localhost/c%3Bd\n",
        )
        .unwrap();
        assert_eq!(parsed.kind, ComparisonType::File);
        assert_eq!(parsed.paths, [PathBuf::from("/a/b"), PathBuf::from("/c;d")]);

        for bad in [
            "",
            "[Comparison]\ntype = File\n",
            "[Comparison]\nuris = file:///a\n",
            "[Comparison]\ntype = Bogus\nuris = file:///a\n",
            "[Comparison]\ntype = File\nuris = sftp://host/a\n",
            "[Other]\ntype = File\nuris = file:///a\n",
            "[Comparison]\nnot a key value line\n",
        ] {
            assert!(parse_meldcmp(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn read_meldcmp_file() {
        let dir = tempdir();
        let path = dir.path().join("recent-x.meldcmp");
        std::fs::write(
            &path,
            "[Comparison]\ntype = Merge\nuris = file:///a;file:///b;file:///c\n",
        )
        .unwrap();
        let c = read_meldcmp(&path).unwrap();
        assert_eq!(c.kind, ComparisonType::Merge);
        assert_eq!(c.paths.len(), 3);
        assert!(read_meldcmp(dir.path().join("missing.meldcmp")).is_err());
    }

    #[test]
    fn uris() {
        assert_eq!(
            uri_to_path("file:///tmp/a%20b"),
            Some(PathBuf::from("/tmp/a b"))
        );
        assert_eq!(
            uri_to_path("/plain/path"),
            Some(PathBuf::from("/plain/path"))
        );
        assert_eq!(uri_to_path("http://example.com/x"), None);
        assert_eq!(
            uri_to_path("file:///bad%zz"),
            Some(PathBuf::from("/bad%zz"))
        );
        assert_eq!(
            uri_to_path("file:///trailing%2"),
            Some(PathBuf::from("/trailing%2"))
        );
    }

    #[test]
    fn add_dedupes_caps_and_round_trips() {
        let dir = tempdir();
        let a = touch(dir.path(), "a");
        let b = touch(dir.path(), "b");
        let store = dir.path().join("data").join("recent.toml");

        let mut recent = RecentComparisons::load_from(&store);
        assert!(recent.is_empty());
        assert!(!recent.add(ComparisonType::File, &[], &[]));
        assert!(!recent.add(ComparisonType::File, &[PathBuf::new()], &[]));

        assert!(recent.add(ComparisonType::File, &[a.clone(), b.clone()], &[]));
        assert!(recent.add(
            ComparisonType::Folder,
            &[dir.path().to_path_buf(), dir.path().to_path_buf()],
            &[]
        ));
        assert!(recent.add(ComparisonType::File, &[a.clone(), b.clone()], &[]));
        assert_eq!(recent.len(), 2);
        assert_eq!(recent.entries()[0].kind, ComparisonType::File);
        // Different flags make a different comparison
        assert!(recent.add(
            ComparisonType::GitCompare,
            std::slice::from_ref(&a),
            &["base=HEAD~1".into()]
        ));
        assert!(recent.add(ComparisonType::GitCompare, std::slice::from_ref(&a), &[]));
        assert_eq!(recent.len(), 4);

        recent.save().unwrap();
        let text = std::fs::read_to_string(&store).unwrap();
        assert!(text.contains("[[comparison]]"));
        assert!(text.contains("type = \"git-compare\""));
        let loaded = RecentComparisons::load_from(&store);
        assert_eq!(loaded.entries(), recent.entries());

        for i in 0..(MAX_RECENT + 5) {
            let p = touch(dir.path(), &format!("f{i}"));
            recent.add(ComparisonType::File, &[p.clone(), p], &[]);
        }
        assert_eq!(recent.len(), MAX_RECENT);
        assert_eq!(
            recent.entries()[0].paths[0],
            dir.path().join(format!("f{}", MAX_RECENT + 4))
        );
    }

    #[test]
    fn prune_missing() {
        let dir = tempdir();
        let a = touch(dir.path(), "a");
        let b = touch(dir.path(), "b");
        let mut recent = RecentComparisons::default();
        recent.add(ComparisonType::File, &[a.clone(), b.clone()], &[]);
        recent.add(ComparisonType::File, &[a.clone(), a.clone()], &[]);
        std::fs::remove_file(&b).unwrap();
        assert_eq!(recent.prune_missing(), 1);
        assert_eq!(recent.entries()[0].paths, [a.clone(), a]);
    }

    #[test]
    fn corrupt_and_duplicate_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("recent.toml");
        std::fs::write(&store, "[[comparison]]\ntype = 3\n").unwrap();
        assert!(RecentComparisons::load_from(&store).is_empty());

        std::fs::write(
            &store,
            "[[comparison]]\ntype = 'file'\npaths = ['/x', '/y']\ntimestamp = 5\n\
             [[comparison]]\ntype = 'file'\npaths = ['/x', '/y']\ntimestamp = 9\n\
             [[comparison]]\ntype = 'folder'\npaths = ['/x', '/y']\ntimestamp = 7\n",
        )
        .unwrap();
        let recent = RecentComparisons::load_from(&store);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent.entries()[0].timestamp, 9);
        assert_eq!(recent.entries()[1].kind, ComparisonType::Folder);
    }

    #[test]
    fn display_names() {
        let c = RecentComparison::new(
            ComparisonType::File,
            vec![PathBuf::from("/tmp/foo1"), PathBuf::from("/tmp/foo2")],
        );
        assert_eq!(c.display_name(), "foo1 : foo2");
        let c = RecentComparison::new(
            ComparisonType::File,
            vec![PathBuf::from("/a/b/c"), PathBuf::from("/a/d/c")],
        );
        assert_eq!(c.display_name(), "[b] c : [d] c");
        let c = RecentComparison::new(
            ComparisonType::VersionControl,
            vec![PathBuf::from("/srv/repo")],
        );
        assert_eq!(c.display_name(), "Version control: /srv/repo");
        if let Some(home) = dirs::home_dir() {
            let c = RecentComparison::new(ComparisonType::VersionControl, vec![home.join("code")]);
            assert_eq!(c.display_name(), "Version control: ~/code");
        }
    }

    #[test]
    fn tmpfs_paths_are_not_recorded() {
        let shm = Path::new("/dev/shm");
        if !is_on_tmpfs(shm) {
            return;
        }
        let mut recent = RecentComparisons::default();
        assert!(!recent.add(ComparisonType::File, &[shm.join("a"), shm.join("b")], &[]));
        assert!(recent.is_empty());
    }

    #[test]
    fn tmpfs_detection_does_not_panic() {
        // Results depend on the machine; just exercise the code.
        let _ = is_on_tmpfs(Path::new("/dev/shm/x"));
        let _ = is_on_tmpfs(Path::new("relative"));
        assert_eq!(unescape_mount("/mnt/my\\040disk"), "/mnt/my disk");
        assert_eq!(unescape_mount("/x\\"), "/x\\");
    }
}
