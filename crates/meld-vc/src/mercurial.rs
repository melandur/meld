//! Mercurial backend (port of `meld/vc/mercurial.py`).
//!
//! Beyond Python, which knew nothing about merges: files left unresolved
//! by `hg merge`, `rebase`, `graft`, `unshelve` etc. (`hg resolve --list`
//! `U` entries) are conflicts, compared 3-way with the sides recorded in
//! Mercurial's merge state (see [`Mercurial`]'s `get_path_for_conflict`)
//! and marked resolved with `hg resolve --mark`. While a merge is pending
//! commit commits the whole working copy, since Mercurial refuses to
//! commit only some files of a merge.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::command::{self, VcCommand};
use crate::entry::{Action, ConflictSide, State};
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

/// Template for `hg resolve --list`: repository-relative paths, whatever
/// the working directory and `ui.relative-paths`.
const RESOLVE_TEMPLATE: &str = "{mergestatus}\\t{path}\\n";

/// Mark the unresolved (`U`) files of `hg resolve --list -T
/// RESOLVE_TEMPLATE` output (paths relative to `root`) as conflicts.
/// Resolved (`R`) files keep their status (modified).
pub fn apply_resolve_list(cache: &mut TreeCache, root: &Path, output: &str) {
    for line in output.lines() {
        let Some((key, name)) = line.split_once('\t') else {
            continue;
        };
        if key.trim() == "U" {
            let path = paths::absolutize(root, Path::new(name));
            cache.states.insert(path, State::Conflict);
        }
    }
}

/// Template for `hg debugmergestate`: one `C` line per commit of the
/// merge (`local`, `other`), one `F` line per file.
const MERGESTATE_TEMPLATE: &str = concat!(
    r#"{commits % "C\t{name}\t{node}\n"}"#,
    r#"{files % "F\t{path}\t{local_key}\t{local_path}\t{ancestor_path}\t{ancestor_node}\t"#,
    r#"{other_path}\t{other_node}\t{extras % '{key}={value};'}\n"}"#,
);

/// What Mercurial's merge state records about a conflicted file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeStateFile {
    pub path: String,
    /// Name of the backup of the local version in `.hg/merge/` (all
    /// zeros when the file was deleted locally).
    pub local_key: String,
    pub ancestor_path: String,
    /// File node of the base version (all zeros when the file was added
    /// on both sides).
    pub ancestor_node: String,
    /// The changeset holding the base version (`ancestorlinknode`).
    pub ancestor_changeset: Option<String>,
    pub other_path: String,
    /// File node of the other version (all zeros when deleted there).
    pub other_node: String,
}

/// Parsed `hg debugmergestate -T MERGESTATE_TEMPLATE` output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeState {
    /// The working copy's parent the merge started from.
    pub local: Option<String>,
    /// The changeset merged in (merge rev, rebase/graft source, ...).
    pub other: Option<String>,
    pub files: Vec<MergeStateFile>,
}

impl MergeState {
    pub fn parse(output: &str) -> MergeState {
        let mut state = MergeState::default();
        for line in output.lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            match fields.as_slice() {
                ["C", "local", node] => state.local = Some(node.to_string()),
                ["C", "other", node] => state.other = Some(node.to_string()),
                [
                    "F",
                    path,
                    local_key,
                    _local_path,
                    ancestor_path,
                    ancestor_node,
                    other_path,
                    other_node,
                    extras,
                ] => {
                    let ancestor_changeset = extras
                        .split(';')
                        .find_map(|kv| kv.strip_prefix("ancestorlinknode="))
                        .filter(|n| !n.is_empty())
                        .map(str::to_string);
                    state.files.push(MergeStateFile {
                        path: path.to_string(),
                        local_key: local_key.to_string(),
                        ancestor_path: ancestor_path.to_string(),
                        ancestor_node: ancestor_node.to_string(),
                        ancestor_changeset,
                        other_path: other_path.to_string(),
                        other_node: other_node.to_string(),
                    });
                }
                _ => {}
            }
        }
        state
    }
}

/// Whether a node or key is Mercurial's null id (no such version).
fn is_null(node: &str) -> bool {
    node.bytes().all(|b| b == b'0')
}

/// The Mercurial backend.
#[derive(Debug, Clone)]
pub struct Mercurial {
    core: VcCore,
    /// Whether the working copy has two parents (an uncommitted merge).
    pending_merge: bool,
}

impl Mercurial {
    pub fn new(path: &Path) -> Result<Mercurial> {
        Ok(Mercurial {
            core: VcCore::new(VcKind::Mercurial, path)?,
            pending_merge: false,
        })
    }

    fn files_cmd(&self, args: &[&str], files: &[PathBuf]) -> VcCommand {
        VcCommand::new(CMD, args, files, &self.core.root)
    }

    /// Whether Mercurial has merge state (left by merge, rebase, graft,
    /// ...; removed by commit or abort). Avoids querying it otherwise.
    fn has_merge_state(&self) -> bool {
        self.core.root.join(".hg").join("merge").is_dir()
    }

    /// `hg cat -r <rev> <path>` into a temporary file; empty if the file
    /// doesn't exist in `rev`.
    fn cat(&self, rev: &str, path: &str, side: ConflictSide, suffix: &str) -> Result<PathBuf> {
        command::call_temp_output(
            CMD,
            &["cat", "-r", rev, "--", path],
            &self.core.root,
            side.label(),
            suffix,
        )
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
            Action::Resolve,
        ]
    }

    fn update_tree_state_cache(&mut self, path: &Path) -> Result<()> {
        let arg = paths::lossy(path);
        let output = command::output_text(CMD, &["status", "-A", &arg], Some(&self.core.location))?;
        let abs = self.core.resolve(path);
        let single = abs.is_file().then_some(abs);
        let location = self.core.location.clone();
        apply_status(&mut self.core.cache, &location, &output, single.as_deref());

        self.pending_merge = false;
        if self.has_merge_state() {
            let root = self.core.root.clone();
            let output = command::output_text(
                CMD,
                &["resolve", "--list", "-T", RESOLVE_TEMPLATE],
                Some(&root),
            )?;
            apply_resolve_list(&mut self.core.cache, &root, &output);
            let p2 =
                command::output_text(CMD, &["log", "-r", "p2()", "-T", "{node}"], Some(&root))?;
            self.pending_merge = !p2.trim().is_empty();
        }
        Ok(())
    }

    /// The sides recorded in Mercurial's merge state: local is the backup
    /// Mercurial made of the working file before merging (so it includes
    /// uncommitted changes merged by `hg update`), remote the version in
    /// the changeset merged in, base the version in the changeset that
    /// introduced the merge's ancestor file revision. That is right for
    /// rebase and graft too, whose base is the parent of the source
    /// changeset rather than `ancestor(p1(), p2())`, and which have no
    /// second parent at all. A side missing from the merge (e.g. deleted
    /// locally) is an empty file. Without merge state for `path` the sides
    /// come from `p1()`, `p2()` and their ancestor. Merged is the working
    /// file.
    fn get_path_for_conflict(&self, path: &Path, side: ConflictSide) -> Result<(PathBuf, bool)> {
        let rel = self.core.repo_relative(NAME, path)?;
        if side == ConflictSide::Merged {
            return Ok((paths::abspath(path), false));
        }
        let suffix = paths::suffix(Path::new(&rel));
        let empty = || command::write_temp(b"", side.label(), &suffix);
        let state = if self.has_merge_state() {
            let out = command::output_text(
                CMD,
                &["debugmergestate", "-T", MERGESTATE_TEMPLATE],
                Some(&self.core.root),
            )?;
            MergeState::parse(&out)
        } else {
            MergeState::default()
        };
        let local = state.local.as_deref().unwrap_or("p1()");
        let other = state.other.as_deref().unwrap_or("p2()");
        let temp = match (state.files.iter().find(|f| f.path == rel), side) {
            (Some(f), ConflictSide::Local) => {
                let backup = self.core.root.join(".hg").join("merge").join(&f.local_key);
                if is_null(&f.local_key) {
                    empty()?
                } else if backup.is_file() {
                    command::write_temp(&std::fs::read(&backup)?, side.label(), &suffix)?
                } else {
                    self.cat(local, &rel, side, &suffix)?
                }
            }
            (Some(f), ConflictSide::Remote) if is_null(&f.other_node) => empty()?,
            (Some(f), ConflictSide::Remote) => self.cat(other, &f.other_path, side, &suffix)?,
            (Some(f), _) if is_null(&f.ancestor_node) => empty()?,
            (Some(f), _) => match &f.ancestor_changeset {
                Some(changeset) => self.cat(changeset, &f.ancestor_path, side, &suffix)?,
                None => {
                    let rev = format!("ancestor({local}, {other})");
                    self.cat(&rev, &f.ancestor_path, side, &suffix)?
                }
            },
            (None, ConflictSide::Local) => self.cat("p1()", &rel, side, &suffix)?,
            (None, ConflictSide::Remote) => self.cat("p2()", &rel, side, &suffix)?,
            (None, _) => self.cat("ancestor(p1(), p2())", &rel, side, &suffix)?,
        };
        Ok((temp, true))
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
    /// Commits the selected `files`, or everything while a merge is
    /// pending (Mercurial can't commit part of a merge).
    fn commit(&self, files: &[PathBuf], message: &str) -> Result<Vec<VcCommand>> {
        let files = if self.pending_merge { &[] } else { files };
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
    fn resolve(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["resolve", "--mark"], files)])
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
        let mut vc = Mercurial {
            core: VcCore {
                root: "/hg".into(),
                location: "/hg".into(),
                cache: TreeCache::default(),
            },
            pending_merge: false,
        };
        let c = &vc.commit(&[PathBuf::from("/hg/a.txt")], "msg").unwrap()[0];
        assert_eq!(c.console_line(), "hg commit -m msg a.txt (in /hg)\n");
        assert_eq!(vc.update().unwrap()[0].argv(), vec!["pull", "-u"]);
        assert!(vc.push().is_err());
        assert!(!vc.supports(Action::Push));
        let c = &vc.resolve(&[PathBuf::from("/hg/a.txt")]).unwrap()[0];
        assert_eq!(c.argv(), vec!["resolve", "--mark", "a.txt"]);
        // A merge can only be committed as a whole
        vc.pending_merge = true;
        let c = &vc.commit(&[PathBuf::from("/hg/a.txt")], "msg").unwrap()[0];
        assert_eq!(c.argv(), vec!["commit", "-m", "msg"]);
    }

    #[test]
    fn parses_resolve_list() {
        let mut cache = TreeCache::default();
        cache.states.insert("/hg/a b.txt".into(), State::Modified);
        cache.states.insert("/hg/done.txt".into(), State::Modified);
        apply_resolve_list(
            &mut cache,
            Path::new("/hg"),
            "U\ta b.txt\nR\tdone.txt\nU\tsub/c.txt\n",
        );
        assert_eq!(cache.state(Path::new("/hg/a b.txt")), State::Conflict);
        assert_eq!(cache.state(Path::new("/hg/sub/c.txt")), State::Conflict);
        assert_eq!(cache.state(Path::new("/hg/done.txt")), State::Modified);
    }

    #[test]
    fn parses_merge_state() {
        let out = "C\tlocal\tf5176b37\nC\tother\tc2697229\n\
                   F\tf.txt\t7ad4af83\tf.txt\tf.txt\tbdcf63a2\tf.txt\t0795ea55\t\
                   ancestorlinknode=3ad92f80;merged=yes;\n\
                   F\tg.txt\t0000000000\tg.txt\told.txt\t0000\tg.txt\t1234\t\n";
        let st = MergeState::parse(out);
        assert_eq!(st.local.as_deref(), Some("f5176b37"));
        assert_eq!(st.other.as_deref(), Some("c2697229"));
        assert_eq!(st.files.len(), 2);
        assert_eq!(st.files[0].local_key, "7ad4af83");
        assert_eq!(st.files[0].ancestor_changeset.as_deref(), Some("3ad92f80"));
        assert_eq!(st.files[0].other_node, "0795ea55");
        assert_eq!(st.files[1].ancestor_path, "old.txt");
        assert_eq!(st.files[1].ancestor_changeset, None);
        assert!(is_null(&st.files[1].local_key) && is_null(&st.files[1].ancestor_node));
        assert!(!is_null(&st.files[1].other_node));
        assert_eq!(MergeState::parse(""), MergeState::default());
    }
}
