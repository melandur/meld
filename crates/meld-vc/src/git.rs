//! Git backend (port of `meld/vc/git.py`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;

use crate::command::{self, VcCommand};
use crate::entry::{Action, ConflictSide, State};
use crate::error::Result;
use crate::paths;
use crate::vc::{TreeCache, Vc, VcCore, VcKind};

const CMD: &str = "git";
const NAME: &str = "Git";
/// The all-zero object id git uses for "not in the index/worktree".
pub const NULL_SHA: &str = "0000000000000000000000000000000000000000";

static DIFF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r":(\d+) (\d+) ([a-z0-9]+) ([a-z0-9]+) ([XADMTU])\t(.*)").expect("valid regex")
});

/// Map a `git diff-*` status letter to a state.
pub fn state_for_key(key: &str) -> State {
    match key {
        "X" => State::Unversioned,
        "A" => State::New,
        "D" => State::Removed,
        "M" | "T" => State::Modified,
        "U" => State::Conflict,
        _ => State::Unversioned,
    }
}

/// One parsed line of `git diff-index`/`git diff-files` raw output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub old_mode: String,
    pub new_mode: String,
    pub old_sha: String,
    pub new_sha: String,
    pub status: String,
    /// Path as printed (already unquoted).
    pub path: String,
}

/// Parse one raw diff line; `None` if it doesn't match.
pub fn parse_diff_line(line: &str) -> Option<DiffLine> {
    let caps = DIFF_RE.captures(line)?;
    Some(DiffLine {
        old_mode: caps[1].to_string(),
        new_mode: caps[2].to_string(),
        old_sha: caps[3].to_string(),
        new_sha: caps[4].to_string(),
        status: caps[5].to_string(),
        path: unquote_path(&caps[6]),
    })
}

/// Undo git's C-style quoting of unusual file names
/// (`"\303\274.txt"` → `ü.txt`). Unquoted names are returned trimmed.
pub fn unquote_path(name: &str) -> String {
    let name = name.trim();
    if !(name.len() >= 2 && name.starts_with('"') && name.ends_with('"')) {
        return name.to_string();
    }
    let inner = &name.as_bytes()[1..name.len() - 1];
    let mut out = Vec::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        let b = inner[i];
        if b != b'\\' || i + 1 >= inner.len() {
            out.push(b);
            i += 1;
            continue;
        }
        let next = inner[i + 1];
        i += 2;
        match next {
            b'a' => out.push(0x07),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(0x0b),
            b'0'..=b'7' => {
                let mut value = u32::from(next - b'0');
                let mut digits = 1;
                while digits < 3 && i < inner.len() && (b'0'..=b'7').contains(&inner[i]) {
                    value = value * 8 + u32::from(inner[i] - b'0');
                    i += 1;
                    digits += 1;
                }
                out.push((value & 0xff) as u8);
            }
            other => out.push(other),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Output of the status commands for one refresh, ready to be applied to a
/// [`TreeCache`]. Split out so the parsing can be tested on fixtures.
#[derive(Debug, Clone, Default)]
pub struct StatusOutput {
    /// `git diff-index --cached HEAD --relative <path>` lines.
    pub cached: Vec<String>,
    /// `git diff-files -0 --relative <path>` lines.
    pub files: Vec<String>,
    /// `git ls-files --others --ignored --exclude-standard --directory`.
    pub ignored: Vec<String>,
    /// `git ls-files --others --exclude-standard`.
    pub unversioned: Vec<String>,
}

/// Apply git status output to `cache`; paths are relative to `location`.
/// `single_file` is the absolute path when refreshing just one file.
pub fn apply_status(
    cache: &mut TreeCache,
    location: &Path,
    out: &StatusOutput,
    single_file: Option<&Path>,
) {
    let real_path = |name: &str| paths::absolutize(location, Path::new(&unquote_path(name)));

    if out.cached.is_empty()
        && out.files.is_empty()
        && let Some(file) = single_file
    {
        // A previously modified file may have been edited back to its
        // unmodified content; clear the stale state.
        cache.states.insert(file.to_path_buf(), State::Normal);
        return;
    }

    let mut meta: HashMap<PathBuf, Vec<String>> = HashMap::new();
    let mut staged = BTreeSet::new();
    let mut unstaged = BTreeSet::new();

    // Accumulate metadata from both lists, using the state from the
    // working-tree entries (they come last).
    for line in out.cached.iter().chain(out.files.iter()) {
        let Some(d) = parse_diff_line(line) else {
            log::warn!("unparseable git status line {line:?}");
            continue;
        };
        let state = state_for_key(d.status.trim());
        let path = paths::absolutize(location, Path::new(&d.path));
        cache.states.insert(path.clone(), state);
        // Git entries can't be MISSING; that's just an unstaged REMOVED
        cache.add_missing_entry(&path, state);
        // A mode of 000000 means the file was added or removed, not that
        // its mode changed (Python Meld reported those too).
        let is_zero = |mode: &str| mode.bytes().all(|b| b == b'0');
        if d.old_mode != d.new_mode && !is_zero(&d.old_mode) && !is_zero(&d.new_mode) {
            meta.entry(path.clone()).or_default().push(format!(
                "Mode changed from {} to {}",
                d.old_mode, d.new_mode
            ));
        }
        if d.new_sha == NULL_SHA {
            unstaged.insert(path);
        } else {
            staged.insert(path);
        }
    }

    for path in &staged {
        let msg = if unstaged.contains(path) {
            "Partially staged"
        } else {
            "Staged"
        };
        meta.entry(path.clone()).or_default().push(msg.to_string());
    }

    for (path, msgs) in meta {
        cache.meta.insert(path, msgs.join("; "));
    }

    for name in &out.ignored {
        cache.states.insert(real_path(name), State::Ignored);
    }
    for name in &out.unversioned {
        cache.states.insert(real_path(name), State::Unversioned);
    }
}

/// Replace every diff3 conflict region by its common ancestor, delimited by
/// `==== BASE ====` lines (`_vc.base_from_diff3`).
pub fn base_from_diff3(merged: &[u8]) -> Vec<u8> {
    static BASE_RE: LazyLock<regex::bytes::Regex> = LazyLock::new(|| {
        regex::bytes::Regex::new(concat!(
            r"(?ms)^<{7}.*?$\r?\n(?P<local>.*?)",
            r"^\|{7}.*?$\r?\n(?P<base>.*?)",
            r"^={7}.*?$\r?\n(?P<remote>.*?)",
            r"^>{7}.*?$\r?\n",
        ))
        .expect("valid regex")
    });
    BASE_RE
        .replace_all(merged, &b"==== BASE ====\n${base}==== BASE ====\n"[..])
        .into_owned()
}

/// Format the push summary (`get_commits_to_push_summary`).
pub fn format_push_summary(unpushed_commits: usize, unpushed_branches: usize) -> String {
    if unpushed_commits == 0 {
        return String::new();
    }
    let commits = if unpushed_commits == 1 {
        format!("{unpushed_commits} unpushed commit")
    } else {
        format!("{unpushed_commits} unpushed commits")
    };
    if unpushed_branches > 1 {
        format!("{commits} in {unpushed_branches} branches")
    } else {
        commits
    }
}

/// Strip `#` comment lines from a git `MERGE_MSG`.
pub fn clean_merge_message(message: &str) -> String {
    message
        .lines()
        .filter(|l| !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The Git backend.
#[derive(Debug, Clone)]
pub struct Git {
    core: VcCore,
}

impl Git {
    pub fn new(path: &Path) -> Result<Git> {
        Ok(Git {
            core: VcCore::new(VcKind::Git, path)?,
        })
    }

    fn run(&self, args: &[&str]) -> Result<Vec<String>> {
        command::output_text(CMD, args, Some(&self.core.location)).map(|t| command::lines(&t))
    }

    /// The repository-relative path (forward slashes) of `path`.
    pub fn get_repo_relative_path(&self, path: &Path) -> Result<String> {
        self.core.repo_relative(NAME, path)
    }

    /// Unpushed revisions per local branch with an upstream.
    pub fn get_commits_to_push(&self) -> Result<BTreeMap<String, Vec<String>>> {
        let lines = self.run(&[
            "for-each-ref",
            "--format=%(refname:short) %(upstream:short) %(upstream:trackshort)",
            "refs/heads",
        ])?;
        let mut result = BTreeMap::new();
        for line in lines {
            let parts: Vec<&str> = line.split_whitespace().collect();
            let [branch, remote, _track] = parts[..] else {
                continue;
            };
            let not_remote = format!("^{remote}");
            let revisions = self.run(&["rev-list", branch, &not_remote, "--"])?;
            result.insert(branch.to_string(), revisions);
        }
        Ok(result)
    }

    /// Status of the index against HEAD and of the worktree against the
    /// index for `path` (`_get_modified_files`).
    fn get_modified_files(&self, path: &str) -> Result<(Vec<String>, Vec<String>)> {
        // Update the index to avoid reading stale status information
        command::call_with_timeout(
            CMD,
            &["update-index", "--refresh"],
            &self.core.location,
            Duration::from_secs(5),
        )?;
        let cached = self.run(&["diff-index", "--cached", "HEAD", "--relative", path])?;
        let files = self.run(&["diff-files", "-0", "--relative", path])?;
        Ok((cached, files))
    }

    /// Re-merge the three conflict sides with diff3 markers and replace the
    /// conflicts with the base text (`remerge_with_ancestor`).
    fn remerge_with_ancestor(
        &self,
        local: &Path,
        base: &Path,
        remote: &Path,
        suffix: &str,
    ) -> Result<PathBuf> {
        let (l, b, r) = (
            paths::lossy(local),
            paths::lossy(base),
            paths::lossy(remote),
        );
        let merged = command::output_bytes(
            CMD,
            &["merge-file", "-p", "--diff3", &l, &b, &r],
            Some(&self.core.location),
        )?;
        let content = base_from_diff3(&merged);
        let id = format!("{}-", ConflictSide::Merged.index());
        command::write_temp(&content, &id, suffix)
    }

    fn files_cmd(&self, args: &[&str], files: &[PathBuf]) -> VcCommand {
        VcCommand::new(CMD, args, files, &self.core.root)
    }
}

impl Vc for Git {
    fn kind(&self) -> VcKind {
        VcKind::Git
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
            Action::Unstage,
        ]
    }

    fn update_tree_state_cache(&mut self, path: &Path) -> Result<()> {
        let arg = paths::lossy(path);
        let (cached, files) = self.get_modified_files(&arg)?;
        let ignored = self.run(&[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
            &arg,
        ])?;
        let unversioned = self.run(&["ls-files", "--others", "--exclude-standard", &arg])?;
        let abs = self.core.resolve(path);
        let single = abs.is_file().then_some(abs);
        let out = StatusOutput {
            cached,
            files,
            ignored,
            unversioned,
        };
        let location = self.core.location.clone();
        apply_status(&mut self.core.cache, &location, &out, single.as_deref());
        Ok(())
    }

    fn get_files_to_commit(&self, paths_in: &[PathBuf]) -> Result<Vec<String>> {
        let mut files = BTreeSet::new();
        for p in paths_in {
            if p.is_dir() {
                let (cached, entries) = self.get_modified_files(&paths::lossy(p))?;
                for line in cached.iter().chain(entries.iter()) {
                    if let Some(d) = parse_diff_line(line) {
                        files.insert(d.path);
                    }
                }
            } else {
                files.insert(paths::lossy(&paths::relpath(p, &self.core.root)));
            }
        }
        Ok(files.into_iter().collect())
    }

    fn get_commit_message_prefill(&self) -> Option<String> {
        let path = self.core.root.join(".git").join("MERGE_MSG");
        let message = std::fs::read_to_string(path).ok()?;
        Some(clean_merge_message(&message))
    }

    fn get_commits_to_push_summary(&self) -> Result<String> {
        let refs = self.get_commits_to_push()?;
        let branches = refs.values().filter(|v| !v.is_empty()).count();
        let commits = refs.values().map(Vec::len).sum();
        Ok(format_push_summary(commits, branches))
    }

    fn get_path_for_repo_file(&self, path: &Path, commit: Option<&str>) -> Result<PathBuf> {
        let commit = commit.unwrap_or("HEAD");
        let repo_path = self.get_repo_relative_path(path)?;
        let obj = format!("{commit}:{repo_path}");
        let suffix = paths::suffix(Path::new(&repo_path));
        command::call_temp_output(
            CMD,
            &["cat-file", "blob", &obj],
            &self.core.root,
            "",
            &suffix,
        )
    }

    fn get_path_for_conflict(&self, path: &Path, side: ConflictSide) -> Result<(PathBuf, bool)> {
        let stage = match side {
            ConflictSide::Merged => {
                // No way to get the merged result from git directly
                let local = self.get_path_for_conflict(path, ConflictSide::Local)?.0;
                let base = self.get_path_for_conflict(path, ConflictSide::Base)?.0;
                let remote = self.get_path_for_conflict(path, ConflictSide::Remote)?.0;
                let suffix = paths::suffix(path);
                let merged = self.remerge_with_ancestor(&local, &base, &remote, &suffix);
                for temp in [&local, &base, &remote] {
                    let _ = std::fs::remove_file(temp);
                }
                return Ok((merged?, true));
            }
            ConflictSide::Base => 1,
            ConflictSide::Local => 2,
            ConflictSide::Remote => 3,
        };
        let repo_path = self.get_repo_relative_path(path)?;
        let suffix = paths::suffix(Path::new(&repo_path));
        let spec = format!(":{stage}:{repo_path}");
        let file = command::call_temp_output(
            CMD,
            &["show", &spec],
            &self.core.location,
            side.label(),
            &suffix,
        )?;
        Ok((file, true))
    }

    fn update(&self) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["pull"], &[])])
    }

    fn push(&self) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["push"], &[])])
    }

    fn commit(&self, files: &[PathBuf], message: &str) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["commit", "-m", message], files)])
    }

    fn add(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["add"], files)])
    }

    fn remove(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["rm", "-r"], files)])
    }

    fn revert(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        let (exists, missing): (Vec<PathBuf>, Vec<PathBuf>) =
            files.iter().cloned().partition(|f| f.exists());
        let mut commands = Vec::new();
        if !exists.is_empty() {
            commands.push(self.files_cmd(&["checkout"], &exists));
        }
        if !missing.is_empty() {
            commands.push(self.files_cmd(&["checkout", "HEAD"], &missing));
        }
        Ok(commands)
    }

    fn resolve(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["add"], files)])
    }

    fn unstage(&self, files: &[PathBuf]) -> Result<Vec<VcCommand>> {
        Ok(vec![self.files_cmd(&["restore", "--staged"], files)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_raw_diff_lines() {
        let d = parse_diff_line(
            ":100644 100755 587be6b4c3f93f93c489c0111bba5596147a26cb 0000000000000000000000000000000000000000 M\tm.txt",
        )
        .unwrap();
        assert_eq!(d.old_mode, "100644");
        assert_eq!(d.new_mode, "100755");
        assert_eq!(d.status, "M");
        assert_eq!(d.path, "m.txt");
        assert_eq!(d.new_sha, NULL_SHA);
        assert!(parse_diff_line("garbage").is_none());
    }

    #[test]
    fn unquotes_paths() {
        assert_eq!(unquote_path("\"\\303\\274.txt\""), "ü.txt");
        assert_eq!(unquote_path("\"a\\\"b\\\\c\\tz\""), "a\"b\\c\tz");
        assert_eq!(unquote_path("r n.txt"), "r n.txt");
    }

    #[test]
    fn applies_status_fixture() {
        let out = StatusOutput {
            cached: vec![
                ":100644 000000 af70 0000000000000000000000000000000000000000 U\tc.txt".into(),
                ":000000 100644 0000000000000000000000000000000000000000 4286 A\tr n.txt".into(),
                ":100644 000000 4286 0000000000000000000000000000000000000000 D\tr.txt".into(),
                ":100644 100644 1111 2222 M\tpartial.txt".into(),
            ],
            files: vec![
                ":000000 100644 0000000000000000000000000000000000000000 0000000000000000000000000000000000000000 U\tc.txt".into(),
                ":100644 000000 4bcf 0000000000000000000000000000000000000000 D\tsub/d.txt".into(),
                ":100644 100755 587b 0000000000000000000000000000000000000000 M\tm.txt".into(),
                ":100644 100644 2222 0000000000000000000000000000000000000000 M\tpartial.txt".into(),
            ],
            ignored: vec!["build/".into(), "x.log".into()],
            unversioned: vec!["\"\\303\\274.txt\"".into()],
        };
        let mut cache = TreeCache::default();
        let loc = Path::new("/repo");
        apply_status(&mut cache, loc, &out, None);
        let st = |p: &str| cache.state(Path::new(p));
        assert_eq!(st("/repo/c.txt"), State::Conflict);
        assert_eq!(st("/repo/r n.txt"), State::New);
        assert_eq!(st("/repo/r.txt"), State::Removed);
        assert_eq!(st("/repo/sub/d.txt"), State::Removed);
        assert_eq!(st("/repo/m.txt"), State::Modified);
        assert_eq!(st("/repo/build"), State::Ignored);
        assert_eq!(st("/repo/x.log"), State::Ignored);
        assert_eq!(st("/repo/ü.txt"), State::Unversioned);
        assert_eq!(st("/repo/other"), State::Normal);
        assert_eq!(cache.meta(Path::new("/repo/r n.txt")), "Staged");
        assert_eq!(
            cache.meta(Path::new("/repo/m.txt")),
            "Mode changed from 100644 to 100755"
        );
        assert_eq!(
            cache.meta(Path::new("/repo/partial.txt")),
            "Partially staged"
        );
        assert!(cache.missing[Path::new("/repo/sub")].contains("d.txt"));
        assert!(cache.missing[Path::new("/repo")].contains("r.txt"));
    }

    #[test]
    fn single_file_reset() {
        let mut cache = TreeCache::default();
        cache.states.insert("/repo/a.txt".into(), State::Modified);
        apply_status(
            &mut cache,
            Path::new("/repo"),
            &StatusOutput::default(),
            Some(Path::new("/repo/a.txt")),
        );
        assert_eq!(cache.state(Path::new("/repo/a.txt")), State::Normal);
    }

    #[test]
    fn diff3_base_extraction() {
        let merged = b"a\n<<<<<<< ours\nMAIN\n||||||| base\nb\n=======\nOTHER\n>>>>>>> theirs\nc\n";
        assert_eq!(
            base_from_diff3(merged),
            b"a\n==== BASE ====\nb\n==== BASE ====\nc\n".to_vec()
        );
        let crlf = b"<<<<<<< x\r\nL\r\n||||||| y\r\nB\r\n======= \r\nR\r\n>>>>>>> z\r\n";
        assert_eq!(
            base_from_diff3(crlf),
            b"==== BASE ====\nB\r\n==== BASE ====\n".to_vec()
        );
    }

    #[test]
    fn push_summary() {
        assert_eq!(format_push_summary(0, 0), "");
        assert_eq!(format_push_summary(1, 1), "1 unpushed commit");
        assert_eq!(format_push_summary(3, 1), "3 unpushed commits");
        assert_eq!(
            format_push_summary(3, 2),
            "3 unpushed commits in 2 branches"
        );
    }

    #[test]
    fn merge_message_cleanup() {
        assert_eq!(
            clean_merge_message("Merge branch 'x'\n\n# Conflicts:\n#\tc.txt\n"),
            "Merge branch 'x'\n"
        );
    }
}
