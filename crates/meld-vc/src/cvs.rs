//! CVS backend (port of `meld/vc/cvs.py`). CVSNT is used instead of CVS
//! when installed.
//!
//! Differences from Python, found by testing against CVS 1.12.13:
//! - Status is also queried for the files recorded in each folder's
//!   `CVS/Entries`, so removed (`cvs remove`) and missing files, which are
//!   no longer on disk, are reported.
//! - All `cvs status` values are mapped: conflicts, files that are out of
//!   date (`Needs Patch`, `Needs Merge`), and versioned files missing from
//!   disk (`no file ... Needs Checkout`). Python showed them as
//!   unversioned.
//! - Folders without a `CVS` metadata folder are unversioned (Python had
//!   this heuristic commented out).
//! - The base revision for comparisons is the working revision from
//!   `CVS/Entries`, read with `cvs checkout -p -r <rev> <module>/<file>`.
//!   Python's `cvs update -p` returned the newest repository revision
//!   (someone else's changes when the file is out of date) and nothing at
//!   all for files with unresolved conflicts.
//! - Revert resurrects removed files with `cvs add`; `cvs update -C`
//!   leaves them removed.
//! - Unversioned files and folders matching CVS's ignore rules are
//!   ignored rather than unversioned ([`IgnoreRules`]; `cvs status`
//!   reports them as unknown).
//! - Conflicts left by `cvs update` are compared 3-way: local is the
//!   backup `.#<name>.<rev>` CVS made of the modified file before merging,
//!   base that backup's revision, remote the revision merged in (the one
//!   now in `CVS/Entries`). CVS has no "resolved" flag: a file without
//!   conflict markers is just modified. Resolve refuses files that still
//!   contain markers and otherwise only re-checks their status.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::command::{self, VcCommand};
use crate::entry::{Action, ConflictSide, State};
use crate::error::{Result, VcError};
use crate::paths;
use crate::vc::{TreeCache, Vc, VcCore, VcKind, unsupported};

const NAME: &str = "CVS";
const VC_DIR: &str = "CVS";

/// Map the `Status:` value of `cvs status` to a state. `present` is false
/// for `File: no file <name>` lines, i.e. files that are not on disk.
pub fn state_for_key(key: &str, present: bool) -> State {
    match key {
        "Unknown" => State::Unversioned,
        "Locally Added" => State::New,
        "Locally Removed" => State::Removed,
        _ if !present => State::Missing,
        "Up-to-date" | "Needs Patch" | "Needs Checkout" => State::Normal,
        "Locally Modified" | "Needs Merge" => State::Modified,
        "Unresolved Conflict" | "File had conflicts on merge" => State::Conflict,
        "Entry Invalid" => State::Error,
        _ => State::Unversioned,
    }
}

/// Apply `cvs -Q status <files>` output. `files` are the queried paths
/// relative to `location`, in the order they were passed; CVS prints one
/// `File:` line per file in the same order.
pub fn apply_status(
    cache: &mut TreeCache,
    location: &Path,
    files: &[String],
    output: &str,
    single_file: Option<&Path>,
) -> Result<()> {
    let entries: Vec<&str> = output.lines().filter(|l| l.starts_with("File:")).collect();
    if entries.is_empty()
        && let Some(file) = single_file
    {
        cache.states.insert(file.to_path_buf(), State::Normal);
        return Ok(());
    }
    for (file, entry) in files.iter().zip(entries) {
        let statekey = entry.rsplit(':').next().unwrap_or_default().trim();
        let present = !entry.starts_with("File: no file ");
        let name = file.trim();
        let base = Path::new(name)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !entry.contains(&base) {
            return Err(VcError::Parse {
                vc: NAME,
                reason: format!("CVS status for {name} does not match {entry}"),
            });
        }
        let path = paths::absolutize(location, Path::new(name));
        let state = state_for_key(statekey, present);
        cache.states.insert(path.clone(), state);
        cache.add_missing_entry(&path, state);
    }
    Ok(())
}

/// A file recorded in a `CVS/Entries` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CvsEntry {
    pub name: String,
    /// Working revision: `0` for added files, `-<rev>` for removed ones.
    pub revision: String,
}

/// Parse the contents of `CVS/Entries` (file entries only; folder entries
/// start with `D`).
pub fn parse_entries(text: &str) -> Vec<CvsEntry> {
    text.lines().filter_map(parse_entry_line).collect()
}

fn parse_entry_line(line: &str) -> Option<CvsEntry> {
    let mut fields = line.strip_prefix('/')?.split('/');
    let name = fields.next().filter(|n| !n.is_empty())?;
    Some(CvsEntry {
        name: name.to_string(),
        revision: fields.next().unwrap_or_default().to_string(),
    })
}

/// The file entries of folder `dir`: `CVS/Entries` plus the additions and
/// removals still pending in `CVS/Entries.Log`.
pub fn read_entries(dir: &Path) -> Vec<CvsEntry> {
    let cvs = dir.join(VC_DIR);
    let mut entries = std::fs::read_to_string(cvs.join("Entries"))
        .map(|t| parse_entries(&t))
        .unwrap_or_default();
    if let Ok(log) = std::fs::read_to_string(cvs.join("Entries.Log")) {
        for line in log.lines() {
            let (op, rest) = line.split_at(line.len().min(2));
            let Some(entry) = parse_entry_line(rest) else {
                continue;
            };
            entries.retain(|e| e.name != entry.name);
            if op == "A " {
                entries.push(entry);
            }
        }
    }
    entries
}

/// The repository (`CVS/Root`) and module path (`CVS/Repository`, made
/// relative to the repository if CVS stored it absolute) of folder `dir`.
pub fn read_module_path(dir: &Path) -> Option<(String, String)> {
    let cvs = dir.join(VC_DIR);
    let root = std::fs::read_to_string(cvs.join("Root")).ok()?;
    let root = root.lines().next()?.trim().to_string();
    let repository = std::fs::read_to_string(cvs.join("Repository")).ok()?;
    let mut module = repository.lines().next()?.trim();
    if module.starts_with('/') {
        // The repository path is what follows the method/host part
        let root_path = root.rsplit(':').next().unwrap_or(&root);
        module = module.strip_prefix(root_path)?;
    }
    let module = module.trim_matches('/');
    (!root.is_empty() && !module.is_empty()).then(|| (root, module.to_string()))
}

/// CVS's default ignore list (`ign_default` in CVS 1.12's `src/ignore.c`).
pub const DEFAULT_IGNORE: &str = ". .. core RCSLOG tags TAGS RCS SCCS .make.state \
     .nse_depinfo #* .#* cvslog.* ,* CVS CVS.adm .del-* *.a *.olb *.o *.obj *.so *.Z \
     *~ *.old *.elc *.ln *.bak *.BAK *.orig *.rej *.exe _$* *$";

/// CVS's rules for ignoring unversioned files ("Ignoring files via
/// cvsignore" in the CVS manual): the default list, then the
/// repository's `CVSROOT/cvsignore` (read when the repository is local),
/// `~/.cvsignore`, the `CVSIGNORE` environment variable and finally the
/// `.cvsignore` of the file's own folder. Each source is a
/// whitespace-separated list of shell wildcards matched against file
/// names; a `!` clears the list built so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IgnoreRules {
    /// Patterns that apply to every folder.
    pub global: Vec<String>,
}

impl IgnoreRules {
    /// The global rules for a checkout whose `CVS/Root` is `cvsroot`.
    pub fn load(cvsroot: Option<&str>) -> IgnoreRules {
        IgnoreRules::load_from(
            cvsroot,
            dirs::home_dir().as_deref(),
            std::env::var("CVSIGNORE").ok().as_deref(),
        )
    }

    /// [`IgnoreRules::load`] with the home folder and the value of
    /// `CVSIGNORE` given.
    pub fn load_from(
        cvsroot: Option<&str>,
        home: Option<&Path>,
        cvsignore_var: Option<&str>,
    ) -> IgnoreRules {
        let mut rules = IgnoreRules::default();
        add_patterns(&mut rules.global, DEFAULT_IGNORE);
        let files = [
            cvsroot
                .and_then(local_repository)
                .map(|repo| repo.join("CVSROOT").join("cvsignore")),
            home.map(|h| h.join(".cvsignore")),
        ];
        for file in files.into_iter().flatten() {
            if let Ok(text) = std::fs::read_to_string(file) {
                add_patterns(&mut rules.global, &text);
            }
        }
        if let Some(text) = cvsignore_var {
            add_patterns(&mut rules.global, text);
        }
        rules
    }

    /// The patterns for files in folder `dir` (global ones plus its
    /// `.cvsignore`).
    pub fn for_folder(&self, dir: &Path) -> Vec<String> {
        let mut patterns = self.global.clone();
        if let Ok(text) = std::fs::read_to_string(dir.join(".cvsignore")) {
            add_patterns(&mut patterns, &text);
        }
        patterns
    }
}

fn add_patterns(patterns: &mut Vec<String>, text: &str) {
    for word in text.split_whitespace() {
        if word == "!" {
            patterns.clear();
        } else {
            patterns.push(word.to_string());
        }
    }
}

/// Whether file name `name` matches any of `patterns`.
pub fn is_ignored_name(patterns: &[String], name: &str) -> bool {
    patterns.iter().any(|p| fnmatch(p, name))
}

/// The repository folder of a local `CVSROOT` (`/path` or
/// `:local:/path`); `None` for remote repositories.
fn local_repository(cvsroot: &str) -> Option<PathBuf> {
    let path = cvsroot
        .strip_prefix(":local:")
        .or_else(|| cvsroot.strip_prefix(":fork:"))
        .unwrap_or(cvsroot);
    path.starts_with('/').then(|| PathBuf::from(path))
}

/// Shell wildcard matching like `fnmatch(pattern, name, 0)`: `*`, `?`,
/// `[...]` (`!` or `^` negates, `a-z` ranges) and `\` escapes; `*` also
/// matches leading dots.
pub fn fnmatch(pattern: &str, name: &str) -> bool {
    fn class(p: &[char], c: char) -> Option<(bool, usize)> {
        // Returns (matched, length of the class in p) or None if unclosed
        let mut i = 1;
        let negate = matches!(p.get(i), Some('!' | '^'));
        if negate {
            i += 1;
        }
        let mut matched = false;
        let mut first = true;
        while i < p.len() && (first || p[i] != ']') {
            first = false;
            let mut lo = p[i];
            if lo == '\\' && i + 1 < p.len() {
                i += 1;
                lo = p[i];
            }
            if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
                let hi = p[i + 2];
                matched |= lo <= c && c <= hi;
                i += 3;
            } else {
                matched |= lo == c;
                i += 1;
            }
        }
        (i < p.len()).then_some((matched != negate, i + 1))
    }
    fn go(p: &[char], n: &[char]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some('*') => (0..=n.len()).any(|k| go(&p[1..], &n[k..])),
            Some('?') => !n.is_empty() && go(&p[1..], &n[1..]),
            Some('[') => match (n.first(), class(p, n.first().copied().unwrap_or('\0'))) {
                (Some(_), Some((true, len))) => go(&p[len..], &n[1..]),
                (Some(_), Some((false, _))) | (None, _) => false,
                // An unclosed bracket is a literal
                (Some(&c), None) => c == '[' && go(&p[1..], &n[1..]),
            },
            Some('\\') if p.len() > 1 => n.first() == Some(&p[1]) && go(&p[2..], &n[1..]),
            Some(&c) => n.first() == Some(&c) && go(&p[1..], &n[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    go(&p, &n)
}

/// What [`find_files`] found below a folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FoundFiles {
    /// Files to query with `cvs status`, relative to the start folder.
    files: Vec<String>,
    /// Folders without CVS metadata (not ignored).
    unversioned: Vec<PathBuf>,
    /// Unversioned files and folders matching the ignore rules; folders
    /// are not entered.
    ignored: Vec<PathBuf>,
}

/// All files below `dir` (on disk or recorded in `CVS/Entries`; `CVS`
/// metadata folders are skipped) relative to `relative_to`, in walk order,
/// the folders that are not under version control and the ignored files
/// and folders.
fn find_files(dir: &Path, relative_to: &Path, ignore: &IgnoreRules) -> FoundFiles {
    let mut found = FoundFiles::default();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(iter) = std::fs::read_dir(&current) else {
            continue;
        };
        let entries: BTreeSet<String> =
            read_entries(&current).into_iter().map(|e| e.name).collect();
        let patterns = ignore.for_folder(&current);
        let mut names = BTreeSet::new();
        let mut subdirs = Vec::new();
        for child in iter.flatten() {
            let path = child.path();
            let name = child.file_name().to_string_lossy().into_owned();
            let Ok(file_type) = child.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if name == VC_DIR {
                    continue;
                }
                let unversioned = !path.join(VC_DIR).is_dir();
                if unversioned && is_ignored_name(&patterns, &name) {
                    found.ignored.push(path);
                    continue;
                }
                if unversioned {
                    found.unversioned.push(path.clone());
                }
                subdirs.push(path);
            } else if !entries.contains(&name) && is_ignored_name(&patterns, &name) {
                found.ignored.push(path);
            } else {
                names.insert(name);
            }
        }
        names.extend(entries);
        found.files.extend(
            names
                .iter()
                .map(|n| paths::lossy(&paths::relpath(&current.join(n), relative_to))),
        );
        // Depth-first in name order, like os.walk's top-down traversal
        subdirs.sort();
        stack.extend(subdirs.into_iter().rev());
    }
    found
}

/// Compare dotted CVS revision numbers numerically (`1.10 > 1.9`).
fn revision_key(rev: &str) -> Vec<u64> {
    rev.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// The newest backup `.#<name>.<rev>` that `cvs update` made of the locally
/// modified file `name` in `dir` before merging into it, with `<rev>`.
pub fn conflict_backup(dir: &Path, name: &str) -> Option<(PathBuf, String)> {
    let prefix = format!(".#{name}.");
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|child| {
            let file_name = child.file_name().to_string_lossy().into_owned();
            let rev = file_name.strip_prefix(&prefix)?;
            let valid = !rev.is_empty() && rev.split('.').all(|p| p.parse::<u64>().is_ok());
            valid.then(|| (child.path(), rev.to_string()))
        })
        .max_by_key(|(_, rev)| revision_key(rev))
}

/// Whether `path` contains CVS's conflict markers (a line starting with
/// `<<<<<<< ` or `>>>>>>> `, or a `=======` line), as checked by `cvs
/// commit`.
pub fn has_conflict_markers(path: &Path) -> bool {
    let Ok(content) = std::fs::read(path) else {
        return false;
    };
    content.split(|&b| b == b'\n').any(|line| {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        line.starts_with(b"<<<<<<< ") || line.starts_with(b">>>>>>> ") || line == b"======="
    })
}

/// Every parent prefix of each repository-relative file, e.g. `a/b/c.txt`
/// gives `a`, `a/b`, `a/b/c.txt`: CVS needs unversioned parents added
/// together with the file.
pub fn add_arguments(relfiles: &[String]) -> Vec<String> {
    let mut args = Vec::new();
    for f in relfiles {
        let with_sep = format!("{f}/");
        for (i, ch) in with_sep.char_indices() {
            if ch == '/' {
                args.push(with_sep[..i].to_string());
            }
        }
    }
    args
}

/// The CVS backend.
#[derive(Debug, Clone)]
pub struct Cvs {
    core: VcCore,
    cmd: &'static str,
}

impl Cvs {
    pub fn new(path: &Path) -> Result<Cvs> {
        Ok(Cvs {
            core: VcCore::new(VcKind::Cvs, path)?,
            cmd: VcKind::Cvs.cmd().unwrap_or("cvs"),
        })
    }

    fn files_cmd(&self, args: &[&str], files: &[PathBuf]) -> VcCommand {
        VcCommand::new(self.cmd, args, files, &self.core.root)
    }

    fn ignore_rules(&self) -> IgnoreRules {
        let cvsroot = read_module_path(&self.core.root).map(|(root, _)| root);
        IgnoreRules::load(cvsroot.as_deref())
    }

    /// Revision `rev` of the file `name` in folder `dir` written to a
    /// temporary file. `cvs update -p` prints nothing for files with
    /// unresolved conflicts; `checkout -p` works whatever the file's state.
    fn revision_temp(
        &self,
        dir: &Path,
        name: &str,
        rev: &str,
        file_id: &str,
        suffix: &str,
    ) -> Result<Option<PathBuf>> {
        let Some((cvsroot, module)) = read_module_path(dir) else {
            return Ok(None);
        };
        let module_file = format!("{module}/{name}");
        command::call_temp_output(
            self.cmd,
            &[
                "-d",
                &cvsroot,
                "-Q",
                "checkout",
                "-p",
                "-r",
                rev,
                &module_file,
            ],
            dir,
            file_id,
            suffix,
        )
        .map(Some)
    }
}

/// Folder and file name of an absolute path.
fn split_path(abs: &Path) -> (PathBuf, String) {
    let dir = paths::parent_or_self(abs);
    let name = abs
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    (dir, name)
}

impl Vc for Cvs {
    fn kind(&self) -> VcKind {
        VcKind::Cvs
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
            Action::Commit,
            Action::Add,
            Action::Remove,
            Action::Revert,
            Action::Resolve,
        ]
    }

    fn update_tree_state_cache(&mut self, path: &Path) -> Result<()> {
        let abs = self.core.resolve(path);
        let ignore = self.ignore_rules();
        let found = if abs.is_dir() {
            find_files(&abs, &self.core.location, &ignore)
        } else {
            let (dir, name) = split_path(&abs);
            let versioned = read_entries(&dir).iter().any(|e| e.name == name);
            if !versioned && is_ignored_name(&ignore.for_folder(&dir), &name) {
                FoundFiles {
                    ignored: vec![abs.clone()],
                    ..Default::default()
                }
            } else {
                let rel = paths::lossy(&paths::relpath(&abs, &self.core.location));
                FoundFiles {
                    files: vec![rel],
                    ..Default::default()
                }
            }
        };
        let FoundFiles {
            files,
            unversioned,
            ignored,
        } = found;
        for dir in unversioned {
            self.core.cache.states.insert(dir, State::Unversioned);
        }
        for path in ignored {
            self.core.cache.states.insert(path, State::Ignored);
        }
        if files.is_empty() {
            return Ok(());
        }
        let mut args = vec!["-Q", "status"];
        args.extend(files.iter().map(String::as_str));
        let output = command::output_text(self.cmd, &args, Some(&self.core.location))?;
        let single = abs.is_file().then_some(abs);
        let location = self.core.location.clone();
        apply_status(
            &mut self.core.cache,
            &location,
            &files,
            &output,
            single.as_deref(),
        )
    }

    fn get_path_for_repo_file(&self, path: &Path, commit: Option<&str>) -> Result<PathBuf> {
        if commit.is_some() {
            return unsupported(self.kind(), "files at a given commit");
        }
        let rel = self.core.repo_relative(NAME, path)?;
        let suffix = paths::suffix(Path::new(&rel));
        let (dir, name) = split_path(&paths::abspath(path));
        let revision = read_entries(&dir)
            .into_iter()
            .find(|e| e.name == name)
            .map(|e| e.revision.trim_start_matches('-').to_string());
        match revision.as_deref() {
            // Added, not yet committed: no base content
            Some("0") => return command::write_temp(b"", "", &suffix),
            Some(rev) if !rev.is_empty() => {
                if let Some(temp) = self.revision_temp(&dir, &name, rev, "", &suffix)? {
                    return Ok(temp);
                }
            }
            _ => {}
        }
        command::call_temp_output(
            self.cmd,
            &["-q", "update", "-p", &rel],
            &self.core.root,
            "",
            &suffix,
        )
    }

    /// Local is CVS's backup of the file before the merge (owned by CVS,
    /// not temporary), base the backup's revision and remote the revision
    /// in `CVS/Entries`, which `cvs update` merged in. Without a backup
    /// (e.g. after deleting it) conflicts are compared 2-way, like files
    /// of backends without conflict support.
    fn get_path_for_conflict(&self, path: &Path, side: ConflictSide) -> Result<(PathBuf, bool)> {
        let rel = self.core.repo_relative(NAME, path)?;
        let abs = paths::abspath(path);
        if side == ConflictSide::Merged {
            return Ok((abs, false));
        }
        let (dir, name) = split_path(&abs);
        let Some((backup, base_rev)) = conflict_backup(&dir, &name) else {
            return unsupported(
                self.kind(),
                "conflicts without the backup of the local file",
            );
        };
        let revision = match side {
            ConflictSide::Local => return Ok((backup, false)),
            ConflictSide::Base => Some(base_rev),
            _ => read_entries(&dir)
                .into_iter()
                .find(|e| e.name == name)
                .map(|e| e.revision),
        };
        let suffix = paths::suffix(Path::new(&rel));
        match revision {
            Some(rev) if !rev.is_empty() && !rev.starts_with(['0', '-']) => {
                match self.revision_temp(&dir, &name, &rev, side.label(), &suffix)? {
                    Some(temp) => Ok((temp, true)),
                    None => unsupported(self.kind(), "conflicts without CVS/Repository"),
                }
            }
            _ => unsupported(self.kind(), "conflicts of files that are not checked out"),
        }
    }

    fn commit(&self, files: &[PathBuf], message: &str) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["commit", "-m", message], files)])
    }
    fn update(&self) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["update"], &[])])
    }
    fn add(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        let relfiles: Vec<String> = files
            .iter()
            .filter(|f| f.is_file())
            .map(|f| paths::lossy(&paths::relpath(f, &self.core.root)))
            .collect();
        let absargs: Vec<PathBuf> = add_arguments(&relfiles)
            .iter()
            .map(|a| self.core.root.join(a))
            .collect();
        Ok(vec![self.files_cmd(&["add"], &absargs)])
    }
    fn remove(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["remove", "-f"], files)])
    }
    /// `cvs update -C` restores modified and missing files but leaves
    /// removed ones removed (as Python did); those are resurrected with
    /// `cvs add`.
    fn revert(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        let (removed, others): (Vec<PathBuf>, Vec<PathBuf>) = files
            .iter()
            .cloned()
            .partition(|f| self.core.cache.state(&paths::abspath(f)) == State::Removed);
        let mut commands = Vec::new();
        if !removed.is_empty() {
            commands.push(self.files_cmd(&["add"], &removed));
        }
        if !others.is_empty() || removed.is_empty() {
            commands.push(self.files_cmd(&["update", "-C"], &others));
        }
        Ok(commands)
    }
    /// CVS can't mark conflicts resolved: a conflicted file whose markers
    /// have been edited out is reported as modified. Files that still
    /// contain markers are refused; for the others `cvs status` is run,
    /// which shows (and refreshes) their new state.
    fn resolve(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        if let Some(file) = files.iter().find(|f| has_conflict_markers(f)) {
            return Err(VcError::ConflictMarkers {
                vc: NAME,
                path: file.clone(),
            });
        }
        Ok(vec![self.files_cmd(&["-Q", "status"], files)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cvs -Q status` output of CVS 1.12.13 (shortened).
    const STATUS: &str = "\
===================================================================
File: normal.txt       \tStatus: Up-to-date

   Working revision:\t1.1.1.1\t2026-10-22 08:58:11 +0200
   Repository revision:\t1.1.1.1\t/cvsroot/proj/normal.txt,v
   Commit Identifier:\t1006AD9B403E5D45567

===================================================================
File: modified.txt     \tStatus: Locally Modified

===================================================================
File: new.txt          \tStatus: Locally Added

   Working revision:\tNew file!
   Repository revision:\tNo revision control file

===================================================================
File: no file gone.txt\t\tStatus: Locally Removed

===================================================================
File: no file missing.txt\t\tStatus: Needs Checkout

===================================================================
File: stale.txt        \tStatus: Needs Patch

===================================================================
File: conflict.txt     \tStatus: Unresolved Conflict

===================================================================
File: unknown.txt      \tStatus: Unknown

   Working revision:\tNo entry for unknown.txt
   Repository revision:\tNo revision control file
";

    #[test]
    fn parses_status() {
        let files: Vec<String> = [
            "normal.txt",
            "modified.txt",
            "new.txt",
            "sub/gone.txt",
            "missing.txt",
            "stale.txt",
            "conflict.txt",
            "unknown.txt",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let mut cache = TreeCache::default();
        apply_status(&mut cache, Path::new("/cvs"), &files, STATUS, None).unwrap();
        let st = |p: &str| cache.state(Path::new(p));
        assert_eq!(st("/cvs/normal.txt"), State::Normal);
        assert_eq!(st("/cvs/modified.txt"), State::Modified);
        assert_eq!(st("/cvs/new.txt"), State::New);
        assert_eq!(st("/cvs/sub/gone.txt"), State::Removed);
        assert_eq!(st("/cvs/missing.txt"), State::Missing);
        assert_eq!(st("/cvs/stale.txt"), State::Normal);
        assert_eq!(st("/cvs/conflict.txt"), State::Conflict);
        assert_eq!(st("/cvs/unknown.txt"), State::Unversioned);
        assert!(cache.missing[Path::new("/cvs/sub")].contains("gone.txt"));
        assert!(cache.missing[Path::new("/cvs")].contains("missing.txt"));

        let bad = vec!["other.txt".to_string()];
        assert!(apply_status(&mut cache, Path::new("/cvs"), &bad, STATUS, None).is_err());
    }

    #[test]
    fn add_includes_parents() {
        assert_eq!(
            add_arguments(&["a/b/c.txt".into(), "d.txt".into()]),
            vec!["a", "a/b", "a/b/c.txt", "d.txt"]
        );
    }

    #[test]
    fn entries() {
        let text = "/a.txt/1.2/Mon Jan  1 00:00:00 2026//\n\
                    /new.txt/0/dummy timestamp//\n\
                    /gone.txt/-1.1.1.1/Mon Jan  1 00:00:00 2026//\n\
                    D/sub////\n\
                    D\n";
        let e = parse_entries(text);
        assert_eq!(
            e.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["a.txt", "new.txt", "gone.txt"]
        );
        assert_eq!(e[0].revision, "1.2");
        assert_eq!(e[2].revision, "-1.1.1.1");

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("CVS")).unwrap();
        std::fs::write(dir.path().join("CVS/Entries"), text).unwrap();
        std::fs::write(
            dir.path().join("CVS/Entries.Log"),
            "A /added.txt/0/dummy//\nR /a.txt/1.2///\nA D/dir////\n",
        )
        .unwrap();
        let names: Vec<String> = read_entries(dir.path())
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["new.txt", "gone.txt", "added.txt"]);
    }

    #[test]
    fn module_paths() {
        let dir = tempfile::tempdir().unwrap();
        let cvs = dir.path().join("CVS");
        std::fs::create_dir(&cvs).unwrap();
        assert_eq!(read_module_path(dir.path()), None);
        std::fs::write(cvs.join("Root"), "/srv/cvsroot\n").unwrap();
        std::fs::write(cvs.join("Repository"), "proj/sub\n").unwrap();
        assert_eq!(
            read_module_path(dir.path()),
            Some(("/srv/cvsroot".into(), "proj/sub".into()))
        );
        // Old CVS versions store the absolute repository path
        std::fs::write(cvs.join("Root"), ":pserver:me@host:/srv/cvsroot\n").unwrap();
        std::fs::write(cvs.join("Repository"), "/srv/cvsroot/proj\n").unwrap();
        assert_eq!(
            read_module_path(dir.path()),
            Some((":pserver:me@host:/srv/cvsroot".into(), "proj".into()))
        );
    }

    #[test]
    fn finds_files_skipping_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("CVS")).unwrap();
        std::fs::create_dir_all(root.join("sub/CVS")).unwrap();
        std::fs::create_dir_all(root.join("unknown")).unwrap();
        std::fs::write(root.join("CVS/Entries"), b"/removed.txt/-1.1///\n").unwrap();
        std::fs::write(root.join("sub/CVS/Entries"), b"").unwrap();
        std::fs::write(root.join("a.txt"), b"").unwrap();
        std::fs::write(root.join("sub/b.txt"), b"").unwrap();
        std::fs::write(root.join("unknown/c.txt"), b"").unwrap();
        let found = find_files(root, root, &IgnoreRules::default());
        assert_eq!(
            found.files,
            vec!["a.txt", "removed.txt", "sub/b.txt", "unknown/c.txt"]
        );
        assert_eq!(found.unversioned, vec![root.join("unknown")]);
        assert!(found.ignored.is_empty());

        // Ignored files and folders are not queried; versioned files are
        // never ignored.
        std::fs::write(root.join("CVS/Entries"), b"/kept.o/1.1///\n").unwrap();
        std::fs::write(root.join("kept.o"), b"").unwrap();
        std::fs::write(root.join("x.o"), b"").unwrap();
        std::fs::write(root.join("sub/.cvsignore"), b"b.txt\n").unwrap();
        std::fs::write(root.join("sub/y.log"), b"").unwrap();
        std::fs::write(root.join(".cvsignore"), b"*.log unknown\n").unwrap();
        std::fs::write(root.join("top.log"), b"").unwrap();
        let rules = IgnoreRules {
            global: vec!["*.o".into()],
        };
        let found = find_files(root, root, &rules);
        assert_eq!(
            found.files,
            vec![
                ".cvsignore",
                "a.txt",
                "kept.o",
                "sub/.cvsignore",
                "sub/y.log"
            ]
        );
        assert!(found.unversioned.is_empty());
        let mut ignored = found.ignored.clone();
        ignored.sort();
        assert_eq!(
            ignored,
            vec![
                root.join("sub/b.txt"),
                root.join("top.log"),
                root.join("unknown"),
                root.join("x.o"),
            ]
        );
    }

    #[test]
    fn wildcards() {
        assert!(fnmatch("*.o", "a.o") && fnmatch("*.o", ".o"));
        assert!(!fnmatch("*.o", "a.obj"));
        assert!(fnmatch(".#*", ".#f.txt.1.2"));
        assert!(fnmatch("#*", "#x#") && fnmatch("*~", "a.txt~"));
        assert!(fnmatch("_$*", "_$x") && fnmatch("*$", "a$"));
        assert!(fnmatch("a?c", "abc") && !fnmatch("a?c", "ac"));
        assert!(fnmatch("[ab]x", "bx") && !fnmatch("[ab]x", "cx"));
        assert!(fnmatch("[!ab]x", "cx") && !fnmatch("[!ab]x", "ax"));
        assert!(fnmatch("[a-c]*", "cat") && !fnmatch("[a-c]*", "dog"));
        assert!(fnmatch("[]]", "]") && fnmatch("[", "["));
        assert!(fnmatch(r"\*", "*") && !fnmatch(r"\*", "a"));
        assert!(fnmatch("core", "core") && !fnmatch("core", "core.c"));
        assert!(fnmatch("*", ""));
    }

    #[test]
    fn ignore_rules() {
        let mut patterns = Vec::new();
        add_patterns(&mut patterns, DEFAULT_IGNORE);
        for name in ["x.o", "core", ".#f.txt.1.1", "a~", "CVS", "tags", "x.orig"] {
            assert!(is_ignored_name(&patterns, name), "{name}");
        }
        for name in ["a.txt", "core.c", "Makefile", "x.log"] {
            assert!(!is_ignored_name(&patterns, name), "{name}");
        }
        add_patterns(&mut patterns, "*.log\n! *.txt");
        assert_eq!(patterns, vec!["*.txt"]);
        assert_eq!(
            local_repository("/srv/cvs"),
            Some(PathBuf::from("/srv/cvs"))
        );
        assert_eq!(
            local_repository(":local:/srv/cvs"),
            Some(PathBuf::from("/srv/cvs"))
        );
        assert_eq!(local_repository(":pserver:me@host:/srv/cvs"), None);

        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("CVSROOT")).unwrap();
        std::fs::write(repo.join("CVSROOT/cvsignore"), "*.repo\n").unwrap();
        std::fs::write(dir.path().join(".cvsignore"), "*.home\n").unwrap();
        let repo_s = paths::lossy(&repo);
        let rules = IgnoreRules::load_from(Some(&repo_s), Some(dir.path()), Some("*.env"));
        for name in ["x.o", "a.repo", "a.home", "a.env"] {
            assert!(is_ignored_name(&rules.global, name), "{name}");
        }
        // `!` in CVSIGNORE drops everything before it
        let rules = IgnoreRules::load_from(Some(&repo_s), Some(dir.path()), Some("! *.env"));
        assert_eq!(rules.global, vec!["*.env"]);
        std::fs::write(dir.path().join(".cvsignore"), "! *.txt").unwrap();
        assert_eq!(
            rules.for_folder(dir.path()),
            vec!["*.txt"],
            "a folder's .cvsignore can clear the global list"
        );
    }

    #[test]
    fn conflict_backups_and_markers() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        assert_eq!(conflict_backup(d, "f.txt"), None);
        for name in [
            ".#f.txt.1.9",
            ".#f.txt.1.10",
            ".#f.txt.x",
            ".#g.txt.1.20",
            "f.txt",
        ] {
            std::fs::write(d.join(name), b"").unwrap();
        }
        assert_eq!(
            conflict_backup(d, "f.txt"),
            Some((d.join(".#f.txt.1.10"), "1.10".to_string()))
        );
        let f = d.join("f.txt");
        std::fs::write(&f, "a\n<<<<<<< f.txt\nb\n=======\nc\n>>>>>>> 1.2\n").unwrap();
        assert!(has_conflict_markers(&f));
        std::fs::write(&f, "a\r\n=======\r\n").unwrap();
        assert!(has_conflict_markers(&f));
        std::fs::write(&f, "a\n<<<<<<<b\n========\n").unwrap();
        assert!(!has_conflict_markers(&f));
        assert!(!has_conflict_markers(&d.join("missing")));
    }
}
