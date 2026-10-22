//! Darcs backend (port of `meld/vc/darcs.py`); requires Darcs >= 2.10.3.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::command::{self, VcCommand};
use crate::entry::{Action, State};
use crate::error::Result;
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

/// Parse `darcs whatsnew -sl --machine-readable` output run in `location`.
/// Darcs lists moves (`F` old name, then `T` new name) before
/// modifications.
pub fn parse_whatsnew(output: &str, location: &Path, root: &Path) -> DarcsStatus {
    let mut status = DarcsStatus::default();
    let mut renames: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut old_name: Option<PathBuf> = None;
    for line in output.lines() {
        // skip empty lines and lines starting with "What's new in foo"
        if line.trim().is_empty() || line.starts_with("What") {
            continue;
        }
        let Some((key, name)) = line.split_once(' ') else {
            continue;
        };
        let name = paths::normalize(Path::new(name));
        if key == "F" {
            old_name = Some(name.clone());
        }
        let path = paths::absolutize(location, &name);
        if key == "T"
            && let Some(old) = old_name.take()
        {
            renames.push((paths::absolutize(location, &old), path.clone()));
        }
        status.states.insert(path, state_for_key(key.trim()));
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

    fn apply(&mut self, status: DarcsStatus) {
        let cache: &mut TreeCache = &mut self.core.cache;
        for (path, state) in &status.states {
            cache.add_missing_entry(path, *state);
        }
        cache.states.extend(status.states);
        cache.meta = status.meta;
        self.reverse_rename_cache = status.reverse_renames;
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
        ]
    }

    /// `path` is ignored for the darcs call itself (darcs fails when given
    /// an empty argument), as in Python.
    fn update_tree_state_cache(&mut self, path: &Path) -> Result<()> {
        let output = command::output_text(
            CMD,
            &["whatsnew", "-sl", "--machine-readable"],
            Some(&self.core.location),
        )?;
        let abs = self.core.resolve(path);
        let has_lines = output.lines().next().is_some();
        if !has_lines && abs.is_file() {
            self.core.cache.states.insert(abs, State::Normal);
            return Ok(());
        }
        let status = parse_whatsnew(&output, &self.core.location, &self.core.root);
        self.apply(status);
        Ok(())
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

    fn commit(&self, _files: &[PathBuf], message: &str) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["record", "-a", "-m", message], &[])])
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
";

    #[test]
    fn parses_testdarcs_whatsnew() {
        let root = Path::new("/d");
        let st = parse_whatsnew(WHATSNEW, root, root);
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
        let c = &vc.commit(&[], "msg").unwrap()[0];
        assert_eq!(c.argv(), vec!["record", "-a", "-m", "msg"]);
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
