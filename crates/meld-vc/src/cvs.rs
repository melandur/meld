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
//!
//! Files ignored by `.cvsignore` or CVS's default ignore list are reported
//! by `cvs status` as unknown, so they show as unversioned.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::command::{self, VcCommand};
use crate::entry::{Action, State};
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

/// All files below `dir` (on disk or recorded in `CVS/Entries`; `CVS`
/// metadata folders are skipped) relative to `relative_to`, in walk order,
/// and the folders that are not under version control.
fn find_files(dir: &Path, relative_to: &Path) -> (Vec<String>, Vec<PathBuf>) {
    let mut out = Vec::new();
    let mut unversioned = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(iter) = std::fs::read_dir(&current) else {
            continue;
        };
        let mut names = BTreeSet::new();
        let mut subdirs = Vec::new();
        for child in iter.flatten() {
            let path = child.path();
            match child.file_type() {
                Ok(t) if t.is_dir() => {
                    if child.file_name() != VC_DIR {
                        if !path.join(VC_DIR).is_dir() {
                            unversioned.push(path.clone());
                        }
                        subdirs.push(path);
                    }
                }
                Ok(_) => {
                    names.insert(child.file_name().to_string_lossy().into_owned());
                }
                _ => {}
            }
        }
        names.extend(read_entries(&current).into_iter().map(|e| e.name));
        out.extend(
            names
                .iter()
                .map(|n| paths::lossy(&paths::relpath(&current.join(n), relative_to))),
        );
        // Depth-first in name order, like os.walk's top-down traversal
        subdirs.sort();
        stack.extend(subdirs.into_iter().rev());
    }
    (out, unversioned)
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
        ]
    }

    fn update_tree_state_cache(&mut self, path: &Path) -> Result<()> {
        let abs = self.core.resolve(path);
        let (files, unversioned_dirs) = if abs.is_dir() {
            find_files(&abs, &self.core.location)
        } else {
            let rel = paths::lossy(&paths::relpath(&abs, &self.core.location));
            (vec![rel], Vec::new())
        };
        for dir in unversioned_dirs {
            self.core.cache.states.insert(dir, State::Unversioned);
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
        let abs = paths::abspath(path);
        let dir = paths::parent_or_self(&abs);
        let name = abs
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let revision = read_entries(&dir)
            .into_iter()
            .find(|e| e.name == name)
            .map(|e| e.revision.trim_start_matches('-').to_string());
        match (revision.as_deref(), read_module_path(&dir)) {
            // Added, not yet committed: no base content
            (Some("0"), _) => command::write_temp(b"", "", &suffix),
            // `cvs update -p` prints nothing for files with unresolved
            // conflicts; `checkout -p` works whatever the file's state.
            (Some(rev), Some((cvsroot, module))) if !rev.is_empty() => {
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
                    &dir,
                    "",
                    &suffix,
                )
            }
            _ => command::call_temp_output(
                self.cmd,
                &["-q", "update", "-p", &rel],
                &self.core.root,
                "",
                &suffix,
            ),
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
        let (files, unversioned) = find_files(root, root);
        assert_eq!(
            files,
            vec!["a.txt", "removed.txt", "sub/b.txt", "unknown/c.txt"]
        );
        assert_eq!(unversioned, vec![root.join("unknown")]);
    }
}
