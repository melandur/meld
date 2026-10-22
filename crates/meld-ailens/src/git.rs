//! The git commands AI Lens needs, with parsed results (port of `git.py`)
//!
//! git runs as a child process, never through a shell, with its output
//! pinned to the shape the parsers expect whatever the user's config says.
//! Every method that runs git takes a [`CancelToken`] and returns
//! [`Error::Cancelled`] when it fires, and [`Error::Git`] when git fails.
//!
//! Paths are relative to the worktree root, `/`-separated, as git prints
//! them. Text read from git is decoded as UTF-8, invalid bytes replaced.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::error::{Error, ProcessErrorKind, Result};
use crate::models::{
    GitBlameLine, GitCommit, GitFileChange, GitFileDiff, GitFileStatus, GitRef, GitRefType,
    GitWorktree, UncommittedLayer,
};
use crate::parsers::{
    GitFileStats, log_format, parse_blame, parse_diff, parse_log, parse_name_status, parse_numstat,
    parse_patch_ids, parse_refs, parse_rev_list_parents_ordered, parse_worktrees, ref_format,
};
use crate::process::{CancelToken, ProcessOptions, ProcessResult, run_process};

/// Keeps git's output in the shape the parsers expect, whatever the user's
/// git config says
const OUTPUT_CONFIG: [&str; 13] = [
    "core.quotePath=false",
    "color.ui=false",
    "log.showSignature=false",
    "i18n.logOutputEncoding=UTF-8",
    "diff.noprefix=false",
    "diff.mnemonicPrefix=false",
    "diff.relative=false",
    "diff.suppressBlankEmpty=false",
    "diff.submodule=short",
    "diff.renames=true",
    "blame.ignoreRevsFile=",
    "gc.auto=0",
    "maintenance.auto=false",
];

/// Variables that point git at a repository, index or object store; they
/// are set when Meld is started by git (difftool, hooks) and must not leak
/// into commands for other repositories or worktrees
const LOCAL_ENV_VARS: [&str; 15] = [
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_DIR",
    "GIT_GRAFT_FILE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_OBJECT_DIRECTORY",
    "GIT_PREFIX",
    "GIT_REPLACE_REF_BASE",
    "GIT_SHALLOW_FILE",
    "GIT_WORK_TREE",
];

/// Commands that reject `--literal-pathspecs`
const NO_LITERAL_PATHSPECS: [&str; 1] = ["check-ignore"];

/// How much of a file git checks for a NUL byte to decide it's binary
pub const BINARY_CHECK_BYTES: usize = 8000;
/// git's default `core.bigFileThreshold`: diff treats larger files as binary
pub const BIG_FILE_THRESHOLD: u64 = 512 * 1024 * 1024;

const READ_CHUNK: usize = 64 * 1024;

/// Paths and file contents
pub(crate) fn text(data: &[u8]) -> String {
    String::from_utf8_lossy(data).into_owned()
}

/// `ai-lens` -> `refs/notes/ai-lens`, as git expands notes refs
pub fn notes_ref_name(name: &str) -> String {
    if name.starts_with("refs/") {
        name.to_string()
    } else if name.starts_with("notes/") {
        format!("refs/{name}")
    } else {
        format!("refs/notes/{name}")
    }
}

/// How git is run
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOptions {
    /// The git executable; `git` on `PATH` by default
    pub git_path: String,
    /// The environment git runs with; this process's when None
    pub env: Option<HashMap<String, String>>,
}

impl Default for GitOptions {
    fn default() -> Self {
        Self {
            git_path: "git".into(),
            env: None,
        }
    }
}

/// `realpath`: symlinks resolved; for a path that doesn't exist (fully),
/// the existing part resolved and the rest appended
pub fn realpath(path: &Path) -> PathBuf {
    let absolute = absolute(path);
    if let Ok(resolved) = std::fs::canonicalize(&absolute) {
        return resolved;
    }
    let mut missing = Vec::new();
    let mut current = absolute.as_path();
    while let Some(parent) = current.parent() {
        if let Some(name) = current.file_name() {
            missing.push(name.to_os_string());
        }
        if let Ok(resolved) = std::fs::canonicalize(parent) {
            let mut result = resolved;
            for name in missing.iter().rev() {
                result.push(name);
            }
            return result;
        }
        current = parent;
    }
    absolute
}

/// `os.path.abspath`: absolute and normalized, without resolving links
pub fn absolute(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    normalize(&joined)
}

/// `os.path.normpath`: `.` and `..` resolved lexically
pub fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut parts: Vec<Component<'_>> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match parts.last() {
                Some(Component::Normal(_)) => {
                    parts.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => parts.push(component),
            },
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return PathBuf::from(".");
    }
    parts.iter().collect()
}

/// git's path order: by bytes, not by locale
pub fn path_sort_key(path: &str) -> &[u8] {
    path.as_bytes()
}

/// Options of [`GitRepository::run`]
#[derive(Debug, Clone, Copy)]
pub struct RunOptions<'a> {
    /// Where git runs; the repository's root by default
    pub worktree: Option<&'a Path>,
    pub input: Option<&'a [u8]>,
    /// Exit codes that aren't errors
    pub ok: &'a [i32],
}

impl Default for RunOptions<'_> {
    fn default() -> Self {
        Self {
            worktree: None,
            input: None,
            ok: &[0],
        }
    }
}

/// One worktree of a git repository
///
/// `root` is the top folder of the worktree; commands about commits run
/// there, and commands about the index or the files on disk run in the
/// worktree they are given (this one by default). Other worktrees of the
/// same repository share its commits, refs and notes.
///
/// Holds no state besides its settings; can be shared between threads.
#[derive(Debug, Clone)]
pub struct GitRepository {
    pub root: PathBuf,
    pub git_path: String,
    env: HashMap<OsString, OsString>,
}

impl GitRepository {
    pub fn new(root: impl AsRef<Path>, options: &GitOptions) -> Self {
        let mut env: HashMap<OsString, OsString> = match &options.env {
            Some(env) => env
                .iter()
                .map(|(k, v)| (OsString::from(k), OsString::from(v)))
                .collect(),
            None => std::env::vars_os().collect(),
        };
        for name in LOCAL_ENV_VARS {
            env.remove(&OsString::from(name));
        }
        for (key, value) in [
            ("LC_ALL", "C"),
            ("GIT_OPTIONAL_LOCKS", "0"),
            ("GIT_TERMINAL_PROMPT", "0"),
        ] {
            env.insert(key.into(), value.into());
        }
        Self {
            root: absolute(root.as_ref()),
            git_path: options.git_path.clone(),
            env,
        }
    }

    /// Set (or override) a variable of the environment git runs with
    pub fn set_env(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) {
        self.env.insert(key.into(), value.into());
    }

    /// The options this repository was opened with, e.g. to open another
    /// worktree the same way
    pub fn options(&self) -> GitOptions {
        GitOptions {
            git_path: self.git_path.clone(),
            env: Some(
                self.env
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.to_string_lossy().into_owned(),
                            v.to_string_lossy().into_owned(),
                        )
                    })
                    .collect(),
            ),
        }
    }

    /// The repository of the worktree that contains `path`
    ///
    /// `path` may be any file or folder inside a worktree. Fails with a
    /// message for the user when it isn't in one (or git is missing, or the
    /// repository is bare).
    pub fn discover(path: &Path, options: &GitOptions, cancel: &CancelToken) -> Result<Self> {
        let path = absolute(path);
        if !path.exists() {
            return Err(Error::msg(format!("“{}” doesn't exist.", path.display())));
        }
        let cwd = if path.is_dir() {
            path.clone()
        } else {
            path.parent().map(Path::to_path_buf).unwrap_or(path.clone())
        };

        let probe = Self::new(&cwd, options);
        let result = probe.run_with(
            &["rev-parse", "--is-bare-repository", "--is-inside-work-tree"],
            RunOptions {
                ok: &[0, 128],
                ..Default::default()
            },
            cancel,
        )?;
        if result.returncode == 128 {
            let stderr = text(&result.stderr).trim().to_string();
            if stderr.contains("not a git repository") {
                return Err(Error::msg(format!(
                    "“{}” isn't in a Git repository.",
                    path.display()
                )));
            }
            return Err(Error::Git {
                message: format!(
                    "Git can't open the repository at “{}”: {stderr}",
                    path.display()
                ),
                command: vec!["rev-parse".into()],
                returncode: Some(128),
                stderr,
                not_found: false,
            });
        }
        let out = text(&result.stdout);
        let mut words = out.split_whitespace();
        let (bare, inside) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
        if bare == "true" {
            return Err(Error::msg(format!(
                "“{}” is a bare Git repository; AI Lens needs one with a working tree.",
                path.display()
            )));
        }
        if inside != "true" {
            return Err(Error::msg(format!(
                "“{}” is inside a Git folder, not in a working tree.",
                path.display()
            )));
        }

        let top = probe.run(&["rev-parse", "--show-toplevel"], cancel)?;
        let top = text(&top.stdout);
        Ok(Self::new(top.trim_end_matches('\n'), options))
    }

    // -- running git --

    /// Run `git <args>` in the root; fails unless git exits with 0
    pub fn run<S: AsRef<str>>(&self, args: &[S], cancel: &CancelToken) -> Result<ProcessResult> {
        self.run_with(args, RunOptions::default(), cancel)
    }

    /// Run `git <args>`; fails unless git exits with a code in `options.ok`
    pub fn run_with<S: AsRef<str>>(
        &self,
        args: &[S],
        options: RunOptions<'_>,
        cancel: &CancelToken,
    ) -> Result<ProcessResult> {
        let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
        let cwd = options.worktree.unwrap_or(&self.root);
        let mut full: Vec<String> = vec![self.git_path.clone(), "--no-pager".into()];
        if args
            .first()
            .is_none_or(|first| !NO_LITERAL_PATHSPECS.contains(first))
        {
            // Paths are never patterns
            full.push("--literal-pathspecs".into());
        }
        for setting in OUTPUT_CONFIG {
            full.push("-c".into());
            full.push(setting.into());
        }
        full.extend(args.iter().map(|arg| arg.to_string()));
        let command: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();

        let result = run_process(
            &full,
            &ProcessOptions {
                cwd: Some(cwd),
                input: options.input,
                env: Some(&self.env),
                no_inherit_env: true,
                cancel: Some(cancel),
                timeout: None,
            },
        );
        let result = match result {
            Ok(result) => result,
            Err(Error::Process {
                kind: ProcessErrorKind::NotFound,
                ..
            }) => {
                // A missing working folder is reported like a missing program
                if !cwd.is_dir() {
                    return Err(Error::Git {
                        message: format!("The folder “{}” doesn't exist.", cwd.display()),
                        command,
                        returncode: None,
                        stderr: String::new(),
                        not_found: false,
                    });
                }
                return Err(Error::Git {
                    message: format!(
                        "Git wasn't found at “{}”. Install Git, or add it to PATH.",
                        self.git_path
                    ),
                    command,
                    returncode: None,
                    stderr: String::new(),
                    not_found: true,
                });
            }
            Err(Error::Process { message, .. }) => {
                return Err(Error::Git {
                    message,
                    command,
                    returncode: None,
                    stderr: String::new(),
                    not_found: false,
                });
            }
            Err(err) => return Err(err),
        };

        if !options.ok.contains(&result.returncode) {
            let stderr = text(&result.stderr).trim().to_string();
            let name = args.first().copied().unwrap_or("");
            let detail = if stderr.is_empty() {
                format!("exit status {}", result.returncode)
            } else {
                stderr.clone()
            };
            return Err(Error::Git {
                message: format!("git {name} failed: {detail}"),
                command,
                returncode: Some(result.returncode),
                stderr,
                not_found: false,
            });
        }
        Ok(result)
    }

    fn lines<S: AsRef<str>>(&self, args: &[S], cancel: &CancelToken) -> Result<Vec<String>> {
        let out = text(&self.run(args, cancel)?.stdout);
        Ok(out
            .split('\n')
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect())
    }

    // -- revisions --

    /// Full SHA of the commit `ref_name` names (tags are peeled), or None
    pub fn resolve_commit(&self, ref_name: &str, cancel: &CancelToken) -> Result<Option<String>> {
        if ref_name.is_empty() {
            return Ok(None);
        }
        let spec = format!("{ref_name}^{{commit}}");
        let result = self.run_with(
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &spec,
            ],
            RunOptions {
                ok: &[0, 1, 128],
                ..Default::default()
            },
            cancel,
        )?;
        if result.returncode != 0 {
            return Ok(None);
        }
        let sha = text(&result.stdout).trim().to_string();
        Ok((!sha.is_empty()).then_some(sha))
    }

    /// The shortest unique abbreviation of `sha`, at least 7 long
    pub fn short_sha(&self, sha: &str, cancel: &CancelToken) -> Result<String> {
        let result = self.run(
            &[
                "rev-parse",
                "--verify",
                "--short=7",
                "--end-of-options",
                sha,
            ],
            cancel,
        )?;
        Ok(text(&result.stdout).trim().to_string())
    }

    /// SHA of HEAD in `worktree`; None on an unborn branch
    pub fn head(&self, worktree: Option<&Path>, cancel: &CancelToken) -> Result<Option<String>> {
        let result = self.run_with(
            &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
            RunOptions {
                worktree,
                ok: &[0, 1, 128],
                ..Default::default()
            },
            cancel,
        )?;
        if result.returncode != 0 {
            return Ok(None);
        }
        let sha = text(&result.stdout).trim().to_string();
        Ok((!sha.is_empty()).then_some(sha))
    }

    /// Absolute path of the worktree's git folder (holds its index)
    pub fn git_dir(&self, worktree: Option<&Path>, cancel: &CancelToken) -> Result<PathBuf> {
        let result = self.run_with(
            &["rev-parse", "--absolute-git-dir"],
            RunOptions {
                worktree,
                ..Default::default()
            },
            cancel,
        )?;
        Ok(PathBuf::from(text(&result.stdout).trim_end_matches('\n')))
    }

    /// ID of the empty tree in this repository's hash
    pub fn empty_tree(&self, cancel: &CancelToken) -> Result<String> {
        let result = self.run_with(
            &["hash-object", "-t", "tree", "--stdin"],
            RunOptions {
                input: Some(b""),
                ..Default::default()
            },
            cancel,
        )?;
        Ok(text(&result.stdout).trim().to_string())
    }

    /// A shallow clone lacks older history, so blame stops at its edge
    pub fn is_shallow(&self, cancel: &CancelToken) -> Result<bool> {
        let result = self.run(&["rev-parse", "--is-shallow-repository"], cancel)?;
        Ok(text(&result.stdout).trim() == "true")
    }

    /// None when the commits have no common ancestor
    pub fn merge_base(&self, a: &str, b: &str, cancel: &CancelToken) -> Result<Option<String>> {
        let result = self.run_with(
            &["merge-base", "--end-of-options", a, b],
            RunOptions {
                ok: &[0, 1],
                ..Default::default()
            },
            cancel,
        )?;
        if result.returncode != 0 {
            return Ok(None);
        }
        let sha = text(&result.stdout).trim().to_string();
        Ok((!sha.is_empty()).then_some(sha))
    }

    /// Whether `ancestor` is reachable from `descendant` (or equal)
    pub fn is_ancestor(
        &self,
        ancestor: &str,
        descendant: &str,
        cancel: &CancelToken,
    ) -> Result<bool> {
        let result = self.run_with(
            &[
                "merge-base",
                "--is-ancestor",
                "--end-of-options",
                ancestor,
                descendant,
            ],
            RunOptions {
                ok: &[0, 1],
                ..Default::default()
            },
            cancel,
        )?;
        Ok(result.returncode == 0)
    }

    // -- refs and worktrees --

    /// Every worktree of the repository, the main one first
    pub fn worktrees(&self, cancel: &CancelToken) -> Result<Vec<GitWorktree>> {
        let result = self.run(&["worktree", "list", "--porcelain", "-z"], cancel)?;
        parse_worktrees(&text(&result.stdout))
    }

    /// Local branches, remote branches (not `*/HEAD`) and tags
    ///
    /// Annotated tags are peeled to their commit. Branches checked out in a
    /// worktree have `worktree` set; the one checked out in this worktree
    /// is `current`.
    pub fn refs(&self, cancel: &CancelToken) -> Result<Vec<GitRef>> {
        let format = ref_format();
        let result = self.run(
            &[
                "for-each-ref",
                format.as_str(),
                "refs/heads",
                "refs/remotes",
                "refs/tags",
            ],
            cancel,
        )?;
        let refs = parse_refs(&text(&result.stdout))?;

        let mut checked_out: HashMap<String, PathBuf> = HashMap::new();
        for worktree in self.worktrees(cancel)? {
            if let Some(branch) = &worktree.branch
                && !worktree.bare
            {
                checked_out
                    .entry(branch.clone())
                    .or_insert(worktree.path.clone());
            }
        }

        let own = realpath(&self.root);
        Ok(refs
            .into_iter()
            .map(|mut r| {
                let path = if r.kind == GitRefType::Branch {
                    checked_out.get(&r.name).cloned()
                } else {
                    None
                };
                r.current = path.as_deref().is_some_and(|path| realpath(path) == own);
                r.worktree = path;
                r
            })
            .collect())
    }

    // -- commits --

    /// Commits selected by `revs` (e.g. `["A..B"]`), newest first
    ///
    /// `all_refs` adds every branch, remote branch and tag, plus HEAD.
    /// `grep` keeps commits whose message, author name or author email
    /// contains it, ignoring case. `notes_ref` fills in each commit's note
    /// from that notes ref. With no revisions on an unborn branch, there
    /// are no commits.
    pub fn log(
        &self,
        revs: &[&str],
        query: &LogQuery<'_>,
        cancel: &CancelToken,
    ) -> Result<Vec<GitCommit>> {
        let Some(grep) = query.grep else {
            return self.log_once(revs, query.max_count, query.skip, None, query, cancel);
        };

        // git ANDs --grep with --author, so search each and merge
        let limit = query.max_count.map(|count| query.skip + count);
        let by_message = self.log_once(
            revs,
            limit,
            0,
            Some(format!("--grep={grep}")),
            query,
            cancel,
        )?;
        let by_author = self.log_once(
            revs,
            limit,
            0,
            Some(format!("--author={grep}")),
            query,
            cancel,
        )?;
        let seen: HashSet<&str> = by_message.iter().map(|c| c.sha.as_str()).collect();
        let extra: Vec<GitCommit> = by_author
            .iter()
            .filter(|c| !seen.contains(c.sha.as_str()))
            .cloned()
            .collect();
        let merged = merge_by_date(by_message.clone(), extra);
        let end = query
            .max_count
            .map_or(merged.len(), |count| (query.skip + count).min(merged.len()));
        let start = query.skip.min(end);
        Ok(merged[start..end].to_vec())
    }

    fn log_once(
        &self,
        revs: &[&str],
        max_count: Option<usize>,
        skip: usize,
        matching: Option<String>,
        query: &LogQuery<'_>,
        cancel: &CancelToken,
    ) -> Result<Vec<GitCommit>> {
        let mut args: Vec<String> = vec![
            "log".into(),
            "-z".into(),
            log_format(query.notes_ref.is_some()),
            "--no-notes".into(),
        ];
        if let Some(notes_ref) = query.notes_ref {
            args.push(format!("--notes={}", notes_ref_name(notes_ref)));
        }
        if let Some(count) = max_count {
            args.push(format!("--max-count={count}"));
        }
        if skip > 0 {
            args.push(format!("--skip={skip}"));
        }
        if let Some(matching) = matching {
            args.extend([
                "--regexp-ignore-case".into(),
                "--fixed-strings".into(),
                matching,
            ]);
        }
        let mut rev_args: Vec<String> = revs.iter().map(|r| r.to_string()).collect();
        if query.all_refs {
            args.extend(["--branches".into(), "--remotes".into(), "--tags".into()]);
            if self.head(None, cancel)?.is_some() {
                rev_args.insert(0, "HEAD".into());
            }
        }
        if rev_args.is_empty() && !query.all_refs && self.head(None, cancel)?.is_none() {
            return Ok(Vec::new());
        }
        args.push("--end-of-options".into());
        args.extend(rev_args);
        args.push("--".into());
        let result = self.run(&args, cancel)?;
        parse_log(&text(&result.stdout), query.notes_ref.is_some())
    }

    /// The commits `shas` name, in that order, read with one git call
    pub fn commits(
        &self,
        shas: &[&str],
        notes_ref: Option<&str>,
        cancel: &CancelToken,
    ) -> Result<Vec<GitCommit>> {
        let unique = unique(shas);
        if unique.is_empty() {
            return Ok(Vec::new());
        }
        let mut args: Vec<String> = vec![
            "log".into(),
            "-z".into(),
            log_format(notes_ref.is_some()),
            "--no-notes".into(),
        ];
        if let Some(notes_ref) = notes_ref {
            args.push(format!("--notes={}", notes_ref_name(notes_ref)));
        }
        args.extend(["--no-walk=unsorted".into(), "--stdin".into()]);
        let revs: String = unique.iter().map(|sha| format!("{sha}\n")).collect();
        let result = self.run_with(
            &args,
            RunOptions {
                input: Some(revs.as_bytes()),
                ..Default::default()
            },
            cancel,
        )?;
        parse_log(&text(&result.stdout), notes_ref.is_some())
    }

    pub fn commit(
        &self,
        sha: &str,
        notes_ref: Option<&str>,
        cancel: &CancelToken,
    ) -> Result<GitCommit> {
        let Some(resolved) = self.resolve_commit(sha, cancel)? else {
            return Err(Error::msg(format!(
                "“{sha}” isn't a commit in this repository."
            )));
        };
        self.commits(&[&resolved], notes_ref, cancel)?
            .into_iter()
            .next()
            .ok_or_else(|| Error::msg(format!("“{sha}” isn't a commit in this repository.")))
    }

    /// SHA -> parent SHAs for the commits `revs` select
    pub fn parents(
        &self,
        revs: &[&str],
        cancel: &CancelToken,
    ) -> Result<HashMap<String, Vec<String>>> {
        Ok(self.parents_in_order(revs, cancel)?.into_iter().collect())
    }

    /// [`Self::parents`] in git's order, newest first
    pub fn parents_in_order(
        &self,
        revs: &[&str],
        cancel: &CancelToken,
    ) -> Result<Vec<(String, Vec<String>)>> {
        let mut args = vec!["rev-list", "--parents", "--end-of-options"];
        args.extend(revs);
        args.push("--");
        let result = self.run(&args, cancel)?;
        Ok(parse_rev_list_parents_ordered(&text(&result.stdout)))
    }

    /// SHAs of the commits `revs` select that touched any of `paths`
    ///
    /// Newest first. `full_history` turns off git's history simplification,
    /// which skips side branches a merge did not take.
    pub fn commits_touching<S: AsRef<str>>(
        &self,
        revs: &[S],
        paths: &[S],
        full_history: bool,
        cancel: &CancelToken,
    ) -> Result<Vec<String>> {
        let mut args: Vec<&str> = vec!["rev-list"];
        if full_history {
            args.push("--full-history");
        }
        args.push("--end-of-options");
        args.extend(revs.iter().map(AsRef::as_ref));
        args.push("--");
        args.extend(paths.iter().map(AsRef::as_ref));
        self.lines(&args, cancel)
    }

    /// SHA -> `git patch-id --stable`; merges and empty commits have none
    pub fn patch_ids(
        &self,
        shas: &[&str],
        cancel: &CancelToken,
    ) -> Result<HashMap<String, String>> {
        if shas.is_empty() {
            return Ok(HashMap::new());
        }
        let revs: String = unique(shas).iter().map(|sha| format!("{sha}\n")).collect();
        let patches = self.run_with(
            &[
                "log",
                "-p",
                "--no-color",
                "--no-ext-diff",
                "--format=commit %H",
                "--no-walk=unsorted",
                "--stdin",
            ],
            RunOptions {
                input: Some(revs.as_bytes()),
                ..Default::default()
            },
            cancel,
        )?;
        let result = self.run_with(
            &["patch-id", "--stable"],
            RunOptions {
                input: Some(&patches.stdout),
                ..Default::default()
            },
            cancel,
        )?;
        parse_patch_ids(&text(&result.stdout))
    }

    /// A commit's changes against its first parent, with renames
    ///
    /// A root commit is compared with the empty tree. Files git shows no
    /// textual change for (mode changes, binary files) get a diff without
    /// hunks.
    pub fn commit_diffs(
        &self,
        sha: &str,
        cancel: &CancelToken,
    ) -> Result<Vec<(GitFileChange, GitFileDiff)>> {
        let parent = self.resolve_commit(&format!("{sha}^1"), cancel)?;
        let revs: Vec<&str> = match &parent {
            Some(parent) => vec![parent.as_str(), sha],
            None => vec!["--root", sha],
        };
        let mut args = vec![
            "diff-tree",
            "-r",
            "-z",
            "-M",
            "--name-status",
            "--no-commit-id",
        ];
        args.extend(&revs);
        args.push("--");
        let changes = parse_name_status(&text(&self.run(&args, cancel)?.stdout))?;
        if changes.is_empty() {
            return Ok(Vec::new());
        }
        let mut args = vec![
            "diff-tree",
            "-r",
            "-p",
            "-M",
            "--no-color",
            "--no-ext-diff",
            "--no-commit-id",
        ];
        args.extend(&revs);
        args.push("--");
        let diffs = parse_diff(&text(&self.run(&args, cancel)?.stdout))?;
        Ok(changes
            .into_iter()
            .map(|change| {
                let diff = pick_diff(&diffs, &change);
                (change, diff)
            })
            .collect())
    }

    // -- notes --

    /// The note on `sha` in `notes_ref`, trimmed; None if there is none
    pub fn read_note(
        &self,
        sha: &str,
        notes_ref: &str,
        cancel: &CancelToken,
    ) -> Result<Option<String>> {
        let ref_arg = format!("--ref={}", notes_ref_name(notes_ref));
        let result = self.run_with(
            &["notes", &ref_arg, "show", sha],
            RunOptions {
                ok: &[0, 1],
                ..Default::default()
            },
            cancel,
        )?;
        if result.returncode != 0 {
            return Ok(None);
        }
        let note = text(&result.stdout).trim().to_string();
        Ok((!note.is_empty()).then_some(note))
    }

    /// Add or replace the note on `sha` in `notes_ref`
    pub fn write_note(
        &self,
        sha: &str,
        note: &str,
        notes_ref: &str,
        cancel: &CancelToken,
    ) -> Result<()> {
        let ref_arg = format!("--ref={}", notes_ref_name(notes_ref));
        self.run_with(
            &["notes", &ref_arg, "add", "--force", "--file=-", sha],
            RunOptions {
                input: Some(note.as_bytes()),
                ..Default::default()
            },
            cancel,
        )?;
        Ok(())
    }

    // -- diffs --

    /// `git diff -M [--cached] <revs> -- <paths>`, parsed
    ///
    /// With no `revs`, compares the index with the files on disk (or,
    /// `cached`, HEAD with the index) in `worktree`. Adjacent hunks are
    /// never joined beyond their context.
    pub fn diff<S: AsRef<str>>(
        &self,
        revs: &[S],
        paths: &[S],
        options: DiffOptions<'_>,
        cancel: &CancelToken,
    ) -> Result<Vec<GitFileDiff>> {
        let context = format!("-U{}", options.context);
        let mut args: Vec<&str> = vec![
            "diff",
            "--no-color",
            "--no-ext-diff",
            "-M",
            &context,
            "--inter-hunk-context=0",
        ];
        if options.cached {
            args.push("--cached");
        }
        if !revs.is_empty() {
            args.push("--end-of-options");
            args.extend(revs.iter().map(AsRef::as_ref));
        }
        args.push("--");
        args.extend(paths.iter().map(AsRef::as_ref));
        let result = self.run_with(
            &args,
            RunOptions {
                worktree: options.worktree,
                ..Default::default()
            },
            cancel,
        )?;
        parse_diff(&text(&result.stdout))
    }

    /// The diff of one changed file; both paths of a rename are passed so
    /// git can pair them. None when git shows no difference.
    pub fn file_diff(
        &self,
        revs: &[String],
        change: &GitFileChange,
        options: DiffOptions<'_>,
        cancel: &CancelToken,
    ) -> Result<Option<GitFileDiff>> {
        if change.untracked {
            return self.untracked_diff(&change.path, options.worktree, cancel);
        }
        let diffs = self.diff(revs, &change.paths(), options, cancel)?;
        if diffs.is_empty() {
            return Ok(None);
        }
        Ok(Some(pick_diff(&diffs, change)))
    }

    /// Files that differ, with renames; same arguments as [`Self::diff`]
    pub fn name_status(
        &self,
        revs: &[String],
        cached: bool,
        worktree: Option<&Path>,
        cancel: &CancelToken,
    ) -> Result<Vec<GitFileChange>> {
        let mut args: Vec<&str> = vec!["diff", "--name-status", "-z", "-M", "--no-ext-diff"];
        if cached {
            args.push("--cached");
        }
        if !revs.is_empty() {
            args.push("--end-of-options");
            args.extend(revs.iter().map(String::as_str));
        }
        args.push("--");
        let result = self.run_with(
            &args,
            RunOptions {
                worktree,
                ..Default::default()
            },
            cancel,
        )?;
        parse_name_status(&text(&result.stdout))
    }

    /// Lines added and deleted per file; same arguments as [`Self::diff`]
    pub fn numstat(
        &self,
        revs: &[String],
        cached: bool,
        worktree: Option<&Path>,
        cancel: &CancelToken,
    ) -> Result<Vec<GitFileStats>> {
        let mut args: Vec<&str> = vec!["diff", "--numstat", "-z", "-M", "--no-ext-diff"];
        if cached {
            args.push("--cached");
        }
        if !revs.is_empty() {
            args.push("--end-of-options");
            args.extend(revs.iter().map(String::as_str));
        }
        args.push("--");
        let result = self.run_with(
            &args,
            RunOptions {
                worktree,
                ..Default::default()
            },
            cancel,
        )?;
        parse_numstat(&text(&result.stdout))
    }

    /// Every file's diff in one uncommitted layer, untracked files aside
    ///
    /// `Staged` is `head` (default: HEAD; the empty tree on an unborn
    /// branch) -> index, with renames; `Unstaged` is index -> files on disk.
    pub fn uncommitted_diffs(
        &self,
        layer: UncommittedLayer,
        head: Option<&str>,
        worktree: Option<&Path>,
        cancel: &CancelToken,
    ) -> Result<Vec<GitFileDiff>> {
        let none: [&str; 0] = [];
        match layer {
            UncommittedLayer::Staged => {
                let revs: Vec<&str> = head.into_iter().collect();
                self.diff(
                    &revs,
                    &none,
                    DiffOptions {
                        cached: true,
                        worktree,
                        ..Default::default()
                    },
                    cancel,
                )
            }
            UncommittedLayer::Unstaged => self.diff(
                &none,
                &none,
                DiffOptions {
                    worktree,
                    ..Default::default()
                },
                cancel,
            ),
        }
    }

    /// Files on disk that git neither tracks nor ignores
    ///
    /// An untracked nested repository is listed as its folder, with a
    /// trailing `/`.
    pub fn untracked_files(
        &self,
        worktree: Option<&Path>,
        cancel: &CancelToken,
    ) -> Result<Vec<String>> {
        let result = self.run_with(
            &["ls-files", "-z", "--others", "--exclude-standard"],
            RunOptions {
                worktree,
                ..Default::default()
            },
            cancel,
        )?;
        Ok(text(&result.stdout)
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// Which of `paths` git ignores
    pub fn ignored(
        &self,
        paths: &[&str],
        worktree: Option<&Path>,
        cancel: &CancelToken,
    ) -> Result<HashSet<String>> {
        if paths.is_empty() {
            return Ok(HashSet::new());
        }
        let data: String = paths.iter().map(|path| format!("{path}\0")).collect();
        // Exits with 1 when none is ignored
        let result = self.run_with(
            &["check-ignore", "-z", "--stdin"],
            RunOptions {
                worktree,
                input: Some(data.as_bytes()),
                ok: &[0, 1],
            },
            cancel,
        )?;
        Ok(text(&result.stdout)
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// An untracked file as an added file's diff; None for folders
    pub fn untracked_diff(
        &self,
        path: &str,
        worktree: Option<&Path>,
        cancel: &CancelToken,
    ) -> Result<Option<GitFileDiff>> {
        let root = worktree.unwrap_or(&self.root);
        let full = root.join(path);
        let is_link = full
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink());
        if path.ends_with('/') || (full.is_dir() && !is_link) {
            return Ok(None);
        }
        if full.symlink_metadata().is_err() {
            return Ok(None);
        }
        // Exits with 1 when the files differ, which they always do here
        let result = self.run_with(
            &[
                "diff",
                "--no-index",
                "--no-color",
                "--no-ext-diff",
                "-U3",
                "--inter-hunk-context=0",
                "--",
                "/dev/null",
                path,
            ],
            RunOptions {
                worktree: Some(root),
                ok: &[0, 1],
                ..Default::default()
            },
            cancel,
        )?;
        Ok(parse_diff(&text(&result.stdout))?.into_iter().next())
    }

    /// An untracked file's lines, all added
    ///
    /// Counts are None for files git treats as binary: a NUL byte in the
    /// first 8000 bytes, or larger than `core.bigFileThreshold`. A file that
    /// vanished since it was listed counts as empty.
    pub fn untracked_stats(
        &self,
        path: &str,
        worktree: Option<&Path>,
        cancel: &CancelToken,
    ) -> Result<GitFileStats> {
        let root = worktree.unwrap_or(&self.root);
        match open_in_worktree(root, path)? {
            Opened::Missing => {
                if root.join(path).is_dir() {
                    Ok(GitFileStats::new(path, None, None))
                } else {
                    Ok(GitFileStats::new(path, Some(0), Some(0)))
                }
            }
            Opened::Link(target) => Ok(count_lines_in(path, [Ok(target)])),
            Opened::File(mut file) => {
                if file.metadata().map(|m| m.len()).unwrap_or(0) > BIG_FILE_THRESHOLD {
                    return Ok(GitFileStats::new(path, None, None));
                }
                let mut failed = None;
                let chunks = std::iter::from_fn(|| {
                    if failed.is_some() {
                        return None;
                    }
                    if let Err(err) = cancel.check() {
                        failed = Some(err);
                        return None;
                    }
                    let mut buffer = vec![0u8; READ_CHUNK];
                    match file.read(&mut buffer) {
                        Ok(0) | Err(_) => None,
                        Ok(read) => {
                            buffer.truncate(read);
                            Some(Ok(buffer))
                        }
                    }
                });
                let stats = count_lines_in(path, chunks);
                match failed {
                    Some(err) => Err(err),
                    None => Ok(stats),
                }
            }
        }
    }

    /// Porcelain blame of `path` over `from_rev..to_rev`
    ///
    /// Normally blames the file at `to_rev`; lines older than `from_rev`
    /// are `boundary`. With `reverse`, blames the file at `from_rev` and
    /// reports the last commit each line existed in. A path the blamed
    /// version doesn't have gives no lines.
    pub fn blame(
        &self,
        from_rev: &str,
        to_rev: &str,
        path: &str,
        reverse: bool,
        cancel: &CancelToken,
    ) -> Result<Vec<GitBlameLine>> {
        let range = format!("{from_rev}..{to_rev}");
        // Root commits inside the range made their lines; without --root
        // git marks them boundary
        let args = [
            "blame",
            "--porcelain",
            if reverse { "--reverse" } else { "--root" },
            &range,
            "--",
            path,
        ];
        match self.run(&args, cancel) {
            Ok(result) => parse_blame(&text(&result.stdout)),
            Err(err) if err.git_stderr().is_some_and(|s| s.contains("no such path")) => {
                Ok(Vec::new())
            }
            Err(err) => Err(err),
        }
    }

    // -- file contents --

    /// The file at `path` in commit `rev`; None if it has no such file
    ///
    /// Symbolic links read as their target, as git stores them.
    pub fn read_blob(
        &self,
        rev: &str,
        path: &str,
        cancel: &CancelToken,
    ) -> Result<Option<Vec<u8>>> {
        let spec = format!("{rev}:{path}");
        let result = self.run_with(
            &["cat-file", "blob", &spec],
            RunOptions {
                ok: &[0, 128],
                ..Default::default()
            },
            cancel,
        )?;
        Ok((result.returncode == 0).then_some(result.stdout))
    }

    /// The staged version of `path` in `worktree`'s index
    ///
    /// None when the index has no such file (or only conflict stages).
    pub fn read_index_file(
        &self,
        path: &str,
        worktree: Option<&Path>,
        cancel: &CancelToken,
    ) -> Result<Option<Vec<u8>>> {
        let spec = format!(":{path}");
        let result = self.run_with(
            &["cat-file", "blob", &spec],
            RunOptions {
                worktree,
                ok: &[0, 128],
                ..Default::default()
            },
            cancel,
        )?;
        Ok((result.returncode == 0).then_some(result.stdout))
    }

    /// The file at `path` on disk in `worktree`
    ///
    /// Never follows a symbolic link, so nothing outside the worktree is
    /// read: a link reads as its target, as git stores it, and a path
    /// through a linked folder reads as None, as does a missing file or a
    /// folder. Fails for paths that leave the worktree.
    pub fn read_worktree_file(
        &self,
        path: &str,
        worktree: Option<&Path>,
        cancel: &CancelToken,
    ) -> Result<Option<Vec<u8>>> {
        cancel.check()?;
        match open_in_worktree(worktree.unwrap_or(&self.root), path)? {
            Opened::Missing => Ok(None),
            Opened::Link(target) => Ok(Some(target)),
            Opened::File(mut file) => {
                let mut data = Vec::new();
                file.read_to_end(&mut data)
                    .map_err(|err| Error::msg(format!("Could not read {path}: {err}")))?;
                Ok(Some(data))
            }
        }
    }
}

/// Options of [`GitRepository::diff`]
#[derive(Debug, Clone, Copy)]
pub struct DiffOptions<'a> {
    pub cached: bool,
    pub context: usize,
    pub worktree: Option<&'a Path>,
}

impl Default for DiffOptions<'_> {
    fn default() -> Self {
        Self {
            cached: false,
            context: 3,
            worktree: None,
        }
    }
}

/// What [`GitRepository::log`] selects besides its revisions
#[derive(Debug, Clone, Copy, Default)]
pub struct LogQuery<'a> {
    pub max_count: Option<usize>,
    pub skip: usize,
    pub grep: Option<&'a str>,
    pub all_refs: bool,
    pub notes_ref: Option<&'a str>,
}

fn unique<'a>(values: &[&'a str]) -> Vec<&'a str> {
    let mut seen = HashSet::new();
    values
        .iter()
        .copied()
        .filter(|value| seen.insert(*value))
        .collect()
}

/// Merge two newest-first commit lists, keeping each one's order
fn merge_by_date(a: Vec<GitCommit>, b: Vec<GitCommit>) -> Vec<GitCommit> {
    let mut merged = Vec::with_capacity(a.len() + b.len());
    let (mut a, mut b) = (a.into_iter().peekable(), b.into_iter().peekable());
    while let (Some(x), Some(y)) = (a.peek(), b.peek()) {
        if timestamp(y) > timestamp(x) {
            merged.extend(b.next());
        } else {
            merged.extend(a.next());
        }
    }
    merged.extend(a);
    merged.extend(b);
    merged
}

fn timestamp(commit: &GitCommit) -> f64 {
    crate::time::parse_iso8601(&commit.committer.date).unwrap_or(0.0)
}

/// The diff of `change` among `diffs`
pub(crate) fn pick_diff(diffs: &[GitFileDiff], change: &GitFileChange) -> GitFileDiff {
    if change.status == GitFileStatus::Deleted
        && let Some(diff) = diffs
            .iter()
            .find(|d| d.old_path.as_deref() == Some(&change.path) && d.new_path.is_none())
    {
        return diff.clone();
    }
    if let Some(diff) = diffs
        .iter()
        .find(|d| d.new_path.as_deref() == Some(&change.path))
    {
        return diff.clone();
    }
    if let Some(diff) = diffs
        .iter()
        .find(|d| d.old_path.as_deref() == Some(&change.path))
    {
        return diff.clone();
    }
    diffs.first().cloned().unwrap_or_else(|| GitFileDiff {
        old_path: Some(
            change
                .original_path
                .clone()
                .unwrap_or_else(|| change.path.clone()),
        ),
        new_path: Some(change.path.clone()),
        binary: false,
        hunks: Vec::new(),
    })
}

fn count_lines_in(path: &str, chunks: impl IntoIterator<Item = Result<Vec<u8>>>) -> GitFileStats {
    let mut lines = 0;
    let mut position = 0;
    let mut last: Option<u8> = None;
    for chunk in chunks {
        let Ok(chunk) = chunk else { break };
        if position < BINARY_CHECK_BYTES {
            let check = (BINARY_CHECK_BYTES - position).min(chunk.len());
            if chunk[..check].contains(&0) {
                return GitFileStats::new(path, None, None);
            }
        }
        lines += chunk.iter().filter(|&&b| b == b'\n').count();
        position += chunk.len();
        if let Some(&byte) = chunk.last() {
            last = Some(byte);
        }
    }
    if last.is_some_and(|byte| byte != b'\n') {
        lines += 1;
    }
    GitFileStats::new(path, Some(lines), Some(0))
}

fn split_relative(path: &str) -> Result<Vec<&str>> {
    let parts: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    if path.is_empty()
        || path.starts_with('/')
        || parts.iter().any(|part| matches!(*part, "" | "." | ".."))
    {
        return Err(Error::msg(format!(
            "“{path}” isn't a path inside the worktree."
        )));
    }
    Ok(parts)
}

enum Opened {
    File(File),
    /// A symbolic link: its target, as git stores it
    Link(Vec<u8>),
    Missing,
}

/// A readable file for a regular file, a link's target, or Missing
///
/// Folders on the way must be real folders, not links.
fn open_in_worktree(root: &Path, path: &str) -> Result<Opened> {
    let parts = split_relative(path)?;
    let mut current = root.to_path_buf();
    for part in &parts[..parts.len() - 1] {
        current.push(part);
        match current.symlink_metadata() {
            Ok(info) if info.file_type().is_dir() => {}
            _ => return Ok(Opened::Missing),
        }
    }
    let full = current.join(parts[parts.len() - 1]);
    let Ok(info) = full.symlink_metadata() else {
        return Ok(Opened::Missing);
    };
    if info.file_type().is_symlink() {
        let target = std::fs::read_link(&full)
            .map_err(|err| Error::msg(format!("Could not read {path}: {err}")))?;
        return Ok(Opened::Link(path_bytes(&target)));
    }
    if !info.file_type().is_file() {
        return Ok(Opened::Missing);
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    match options.open(&full) {
        Ok(file) => Ok(Opened::File(file)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Opened::Missing),
        Err(err) => Err(Error::msg(format!("Could not read {path}: {err}"))),
    }
}

fn path_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        path.to_string_lossy().into_owned().into_bytes()
    }
}
