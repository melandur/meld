//! Darcs backend (port of `meld/vc/darcs.py`); requires Darcs >= 2.10.3.
//!
//! Beyond Python:
//! - Conflicts. `darcs whatsnew` doesn't report them (darcs issue2138):
//!   a conflicting pull or apply writes conflict markup (`v v v v v v v`
//!   ... `^ ^ ^ ^ ^ ^ ^`) into the working file, which is then just
//!   modified. Modified files containing that markup are conflicts,
//!   compared 3-way with sides rebuilt from the markup ([`ConflictMarkup`]).
//!   Darcs has no "resolved" flag (recording the edited file records the
//!   resolution), so Resolve refuses files that still contain markup and
//!   otherwise only re-checks.
//! - Boring files (Darcs' ignored files), which `darcs whatsnew -l` leaves
//!   out, are ignored ([`BoringRules`]).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::command::{self, VcCommand};
use crate::entry::{Action, ConflictSide, State};
use crate::error::{Result, VcError};
use crate::paths;
use crate::vc::{TreeCache, Vc, VcCore, VcKind, unsupported};

const CMD: &str = "darcs";
const NAME: &str = "Darcs";

/// Whether `darcs --version` output (e.g. `2.16.5 (release)`) is at least
/// 2.10.3.
pub fn version_supported(output: &str) -> bool {
    let first = output.split_whitespace().next().unwrap_or_default();
    let parts: Vec<u64> = first
        .splitn(3, '.')
        .map(|p| {
            p.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .unwrap_or(0)
        })
        .collect();
    if parts.len() < 3 {
        return false;
    }
    (parts[0], parts[1], parts[2]) >= (2, 10, 3)
}

/// Map a `darcs whatsnew -sl --machine-readable` key to a state.
pub fn state_for_key(key: &str) -> State {
    match key {
        "a" => State::Unversioned,
        "A" => State::New,
        "M" => State::Modified,
        "M!" => State::Conflict,
        "R" => State::Removed,
        "F" => State::NonExist, // previous name of a moved file
        "T" => State::Renamed,  // new name of a moved file
        _ => State::Unversioned,
    }
}

/// Parsed `darcs whatsnew` output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DarcsStatus {
    pub states: HashMap<PathBuf, State>,
    pub meta: HashMap<PathBuf, String>,
    /// New path → old path for moved files.
    pub reverse_renames: HashMap<PathBuf, PathBuf>,
}

/// Parse `darcs whatsnew -sl --machine-readable` output. Paths are
/// relative to the repository `root` wherever darcs runs (Python joined
/// them to the comparison location, which broke sub-folder locations).
/// Darcs lists moves (`F` old name, then `T` new name) before
/// modifications.
///
/// Lines that aren't status lines, like `No changes!`, are skipped (Python
/// recorded a file named `changes!`). A file removed with `darcs remove`
/// stays on disk and is listed twice, as removed (`R`) and unversioned
/// (`a`); it is shown as removed.
pub fn parse_whatsnew(output: &str, root: &Path) -> DarcsStatus {
    let mut status = DarcsStatus::default();
    let mut renames: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut old_name: Option<PathBuf> = None;
    for line in output.lines() {
        let Some((key, name)) = line.split_once(' ') else {
            continue;
        };
        let key = key.trim();
        if !matches!(key, "a" | "A" | "M" | "M!" | "R" | "F" | "T") {
            continue;
        }
        let name = paths::normalize(Path::new(name));
        if key == "F" {
            old_name = Some(name.clone());
        }
        let path = paths::absolutize(root, &name);
        if key == "T"
            && let Some(old) = old_name.take()
        {
            renames.push((paths::absolutize(root, &old), path.clone()));
        }
        if key == "a" && status.states.contains_key(&path) {
            continue;
        }
        status.states.insert(path, state_for_key(key));
    }
    for (old, new) in renames {
        let rel = |p: &Path| {
            p.strip_prefix(root)
                .map(paths::lossy)
                .unwrap_or_else(|_| paths::lossy(p))
        };
        status
            .meta
            .insert(new.clone(), format!("{} ➡ {}", rel(&old), rel(&new)));
        status.reverse_renames.insert(new, old);
    }
    status
}

const MARK_START: &[u8] = b"v v v v v v v";
const MARK_BASE_END: &[u8] = b"=============";
const MARK_NEXT: &[u8] = b"*************";
const MARK_END: &[u8] = b"^ ^ ^ ^ ^ ^ ^";

/// A part of a file with Darcs conflict markup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkupPart {
    /// Lines outside conflicts (with their line endings).
    Common(Vec<Vec<u8>>),
    /// One conflict: the original lines and the conflicting versions, in
    /// the order Darcs wrote them (which isn't local first).
    Conflict {
        base: Vec<Vec<u8>>,
        versions: Vec<Vec<Vec<u8>>>,
    },
}

/// A working file with Darcs conflict markup:
///
/// ```text
/// v v v v v v v
/// original lines
/// =============
/// one version
/// *************
/// another version
/// ^ ^ ^ ^ ^ ^ ^
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictMarkup {
    pub parts: Vec<MarkupPart>,
}

fn marker_line(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

impl ConflictMarkup {
    /// Parse `content`; `None` if it holds no complete conflict.
    pub fn parse(content: &[u8]) -> Option<ConflictMarkup> {
        let lines: Vec<&[u8]> = content.split_inclusive(|&b| b == b'\n').collect();
        let mut parts = Vec::new();
        let mut common: Vec<Vec<u8>> = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            if marker_line(lines[i]) != MARK_START {
                common.push(lines[i].to_vec());
                i += 1;
                continue;
            }
            // Find the end of this conflict; an unterminated one is text
            let Some(end) = (i + 1..lines.len()).find(|&j| marker_line(lines[j]) == MARK_END)
            else {
                common.extend(lines[i..].iter().map(|l| l.to_vec()));
                break;
            };
            let body = &lines[i + 1..end];
            let (base, rest) = match body.iter().position(|l| marker_line(l) == MARK_BASE_END) {
                Some(p) => (&body[..p], &body[p + 1..]),
                None => (&body[..0], body),
            };
            let versions = rest
                .split(|l| marker_line(l) == MARK_NEXT)
                .map(|v| v.iter().map(|l| l.to_vec()).collect())
                .collect();
            if !common.is_empty() {
                parts.push(MarkupPart::Common(std::mem::take(&mut common)));
            }
            parts.push(MarkupPart::Conflict {
                base: base.iter().map(|l| l.to_vec()).collect(),
                versions,
            });
            i = end + 1;
        }
        if !common.is_empty() {
            parts.push(MarkupPart::Common(common));
        }
        parts
            .iter()
            .any(|p| matches!(p, MarkupPart::Conflict { .. }))
            .then_some(ConflictMarkup { parts })
    }

    /// For each conflict, the index of the local version. Darcs doesn't
    /// write the local version first, so it is told apart with `local`,
    /// the file before the conflicting pull: the version found in it, if
    /// only one is (an empty version, i.e. a deletion, is local when the
    /// other isn't found). Without `local`, or when that doesn't decide,
    /// the first version.
    pub fn local_versions(&self, local: Option<&[u8]>) -> Vec<usize> {
        let local_lines: Vec<&[u8]> = local
            .map(|l| l.split_inclusive(|&b| b == b'\n').collect())
            .unwrap_or_default();
        let contains = |version: &[Vec<u8>]| {
            !version.is_empty()
                && local_lines.windows(version.len()).any(|w| {
                    w.iter()
                        .zip(version)
                        .all(|(a, b)| marker_line(a) == marker_line(b))
                })
        };
        self.conflicts()
            .map(|(_, versions)| {
                let found: Vec<usize> = (0..versions.len())
                    .filter(|&k| contains(&versions[k]))
                    .collect();
                let empty: Vec<usize> = (0..versions.len())
                    .filter(|&k| versions[k].is_empty())
                    .collect();
                match (found.as_slice(), empty.as_slice()) {
                    _ if local.is_none() => 0,
                    ([k], _) => *k,
                    ([], [k]) => *k,
                    _ => 0,
                }
            })
            .collect()
    }

    fn conflicts(&self) -> impl Iterator<Item = (&Vec<Vec<u8>>, &Vec<Vec<Vec<u8>>>)> {
        self.parts.iter().filter_map(|p| match p {
            MarkupPart::Conflict { base, versions } => Some((base, versions)),
            MarkupPart::Common(_) => None,
        })
    }

    /// One side of the conflicts: the file with each conflict replaced by
    /// its original lines (base), its local version (`local[n]` for the
    /// n-th conflict) or the first other version (remote). The merged
    /// side is the file with its markup, so `Merged` gives the base.
    pub fn side(&self, side: ConflictSide, local: &[usize]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut n = 0;
        for part in &self.parts {
            match part {
                MarkupPart::Common(lines) => out.extend(lines.iter().flatten()),
                MarkupPart::Conflict { base, versions } => {
                    let last = versions.len().saturating_sub(1);
                    let mine = local.get(n).copied().unwrap_or(0).min(last);
                    let theirs = (0..versions.len()).find(|&k| k != mine).unwrap_or(mine);
                    let lines = match side {
                        ConflictSide::Local => &versions[mine],
                        ConflictSide::Remote => &versions[theirs],
                        ConflictSide::Base | ConflictSide::Merged => base,
                    };
                    out.extend(lines.iter().flatten());
                    n += 1;
                }
            }
        }
        out
    }
}

/// Whether `path` contains Darcs conflict markup.
pub fn has_conflict_markup(path: &Path) -> bool {
    std::fs::read(path)
        .map(|c| {
            let mut lines = c.split(|&b| b == b'\n').map(marker_line);
            lines.any(|l| l == MARK_START) && lines.any(|l| l == MARK_END)
        })
        .unwrap_or(false)
}

/// The newest backup `<name>.~<n>~` that Darcs made of `path` before
/// writing conflict markup into it, skipping backups that hold markup
/// themselves (made by later, aborted pulls).
pub fn markup_backup(path: &Path) -> Option<PathBuf> {
    let dir = paths::parent_or_self(path);
    let name = path.file_name()?.to_string_lossy().into_owned();
    let prefix = format!("{name}.~");
    let mut backups: Vec<(u64, PathBuf)> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter_map(|child| {
            let file_name = child.file_name().to_string_lossy().into_owned();
            let n = file_name.strip_prefix(&prefix)?.strip_suffix('~')?;
            Some((n.parse().ok()?, child.path()))
        })
        .collect();
    backups.sort();
    backups
        .into_iter()
        .rev()
        .map(|(_, p)| p)
        .find(|p| !has_conflict_markup(p))
}

/// Darcs' boring file rules: extended regular expressions from
/// `_darcs/prefs/boring`, the file named by the `boringfile` preference
/// and `~/.darcs/boring`, matched against repository-relative paths
/// (`dir/file`; folders with a trailing `/`). Comment (`#`) and blank
/// lines are skipped, as are expressions Rust's regex engine rejects.
#[derive(Debug, Clone, Default)]
pub struct BoringRules {
    pub regexes: Vec<Regex>,
}

impl BoringRules {
    pub fn load(root: &Path) -> BoringRules {
        let prefs = root.join("_darcs").join("prefs");
        let mut files = vec![prefs.join("boring")];
        if let Ok(text) = std::fs::read_to_string(prefs.join("prefs")) {
            for line in text.lines() {
                if let Some(file) = line.strip_prefix("boringfile ") {
                    files.push(root.join(file.trim()));
                }
            }
        }
        if let Some(home) = dirs::home_dir() {
            files.push(home.join(".darcs").join("boring"));
        }
        let mut rules = BoringRules::default();
        for file in files {
            if let Ok(text) = std::fs::read_to_string(file) {
                rules.add(&text);
            }
        }
        rules
    }

    pub fn add(&mut self, text: &str) {
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match Regex::new(line) {
                Ok(re) => self.regexes.push(re),
                Err(e) => log::debug!("skipping boring regex {line:?}: {e}"),
            }
        }
    }

    /// Whether repository-relative `rel` (folders ending in `/`) is boring.
    pub fn is_boring(&self, rel: &str) -> bool {
        self.regexes.iter().any(|re| re.is_match(rel))
    }
}

/// The Darcs backend.
#[derive(Debug, Clone)]
pub struct Darcs {
    core: VcCore,
    reverse_rename_cache: HashMap<PathBuf, PathBuf>,
}

impl Darcs {
    pub fn new(path: &Path) -> Result<Darcs> {
        Ok(Darcs {
            core: VcCore::new(VcKind::Darcs, path)?,
            reverse_rename_cache: HashMap::new(),
        })
    }

    fn files_cmd(&self, args: &[&str], files: &[PathBuf]) -> VcCommand {
        VcCommand::new(CMD, args, files, &self.core.root)
    }

    /// Replace the cached state with `status`, which always covers the
    /// whole repository; stale entries (e.g. a modified file edited back)
    /// are dropped.
    fn apply(&mut self, status: DarcsStatus) {
        let cache: &mut TreeCache = &mut self.core.cache;
        cache.states.clear();
        cache.missing.clear();
        for (path, state) in &status.states {
            cache.add_missing_entry(path, *state);
        }
        cache.states.extend(status.states);
        cache.meta = status.meta;
        self.reverse_rename_cache = status.reverse_renames;
    }

    /// Modified files with conflict markup are conflicts.
    fn mark_conflicts(&mut self) {
        for (path, state) in self.core.cache.states.iter_mut() {
            if *state == State::Modified && path.is_file() && has_conflict_markup(path) {
                *state = State::Conflict;
            }
        }
    }

    /// Mark the boring files and folders below the location that darcs
    /// left out of `whatsnew -l` as ignored. Boring files that are
    /// recorded (added with `darcs add --boring`) are not; the recorded
    /// files are listed only once something boring is found.
    fn mark_boring(&mut self) {
        let rules = BoringRules::load(&self.core.root);
        if rules.regexes.is_empty() {
            return;
        }
        let root = self.core.root.clone();
        let mut recorded: Option<HashSet<PathBuf>> = None;
        let mut ignored = Vec::new();
        let mut stack = vec![self.core.location.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(iter) = std::fs::read_dir(&dir) else {
                continue;
            };
            for child in iter.flatten() {
                let path = child.path();
                let is_dir = child.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if dir == root && child.file_name() == VcKind::Darcs.vc_dir() {
                    continue;
                }
                if !self.core.cache.states.contains_key(&path) {
                    let mut rel = paths::lossy(&paths::relpath(&path, &root)).replace('\\', "/");
                    if is_dir {
                        rel.push('/');
                    }
                    if rules.is_boring(&rel) {
                        let recorded = recorded.get_or_insert_with(|| self.recorded_files());
                        if !recorded.contains(&path) {
                            ignored.push(path);
                            continue;
                        }
                    }
                }
                if is_dir {
                    stack.push(path);
                }
            }
        }
        for path in ignored {
            self.core.cache.states.insert(path, State::Ignored);
        }
    }

    /// Recorded files and folders (`darcs show files`).
    fn recorded_files(&self) -> HashSet<PathBuf> {
        command::output_text(CMD, &["show", "files"], Some(&self.core.root))
            .map(|out| {
                out.lines()
                    .map(|l| paths::absolutize(&self.core.root, Path::new(l)))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Vc for Darcs {
    fn kind(&self) -> VcKind {
        VcKind::Darcs
    }
    fn core(&self) -> &VcCore {
        &self.core
    }
    fn core_mut(&mut self) -> &mut VcCore {
        &mut self.core
    }

    fn supported_actions(&self) -> &'static [Action] {
        &[
            Action::Update,
            Action::Push,
            Action::Commit,
            Action::Add,
            Action::Remove,
            Action::Revert,
            Action::Resolve,
        ]
    }

    /// `path` is ignored for the darcs call itself (darcs fails when given
    /// an empty argument), as in Python; the whole repository is
    /// refreshed.
    fn update_tree_state_cache(&mut self, _path: &Path) -> Result<()> {
        let output = command::output_text(
            CMD,
            &["whatsnew", "-sl", "--machine-readable"],
            Some(&self.core.location),
        )?;
        let status = parse_whatsnew(&output, &self.core.root);
        self.apply(status);
        self.mark_conflicts();
        self.mark_boring();
        Ok(())
    }

    /// Sides rebuilt from the conflict markup in the working file (the
    /// merged side): base from the original lines of each conflict, local
    /// and remote from the conflicting versions. Which version is local
    /// is found with the backup `<file>.~<n>~` Darcs makes before writing
    /// markup (see [`ConflictMarkup::local_versions`]). Lines outside
    /// conflicts, including changes merged cleanly from either side, are
    /// the same on every side.
    fn get_path_for_conflict(&self, path: &Path, side: ConflictSide) -> Result<(PathBuf, bool)> {
        let rel = self.core.repo_relative(NAME, path)?;
        let abs = paths::abspath(path);
        if side == ConflictSide::Merged {
            return Ok((abs, false));
        }
        let content = std::fs::read(&abs)?;
        let Some(markup) = ConflictMarkup::parse(&content) else {
            return unsupported(self.kind(), "conflicts without conflict markup");
        };
        let backup = markup_backup(&abs).and_then(|b| std::fs::read(b).ok());
        let local = markup.local_versions(backup.as_deref());
        let suffix = paths::suffix(Path::new(&rel));
        let temp = command::write_temp(&markup.side(side, &local), side.label(), &suffix)?;
        Ok((temp, true))
    }

    fn get_path_for_repo_file(&self, path: &Path, commit: Option<&str>) -> Result<PathBuf> {
        if commit.is_some() {
            return unsupported(self.kind(), "files at a given commit");
        }
        self.core.repo_relative(NAME, path)?;
        // `darcs show contents` needs the path before a rename
        let abs = paths::abspath(path);
        let original = self.reverse_rename_cache.get(&abs).unwrap_or(&abs);
        let rel = self.core.repo_relative(NAME, original)?;
        let suffix = paths::suffix(Path::new(&rel));
        command::call_temp_output(
            CMD,
            &["show", "contents", &rel],
            &self.core.root,
            "",
            &suffix,
        )
    }

    /// Records the changes to the selected `files` (Python recorded every
    /// change, although the commit dialog lists only the selection).
    /// Darcs rejects patch names with line breaks, so a multi-line message
    /// is passed in a temporary log file (first line: patch name, rest:
    /// long comment) that darcs deletes after recording.
    fn commit(&self, files: &[PathBuf], message: &str) -> Result<Vec<VcCommand>> {
        let message = message.trim_end();
        if !message.contains('\n') {
            return Ok(vec![
                self.files_cmd(&["record", "-a", "-m", message], files),
            ]);
        }
        let logfile = command::write_temp(format!("{message}\n").as_bytes(), "commit", ".txt")?;
        let logfile = paths::lossy(&logfile);
        Ok(vec![self.files_cmd(
            &["record", "-a", "--logfile", &logfile, "--delete-logfile"],
            files,
        )])
    }
    fn update(&self) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["pull", "-a"], &[])])
    }
    fn push(&self) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["push", "-a"], &[])])
    }
    fn add(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["add", "-r"], files)])
    }
    fn remove(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["remove", "-r"], files)])
    }
    fn revert(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["revert", "-a"], files)])
    }
    /// Darcs can't mark conflicts resolved: recording the edited file
    /// records the resolution. Files that still contain conflict markup
    /// are refused; otherwise `darcs mark-conflicts --dry-run` (read-only)
    /// lists any conflicts left without markup, and the view refreshes.
    fn resolve(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        if let Some(file) = files.iter().find(|f| has_conflict_markup(f)) {
            return Err(VcError::ConflictMarkers {
                vc: NAME,
                path: file.clone(),
            });
        }
        Ok(vec![self.files_cmd(&["mark-conflicts", "--dry-run"], &[])])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `darcs whatsnew -sl --machine-readable` in darcstest2 from
    /// test/testdarcs.sh.
    const WHATSNEW: &str = "\
F ./conflict2.txt
T ./conflict2-moved.txt
F ./renamed-dir
T ./renamed-new-dir
F ./renamed.txt
T ./renamed-new.txt
R ./deleted-dir/
R ./deleted.txt
M ./modified.txt
A ./new-dir/
A ./new-dir/new-dir-file.txt
A ./new.txt
a ./unknown.txt
M! ./conflict.txt
R ./kept.txt
a ./kept.txt
a ./sub/new file.txt
";

    #[test]
    fn parses_testdarcs_whatsnew() {
        let root = Path::new("/d");
        let st = parse_whatsnew(WHATSNEW, root);
        let s = |p: &str| st.states[Path::new(p)];
        assert_eq!(s("/d/conflict2.txt"), State::NonExist);
        assert_eq!(s("/d/conflict2-moved.txt"), State::Renamed);
        assert_eq!(s("/d/renamed-new-dir"), State::Renamed);
        assert_eq!(s("/d/deleted-dir"), State::Removed);
        assert_eq!(s("/d/deleted.txt"), State::Removed);
        assert_eq!(s("/d/modified.txt"), State::Modified);
        assert_eq!(s("/d/new-dir/new-dir-file.txt"), State::New);
        assert_eq!(s("/d/unknown.txt"), State::Unversioned);
        assert_eq!(s("/d/conflict.txt"), State::Conflict);
        // Removed with `darcs remove`, which keeps the file
        assert_eq!(s("/d/kept.txt"), State::Removed);
        assert_eq!(s("/d/sub/new file.txt"), State::Unversioned);
        assert_eq!(
            parse_whatsnew("No changes!\n", root),
            DarcsStatus::default()
        );
        assert_eq!(
            st.meta[Path::new("/d/renamed-new.txt")],
            "renamed.txt ➡ renamed-new.txt"
        );
        assert_eq!(
            st.reverse_renames[Path::new("/d/conflict2-moved.txt")],
            PathBuf::from("/d/conflict2.txt")
        );

        let mut vc = Darcs {
            core: VcCore {
                root: root.into(),
                location: root.into(),
                cache: TreeCache::default(),
            },
            reverse_rename_cache: HashMap::new(),
        };
        vc.apply(st);
        assert!(vc.core.cache.missing[root].contains("deleted.txt"));
        let c = &vc.commit(&[], "msg\n").unwrap()[0];
        assert_eq!(c.argv(), vec!["record", "-a", "-m", "msg"]);
        let c = &vc.commit(&[root.join("a.txt")], "msg").unwrap()[0];
        assert_eq!(c.argv(), vec!["record", "-a", "-m", "msg", "a.txt"]);

        let c = &vc.commit(&[root.join("a.txt")], "Title\n\nBody").unwrap()[0];
        let argv = c.argv();
        assert_eq!(argv[..3], ["record", "-a", "--logfile"]);
        assert_eq!(argv[4..], ["--delete-logfile", "a.txt"]);
        let logfile = PathBuf::from(&argv[3]);
        assert_eq!(
            std::fs::read_to_string(&logfile).unwrap(),
            "Title\n\nBody\n"
        );
        std::fs::remove_file(logfile).unwrap();
    }

    /// A file after a conflicting `darcs pull` (Darcs 2.18).
    const MARKUP: &str = "\
a
v v v v v v v
b
=============
MAIN
*************
OTHER
^ ^ ^ ^ ^ ^ ^
c
v v v v v v v
d
=============
*************
D2
^ ^ ^ ^ ^ ^ ^
E
";

    #[test]
    fn parses_conflict_markup() {
        let m = ConflictMarkup::parse(MARKUP.as_bytes()).unwrap();
        assert_eq!(m.parts.len(), 5);
        let text = |side, local: &[usize]| String::from_utf8(m.side(side, local)).unwrap();
        assert_eq!(text(ConflictSide::Base, &[]), "a\nb\nc\nd\nE\n");
        assert_eq!(text(ConflictSide::Local, &[0, 0]), "a\nMAIN\nc\nE\n");
        assert_eq!(text(ConflictSide::Remote, &[0, 0]), "a\nOTHER\nc\nD2\nE\n");
        assert_eq!(text(ConflictSide::Local, &[1, 1]), "a\nOTHER\nc\nD2\nE\n");
        assert_eq!(text(ConflictSide::Remote, &[1, 1]), "a\nMAIN\nc\nE\n");

        // The local file decides which version is local; an empty version
        // (local deletion) is local when the other isn't in the local file.
        assert_eq!(m.local_versions(None), vec![0, 0]);
        assert_eq!(m.local_versions(Some(b"a\nOTHER\nc\nE\n")), vec![1, 0]);
        assert_eq!(m.local_versions(Some(b"a\nMAIN\nc\nD2\nE\n")), vec![0, 1]);
        assert_eq!(m.local_versions(Some(b"MAIN\nOTHER\n")), vec![0, 0]);

        // CRLF files keep their line endings
        let crlf = MARKUP.replace('\n', "\r\n");
        let m = ConflictMarkup::parse(crlf.as_bytes()).unwrap();
        assert_eq!(
            m.side(ConflictSide::Base, &[]),
            b"a\r\nb\r\nc\r\nd\r\nE\r\n".to_vec()
        );

        assert_eq!(ConflictMarkup::parse(b"a\nb\n"), None);
        // An unterminated conflict is plain text
        assert_eq!(ConflictMarkup::parse(b"v v v v v v v\nb\n"), None);
    }

    #[test]
    fn markup_files_and_backups() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.txt");
        std::fs::write(&f, MARKUP).unwrap();
        assert!(has_conflict_markup(&f));
        assert_eq!(markup_backup(&f), None);
        std::fs::write(dir.path().join("f.txt.~0~"), "a\nMAIN\n").unwrap();
        std::fs::write(dir.path().join("f.txt.~1~"), MARKUP).unwrap();
        std::fs::write(dir.path().join("f.txt.~x~"), "").unwrap();
        assert_eq!(markup_backup(&f), Some(dir.path().join("f.txt.~0~")));
        std::fs::write(dir.path().join("f.txt.~10~"), "newer\n").unwrap();
        assert_eq!(markup_backup(&f), Some(dir.path().join("f.txt.~10~")));
        std::fs::write(&f, "a\nv v v v v v v\n").unwrap();
        assert!(!has_conflict_markup(&f));
        assert!(!has_conflict_markup(&dir.path().join("missing")));
    }

    #[test]
    fn boring_rules() {
        let mut rules = BoringRules::default();
        // Excerpts of Darcs 2.18's default boring file
        rules.add(
            "# comment\n\n\\.o$\n(^|/)\\.git/\n\\_vti_cnf$\n\\#\n(^|/)\\{arch\\}/\n\
             (^|/)\\.waf-[[:digit:].]+-[[:digit:]]+/\n~$\n(^|/|\\.)core$\n^\\.#\n",
        );
        assert_eq!(rules.regexes.len(), 9);
        for rel in [
            "x.o",
            "sub/x.o",
            ".git/",
            "sub/.git/",
            "a_vti_cnf",
            "#a#",
            "{arch}/",
            ".waf-1.8-12/",
            "f.txt.~0~",
            "core",
            ".#lock",
        ] {
            assert!(rules.is_boring(rel), "{rel}");
        }
        for rel in ["x.obj", ".git", "src/", "core.c", "a/.lock", "x.o.c"] {
            assert!(!rules.is_boring(rel), "{rel}");
        }
        // Expressions Rust can't compile are skipped
        rules.add("(unclosed\n");
        assert_eq!(rules.regexes.len(), 9);

        let dir = tempfile::tempdir().unwrap();
        let prefs = dir.path().join("_darcs/prefs");
        std::fs::create_dir_all(&prefs).unwrap();
        std::fs::write(prefs.join("boring"), "\\.o$\n").unwrap();
        std::fs::write(prefs.join("prefs"), "boringfile .myboring\n").unwrap();
        std::fs::write(dir.path().join(".myboring"), "^build/$\n").unwrap();
        let rules = BoringRules::load(dir.path());
        assert!(rules.is_boring("a.o") && rules.is_boring("build/"));
    }

    #[test]
    fn versions() {
        assert!(version_supported("2.16.5 (release)\n"));
        assert!(version_supported("2.10.3\n"));
        assert!(!version_supported("2.10.2\n"));
        assert!(!version_supported("2.8.4 (release)\n"));
        assert!(!version_supported("garbage"));
    }
}
