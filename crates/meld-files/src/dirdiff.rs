//! Folder comparison
//!
//! The GUI-free logic of `meld/dirdiff.py`: comparing files
//! ([`files_same`]), listing folders in parallel with case-insensitive
//! and Unicode-normalised matching
//! ([`FolderComparison::scan_dir_cancellable`]), computing row states
//! ([`FolderComparison::row_status`]) and status filtering. Scans are
//! synchronous; run them on a background task.
//!
//! [`FolderComparison::walk`] is a depth-first iterator over every
//! folder level, matching the order of Meld's incremental scan.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use unicode_normalization::UnicodeNormalization;

use crate::filters::{ByteRegex, FilterEntry};
use crate::misc::apply_byte_filters;
use crate::state::State;

/// Block size used for comparing file contents
pub const CHUNK_SIZE: usize = 4096;

/// Files larger than this aren't read into memory for text-filtered
/// comparisons; they give [`FileComparison::DodgySame`] or
/// [`FileComparison::DodgyDifferent`] instead (Python's `MemoryError`
/// fallback)
pub const MAX_FILTERED_FILE_SIZE: u64 = 256 * 1024 * 1024;

/// The result of comparing a set of files
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FileComparison {
    /// The files are the same
    Same,
    /// The files are identical only after filtering
    SameFiltered,
    /// The files are superficially the same (type, size, mtime)
    DodgySame,
    /// The files are superficially different
    DodgyDifferent,
    /// The files are different
    Different,
    /// There was a problem reading one or more of the files
    FileError,
}

/// Options for comparing file contents
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CompareArgs {
    /// `folder-shallow-comparison`: compare only size and mtime
    pub shallow_comparison: bool,
    /// `folder-time-resolution`: mtime granularity in nanoseconds for
    /// shallow comparisons; -1 ignores timestamps
    pub time_resolution_ns: i64,
    /// `ignore-blank-lines`
    pub ignore_blank_lines: bool,
    /// `folder-filter-text`: apply text filters (and ignore newline
    /// differences) when comparing contents
    pub apply_text_filters: bool,
}

impl Default for CompareArgs {
    fn default() -> Self {
        CompareArgs {
            shallow_comparison: false,
            time_resolution_ns: 100,
            ignore_blank_lines: false,
            apply_text_filters: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum FileKind {
    Dir,
    Regular,
    Other,
}

/// Type, size and mtime of a file; `StatItem`
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct StatItem {
    kind: FileKind,
    size: u64,
    /// Nanoseconds since the epoch
    mtime_ns: i128,
}

impl StatItem {
    fn from_metadata(meta: &fs::Metadata) -> StatItem {
        let kind = if meta.is_dir() {
            FileKind::Dir
        } else if meta.is_file() {
            FileKind::Regular
        } else {
            FileKind::Other
        };
        let mtime_ns = meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128;
        StatItem {
            kind,
            size: meta.size(),
            mtime_ns,
        }
    }

    fn shallow_equal(&self, other: &StatItem, time_resolution_ns: i64) -> bool {
        if self.size != other.size {
            return false;
        }
        // Check for the ignore-timestamp configuration first
        if time_resolution_ns == -1 {
            return true;
        }
        // 2 seconds is our current accuracy threshold (for VFAT)
        if (self.mtime_ns - other.mtime_ns).abs() > 2_000_000_000 {
            return false;
        }
        let resolution = i128::from(time_resolution_ns.max(1));
        self.mtime_ns.div_euclid(resolution) == other.mtime_ns.div_euclid(resolution)
    }
}

type CacheKey = (Vec<PathBuf>, bool, Vec<String>, bool);
type CacheMap = HashMap<CacheKey, (Vec<StatItem>, FileComparison)>;

/// A cache of [`files_same`] results, invalidated by file stats
#[derive(Clone, Debug, Default)]
pub struct ComparisonCache {
    inner: Arc<Mutex<CacheMap>>,
}

impl ComparisonCache {
    /// An empty cache
    pub fn new() -> ComparisonCache {
        ComparisonCache::default()
    }

    /// Forget all cached results
    pub fn clear(&self) {
        if let Ok(mut map) = self.inner.lock() {
            map.clear();
        }
    }
}

/// Split bytes like Python's `bytes.splitlines()` (`\n`, `\r\n`, `\r`)
fn bytes_splitlines(text: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < text.len() {
        match text[i] {
            b'\n' => {
                lines.push(&text[start..i]);
                start = i + 1;
            }
            b'\r' => {
                lines.push(&text[start..i]);
                if text.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// Remove blank lines from text, normalising line endings to `\n`
pub fn remove_blank_lines(text: &[u8]) -> Vec<u8> {
    bytes_splitlines(text)
        .into_iter()
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(&b"\n"[..])
}

fn normalize(content: &[u8], ignore_blank_lines: bool, regexes: &[&ByteRegex]) -> Vec<u8> {
    // For probable text files, discard newline differences to match
    let mut content = if ignore_blank_lines {
        remove_blank_lines(content)
    } else {
        bytes_splitlines(content).join(&b"\n"[..])
    };
    if !regexes.is_empty() {
        // Filters matching across lines can't be applied; compare the
        // unfiltered text instead
        if let Ok(filtered) = apply_byte_filters(&content, regexes.iter().copied()) {
            content = filtered;
        }
        if ignore_blank_lines {
            // Applying text filters may have made more lines blank
            content = remove_blank_lines(&content);
        }
    }
    content
}

/// Compare the contents of same-size files chunk by chunk
fn contents_same(files: &[PathBuf]) -> io::Result<bool> {
    let mut handles = files
        .iter()
        .map(File::open)
        .collect::<io::Result<Vec<_>>>()?;
    let mut first = vec![0u8; CHUNK_SIZE * 4];
    let mut other = vec![0u8; CHUNK_SIZE * 4];
    loop {
        let n = read_full(&mut handles[0], &mut first)?;
        for handle in &mut handles[1..] {
            let m = read_full(handle, &mut other[..n.max(1)])?;
            if m != n || first[..n] != other[..n] {
                return Ok(false);
            }
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

fn read_full(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match file.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}

fn looks_binary(path: &Path) -> io::Result<bool> {
    let mut buf = vec![0u8; CHUNK_SIZE];
    let n = read_full(&mut File::open(path)?, &mut buf)?;
    Ok(buf[..n].contains(&0))
}

fn files_same_uncached(
    files: &[PathBuf],
    stats: &[StatItem],
    regexes: &[&ByteRegex],
    args: &CompareArgs,
    need_contents: bool,
) -> io::Result<FileComparison> {
    let same_size = stats.windows(2).all(|w| w[0].size == w[1].size);
    let mut result = if same_size {
        if contents_same(files)? {
            FileComparison::Same
        } else {
            FileComparison::Different
        }
    } else {
        FileComparison::Different
    };

    if result == FileComparison::Different && need_contents {
        let mut is_bin = false;
        for file in files {
            is_bin |= looks_binary(file)?;
        }
        if !is_bin {
            if stats.iter().any(|s| s.size > MAX_FILTERED_FILE_SIZE) {
                // Files are too large; we can't apply filters
                let all_same = stats.windows(2).all(|w| w[0] == w[1]);
                return Ok(if all_same {
                    FileComparison::DodgySame
                } else {
                    FileComparison::DodgyDifferent
                });
            }
            let mut normalized = Vec::with_capacity(files.len());
            for file in files {
                normalized.push(normalize(
                    &fs::read(file)?,
                    args.ignore_blank_lines,
                    regexes,
                ));
            }
            let all_same = normalized.windows(2).all(|w| w[0] == w[1]);
            result = if all_same {
                FileComparison::SameFiltered
            } else {
                FileComparison::Different
            };
        }
    }
    Ok(result)
}

/// Determine whether a list of files are the same; `_files_same()`
///
/// Folders are all the same; a folder and a file, or anything that
/// isn't a regular file, are different. With `shallow_comparison` only
/// size and mtime are compared. Otherwise contents are compared chunk
/// by chunk and, if they differ and text filtering or blank line
/// trimming is enabled, compared again after normalising line endings,
/// removing blank lines and applying `regexes` (unless a file looks
/// binary). Results are cached in `cache`, if given, until a file's
/// stats change.
pub fn files_same<P: AsRef<Path>>(
    files: &[P],
    regexes: &[&ByteRegex],
    args: &CompareArgs,
    cache: Option<&ComparisonCache>,
) -> FileComparison {
    let files: Vec<PathBuf> = files.iter().map(|f| f.as_ref().to_path_buf()).collect();
    if files.windows(2).all(|w| w[0] == w[1]) {
        return FileComparison::Same;
    }

    let mut stats = Vec::with_capacity(files.len());
    for file in &files {
        match fs::metadata(file) {
            Ok(meta) => stats.push(StatItem::from_metadata(&meta)),
            Err(_) => return FileComparison::FileError,
        }
    }

    let need_contents = args.ignore_blank_lines || args.apply_text_filters;
    let regexes: &[&ByteRegex] = if args.apply_text_filters {
        regexes
    } else {
        &[]
    };

    // If all entries are directories, they are considered to be the same
    if stats.iter().all(|s| s.kind == FileKind::Dir) {
        return FileComparison::Same;
    }
    // If any entries are not regular files, consider them different
    if !stats.iter().all(|s| s.kind == FileKind::Regular) {
        return FileComparison::Different;
    }

    // Compare files superficially if the options tells us to
    if args.shallow_comparison {
        let same = stats[1..]
            .iter()
            .all(|s| s.shallow_equal(&stats[0], args.time_resolution_ns));
        return if same {
            FileComparison::DodgySame
        } else {
            FileComparison::Different
        };
    }

    // If there are no text filters, unequal sizes imply a difference
    let same_size = stats.windows(2).all(|w| w[0].size == w[1].size);
    if !need_contents && !same_size {
        return FileComparison::Different;
    }

    let key: CacheKey = (
        files.clone(),
        need_contents,
        regexes.iter().map(|r| r.as_str().to_string()).collect(),
        args.ignore_blank_lines,
    );
    if let Some(cache) = cache
        && let Ok(map) = cache.inner.lock()
        && let Some((cached_stats, result)) = map.get(&key)
        && *cached_stats == stats
    {
        return *result;
    }

    match files_same_uncached(&files, &stats, regexes, args, need_contents) {
        Ok(result) => {
            if let Some(cache) = cache
                && let Ok(mut map) = cache.inner.lock()
            {
                map.insert(key, (stats, result));
            }
            result
        }
        // Don't cache generic errors as results
        Err(_) => FileComparison::FileError,
    }
}

/// Which row states are shown; the `folder-status-filters` setting
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StatusFilters {
    /// `"normal"`: identical rows (normal and no-change states)
    pub normal: bool,
    /// `"new"`: rows missing from some panes
    pub new: bool,
    /// `"modified"`: rows that differ
    pub modified: bool,
}

impl Default for StatusFilters {
    fn default() -> Self {
        StatusFilters {
            normal: true,
            new: true,
            modified: true,
        }
    }
}

impl StatusFilters {
    /// Parse the setting's strings; unknown names are ignored
    pub fn from_names<S: AsRef<str>>(names: &[S]) -> StatusFilters {
        let has = |n: &str| names.iter().any(|s| s.as_ref() == n);
        StatusFilters {
            normal: has("normal"),
            new: has("new"),
            modified: has("modified"),
        }
    }

    /// The setting's strings
    pub fn names(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.normal {
            names.push("normal");
        }
        if self.modified {
            names.push("modified");
        }
        if self.new {
            names.push("new");
        }
        names
    }

    /// Whether rows in this state are shown
    pub fn shows(&self, state: State) -> bool {
        match state {
            State::Normal | State::NoChange => self.normal,
            State::New => self.new,
            State::Modified => self.modified,
            _ => false,
        }
    }
}

/// Options for a folder comparison
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderOptions {
    /// Match names case-insensitively
    pub ignore_case: bool,
    /// Match names after Unicode NFC normalisation
    pub normalize_encoding: bool,
    /// `folder-ignore-symlinks`: skip symbolic links entirely
    pub ignore_symlinks: bool,
    /// How file contents are compared
    pub compare: CompareArgs,
    /// Which states are shown
    pub status_filters: StatusFilters,
}

/// One pane's details for a row
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneStatus {
    /// The pane's state: the row's state, or [`State::NonExist`]
    pub state: State,
    /// Whether the path exists (following symlinks)
    pub exists: bool,
    /// Whether the path is a folder (following symlinks)
    pub is_dir: bool,
    /// Size in bytes, if the path exists
    pub size: Option<u64>,
    /// Modification time, if the path exists
    pub mtime: Option<SystemTime>,
    /// `st_mode` (type and permission bits), if the path exists
    pub mode: Option<u32>,
    /// The link target if the path is a symlink (shown as
    /// "name ⟶ target")
    pub symlink_target: Option<PathBuf>,
}

/// The state of a comparison row; `_update_item_state()`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowStatus {
    /// The state of every pane where the path exists
    pub state: State,
    /// Per-pane details
    pub panes: Vec<PaneStatus>,
    /// Whether the row counts as a difference (not normal/no-change)
    pub different: bool,
}

/// A row in a folder comparison
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The entry's name in each pane; panes without the entry use the
    /// name from another pane
    pub names: Vec<String>,
    /// The entry's path in each pane
    pub paths: Vec<PathBuf>,
    /// Whether this is a folder row (listed as a folder in some pane)
    pub is_dir: bool,
    /// The row's state
    pub status: RowStatus,
}

/// A problem listing a folder, shown as an error row in a pane
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanError {
    /// The pane the error belongs to
    pub pane: usize,
    /// The error message
    pub message: String,
}

/// A file whose name was hidden by another (e.g., `A` and `a` in a
/// case-insensitive comparison)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShadowedEntry {
    /// The pane
    pub pane: usize,
    /// The folder containing both entries
    pub root: PathBuf,
    /// The hidden entry
    pub hidden: String,
    /// The entry hiding it
    pub by: String,
}

/// A file name problem found while scanning
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameIssue {
    /// The pane
    pub pane: usize,
    /// The folder containing the entry
    pub root: PathBuf,
    /// The (printable) entry name
    pub name: String,
}

/// What to do with a scanned folder's row
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanOutcome {
    /// The folder has child rows (or error rows)
    Entries,
    /// The folder is empty, or filtered to be empty; show an "empty
    /// folder" placeholder row
    Empty,
    /// The folder is filtered to be empty and the status filters hide
    /// identical rows; remove its row, and then any ancestors left
    /// without children (except the root, which gets a placeholder)
    Prune,
}

/// The result of listing one folder level in every pane
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirScan {
    /// The folder's path in each pane
    pub roots: Vec<PathBuf>,
    /// Child rows: folders first, then files, each sorted by name
    pub children: Vec<Row>,
    /// Errors to show as error rows under this folder
    pub errors: Vec<ScanError>,
    /// Entries with names that aren't valid UTF-8 (skipped)
    pub invalid_filenames: Vec<NameIssue>,
    /// Entries hidden by case-insensitive or normalised matching
    pub shadowed: Vec<ShadowedEntry>,
    /// Entries that differ from another only in surrounding whitespace
    pub whitespace: Vec<NameIssue>,
    /// What to do with the folder's row
    pub outcome: ScanOutcome,
    /// Whether any child differs or an error occurred; Meld expands
    /// such rows
    pub differences: bool,
}

/// Symlinks already followed during a scan, so that each is followed
/// only once (which also stops symlink loops)
#[derive(Clone, Debug, Default)]
pub struct SymlinkTracker {
    followed: HashSet<(u64, u64)>,
}

/// Multi-pane listing with canonicalised matching and error detection
struct CanonicalListing<'a> {
    n: usize,
    options: &'a FolderOptions,
    index: HashMap<String, usize>,
    items: Vec<Vec<Option<String>>>,
    stripped_items: HashMap<String, String>,
    errors: Vec<(usize, String, String)>,
    whitespace: Vec<(usize, String)>,
}

impl<'a> CanonicalListing<'a> {
    fn new(n: usize, options: &'a FolderOptions) -> Self {
        CanonicalListing {
            n,
            options,
            index: HashMap::new(),
            items: Vec::new(),
            stripped_items: HashMap::new(),
            errors: Vec::new(),
            whitespace: Vec::new(),
        }
    }

    fn add(&mut self, pane: usize, item: &str) {
        let mut ci = item.to_string();
        if self.options.ignore_case {
            ci = ci.to_lowercase();
        }
        if self.options.normalize_encoding {
            // NFC or NFD would work; this is for matching only
            ci = ci.nfc().collect();
        }

        let idx = *self.index.entry(ci.clone()).or_insert_with(|| {
            self.items.push(vec![None; self.n]);
            self.items.len() - 1
        });
        match &self.items[idx][pane] {
            None => self.items[idx][pane] = Some(item.to_string()),
            Some(existing) => self.errors.push((pane, item.to_string(), existing.clone())),
        }

        let stripped = ci.trim().to_string();
        match self.stripped_items.get(&stripped) {
            // An existing stripped item with a different unstripped
            // value is a case of misleading whitespace
            Some(existing) if *existing != ci => self.whitespace.push((pane, item.to_string())),
            Some(_) => {}
            None => {
                self.stripped_items.insert(stripped, ci);
            }
        }
    }

    fn get(&self) -> Vec<Vec<String>> {
        let mut out: Vec<Vec<String>> = self
            .items
            .iter()
            .map(|seq| {
                let fill = seq
                    .iter()
                    .flatten()
                    .find(|s| !s.is_empty())
                    .cloned()
                    .unwrap_or_default();
                seq.iter()
                    .map(|s| {
                        s.clone()
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| fill.clone())
                    })
                    .collect()
            })
            .collect();
        out.sort();
        out
    }
}

/// Render invalid UTF-8 like Python's "backslashreplace" handler
fn printable_name(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.utf8_chunks() {
        out.push_str(chunk.valid());
        for b in chunk.invalid() {
            out.push_str(&format!("\\x{b:02x}"));
        }
    }
    out
}

fn io_message(err: &io::Error) -> String {
    // Python shows strerror, without the "(os error N)" suffix
    let text = err.to_string();
    match text.find(" (os error") {
        Some(i) => text[..i].to_string(),
        None => text,
    }
}

fn system_time(meta: &fs::Metadata) -> SystemTime {
    let secs = meta.mtime();
    let nanos = meta.mtime_nsec().clamp(0, 999_999_999) as u32;
    if secs >= 0 {
        UNIX_EPOCH + std::time::Duration::new(secs as u64, nanos)
    } else {
        UNIX_EPOCH - std::time::Duration::from_secs(secs.unsigned_abs())
            + std::time::Duration::from_nanos(nanos.into())
    }
}

/// A folder comparison: options, filters and a result cache
///
/// Cloning is cheap and shares the cache, so a clone can be moved to a
/// background thread.
#[derive(Clone, Debug)]
pub struct FolderComparison {
    /// Comparison options
    pub options: FolderOptions,
    /// Filename filters; active ones hide matching entries
    pub name_filters: Vec<FilterEntry>,
    /// Text filters; active ones apply to content comparisons
    pub text_filters: Vec<FilterEntry>,
    cache: ComparisonCache,
}

impl FolderComparison {
    /// A comparison with the given options and filters
    pub fn new(
        options: FolderOptions,
        name_filters: Vec<FilterEntry>,
        text_filters: Vec<FilterEntry>,
    ) -> Self {
        FolderComparison {
            options,
            name_filters,
            text_filters,
            cache: ComparisonCache::new(),
        }
    }

    /// The shared comparison cache
    pub fn cache(&self) -> &ComparisonCache {
        &self.cache
    }

    fn regexes(&self) -> Vec<&ByteRegex> {
        self.text_filters
            .iter()
            .filter(|f| f.active)
            .filter_map(FilterEntry::byte_regex)
            .collect()
    }

    /// Compare files with this comparison's options, filters and cache
    pub fn compare<P: AsRef<Path>>(&self, files: &[P]) -> FileComparison {
        files_same(
            files,
            &self.regexes(),
            &self.options.compare,
            Some(&self.cache),
        )
    }

    /// Whether a name is hidden by an active filename filter
    pub fn is_filtered_name(&self, name: &str) -> bool {
        self.name_filters
            .iter()
            .any(|f| f.active && f.is_valid() && f.matches_name(name))
    }

    /// The top-level row for the compared folders
    pub fn root_row(&self, roots: &[PathBuf]) -> Row {
        let names = roots
            .iter()
            .map(|r| {
                let canonical = crate::iohelpers::canonicalize_lexically(r);
                canonical
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| canonical.to_string_lossy().into_owned())
            })
            .collect();
        Row {
            names,
            paths: roots.to_vec(),
            is_dir: true,
            status: self.row_status(roots),
        }
    }

    /// Compute a row's state from the files on disk
    pub fn row_status(&self, files: &[PathBuf]) -> RowStatus {
        let stats: Vec<Option<fs::Metadata>> = files.iter().map(|f| fs::metadata(f).ok()).collect();
        let lstats: Vec<Option<fs::Metadata>> =
            files.iter().map(|f| fs::symlink_metadata(f).ok()).collect();

        let (all_same, all_present_same) = if stats.iter().all(Option::is_some) {
            let result = self.compare(files);
            (result, result)
        } else {
            let present: Vec<&PathBuf> = files
                .iter()
                .zip(&stats)
                .filter(|(_, s)| {
                    s.as_ref()
                        .is_some_and(|m| m.mtime() != 0 || m.mtime_nsec() != 0)
                })
                .map(|(f, _)| f)
                .collect();
            (FileComparison::Different, self.compare(&present))
        };

        use FileComparison::*;
        let state = if matches!(all_same, Same | DodgySame) {
            State::Normal
        } else if all_same == SameFiltered {
            State::NoChange
        } else if matches!(all_present_same, Same | SameFiltered | DodgySame) {
            State::New
        } else if all_same == FileError || all_present_same == FileError {
            State::Error
        } else {
            State::Modified
        };
        let different = !matches!(state, State::Normal | State::NoChange);

        let any_dir = stats.iter().flatten().any(fs::Metadata::is_dir);
        let panes = files
            .iter()
            .zip(stats.iter().zip(&lstats))
            .map(|(file, (stat, lstat))| {
                let symlink_target = lstat
                    .as_ref()
                    .filter(|m| m.file_type().is_symlink())
                    .and_then(|_| fs::read_link(file).ok());
                match stat {
                    Some(meta) => PaneStatus {
                        state,
                        exists: true,
                        is_dir: meta.is_dir(),
                        size: Some(meta.size()),
                        mtime: Some(system_time(meta)),
                        mode: Some(meta.mode()),
                        symlink_target,
                    },
                    None => PaneStatus {
                        state: State::NonExist,
                        exists: false,
                        is_dir: any_dir,
                        size: None,
                        mtime: None,
                        mode: None,
                        symlink_target,
                    },
                }
            })
            .collect();

        RowStatus {
            state,
            panes,
            different,
        }
    }

    /// The states of an entry for status filtering; `_filter_on_state()`
    ///
    /// Returns whether the row should be listed: its states match the
    /// status filters, or it is a folder in every pane where it exists
    /// (such folders are pruned later if they end up empty).
    fn passes_status_filters(&self, files: &[PathBuf]) -> bool {
        let present: Vec<&PathBuf> = files.iter().filter(|f| f.exists()).collect();
        let states: &[State] = if present.len() == files.len() {
            match self.compare(files) {
                FileComparison::Same | FileComparison::DodgySame => &[State::Normal],
                FileComparison::SameFiltered => &[State::NoChange],
                _ => &[State::Modified],
            }
        } else if present.len() > 1 {
            // In a three-way comparison, files in panes 1 and 2 can
            // differ with no file in pane 3: both modified and new
            match self.compare(&present) {
                FileComparison::Same | FileComparison::DodgySame | FileComparison::SameFiltered => {
                    &[State::New]
                }
                _ => &[State::New, State::Modified],
            }
        } else {
            &[State::New]
        };
        let checked: Vec<&PathBuf> = if present.len() > 1 && present.len() < files.len() {
            present
        } else {
            files.iter().collect()
        };
        let all_folders = checked.iter().all(|f| f.is_dir());
        let filters = self.options.status_filters;
        states.iter().any(|s| filters.shows(*s)) || all_folders
    }

    /// [`scan_dir_cancellable`](Self::scan_dir_cancellable) without a
    /// cancel flag
    #[cfg(test)]
    pub fn scan_dir(&self, roots: &[PathBuf], symlinks: &mut SymlinkTracker) -> DirScan {
        self.scan_dir_cancellable(roots, symlinks, None)
            .unwrap_or_else(|| unreachable!("scans without a cancel flag finish"))
    }

    /// List one folder level in every pane, giving up, returning `None`,
    /// as soon as `cancel` is set; it is checked between folder entries
    /// and between file comparisons.
    ///
    /// `roots` are the folder's paths in each pane (some may not exist).
    /// `symlinks` tracks followed symlinks across a scan; pass a fresh
    /// tracker when expanding a single row.
    pub fn scan_dir_cancellable(
        &self,
        roots: &[PathBuf],
        symlinks: &mut SymlinkTracker,
        cancel: Option<&AtomicBool>,
    ) -> Option<DirScan> {
        let cancelled = || cancel.is_some_and(|c| c.load(Ordering::Relaxed));
        let n = roots.len();
        let mut errors = Vec::new();
        let mut invalid_filenames = Vec::new();
        let mut differences = false;
        let mut dirs = CanonicalListing::new(n, &self.options);
        let mut files = CanonicalListing::new(n, &self.options);

        for (pane, root) in roots.iter().enumerate() {
            if !root.is_dir() {
                continue;
            }
            let entries = match fs::read_dir(root) {
                Ok(entries) => entries,
                Err(err) => {
                    errors.push(ScanError {
                        pane,
                        message: io_message(&err),
                    });
                    differences = true;
                    continue;
                }
            };
            for entry in entries {
                if cancelled() {
                    return None;
                }
                let Ok(entry) = entry else { continue };
                let os_name = entry.file_name();
                let Some(name) = os_name.to_str() else {
                    use std::os::unix::ffi::OsStrExt;
                    invalid_filenames.push(NameIssue {
                        pane,
                        root: root.clone(),
                        name: printable_name(os_name.as_bytes()),
                    });
                    continue;
                };
                if self.is_filtered_name(name) {
                    continue;
                }
                let path = root.join(name);
                let meta = match fs::symlink_metadata(&path) {
                    Ok(meta) => meta,
                    // Covers certain unreadable symlink cases; see bgo#585895
                    Err(err) => {
                        errors.push(ScanError {
                            pane,
                            message: format!("{name}: {}", io_message(&err)),
                        });
                        continue;
                    }
                };
                let file_type = meta.file_type();
                if file_type.is_symlink() {
                    if self.options.ignore_symlinks {
                        continue;
                    }
                    if !symlinks.followed.insert((meta.dev(), meta.ino())) {
                        continue;
                    }
                    match fs::metadata(&path) {
                        Ok(target) if target.is_file() => files.add(pane, name),
                        Ok(target) if target.is_dir() => dirs.add(pane, name),
                        Ok(_) => {}
                        Err(err) => {
                            let message = if err.kind() == io::ErrorKind::NotFound {
                                format!("{name}: Dangling symlink")
                            } else {
                                format!("{name}: {}", io_message(&err))
                            };
                            errors.push(ScanError { pane, message });
                            differences = true;
                        }
                    }
                } else if file_type.is_file() {
                    files.add(pane, name);
                } else if file_type.is_dir() {
                    dirs.add(pane, name);
                }
                // Other file types (devices, sockets, ...) are ignored
            }
        }

        let shadowed = dirs
            .errors
            .iter()
            .chain(&files.errors)
            .map(|(pane, hidden, by)| ShadowedEntry {
                pane: *pane,
                root: roots[*pane].clone(),
                hidden: hidden.clone(),
                by: by.clone(),
            })
            .collect();
        let whitespace = dirs
            .whitespace
            .iter()
            .chain(&files.whitespace)
            .map(|(pane, name)| NameIssue {
                pane: *pane,
                root: roots[*pane].clone(),
                name: name.clone(),
            })
            .collect();

        let join = |names: &[String]| -> Vec<PathBuf> {
            roots.iter().zip(names).map(|(r, n)| r.join(n)).collect()
        };
        let mut children = Vec::new();
        for (names, is_dir) in dirs
            .get()
            .into_iter()
            .map(|n| (n, true))
            .chain(files.get().into_iter().map(|n| (n, false)))
        {
            if cancelled() {
                return None;
            }
            let paths = join(&names);
            if !self.passes_status_filters(&paths) {
                continue;
            }
            let status = self.row_status(&paths);
            differences |= status.different;
            children.push(Row {
                names,
                paths,
                is_dir,
                status,
            });
        }

        let outcome = if !children.is_empty() {
            ScanOutcome::Entries
        } else if self.options.status_filters.normal || !roots.iter().all(|r| r.is_dir()) {
            ScanOutcome::Empty
        } else if !errors.is_empty() {
            ScanOutcome::Entries
        } else {
            ScanOutcome::Prune
        };

        Some(DirScan {
            roots: roots.to_vec(),
            children,
            errors,
            invalid_filenames,
            shadowed,
            whitespace,
            outcome,
            differences,
        })
    }

    /// Scan every folder level depth-first, in the order Meld does
    ///
    /// The iterator owns a clone of the comparison, so it can be moved
    /// to another thread.
    pub fn walk(&self, roots: &[PathBuf]) -> Walk {
        Walk {
            comparison: self.clone(),
            stack: vec![(Vec::new(), roots.to_vec())],
            symlinks: SymlinkTracker::default(),
            cancel: None,
        }
    }

    /// [`walk`](Self::walk) that stops as soon as `cancel` is set, even
    /// in the middle of a folder: the folder being scanned is dropped
    /// and the iterator ends. [`Walk::cancelled`] tells the two endings
    /// apart.
    pub fn walk_cancellable(&self, roots: &[PathBuf], cancel: Arc<AtomicBool>) -> Walk {
        Walk {
            cancel: Some(cancel),
            ..self.walk(roots)
        }
    }
}

/// One scanned folder from [`Walk`]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalkItem {
    /// The folder's position: indices into each ancestor's
    /// [`DirScan::children`], empty for the root
    pub tree_path: Vec<usize>,
    /// The scan of the folder
    pub scan: DirScan,
}

/// Depth-first iterator over every folder level of a comparison
///
/// Rows are never removed by the iterator itself; apply
/// [`ScanOutcome::Prune`] in the consumer.
#[derive(Debug)]
pub struct Walk {
    comparison: FolderComparison,
    stack: Vec<(Vec<usize>, Vec<PathBuf>)>,
    symlinks: SymlinkTracker,
    cancel: Option<Arc<AtomicBool>>,
}

impl Walk {
    /// Whether the walk was cancelled through its cancel flag.
    pub fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    }
}

impl Iterator for Walk {
    type Item = WalkItem;

    fn next(&mut self) -> Option<WalkItem> {
        loop {
            if self.cancelled() {
                return None;
            }
            let (tree_path, roots) = self.stack.pop()?;
            // Rows whose folders have vanished are skipped
            if !roots.iter().any(|r| r.is_dir()) {
                continue;
            }
            let scan = self.comparison.scan_dir_cancellable(
                &roots,
                &mut self.symlinks,
                self.cancel.as_deref(),
            )?;
            for (i, child) in scan.children.iter().enumerate().rev() {
                if child.is_dir {
                    let mut path = tree_path.clone();
                    path.push(i);
                    self.stack.push((path, child.paths.clone()));
                }
            }
            return Some(WalkItem { tree_path, scan });
        }
    }
}

/// File name problems found anywhere in a scan, for Meld's tree-wide
/// warnings
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanIssues {
    /// Names that aren't valid UTF-8
    pub invalid_filenames: Vec<NameIssue>,
    /// Entries hidden by case-insensitive or normalised matching
    pub shadowed: Vec<ShadowedEntry>,
    /// Names differing only in surrounding whitespace
    pub whitespace: Vec<NameIssue>,
}

impl ScanIssues {
    /// Whether any issue was found
    pub fn is_empty(&self) -> bool {
        self.invalid_filenames.is_empty() && self.shadowed.is_empty() && self.whitespace.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filters::{FilterKind, default_filename_filters};

    const TEST_CHUNK: usize = 4096 * 10;

    enum Def {
        File(Vec<u8>),
        Dir(Vec<(&'static str, Def)>),
    }

    fn file(data: &[u8]) -> Def {
        Def::File(data.to_vec())
    }

    fn populate(defs: &[(&'static str, Def)], path: &Path) {
        for (name, def) in defs {
            let p = path.join(name);
            match def {
                Def::File(data) => fs::write(&p, data).unwrap(),
                Def::Dir(children) => {
                    fs::create_dir(&p).unwrap();
                    populate(children, &p);
                }
            }
        }
    }

    /// test/dirdiff/conftest.py
    fn create_sample_dir() -> tempfile::TempDir {
        let d = |c: u8| vec![c; TEST_CHUNK];
        let with = |mut v: Vec<u8>, c: u8| {
            v.push(c);
            v
        };
        let defs = vec![
            (
                "a",
                Def::Dir(vec![
                    ("a.txt", file(b"")),
                    ("c", Def::Dir(vec![("c.txt", file(b""))])),
                    (
                        "d",
                        Def::Dir(vec![("d.txt", Def::File(with(d(b'd'), b'd')))]),
                    ),
                    (
                        "e",
                        Def::Dir(vec![
                            ("f", Def::Dir(vec![])),
                            ("g", Def::Dir(vec![("g.txt", file(b"g"))])),
                            ("h", Def::Dir(vec![("h.txt", file(b"h"))])),
                            ("e.txt", file(b"")),
                        ]),
                    ),
                    ("crlf.txt", file(b"foo\r\nbar\r\n")),
                    ("crlftrailing.txt", file(b"foo\r\nbar\r\n\r\n")),
                ]),
            ),
            (
                "b",
                Def::Dir(vec![
                    ("b.txt", file(b"")),
                    ("c", Def::Dir(vec![("c.txt", file(b""))])),
                    (
                        "d",
                        Def::Dir(vec![
                            ("d.txt", Def::File(with(d(b'd'), b'd'))),
                            ("d.1.txt", Def::File(with(d(b'D'), b'D'))),
                            ("d.2.txt", Def::File(with(d(b'd'), b'D'))),
                        ]),
                    ),
                    (
                        "e",
                        Def::Dir(vec![
                            ("f", Def::Dir(vec![("f.txt", file(b""))])),
                            ("g", Def::Dir(vec![("g.txt", file(b""))])),
                            ("h", Def::Dir(vec![("h.txt", file(b"h"))])),
                            ("e.txt", file(b"")),
                        ]),
                    ),
                    ("lf.txt", file(b"foo\nbar\n")),
                    ("lftrailing.txt", file(b"foo\nbar\n\n")),
                ]),
            ),
        ];
        let dir = tempfile::tempdir().unwrap();
        populate(&defs, dir.path());
        // The shallow comparison case needs matching mtimes, which
        // Python's test gets from creating the files quickly
        let mtime = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        for f in ["a/d/d.txt", "b/d/d.1.txt"] {
            File::options()
                .write(true)
                .open(dir.path().join(f))
                .unwrap()
                .set_modified(mtime)
                .unwrap();
        }
        dir
    }

    /// test/dirdiff/test_files_same.py
    #[test]
    fn files_same_cases() {
        use FileComparison::*;
        let cmp = CompareArgs {
            shallow_comparison: false,
            time_resolution_ns: 10_000_000_000,
            ignore_blank_lines: true,
            apply_text_filters: true,
        };
        let no_ignore = CompareArgs {
            ignore_blank_lines: false,
            apply_text_filters: false,
            ..cmp
        };
        let dodgy = CompareArgs {
            shallow_comparison: true,
            ..cmp
        };

        let cases: &[(&[&str], CompareArgs, FileComparison)] = &[
            (&[], cmp, Same),
            (&["a", "b"], cmp, Same),
            (&["a", "b/b.txt"], cmp, Different),
            (&["a/d/d.txt", "b/d/d.1.txt"], dodgy, DodgySame),
            (&["a/c/c.txt", "b/c/c.txt"], cmp, Same),
            (&["a/d/d.txt", "b/d/d.txt"], cmp, Same),
            (&["a/d/d.txt", "b/d/d.1.txt"], cmp, Different),
            (&["a/d/d.txt", "b/d/d.2.txt"], cmp, Different),
            (&["a/e/g/g.txt", "b/e/g/g.txt"], cmp, Different),
            (&["a/crlf.txt", "a/crlftrailing.txt"], cmp, SameFiltered),
            (&["a/crlf.txt", "a/crlftrailing.txt"], no_ignore, Different),
            (&["b/lf.txt", "b/lftrailing.txt"], cmp, SameFiltered),
            (&["b/lf.txt", "b/lftrailing.txt"], no_ignore, Different),
            (&["a/crlf.txt", "b/lf.txt"], cmp, SameFiltered),
            (&["a/crlf.txt", "b/lf.txt"], no_ignore, Different),
            (
                &["a/crlftrailing.txt", "b/lftrailing.txt"],
                cmp,
                SameFiltered,
            ),
            (
                &["a/crlftrailing.txt", "b/lftrailing.txt"],
                no_ignore,
                Different,
            ),
        ];
        let dir = create_sample_dir();
        let cache = ComparisonCache::new();
        for (files, args, expected) in cases {
            let paths: Vec<PathBuf> = files.iter().map(|f| dir.path().join(f)).collect();
            assert_eq!(files_same(&paths, &[], args, None), *expected, "{files:?}");
            // ... and the same again through the cache
            assert_eq!(
                files_same(&paths, &[], args, Some(&cache)),
                *expected,
                "{files:?}"
            );
            assert_eq!(
                files_same(&paths, &[], args, Some(&cache)),
                *expected,
                "{files:?}"
            );
        }
    }

    /// test/dirdiff/test_remove_blank_lines.py
    #[test]
    fn remove_blank_lines_cases() {
        let cases: &[(&[u8], &[u8])] = &[
            (b"", b""),
            (b" ", b" "),
            (b"\n", b""),
            (b"\n ", b" "),
            (b" \n", b" "),
            (b" \n ", b" \n "),
            (b"\n\n", b""),
            (b"\n\n ", b" "),
            (b"\n \n", b" "),
            (b"\n \n ", b" \n "),
            (b" \n \n ", b" \n \n "),
            (b" content", b" content"),
            (b"content\n\ncontent", b"content\ncontent"),
            (b"\n\n\ncontent\n\n\ncontent\n\n\n", b"content\ncontent"),
        ];
        for (text, expected) in cases {
            assert_eq!(remove_blank_lines(text), *expected, "{text:?}");
        }
    }

    #[test]
    fn text_filters_in_comparison() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.py");
        let b = dir.path().join("b.py");
        fs::write(&a, b"x = 1  # one\n").unwrap();
        fs::write(&b, b"x = 1  # two\n").unwrap();
        let filter = FilterEntry::new("Script Comment", true, "#.*", FilterKind::Regex);
        let regexes = [filter.byte_regex().unwrap()];
        let args = CompareArgs::default();
        assert_eq!(
            files_same(&[&a, &b], &regexes, &args, None),
            FileComparison::SameFiltered
        );
        assert_eq!(
            files_same(&[&a, &b], &[], &args, None),
            FileComparison::Different
        );
        let unfiltered = CompareArgs {
            apply_text_filters: false,
            ..args
        };
        assert_eq!(
            files_same(&[&a, &b], &regexes, &unfiltered, None),
            FileComparison::Different
        );

        // Binary files aren't filtered
        fs::write(&a, b"\0x # one\n").unwrap();
        fs::write(&b, b"\0x # two!\n").unwrap();
        assert_eq!(
            files_same(&[&a, &b], &regexes, &args, None),
            FileComparison::Different
        );
    }

    #[test]
    fn shallow_time_resolution() {
        let s = |size, secs: i128, nanos: i128| StatItem {
            kind: FileKind::Regular,
            size,
            mtime_ns: secs * 1_000_000_000 + nanos,
        };
        assert!(s(1, 10, 50).shallow_equal(&s(1, 10, 99), 100));
        assert!(!s(1, 10, 50).shallow_equal(&s(1, 10, 150), 100));
        assert!(!s(1, 10, 0).shallow_equal(&s(2, 10, 0), 100));
        assert!(s(1, 10, 0).shallow_equal(&s(1, 99, 0), -1));
        assert!(!s(1, 10, 0).shallow_equal(&s(1, 13, 0), 10_000_000_000));
    }

    fn comparison(status_filters: StatusFilters) -> FolderComparison {
        let options = FolderOptions {
            status_filters,
            ..FolderOptions::default()
        };
        FolderComparison::new(options, default_filename_filters(), Vec::new())
    }

    fn names(scan: &DirScan) -> Vec<(String, bool, State)> {
        scan.children
            .iter()
            .map(|r| (r.names[0].clone(), r.is_dir, r.status.state))
            .collect()
    }

    #[test]
    fn scan_level_states() {
        let dir = create_sample_dir();
        let roots = vec![dir.path().join("a"), dir.path().join("b")];
        let cmp = comparison(StatusFilters::default());
        let scan = cmp.scan_dir(&roots, &mut SymlinkTracker::default());
        assert_eq!(
            names(&scan),
            vec![
                ("c".into(), true, State::Normal),
                ("d".into(), true, State::Normal),
                ("e".into(), true, State::Normal),
                ("a.txt".into(), false, State::New),
                ("b.txt".into(), false, State::New),
                ("crlf.txt".into(), false, State::New),
                ("crlftrailing.txt".into(), false, State::New),
                ("lf.txt".into(), false, State::New),
                ("lftrailing.txt".into(), false, State::New),
            ]
        );
        assert!(scan.differences);
        assert_eq!(scan.outcome, ScanOutcome::Entries);
        let a_txt = &scan.children[3];
        assert!(a_txt.status.panes[0].exists);
        assert_eq!(a_txt.status.panes[1].state, State::NonExist);
        assert_eq!(a_txt.names, vec!["a.txt", "a.txt"]);

        let d = cmp.scan_dir(&scan.children[1].paths, &mut SymlinkTracker::default());
        assert_eq!(
            names(&d),
            vec![
                ("d.1.txt".into(), false, State::New),
                ("d.2.txt".into(), false, State::New),
                ("d.txt".into(), false, State::Normal),
            ]
        );
        let e_f = cmp.scan_dir(
            &[dir.path().join("a/e/f"), dir.path().join("b/e/f")],
            &mut SymlinkTracker::default(),
        );
        assert_eq!(names(&e_f), vec![("f.txt".into(), false, State::New)]);
        let e_g = cmp.scan_dir(
            &[dir.path().join("a/e/g"), dir.path().join("b/e/g")],
            &mut SymlinkTracker::default(),
        );
        assert_eq!(names(&e_g), vec![("g.txt".into(), false, State::Modified)]);
    }

    #[test]
    fn empty_folders() {
        let dir = tempfile::tempdir().unwrap();
        for p in ["a/x", "b/x"] {
            fs::create_dir_all(dir.path().join(p)).unwrap();
        }
        let roots = vec![dir.path().join("a"), dir.path().join("b")];

        // Hiding identical rows prunes x; the walk reports the prune for
        // the consumer to apply
        let walk: Vec<WalkItem> = comparison(StatusFilters {
            normal: false,
            ..Default::default()
        })
        .walk(&roots)
        .collect();
        assert_eq!(walk.len(), 2);
        assert_eq!(walk[0].tree_path, Vec::<usize>::new());
        assert_eq!(walk[1].tree_path, vec![0]);
        assert_eq!(walk[1].scan.outcome, ScanOutcome::Prune);
    }

    #[test]
    fn walk_is_depth_first() {
        let dir = create_sample_dir();
        let roots = vec![dir.path().join("a"), dir.path().join("b")];
        let paths: Vec<Vec<usize>> = comparison(StatusFilters::default())
            .walk(&roots)
            .map(|w| w.tree_path)
            .collect();
        assert_eq!(
            paths,
            vec![
                vec![],
                vec![0],
                vec![1],
                vec![2],
                vec![2, 0],
                vec![2, 1],
                vec![2, 2]
            ]
        );
    }

    #[test]
    fn cancelled_walks_stop() {
        let dir = create_sample_dir();
        let roots = vec![dir.path().join("a"), dir.path().join("b")];
        let cancel = Arc::new(AtomicBool::new(false));
        let mut walk =
            comparison(StatusFilters::default()).walk_cancellable(&roots, cancel.clone());
        assert_eq!(walk.next().map(|w| w.tree_path), Some(vec![]));
        assert!(!walk.cancelled());
        cancel.store(true, Ordering::Relaxed);
        assert!(walk.next().is_none());
        assert!(walk.cancelled());
        // A cancelled folder scan gives nothing rather than a partial scan
        let scan = comparison(StatusFilters::default()).scan_dir_cancellable(
            &roots,
            &mut SymlinkTracker::default(),
            Some(&cancel),
        );
        assert!(scan.is_none());
    }

    #[test]
    fn case_insensitive_matching_and_collisions() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("Readme"), b"x").unwrap();
        fs::write(a.join("README"), b"y").unwrap();
        fs::write(b.join("readme"), b"x").unwrap();
        fs::write(a.join(" name"), b"").unwrap();
        fs::write(b.join("name"), b"").unwrap();

        let roots = vec![a.clone(), b.clone()];
        let sensitive = comparison(StatusFilters::default());
        let scan = sensitive.scan_dir(&roots, &mut SymlinkTracker::default());
        assert_eq!(scan.children.len(), 5);
        assert!(scan.shadowed.is_empty());
        assert_eq!(scan.whitespace.len(), 1);
        assert_eq!(scan.whitespace[0].name.trim(), "name");

        let options = FolderOptions {
            ignore_case: true,
            ..FolderOptions::default()
        };
        let insensitive = FolderComparison::new(options, Vec::new(), Vec::new());
        let scan = insensitive.scan_dir(&roots, &mut SymlinkTracker::default());
        let readme = scan
            .children
            .iter()
            .find(|r| r.names[1] == "readme")
            .unwrap();
        assert_eq!(readme.names[0].to_lowercase(), "readme");
        assert_eq!(scan.shadowed.len(), 1);
        assert_eq!(scan.shadowed[0].pane, 0);
    }

    #[test]
    fn unicode_normalisation_matching() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("caf\u{e9}"), b"").unwrap();
        fs::write(b.join("cafe\u{301}"), b"").unwrap();
        let roots = vec![a, b];
        assert_eq!(
            comparison(StatusFilters::default())
                .scan_dir(&roots, &mut SymlinkTracker::default())
                .children
                .len(),
            2
        );
        let options = FolderOptions {
            normalize_encoding: true,
            ..FolderOptions::default()
        };
        let cmp = FolderComparison::new(options, Vec::new(), Vec::new());
        let scan = cmp.scan_dir(&roots, &mut SymlinkTracker::default());
        assert_eq!(scan.children.len(), 1);
        assert_eq!(scan.children[0].status.state, State::Normal);
    }

    #[test]
    fn filters_and_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::create_dir_all(a.join(".git")).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("x.orig"), b"").unwrap();
        fs::write(a.join("target"), b"t").unwrap();
        std::os::unix::fs::symlink("target", a.join("link")).unwrap();
        std::os::unix::fs::symlink("missing", a.join("dangling")).unwrap();
        // A symlink loop is followed once only
        std::os::unix::fs::symlink(".", a.join("loop")).unwrap();

        let roots = vec![a.clone(), b.clone()];
        let cmp = comparison(StatusFilters::default());
        let scan = cmp.scan_dir(&roots, &mut SymlinkTracker::default());
        let listed: Vec<&str> = scan.children.iter().map(|r| r.names[0].as_str()).collect();
        assert_eq!(listed, vec!["loop", "link", "target"]);
        assert_eq!(
            scan.errors,
            vec![ScanError {
                pane: 0,
                message: "dangling: Dangling symlink".into()
            }]
        );
        let link = &scan.children[1];
        assert_eq!(
            link.status.panes[0].symlink_target,
            Some(PathBuf::from("target"))
        );

        let walked: Vec<WalkItem> = cmp.walk(&roots).collect();
        assert_eq!(walked.len(), 2, "the loop is followed only once");

        let options = FolderOptions {
            ignore_symlinks: true,
            ..FolderOptions::default()
        };
        let cmp = FolderComparison::new(options, default_filename_filters(), Vec::new());
        let scan = cmp.scan_dir(&roots, &mut SymlinkTracker::default());
        let listed: Vec<&str> = scan.children.iter().map(|r| r.names[0].as_str()).collect();
        assert_eq!(listed, vec!["target"]);
        assert!(scan.errors.is_empty());
    }

    #[test]
    fn three_way_states() {
        let dir = tempfile::tempdir().unwrap();
        let roots: Vec<PathBuf> = ["a", "b", "c"].iter().map(|p| dir.path().join(p)).collect();
        for r in &roots {
            fs::create_dir_all(r).unwrap();
        }
        fs::write(roots[0].join("f"), b"1").unwrap();
        fs::write(roots[1].join("f"), b"2").unwrap();
        fs::write(roots[0].join("g"), b"1").unwrap();
        fs::write(roots[1].join("g"), b"1").unwrap();

        // f is both new and modified; only showing modified keeps it
        let cmp = comparison(StatusFilters {
            normal: false,
            new: false,
            modified: true,
        });
        let scan = cmp.scan_dir(&roots, &mut SymlinkTracker::default());
        let listed: Vec<(&str, State)> = scan
            .children
            .iter()
            .map(|r| (r.names[0].as_str(), r.status.state))
            .collect();
        assert_eq!(listed, vec![("f", State::Modified)]);

        let all =
            comparison(StatusFilters::default()).scan_dir(&roots, &mut SymlinkTracker::default());
        let g = &all.children[1];
        assert_eq!(g.status.state, State::New);
        assert_eq!(g.status.panes[2].state, State::NonExist);
    }

    #[test]
    fn status_filter_names() {
        let filters = StatusFilters::from_names(&["normal", "modified", "new"]);
        assert_eq!(filters, StatusFilters::default());
        assert_eq!(filters.names(), vec!["normal", "modified", "new"]);
        assert!(StatusFilters::from_names(&["new"]).shows(State::New));
        assert!(!StatusFilters::from_names(&["new"]).shows(State::NoChange));
    }

    #[test]
    fn non_utf8_names_are_reported() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        fs::create_dir_all(&a).unwrap();
        let bad = a.join(std::ffi::OsStr::from_bytes(b"bad\xffname"));
        if fs::write(&bad, b"").is_err() {
            return; // File system doesn't allow non-UTF-8 names
        }
        let scan = comparison(StatusFilters::default())
            .scan_dir(&[a.clone(), a], &mut SymlinkTracker::default());
        assert_eq!(scan.invalid_filenames.len(), 2);
        assert_eq!(scan.invalid_filenames[0].name, "bad\\xffname");
    }
}
