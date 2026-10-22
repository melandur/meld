//! CVS backend (port of `meld/vc/cvs.py`). CVSNT is used instead of CVS
//! when installed.

use std::path::{Path, PathBuf};

use crate::command::{self, VcCommand};
use crate::entry::{Action, State};
use crate::error::{Result, VcError};
use crate::paths;
use crate::vc::{TreeCache, Vc, VcCore, VcKind, unsupported};

const NAME: &str = "CVS";
const VC_DIR: &str = "CVS";

/// Map the `Status:` value of `cvs status` to a state.
pub fn state_for_key(key: &str) -> State {
    match key {
        "Unknown" => State::Unversioned,
        "Locally Added" => State::New,
        "Up-to-date" => State::Normal,
        "!" => State::Missing,
        "I" => State::Ignored,
        "Locally Modified" => State::Modified,
        "Locally Removed" => State::Removed,
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
        let state = state_for_key(statekey);
        cache.states.insert(path.clone(), state);
        cache.add_missing_entry(&path, state);
    }
    Ok(())
}

/// All files below `dir` (skipping `CVS` metadata folders), relative to
/// `relative_to`, in walk order.
fn find_files(dir: &Path, relative_to: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(iter) = std::fs::read_dir(&current) else {
            continue;
        };
        let mut children: Vec<_> = iter.flatten().collect();
        children.sort_by_key(|e| e.file_name());
        let skip_files = current.to_string_lossy().ends_with(VC_DIR);
        let mut subdirs = Vec::new();
        for child in children {
            let path = child.path();
            match child.file_type() {
                Ok(t) if t.is_dir() => subdirs.push(path),
                Ok(_) if !skip_files => {
                    out.push(paths::lossy(&paths::relpath(&path, relative_to)));
                }
                _ => {}
            }
        }
        // Depth-first in name order, like os.walk's top-down traversal
        stack.extend(subdirs.into_iter().rev());
    }
    out
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
        let files = if abs.is_dir() {
            find_files(&abs, &self.core.location)
        } else {
            vec![paths::lossy(&paths::relpath(&abs, &self.core.location))]
        };
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
        command::call_temp_output(
            self.cmd,
            &["-q", "update", "-p", &rel],
            &self.core.root,
            "",
            &suffix,
        )
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
    fn revert(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["update", "-C"], files)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = "\
===================================================================
File: normal.txt       \tStatus: Up-to-date

   Working revision:\t1.1\tMon Jan  1 00:00:00 2015
   Repository revision:\t1.1\t/cvsroot/proj/normal.txt,v

===================================================================
File: modified.txt     \tStatus: Locally Modified

===================================================================
File: new.txt          \tStatus: Locally Added

===================================================================
File: no file gone.txt \tStatus: Locally Removed

===================================================================
File: stale.txt        \tStatus: Needs Patch

===================================================================
File: unknown.txt      \tStatus: Unknown
";

    #[test]
    fn parses_status() {
        let files: Vec<String> = [
            "normal.txt",
            "modified.txt",
            "new.txt",
            "sub/gone.txt",
            "stale.txt",
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
        assert_eq!(st("/cvs/stale.txt"), State::Unversioned);
        assert_eq!(st("/cvs/unknown.txt"), State::Unversioned);
        assert!(cache.missing[Path::new("/cvs/sub")].contains("gone.txt"));

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
    fn finds_files_skipping_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("CVS")).unwrap();
        std::fs::create_dir_all(root.join("sub/CVS")).unwrap();
        std::fs::write(root.join("CVS/Entries"), b"").unwrap();
        std::fs::write(root.join("sub/CVS/Entries"), b"").unwrap();
        std::fs::write(root.join("a.txt"), b"").unwrap();
        std::fs::write(root.join("sub/b.txt"), b"").unwrap();
        assert_eq!(find_files(root, root), vec!["a.txt", "sub/b.txt"]);
    }
}
