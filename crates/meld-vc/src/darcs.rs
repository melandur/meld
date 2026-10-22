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

    #[test]
    fn versions() {
        assert!(version_supported("2.16.5 (release)\n"));
        assert!(version_supported("2.10.3\n"));
        assert!(!version_supported("2.10.2\n"));
        assert!(!version_supported("2.8.4 (release)\n"));
        assert!(!version_supported("garbage"));
    }
}
