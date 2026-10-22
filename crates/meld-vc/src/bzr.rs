//! Bazaar backend (port of `meld/vc/bzr.py`).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::command::{self, VcCommand};
use crate::entry::{Action, ConflictSide, State};
use crate::error::Result;
use crate::paths;
use crate::vc::{TreeCache, Vc, VcCore, VcKind};

const CMD: &str = "bzr";
const CMDARGS: [&str; 2] = ["--no-aliases", "--no-plugins"];
const NAME: &str = "Bazaar";

static PATCH_INDEX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^=== modified file '(.*)' (.*)$").expect("valid regex"));
static CONFLICT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"conflict in (.*)$").expect("valid regex"));
static RENAMED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.*) => (.*)$").expect("valid regex"));
static VALID_STATUS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[ +\-R?XCP][ NDKM][ */@]").expect("valid regex"));

/// States that are committed by `bzr commit`.
const COMMIT_STATUSES: [State; 4] = [State::Modified, State::Renamed, State::New, State::Removed];

/// First status column; `None` for flags we don't care about.
fn state_1(c: char) -> Option<State> {
    match c {
        'R' => Some(State::Renamed),
        '?' => Some(State::Unversioned),
        'C' => Some(State::Conflict),
        _ => None, // ' ', '+', '-', 'X', 'P'
    }
}

/// Second status column.
fn state_2(c: char) -> Option<State> {
    match c {
        ' ' => Some(State::Normal),
        'N' => Some(State::New),
        'D' => Some(State::Removed),
        'M' => Some(State::Modified),
        _ => None, // 'K': kind changed
    }
}

/// Third status column (executable/content changes).
fn state_3(c: char) -> Option<State> {
    match c {
        '*' | '/' | '@' => Some(State::Modified),
        _ => None,
    }
}

/// Result of parsing `bzr status -S --no-pending`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BzrStatus {
    /// Final state per absolute path (the maximum of its flags).
    pub states: HashMap<PathBuf, State>,
    /// Extra information per path (joined with `,` like Python's options).
    pub meta: HashMap<PathBuf, String>,
    /// Paths missing from disk (removed in the second column).
    pub removed: Vec<PathBuf>,
    /// New path → old path for renamed entries.
    pub reverse_renames: HashMap<PathBuf, PathBuf>,
}

/// Parse `bzr status -S --no-pending <branch_root>` output.
/// `diff_first_line` returns the first line of `bzr diff <path>`, used to
/// describe executable-bit changes.
pub fn parse_status(
    output: &str,
    branch_root: &Path,
    diff_first_line: &mut dyn FnMut(&Path) -> String,
) -> BzrStatus {
    let mut tree: HashMap<PathBuf, BTreeSet<State>> = HashMap::new();
    let mut meta: HashMap<PathBuf, Vec<String>> = HashMap::new();
    let mut renames: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut removed = Vec::new();

    // Files can appear twice in the list if they conflict and were renamed
    // at once.
    for entry in output.lines() {
        let chars: Vec<char> = entry.chars().collect();
        if chars.len() < 3 {
            continue;
        }
        let state_string: String = chars[..3].iter().collect();
        if !VALID_STATUS_RE.is_match(&state_string) {
            continue;
        }
        let mut name: String = chars.iter().skip(4).collect::<String>().trim().to_string();
        let s1 = state_1(chars[0]);
        let s2 = state_2(chars[1]);
        let s3 = state_3(chars[2]);
        let states: BTreeSet<State> = [s1, s2, s3].into_iter().flatten().collect();
        let mut entry_meta = Vec::new();
        let mut old_name = None;

        if states.contains(&State::Conflict)
            && let Some(c) = CONFLICT_RE.captures(&name)
        {
            name = c[1].to_string();
        }
        if states.contains(&State::Renamed)
            && let Some(c) = RENAMED_RE.captures(&name)
        {
            let (old, new) = (c[1].to_string(), c[2].to_string());
            entry_meta.push(format!("{old} ➡ {new}"));
            old_name = Some(old);
            name = new;
        }

        let joined = branch_root.join(&name);
        if let Some(old) = old_name {
            renames.push((
                normalize_entry(&branch_root.join(old)),
                normalize_entry(&joined),
            ));
        }
        if s3 == Some(State::Modified) {
            let line = diff_first_line(&joined);
            if let Some(c) = PATCH_INDEX_RE.captures(line.trim_end()) {
                entry_meta.push(c[2].to_string());
            }
        }
        let path = normalize_entry(&joined);
        tree.entry(path.clone()).or_default().extend(states);
        meta.entry(path.clone()).or_default().extend(entry_meta);
        if s2 == Some(State::Removed) {
            removed.push(path);
        }
    }

    // Handle any renames now
    let mut reverse_renames = HashMap::new();
    for (old, new) in renames {
        if let Some(old_states) = tree.remove(&old) {
            tree.entry(new.clone()).or_default().extend(old_states);
            let old_meta = meta.remove(&old).unwrap_or_default();
            meta.entry(new.clone()).or_default().extend(old_meta);
        }
        reverse_renames.insert(new, old);
    }

    BzrStatus {
        states: tree
            .into_iter()
            .filter_map(|(p, s)| s.iter().max().map(|m| (p, *m)))
            .collect(),
        meta: meta.into_iter().map(|(p, m)| (p, m.join(","))).collect(),
        removed,
        reverse_renames,
    }
}

/// `path.removesuffix("/")` after joining (`os.path.join` keeps `..`).
fn normalize_entry(path: &Path) -> PathBuf {
    paths::normalize(path)
}

/// The Bazaar backend.
#[derive(Debug, Clone)]
pub struct Bazaar {
    core: VcCore,
    reverse_rename_cache: HashMap<PathBuf, PathBuf>,
}

impl Bazaar {
    pub fn new(path: &Path) -> Result<Bazaar> {
        Ok(Bazaar {
            core: VcCore::new(VcKind::Bazaar, path)?,
            reverse_rename_cache: HashMap::new(),
        })
    }

    fn bzr_cmd(&self, args: &[&str], files: &[PathBuf]) -> VcCommand {
        let mut full: Vec<&str> = CMDARGS.to_vec();
        full.extend_from_slice(args);
        VcCommand::new(CMD, &full, files, &self.core.root)
    }

    fn apply(&mut self, status: BzrStatus) {
        let cache: &mut TreeCache = &mut self.core.cache;
        for path in &status.removed {
            cache.add_missing_entry(path, State::Removed);
        }
        cache.states.extend(status.states);
        // Python replaces the whole meta cache here.
        cache.meta = status.meta;
        self.reverse_rename_cache = status.reverse_renames;
    }
}

impl Vc for Bazaar {
    fn kind(&self) -> VcKind {
        VcKind::Bazaar
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

    fn update_tree_state_cache(&mut self, path: &Path) -> Result<()> {
        let mut root_args: Vec<&str> = CMDARGS.to_vec();
        let arg = paths::lossy(path);
        root_args.extend(["root", arg.as_str()]);
        let branch_root = command::output_text(CMD, &root_args, Some(&self.core.location))?;
        let branch_root = PathBuf::from(branch_root.trim_end_matches('\n'));
        let branch_root_s = paths::lossy(&branch_root);
        let mut status_args: Vec<&str> = CMDARGS.to_vec();
        status_args.extend(["status", "-S", "--no-pending", branch_root_s.as_str()]);
        let output = command::output_text(CMD, &status_args, None)?;
        let mut diff = |p: &Path| {
            let p = paths::lossy(p);
            command::output_text(CMD, &["diff", &p], None)
                .map(|o| o.lines().next().unwrap_or_default().to_string())
                .unwrap_or_default()
        };
        let status = parse_status(&output, &branch_root, &mut diff);
        self.apply(status);
        Ok(())
    }

    fn get_files_to_commit(&self, paths_in: &[PathBuf]) -> Result<Vec<String>> {
        let mut files = BTreeSet::new();
        for p in paths_in {
            if p.is_dir() {
                let prefix = paths::lossy(p);
                for (path, status) in &self.core.cache.states {
                    if COMMIT_STATUSES.contains(status) && paths::lossy(path).starts_with(&prefix) {
                        files.insert(paths::lossy(&paths::relpath(path, &self.core.root)));
                    }
                }
            } else {
                files.insert(paths::lossy(&paths::relpath(p, &self.core.root)));
            }
        }
        Ok(files.into_iter().collect())
    }

    fn get_path_for_repo_file(&self, path: &Path, commit: Option<&str>) -> Result<PathBuf> {
        let rel = self.core.repo_relative(NAME, path)?;
        let suffix = paths::suffix(Path::new(&rel));
        let rev;
        let mut args = vec!["cat", rel.as_str()];
        if let Some(c) = commit.filter(|c| !c.is_empty()) {
            rev = format!("-r{c}");
            args.push(&rev);
        }
        command::call_temp_output(CMD, &args, &self.core.root, "", &suffix)
    }

    fn get_path_for_conflict(&self, path: &Path, side: ConflictSide) -> Result<(PathBuf, bool)> {
        let mut path = paths::abspath(path);
        if side != ConflictSide::Merged
            && let Some(old) = self.reverse_rename_cache.get(&path)
        {
            path = old.clone();
        }
        self.core.repo_relative(NAME, &path)?;
        let suffix = match side {
            ConflictSide::Base => ".BASE",
            ConflictSide::Remote => ".OTHER",
            ConflictSide::Local => ".THIS",
            ConflictSide::Merged => "",
        };
        // bzr's conflict files are owned by bzr, not temporary for us
        let mut s = path.into_os_string();
        s.push(suffix);
        Ok((PathBuf::from(s), false))
    }

    fn add(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        Ok(vec![self.bzr_cmd(&["add"], files)])
    }
    /// As in Python, bzr commits the whole tree; `files` are ignored.
    fn commit(&self, _files: &[PathBuf], message: &str) -> Result<Vec<VcCommand>> {
        Ok(vec![self.bzr_cmd(&["commit", "-m", message], &[])])
    }
    fn revert(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        Ok(vec![self.bzr_cmd(&["revert"], files)])
    }
    fn push(&self) -> Result<Vec<VcCommand>> {
        Ok(vec![self.bzr_cmd(&["push"], &[])])
    }
    fn update(&self) -> Result<Vec<VcCommand>> {
        Ok(vec![self.bzr_cmd(&["pull"], &[])])
    }
    fn resolve(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        Ok(vec![self.bzr_cmd(&["resolve"], files)])
    }
    fn remove(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        Ok(vec![self.bzr_cmd(&["rm"], files)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Output of `bzr status -S --no-pending` in the bzrtest2 tree built by
    /// test/testbzr.sh.
    const STATUS: &str = "\
R   conflict2.txt => conflict2-moved.txt
RM  renamed-dir/ => renamed-new-dir/
R   renamed.txt => renamed-new.txt
 M  conflict.txt
 M  conflict2.txt
 D  deleted-dir/
 D  deleted.txt
  * exec.txt
 M  modified.txt
+N  new-dir/
+N  new-dir/new-dir-file.txt
+N  new.txt
?   unknown.txt
C   Text conflict in conflict.txt
C   Text conflict in conflict2-moved.txt
P   pending merge tip
";

    #[test]
    fn parses_testbzr_status() {
        let root = Path::new("/b");
        let mut diffs = Vec::new();
        let mut diff = |p: &Path| {
            diffs.push(p.to_path_buf());
            "=== modified file 'exec.txt' (properties changed: -x to +x)\n".to_string()
        };
        let st = parse_status(STATUS, root, &mut diff);
        let s = |p: &str| st.states.get(Path::new(p)).copied();
        assert_eq!(s("/b/conflict2-moved.txt"), Some(State::Conflict));
        assert_eq!(s("/b/conflict2.txt"), None, "moved to the new name");
        assert_eq!(s("/b/renamed-new-dir"), Some(State::Renamed));
        assert_eq!(s("/b/renamed-new.txt"), Some(State::Renamed));
        assert_eq!(s("/b/conflict.txt"), Some(State::Conflict));
        assert_eq!(s("/b/deleted.txt"), Some(State::Removed));
        assert_eq!(s("/b/deleted-dir"), Some(State::Removed));
        assert_eq!(s("/b/exec.txt"), Some(State::Modified));
        assert_eq!(s("/b/modified.txt"), Some(State::Modified));
        assert_eq!(s("/b/new.txt"), Some(State::New));
        assert_eq!(s("/b/new-dir/new-dir-file.txt"), Some(State::New));
        // "?" plus an empty second column gives {Unversioned, Normal}; like
        // Python, the maximum (Normal) wins.
        assert_eq!(s("/b/unknown.txt"), Some(State::Normal));
        assert_eq!(diffs, vec![PathBuf::from("/b/exec.txt")]);
        assert_eq!(
            st.meta[Path::new("/b/exec.txt")],
            "(properties changed: -x to +x)"
        );
        assert_eq!(
            st.meta[Path::new("/b/renamed-new.txt")],
            "renamed.txt ➡ renamed-new.txt"
        );
        assert_eq!(
            st.reverse_renames[Path::new("/b/conflict2-moved.txt")],
            PathBuf::from("/b/conflict2.txt")
        );
        assert!(st.removed.contains(&PathBuf::from("/b/deleted.txt")));
        assert!(st.removed.contains(&PathBuf::from("/b/deleted-dir")));
    }

    #[test]
    fn conflict_paths_follow_renames() {
        let mut vc = Bazaar {
            core: VcCore {
                root: "/b".into(),
                location: "/b".into(),
                cache: TreeCache::default(),
            },
            reverse_rename_cache: HashMap::new(),
        };
        let st = parse_status(STATUS, Path::new("/b"), &mut |_| String::new());
        vc.apply(st);
        assert_eq!(vc.core.cache.state(Path::new("/b/new.txt")), State::New);
        assert!(vc.core.cache.missing[Path::new("/b")].contains("deleted.txt"));
        let p = Path::new("/b/conflict2-moved.txt");
        assert_eq!(
            vc.get_path_for_conflict(p, ConflictSide::Base).unwrap(),
            (PathBuf::from("/b/conflict2.txt.BASE"), false)
        );
        assert_eq!(
            vc.get_path_for_conflict(p, ConflictSide::Merged).unwrap().0,
            PathBuf::from("/b/conflict2-moved.txt")
        );
        assert_eq!(
            vc.get_path_for_conflict(Path::new("/b/c.txt"), ConflictSide::THIS)
                .unwrap()
                .0,
            PathBuf::from("/b/c.txt.THIS")
        );
        assert!(
            vc.get_path_for_conflict(Path::new("/x/c.txt"), ConflictSide::OTHER)
                .is_err()
        );

        let files = vc
            .get_files_to_commit(&[PathBuf::from("/b/new.txt")])
            .unwrap();
        assert_eq!(files, vec!["new.txt"]);
        let c = &vc.add(&[PathBuf::from("/b/new.txt")]).unwrap()[0];
        assert_eq!(
            c.console_line(),
            "bzr --no-aliases --no-plugins add new.txt (in /b)\n"
        );
        let c = &vc.commit(&[PathBuf::from("/b/new.txt")], "m").unwrap()[0];
        assert_eq!(
            c.argv(),
            vec!["--no-aliases", "--no-plugins", "commit", "-m", "m"]
        );
    }
}
