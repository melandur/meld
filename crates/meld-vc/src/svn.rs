//! Subversion backend (port of `meld/vc/svn.py`).

use std::path::{Path, PathBuf};

use quick_xml::events::{BytesStart, Event};

use crate::command::{self, VcCommand};
use crate::entry::{Action, ConflictSide, State};
use crate::error::{Result, VcError};
use crate::paths;
use crate::vc::{TreeCache, Vc, VcCore, VcKind, unsupported};

const NAME: &str = "Subversion";

/// Map an `svn status --xml` `item` attribute to a state.
pub fn state_for_item(item: &str) -> State {
    match item {
        "unversioned" => State::Unversioned,
        "added" => State::New,
        "normal" => State::Normal,
        "missing" => State::Missing,
        "ignored" => State::Ignored,
        "modified" => State::Modified,
        "deleted" => State::Removed,
        "conflicted" => State::Conflict,
        _ => State::Unversioned,
    }
}

/// One `<entry>`/`<wc-status>` pair from `svn status -v --xml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusItem {
    pub path: String,
    pub item: String,
    pub revision: Option<String>,
}

fn attr(e: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == name)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

/// Parse `svn status -v --xml` output. Items below `<target>` elements
/// come first, then those below `<changelist>` elements, like Python's
/// `findall("target") + findall("changelist")`.
pub fn parse_status_xml(xml: &str) -> Result<Vec<StatusItem>> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut targets = Vec::new();
    let mut changelists = Vec::new();
    // Element stack of names
    let mut stack: Vec<Vec<u8>> = Vec::new();
    let mut section: Option<bool> = None; // Some(is_changelist)
    let mut entry_path: Option<String> = None;

    loop {
        let event = reader.read_event().map_err(|e| VcError::Parse {
            vc: NAME,
            reason: e.to_string(),
        })?;
        let (start, empty) = match &event {
            Event::Start(e) => (Some(e.clone()), false),
            Event::Empty(e) => (Some(e.clone()), true),
            Event::End(_) => {
                if let Some(name) = stack.pop() {
                    match name.as_slice() {
                        b"entry" => entry_path = None,
                        b"target" | b"changelist" if stack.len() == 1 => section = None,
                        _ => {}
                    }
                }
                continue;
            }
            Event::Eof => break,
            _ => continue,
        };
        let Some(e) = start else { continue };
        let name = e.name().as_ref().to_vec();
        match name.as_slice() {
            b"target" | b"changelist" if stack.len() == 1 => {
                section = Some(name.as_slice() == b"changelist");
            }
            b"entry" if section.is_some() => {
                entry_path = Some(attr(&e, b"path").unwrap_or_default());
            }
            b"wc-status" => {
                if let (Some(is_cl), Some(path)) = (section, entry_path.as_ref()) {
                    let item = StatusItem {
                        path: path.clone(),
                        item: attr(&e, b"item").unwrap_or_default(),
                        revision: attr(&e, b"revision"),
                    };
                    if is_cl {
                        changelists.push(item);
                    } else {
                        targets.push(item);
                    }
                }
            }
            _ => {}
        }
        if !empty {
            stack.push(name);
        } else if name.as_slice() == b"entry" {
            entry_path = None;
        }
    }
    targets.extend(changelists);
    Ok(targets)
}

/// Apply parsed status items (paths relative to `location`) to `cache`.
pub fn apply_status(cache: &mut TreeCache, location: &Path, items: &[StatusItem]) {
    for it in items {
        if it.path.is_empty() || it.item.is_empty() {
            continue;
        }
        let path = paths::absolutize(location, Path::new(&it.path));
        let state = state_for_item(&it.item);
        cache.states.insert(path.clone(), state);
        let rev_label = it
            .revision
            .as_ref()
            .map(|r| format!("Rev {r}"))
            .unwrap_or_default();
        cache.meta.insert(path.clone(), rev_label);
        cache.add_missing_entry(&path, state);
    }
}

/// Working copy formats supported (SVN 1.7+, format 12 and later).
pub fn repo_version_supported(version: Option<u32>) -> bool {
    version.is_some_and(|v| v >= 12)
}

/// Read the working copy format of the `.svn` folder `vc_dir`, trusting the
/// `format` file, then `entries`, then the presence of `wc.db`.
pub fn read_repo_version(vc_dir: &Path) -> Option<u32> {
    for filename in ["format", "entries"] {
        let path = vc_dir.join(filename);
        if path.exists() {
            let text = std::fs::read_to_string(path).ok()?;
            return text.lines().next()?.trim().parse().ok();
        }
    }
    vc_dir.join("wc.db").exists().then_some(12)
}

pub(crate) fn valid_repo(cmd: &str, path: &Path) -> bool {
    if !command::call_ok(cmd, &["info"], Some(path)) {
        return false;
    }
    let (Some(root), _) = VcKind::Subversion.is_in_repo(path) else {
        return false;
    };
    repo_version_supported(read_repo_version(&root.join(".svn")))
}

/// Files in the folder of `path` whose names start with `name + pattern`
/// (a minimal `glob("<path><pattern>*")`), sorted by trailing revision
/// number where there is one.
fn glob_prefix(path: &Path, pattern: &str, exact: bool) -> Vec<PathBuf> {
    let Some(name) = path.file_name() else {
        return Vec::new();
    };
    let prefix = format!("{}{pattern}", name.to_string_lossy());
    let dir = paths::parent_or_self(path);
    let Ok(iter) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut found: Vec<(u64, String, PathBuf)> = iter
        .flatten()
        .filter_map(|e| {
            let fname = e.file_name().to_string_lossy().into_owned();
            let rest = fname.strip_prefix(&prefix)?;
            if exact && !rest.is_empty() {
                return None;
            }
            let rev = rest.parse().unwrap_or(u64::MAX);
            Some((rev, fname.clone(), dir.join(&fname)))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, _, p)| p).collect()
}

/// The Subversion backend.
#[derive(Debug, Clone)]
pub struct Subversion {
    core: VcCore,
    cmd: &'static str,
}

impl Subversion {
    pub fn new(path: &Path) -> Result<Subversion> {
        Ok(Subversion {
            core: VcCore::new(VcKind::Subversion, path)?,
            cmd: "svn",
        })
    }

    fn files_cmd(&self, args: &[&str], files: &[PathBuf], cwd: &Path) -> VcCommand {
        VcCommand::new(self.cmd, args, files, cwd)
    }
}

impl Vc for Subversion {
    fn kind(&self) -> VcKind {
        VcKind::Subversion
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
        // "svn --xml" outputs utf8, even with Windows non-utf8 locale
        let xml = command::output_text(
            self.cmd,
            &["status", "-v", "--xml", &arg],
            Some(&self.core.location),
        )?;
        let items = parse_status_xml(&xml)?;
        let location = self.core.location.clone();
        apply_status(&mut self.core.cache, &location, &items);
        Ok(())
    }

    fn get_path_for_repo_file(&self, path: &Path, commit: Option<&str>) -> Result<PathBuf> {
        if commit.is_some() {
            return unsupported(self.kind(), "files at a given commit");
        }
        let rel = self.core.repo_relative(NAME, path)?;
        let suffix = paths::suffix(Path::new(&rel));
        command::call_temp_output(
            self.cmd,
            &["cat", "-r", "BASE", &rel],
            &self.core.root,
            "",
            &suffix,
        )
    }

    /// SVN has two types of conflicts. Merge conflicts give
    /// `.merge-left.r*` (THIS), `.working` (BASE) and `.merge-right.r*`
    /// (OTHER); update conflicts give `.mine` (BASE here, as in Python),
    /// and two `.r*` files (lower: THIS, higher: OTHER).
    fn get_path_for_conflict(&self, path: &Path, side: ConflictSide) -> Result<(PathBuf, bool)> {
        let invalid = |reason: &str| VcError::InvalidPath {
            vc: NAME,
            path: path.to_path_buf(),
            reason: reason.to_string(),
        };
        self.core.repo_relative(NAME, path)?;

        if side == ConflictSide::Merged {
            return Ok((path.to_path_buf(), false));
        }

        let working = glob_prefix(path, ".working", true);
        let (is_merge, base) = if working.len() == 1 {
            (true, working)
        } else {
            let mine = glob_prefix(path, ".mine", true);
            if mine.len() == 1 {
                (false, mine)
            } else {
                return Err(invalid("No known conflict type found"));
            }
        };

        let missing = || invalid("Conflict file does not exist");
        let file = match side {
            ConflictSide::Base => base[0].clone(),
            ConflictSide::Local if is_merge => glob_prefix(path, ".merge-left.r", false)
                .into_iter()
                .next()
                .ok_or_else(missing)?,
            ConflictSide::Local => glob_prefix(path, ".r", false)
                .into_iter()
                .next()
                .ok_or_else(missing)?,
            ConflictSide::Remote if is_merge => glob_prefix(path, ".merge-right.r", false)
                .into_iter()
                .next()
                .ok_or_else(missing)?,
            ConflictSide::Remote => glob_prefix(path, ".r", false)
                .into_iter()
                .last()
                .ok_or_else(missing)?,
            ConflictSide::Merged => unreachable!("handled above"),
        };
        Ok((file, false))
    }

    fn update(&self) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["update"], &[], &self.core.root)])
    }
    fn commit(&self, files: &[PathBuf], message: &str) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(
            &["commit", "-m", message],
            files,
            &self.core.root,
        )])
    }
    fn remove(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(
            &["rm", "--force"],
            files,
            &self.core.root,
        )])
    }
    fn revert(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["revert"], files, &self.core.root)])
    }
    fn resolve(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(
            &["resolve", "--accept=working"],
            files,
            &self.core.root,
        )])
    }

    /// SVN < 1.7 needs to add folders from their immediate parent, so each
    /// folder gets its own command.
    fn add(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        let mut commands = Vec::new();
        for dir in files.iter().filter(|p| p.is_dir()) {
            let cwd = paths::parent_or_self(dir);
            commands.push(self.files_cmd(&["add"], std::slice::from_ref(dir), &cwd));
        }
        let plain: Vec<PathBuf> = files.iter().filter(|p| p.is_file()).cloned().collect();
        if !plain.is_empty() {
            commands.push(self.files_cmd(&["add"], &plain, &self.core.location));
        }
        Ok(commands)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<status>
<target path=".">
<entry path=".">
<wc-status props="none" item="normal" revision="3">
<commit revision="3"><author>me</author><date>2015-01-01T00:00:00.000000Z</date></commit>
</wc-status>
</entry>
<entry path="modified.txt">
<wc-status props="none" item="modified" revision="3">
<commit revision="2"><author>me</author></commit>
</wc-status>
</entry>
<entry path="new.txt">
<wc-status props="none" item="added" revision="-1"></wc-status>
</entry>
<entry path="unknown &amp; file.txt">
<wc-status props="none" item="unversioned"/>
</entry>
<entry path="sub/deleted.txt">
<wc-status props="none" item="deleted" revision="3"/>
</entry>
<entry path="gone.txt">
<wc-status props="none" item="missing" revision="3"/>
</entry>
<entry path="conflict.txt">
<wc-status props="none" item="conflicted" revision="3"/>
</entry>
<entry path="external">
<wc-status props="none" item=""/>
</entry>
</target>
<changelist name="cl">
<entry path="cl.txt">
<wc-status props="none" item="modified" revision="4"/>
</entry>
</changelist>
</status>
"#;

    #[test]
    fn parses_status_xml() {
        let items = parse_status_xml(STATUS_XML).unwrap();
        assert_eq!(items.len(), 9);
        assert_eq!(items[1].path, "modified.txt");
        assert_eq!(items[3].path, "unknown & file.txt");
        assert_eq!(items[3].revision, None);
        assert_eq!(items.last().unwrap().path, "cl.txt");

        let mut cache = TreeCache::default();
        apply_status(&mut cache, Path::new("/wc"), &items);
        let st = |p: &str| cache.state(Path::new(p));
        assert_eq!(st("/wc"), State::Normal);
        assert_eq!(st("/wc/modified.txt"), State::Modified);
        assert_eq!(st("/wc/new.txt"), State::New);
        assert_eq!(st("/wc/unknown & file.txt"), State::Unversioned);
        assert_eq!(st("/wc/sub/deleted.txt"), State::Removed);
        assert_eq!(st("/wc/gone.txt"), State::Missing);
        assert_eq!(st("/wc/conflict.txt"), State::Conflict);
        assert_eq!(st("/wc/cl.txt"), State::Modified);
        assert!(!cache.states.contains_key(Path::new("/wc/external")));
        assert_eq!(cache.meta(Path::new("/wc/modified.txt")), "Rev 3");
        assert_eq!(cache.meta(Path::new("/wc/unknown & file.txt")), "");
        assert!(cache.missing[Path::new("/wc/sub")].contains("deleted.txt"));
        assert!(cache.missing[Path::new("/wc")].contains("gone.txt"));
    }

    #[test]
    fn repo_versions() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_repo_version(dir.path()), None);
        std::fs::write(dir.path().join("wc.db"), b"").unwrap();
        assert_eq!(read_repo_version(dir.path()), Some(12));
        std::fs::write(dir.path().join("entries"), b"10\n").unwrap();
        assert_eq!(read_repo_version(dir.path()), Some(10));
        std::fs::write(dir.path().join("format"), b"31\n").unwrap();
        assert_eq!(read_repo_version(dir.path()), Some(31));
        assert!(repo_version_supported(Some(31)));
        assert!(!repo_version_supported(Some(10)));
        assert!(!repo_version_supported(None));
    }

    fn fake_vc(root: &Path) -> Subversion {
        Subversion {
            core: VcCore {
                root: root.to_path_buf(),
                location: root.to_path_buf(),
                cache: TreeCache::default(),
            },
            cmd: "svn",
        }
    }

    #[test]
    fn conflict_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let vc = fake_vc(root);
        let f = root.join("a.txt");
        for name in ["a.txt", "a.txt.mine", "a.txt.r10", "a.txt.r9"] {
            std::fs::write(root.join(name), b"").unwrap();
        }
        let get = |side| vc.get_path_for_conflict(&f, side).unwrap();
        assert_eq!(get(ConflictSide::Merged), (f.clone(), false));
        assert_eq!(get(ConflictSide::Base).0, root.join("a.txt.mine"));
        assert_eq!(get(ConflictSide::Local).0, root.join("a.txt.r9"));
        assert_eq!(get(ConflictSide::Remote).0, root.join("a.txt.r10"));

        let g = root.join("b.txt");
        for name in [
            "b.txt",
            "b.txt.working",
            "b.txt.merge-left.r3",
            "b.txt.merge-right.r5",
        ] {
            std::fs::write(root.join(name), b"").unwrap();
        }
        let get = |side| vc.get_path_for_conflict(&g, side).unwrap().0;
        assert_eq!(get(ConflictSide::Base), root.join("b.txt.working"));
        assert_eq!(get(ConflictSide::Local), root.join("b.txt.merge-left.r3"));
        assert_eq!(get(ConflictSide::Remote), root.join("b.txt.merge-right.r5"));

        std::fs::write(root.join("c.txt"), b"").unwrap();
        assert!(
            vc.get_path_for_conflict(&root.join("c.txt"), ConflictSide::Base)
                .is_err()
        );
        assert!(
            vc.get_path_for_conflict(Path::new("/elsewhere/x"), ConflictSide::Base)
                .is_err()
        );
    }

    #[test]
    fn add_splits_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("d")).unwrap();
        std::fs::write(root.join("f.txt"), b"").unwrap();
        let vc = fake_vc(root);
        let cmds = vc.add(&[root.join("d"), root.join("f.txt")]).unwrap();
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0].argv(), vec!["add", "d"]);
        assert_eq!(cmds[0].working_dir, root);
        assert_eq!(cmds[1].argv(), vec!["add", "f.txt"]);
        assert_eq!(
            vc.resolve(&[root.join("f.txt")]).unwrap()[0].argv(),
            vec!["resolve", "--accept=working", "f.txt"]
        );
    }
}
