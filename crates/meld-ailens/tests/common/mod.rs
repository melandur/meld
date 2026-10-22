//! Deterministic git repositories for the AI Lens tests (port of
//! `test/ailens/fixture_repo.py` and `conftest.py`)
//!
//! Authors, dates and config are fixed and isolated from the user's git
//! config, so the same steps always produce the same SHAs.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use meld_ailens::git::{GitOptions, GitRepository};

pub const HUMAN: (&str, &str) = ("Jane Doe", "jane@example.com");

pub const START_EPOCH: u64 = 1_700_000_000;

/// The trailer Claude Code adds, e.g. `co_authored_by_claude("Opus 5.5")`
pub fn co_authored_by_claude(model: &str) -> (String, String) {
    (
        "Co-Authored-By".into(),
        format!("Claude {model} <noreply@anthropic.com>"),
    )
}

pub fn isolated_git_env() -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars()
        .filter(|(key, _)| !key.starts_with("GIT_"))
        .collect();
    let home = env
        .get("HOME")
        .cloned()
        .unwrap_or_else(|| "/nonexistent".into());
    env.insert("GIT_CONFIG_GLOBAL".into(), "/dev/null".into());
    env.insert("GIT_CONFIG_NOSYSTEM".into(), "1".into());
    env.insert("LC_ALL".into(), "C".into());
    env.insert("HOME".into(), home);
    env
}

/// File contents for [`FixtureRepo::commit`] and [`FixtureRepo::write_files`]
#[derive(Debug, Clone)]
pub enum Content {
    Text(String),
    Bytes(Vec<u8>),
    /// Deletes the file
    Delete,
}

impl From<&str> for Content {
    fn from(text: &str) -> Self {
        Content::Text(text.to_string())
    }
}

impl From<String> for Content {
    fn from(text: String) -> Self {
        Content::Text(text)
    }
}

impl From<&[u8]> for Content {
    fn from(data: &[u8]) -> Self {
        Content::Bytes(data.to_vec())
    }
}

impl<const N: usize> From<&[u8; N]> for Content {
    fn from(data: &[u8; N]) -> Self {
        Content::Bytes(data.to_vec())
    }
}

impl From<Vec<u8>> for Content {
    fn from(data: Vec<u8>) -> Self {
        Content::Bytes(data)
    }
}

impl From<Option<&str>> for Content {
    fn from(text: Option<&str>) -> Self {
        text.map_or(Content::Delete, Content::from)
    }
}

/// `files![("a.txt", "a\n"), ("gone", None::<&str>)]`
#[macro_export]
macro_rules! files {
    ($(($name:expr, $content:expr)),* $(,)?) => {
        vec![$(($name.to_string(), $crate::common::Content::from($content))),*]
    };
}

/// Options of [`FixtureRepo::commit_with`]
#[derive(Debug, Clone, Default)]
pub struct CommitOptions {
    pub label: Option<String>,
    pub author: Option<(String, String)>,
    /// Applied before the files are written
    pub renames: Vec<(String, String)>,
    pub trailers: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
}

impl CommitOptions {
    pub fn label(label: &str) -> Self {
        Self {
            label: Some(label.into()),
            ..Default::default()
        }
    }

    pub fn with_label(mut self, label: &str) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn with_author(mut self, author: (&str, &str)) -> Self {
        self.author = Some((author.0.into(), author.1.into()));
        self
    }

    pub fn with_trailer(mut self, trailer: (String, String)) -> Self {
        self.trailers.push(trailer);
        self
    }

    pub fn with_rename(mut self, old: &str, new: &str) -> Self {
        self.renames.push((old.into(), new.into()));
        self
    }

    pub fn in_worktree(mut self, cwd: &Path) -> Self {
        self.cwd = Some(cwd.to_path_buf());
        self
    }
}

/// A git repo in `path`, built step by step; `shas` maps labels to commits
pub struct FixtureRepo {
    pub path: PathBuf,
    pub env: HashMap<String, String>,
    pub shas: HashMap<String, String>,
    tick: u64,
    /// Keeps the temporary folder alive
    _temp: Option<tempfile::TempDir>,
}

impl FixtureRepo {
    /// An empty git repo on `main` in a new temporary folder (the
    /// `fixture_repo` pytest fixture); its parent folder is
    /// [`Self::tmp_path`]
    pub fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary folder");
        let mut repo = Self::at(&temp.path().join("repo"), "main");
        repo._temp = Some(temp);
        repo
    }

    /// The temporary folder the repository is in (pytest's `tmp_path`)
    pub fn tmp_path(&self) -> PathBuf {
        self.path.parent().expect("has a parent").to_path_buf()
    }

    pub fn at(path: &Path, initial_branch: &str) -> Self {
        std::fs::create_dir_all(path).expect("create the repository folder");
        let path = std::fs::canonicalize(path).expect("canonical path");
        let repo = Self {
            path,
            env: isolated_git_env(),
            shas: HashMap::new(),
            tick: 0,
            _temp: None,
        };
        repo.git(&[
            "init",
            "--quiet",
            &format!("--initial-branch={initial_branch}"),
        ]);
        repo.git(&["config", "user.name", HUMAN.0]);
        repo.git(&["config", "user.email", HUMAN.1]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        repo.git(&["config", "tag.gpgsign", "false"]);
        repo.git(&["config", "core.autocrlf", "false"]);
        repo
    }

    /// A [`GitRepository`] on this fixture, isolated from the user's config
    pub fn open(&self) -> GitRepository {
        GitRepository::new(&self.path, &self.git_options())
    }

    pub fn git_options(&self) -> GitOptions {
        GitOptions {
            git_path: "git".into(),
            env: Some(self.env.clone()),
        }
    }

    // -- plumbing --

    pub fn git(&self, args: &[&str]) -> String {
        self.git_full(args, None, None, None).expect("git succeeds")
    }

    pub fn git_in(&self, cwd: &Path, args: &[&str]) -> String {
        self.git_full(args, None, Some(cwd), None)
            .expect("git succeeds")
    }

    /// Run git; Err with its stderr when it fails
    pub fn git_full(
        &self,
        args: &[&str],
        env: Option<&HashMap<String, String>>,
        cwd: Option<&Path>,
        input: Option<&[u8]>,
    ) -> Result<String, String> {
        let mut command = Command::new("git");
        command
            .args(args)
            .current_dir(cwd.unwrap_or(&self.path))
            .env_clear()
            .envs(env.unwrap_or(&self.env))
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|err| err.to_string())?;
        if let Some(input) = input {
            use std::io::Write;
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(input)
                .map_err(|err| err.to_string())?;
        }
        let output = child.wait_with_output().map_err(|err| err.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// The environment of the next commit: a fixed author and a date one
    /// minute after the previous one
    pub fn commit_env(&mut self, author: (&str, &str)) -> HashMap<String, String> {
        let date = format!("@{} +0000", START_EPOCH + 60 * self.tick);
        self.tick += 1;
        let mut env = self.env.clone();
        for (key, value) in [
            ("GIT_AUTHOR_NAME", author.0),
            ("GIT_AUTHOR_EMAIL", author.1),
            ("GIT_AUTHOR_DATE", &date),
            ("GIT_COMMITTER_NAME", author.0),
            ("GIT_COMMITTER_EMAIL", author.1),
            ("GIT_COMMITTER_DATE", &date),
        ] {
            env.insert(key.into(), value.into());
        }
        env
    }

    fn record(&mut self, label: Option<&str>, cwd: Option<&Path>) -> String {
        let sha = self
            .git_full(&["rev-parse", "HEAD"], None, cwd, None)
            .expect("rev-parse")
            .trim()
            .to_string();
        if let Some(label) = label {
            assert!(
                !self.shas.contains_key(label),
                "Duplicate fixture label {label:?}"
            );
            self.shas.insert(label.into(), sha.clone());
        }
        sha
    }

    /// A label's SHA, or `reference` itself
    pub fn resolve(&self, reference: &str) -> String {
        self.shas
            .get(reference)
            .cloned()
            .unwrap_or_else(|| reference.to_string())
    }

    pub fn sha(&self, label: &str) -> String {
        self.shas
            .get(label)
            .cloned()
            .unwrap_or_else(|| panic!("no fixture label {label:?}"))
    }

    /// Write files on disk only; `Content::Delete` deletes one
    pub fn write_files(&self, files: Vec<(String, Content)>) {
        self.write_files_in(&self.path.clone(), files);
    }

    pub fn write_files_in(&self, root: &Path, files: Vec<(String, Content)>) {
        for (name, content) in files {
            let target = root.join(&name);
            match content {
                Content::Delete => std::fs::remove_file(&target).expect("delete the file"),
                Content::Text(text) => {
                    std::fs::create_dir_all(target.parent().expect("parent"))
                        .expect("create folders");
                    std::fs::write(&target, text.as_bytes()).expect("write the file");
                }
                Content::Bytes(data) => {
                    std::fs::create_dir_all(target.parent().expect("parent"))
                        .expect("create folders");
                    std::fs::write(&target, data).expect("write the file");
                }
            }
        }
    }

    // -- steps --

    /// Commit `files` (`Content::Delete` deletes) as Jane Doe
    pub fn commit(
        &mut self,
        message: &str,
        files: Vec<(String, Content)>,
        label: Option<&str>,
    ) -> String {
        let options = CommitOptions {
            label: label.map(str::to_string),
            ..Default::default()
        };
        self.commit_with(message, files, options)
    }

    /// Commit `files`; renames are applied first
    pub fn commit_with(
        &mut self,
        message: &str,
        files: Vec<(String, Content)>,
        options: CommitOptions,
    ) -> String {
        let root = options.cwd.clone().unwrap_or_else(|| self.path.clone());
        for (old, new) in &options.renames {
            std::fs::create_dir_all(root.join(new).parent().expect("parent"))
                .expect("create folders");
            self.git_in(&root, &["mv", old, new]);
        }
        for (name, content) in files {
            match content {
                Content::Delete => {
                    self.git_in(&root, &["rm", "--quiet", "--", &name]);
                }
                content => self.write_files_in(&root, vec![(name, content)]),
            }
        }
        let mut message = message.to_string();
        if !options.trailers.is_empty() {
            message.push_str("\n\n");
            message.push_str(
                &options
                    .trailers
                    .iter()
                    .map(|(k, v)| format!("{k}: {v}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        self.git_in(&root, &["add", "--all"]);
        let author = options
            .author
            .clone()
            .unwrap_or((HUMAN.0.into(), HUMAN.1.into()));
        let env = self.commit_env((&author.0, &author.1));
        self.git_full(
            &[
                "commit",
                "--quiet",
                "--allow-empty",
                "--no-verify",
                "-m",
                &message,
            ],
            Some(&env),
            Some(&root),
            None,
        )
        .expect("commit");
        self.record(options.label.as_deref(), Some(&root))
    }

    /// Create a branch at `start` and check it out
    pub fn branch(&self, name: &str, start: &str) {
        let start = self.resolve(start);
        self.git(&["checkout", "--quiet", "-b", name, &start]);
    }

    pub fn checkout(&self, reference: &str) {
        let reference = self.resolve(reference);
        self.git(&["checkout", "--quiet", &reference]);
    }

    /// A lightweight tag, or an annotated one with a message
    pub fn tag(&mut self, name: &str, reference: &str, message: Option<&str>) {
        let reference = self.resolve(reference);
        match message {
            None => {
                self.git(&["tag", name, &reference]);
            }
            Some(message) => {
                let env = self.commit_env(HUMAN);
                self.git_full(
                    &["tag", "-a", name, "-m", message, &reference],
                    Some(&env),
                    None,
                    None,
                )
                .expect("tag");
            }
        }
    }

    /// Always creates a merge commit (`--no-ff`)
    pub fn merge(
        &mut self,
        reference: &str,
        message: &str,
        label: Option<&str>,
        strategy_option: Option<&str>,
    ) -> String {
        let reference = self.resolve(reference);
        let mut args = vec!["merge", "--quiet", "--no-ff", "--no-edit", "-m", message];
        if let Some(option) = strategy_option {
            args.extend(["-X", option]);
        }
        args.push(&reference);
        let env = self.commit_env(HUMAN);
        self.git_full(&args, Some(&env), None, None).expect("merge");
        self.record(label, None)
    }

    pub fn stage(&self, paths: &[&str]) {
        if paths.is_empty() {
            self.git(&["add", "--all"]);
        } else {
            let mut args = vec!["add", "--"];
            args.extend(paths);
            self.git(&args);
        }
    }

    pub fn note(&mut self, reference: &str, text: &str, notes_ref: &str) {
        let reference = self.resolve(reference);
        let env = self.commit_env(HUMAN);
        self.git_full(
            &[
                "notes",
                &format!("--ref={notes_ref}"),
                "add",
                "-f",
                "-m",
                text,
                &reference,
            ],
            Some(&env),
            None,
            None,
        )
        .expect("note");
    }

    /// Add a worktree at `path` on a new `branch`
    pub fn worktree(&self, path: &Path, branch: &str, start: &str) -> PathBuf {
        let start = self.resolve(start);
        self.git(&[
            "worktree",
            "add",
            "--quiet",
            "-b",
            branch,
            path.to_str().expect("UTF-8 path"),
            &start,
        ]);
        std::fs::canonicalize(path).expect("canonical path")
    }
}

/// `"".join(f"{value}\n" for value in values)`
pub fn lines(values: &[&str]) -> String {
    values.iter().map(|value| format!("{value}\n")).collect()
}
