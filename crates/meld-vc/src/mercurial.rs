//! Mercurial backend (port of `meld/vc/mercurial.py`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::command::{self, VcCommand};
use crate::entry::{Action, State};
use crate::error::Result;
use crate::paths;
use crate::vc::{TreeCache, Vc, VcCore, VcKind, unsupported};

const CMD: &str = "hg";
const NAME: &str = "Mercurial";

/// Map an `hg status` key to a state.
pub fn state_for_key(key: &str) -> State {
    match key {
        "?" => State::Unversioned,
        "A" => State::New,
        "C" => State::Normal,
        "!" => State::Missing,
        "I" => State::Ignored,
        "M" => State::Modified,
        "R" => State::Removed,
        _ => State::Unversioned,
    }
}

/// Apply `hg status -A <path>` output (paths relative to `location`).
/// `single_file` is the absolute path when refreshing one file.
pub fn apply_status(
    cache: &mut TreeCache,
    location: &Path,
    output: &str,
    single_file: Option<&Path>,
) {
    // Remove duplicate entries, just in case.
    let entries: BTreeSet<&str> = output.lines().collect();
    if entries.is_empty() {
        if let Some(file) = single_file {
            cache.states.insert(file.to_path_buf(), State::Normal);
        }
        return;
    }
    for entry in entries {
        // There may be a space in the file name; only split once.
        let Some((key, name)) = entry.split_once(' ') else {
            continue;
        };
        let path = paths::absolutize(location, Path::new(name.trim()));
        let state = state_for_key(key.trim());
        cache.states.insert(path.clone(), state);
        cache.add_missing_entry(&path, state);
    }
}

/// The Mercurial backend.
#[derive(Debug, Clone)]
pub struct Mercurial {
    core: VcCore,
}

impl Mercurial {
    pub fn new(path: &Path) -> Result<Mercurial> {
        Ok(Mercurial {
            core: VcCore::new(VcKind::Mercurial, path)?,
        })
    }

    fn files_cmd(&self, args: &[&str], files: &[PathBuf]) -> VcCommand {
        VcCommand::new(CMD, args, files, &self.core.root)
    }
}

impl Vc for Mercurial {
    fn kind(&self) -> VcKind {
        VcKind::Mercurial
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
        let arg = paths::lossy(path);
        let output = command::output_text(CMD, &["status", "-A", &arg], Some(&self.core.location))?;
        let abs = self.core.resolve(path);
        let single = abs.is_file().then_some(abs);
        let location = self.core.location.clone();
        apply_status(&mut self.core.cache, &location, &output, single.as_deref());
        Ok(())
    }

    fn get_path_for_repo_file(&self, path: &Path, commit: Option<&str>) -> Result<PathBuf> {
        if commit.is_some() {
            return unsupported(self.kind(), "files at a given commit");
        }
        let rel = self.core.repo_relative(NAME, path)?;
        let suffix = paths::suffix(Path::new(&rel));
        command::call_temp_output(CMD, &["cat", &rel], &self.core.root, "", &suffix)
    }

    fn update(&self) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["pull", "-u"], &[])])
    }
    fn commit(&self, files: &[PathBuf], message: &str) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["commit", "-m", message], files)])
    }
    fn add(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["add"], files)])
    }
    fn remove(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["rm"], files)])
    }
    fn revert(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["revert"], files)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = "\
M modified.txt
A new.txt
R removed.txt
C clean.txt
! missing.txt
? unknown file.txt
I ignored.o
M modified.txt
";

    #[test]
    fn parses_status() {
        let mut cache = TreeCache::default();
        apply_status(&mut cache, Path::new("/hg"), STATUS, None);
        let st = |p: &str| cache.state(Path::new(p));
        assert_eq!(st("/hg/modified.txt"), State::Modified);
        assert_eq!(st("/hg/new.txt"), State::New);
        assert_eq!(st("/hg/removed.txt"), State::Removed);
        assert_eq!(st("/hg/clean.txt"), State::Normal);
        assert_eq!(st("/hg/missing.txt"), State::Missing);
        assert_eq!(st("/hg/unknown file.txt"), State::Unversioned);
        assert_eq!(st("/hg/ignored.o"), State::Ignored);
        let missing = &cache.missing[Path::new("/hg")];
        assert!(missing.contains("removed.txt") && missing.contains("missing.txt"));
    }

    #[test]
    fn commands() {
        let vc = Mercurial {
            core: VcCore {
                root: "/hg".into(),
                location: "/hg".into(),
                cache: TreeCache::default(),
            },
        };
        let c = &vc.commit(&[PathBuf::from("/hg/a.txt")], "msg").unwrap()[0];
        assert_eq!(c.console_line(), "hg commit -m msg a.txt (in /hg)\n");
        assert_eq!(vc.update().unwrap()[0].argv(), vec!["pull", "-u"]);
        assert!(vc.push().is_err());
        assert!(!vc.supports(Action::Push));
    }
}
