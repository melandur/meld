//! Running `gh` and `glab`
//!
//! The clients only ever talk to a [`Runner`], so tests (and anything else
//! that must not reach a real forge) swap in [`ScriptedRunner`].

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Mutex;

use meld_ailens::CancelToken;
use meld_ailens::process::{ProcessOptions, run_process};

use crate::error::{Error, Result};
use crate::models::ForgeKind;

/// Runs a forge's tool and returns what it printed
pub trait Runner: Send + Sync {
    /// Runs the tool of `kind` with `args` (and `input` on stdin); fails
    /// unless it exits with 0
    fn run(
        &self,
        kind: ForgeKind,
        args: &[String],
        input: Option<&[u8]>,
        cancel: &CancelToken,
    ) -> Result<Vec<u8>>;
}

/// Runs the real tools as child processes
#[derive(Debug, Clone)]
pub struct ProcessRunner {
    /// `gh` on `PATH` by default
    pub gh: String,
    /// `glab` on `PATH` by default
    pub glab: String,
    /// Where the tools run
    pub cwd: Option<PathBuf>,
}

impl Default for ProcessRunner {
    fn default() -> Self {
        Self {
            gh: "gh".into(),
            glab: "glab".into(),
            cwd: None,
        }
    }
}

/// The environment the tools run with: no prompts, no pager, no colors
fn environment() -> HashMap<OsString, OsString> {
    let mut env: HashMap<OsString, OsString> = std::env::vars_os().collect();
    for (key, value) in [
        ("GH_PROMPT_DISABLED", "1"),
        ("GH_NO_UPDATE_NOTIFIER", "1"),
        ("NO_COLOR", "1"),
        ("CLICOLOR", "0"),
        ("GH_PAGER", "cat"),
        ("GLAB_PAGER", "cat"),
        ("PAGER", "cat"),
        ("GLAB_NO_PROMPT", "1"),
        ("NO_PROMPT", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
    ] {
        env.insert(key.into(), value.into());
    }
    env
}

/// Whether the tool's error output says it isn't logged in
pub fn looks_like_auth_error(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    [
        "gh auth login",
        "glab auth login",
        "not logged",
        "authentication",
        "401 unauthorized",
        "no token",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

impl Runner for ProcessRunner {
    fn run(
        &self,
        kind: ForgeKind,
        args: &[String],
        input: Option<&[u8]>,
        cancel: &CancelToken,
    ) -> Result<Vec<u8>> {
        let program = match kind {
            ForgeKind::GitHub => &self.gh,
            ForgeKind::GitLab => &self.glab,
        };
        let mut full = vec![program.clone()];
        full.extend(args.iter().cloned());
        let env = environment();
        let result = run_process(
            &full,
            &ProcessOptions {
                cwd: self.cwd.as_deref(),
                input,
                env: Some(&env),
                no_inherit_env: true,
                cancel: Some(cancel),
                timeout: None,
            },
        );
        let result = match result {
            Ok(result) => result,
            Err(meld_ailens::Error::Cancelled) => return Err(Error::Cancelled),
            Err(meld_ailens::Error::Process {
                kind: meld_ailens::error::ProcessErrorKind::NotFound,
                ..
            }) => {
                return Err(Error::ToolMissing {
                    program: program.clone(),
                    cli: kind.cli_name().into(),
                    nouns: kind.nouns().into(),
                });
            }
            Err(err) => {
                return Err(Error::Command {
                    program: kind.tool().into(),
                    message: err.to_string(),
                    auth: false,
                });
            }
        };
        if result.returncode != 0 {
            let stderr = String::from_utf8_lossy(&result.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&result.stdout).trim().to_string();
            let message = if stderr.is_empty() { stdout } else { stderr };
            let message = if message.is_empty() {
                format!("exit status {}", result.returncode)
            } else {
                message
            };
            return Err(Error::Command {
                program: kind.tool().into(),
                auth: looks_like_auth_error(&message),
                message,
            });
        }
        Ok(result.stdout)
    }
}

/// One call a [`ScriptedRunner`] saw
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub kind: ForgeKind,
    pub args: Vec<String>,
    pub input: Option<String>,
}

impl Call {
    /// The arguments joined by spaces, for matching in tests
    pub fn line(&self) -> String {
        self.args.join(" ")
    }
}

type Answer = Box<dyn Fn(&Call) -> Option<Result<String>> + Send + Sync>;

/// A runner that answers from a script instead of running anything, and
/// records every call (for tests, and as the safe default when posting
/// must not happen)
#[derive(Default)]
pub struct ScriptedRunner {
    answers: Vec<Answer>,
    calls: Mutex<Vec<Call>>,
}

impl ScriptedRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer calls whose arguments contain every word of `pattern` (in
    /// order of `when` calls; the first match answers)
    pub fn when(mut self, pattern: &str, output: impl Into<String>) -> Self {
        let words: Vec<String> = pattern.split_whitespace().map(str::to_string).collect();
        let output = output.into();
        self.answers.push(Box::new(move |call| {
            words
                .iter()
                .all(|w| call.args.iter().any(|a| a.contains(w.as_str())))
                .then(|| Ok(output.clone()))
        }));
        self
    }

    /// Fail calls matching `pattern` with `error`
    pub fn fail(mut self, pattern: &str, error: Error) -> Self {
        let words: Vec<String> = pattern.split_whitespace().map(str::to_string).collect();
        self.answers.push(Box::new(move |call| {
            words
                .iter()
                .all(|w| call.args.iter().any(|a| a.contains(w.as_str())))
                .then(|| Err(error.clone()))
        }));
        self
    }

    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().map(|c| c.clone()).unwrap_or_default()
    }
}

impl Runner for ScriptedRunner {
    fn run(
        &self,
        kind: ForgeKind,
        args: &[String],
        input: Option<&[u8]>,
        cancel: &CancelToken,
    ) -> Result<Vec<u8>> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let call = Call {
            kind,
            args: args.to_vec(),
            input: input.map(|i| String::from_utf8_lossy(i).into_owned()),
        };
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(call.clone());
        }
        for answer in &self.answers {
            if let Some(result) = answer(&call) {
                return result.map(String::into_bytes);
            }
        }
        Err(Error::Command {
            program: kind.tool().into(),
            message: format!("no scripted answer for: {}", call.line()),
            auth: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn script(dir: &std::path::Path, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    #[cfg(unix)]
    #[test]
    fn process_runner_runs_a_fake_tool() {
        let dir = tempfile::tempdir().unwrap();
        let gh = script(dir.path(), "gh", r#"echo "args: $*"; cat"#);
        let runner = ProcessRunner {
            gh,
            ..Default::default()
        };
        let out = runner
            .run(
                ForgeKind::GitHub,
                &["api".into(), "x".into()],
                Some(b"{\"a\":1}"),
                &CancelToken::new(),
            )
            .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "args: api x\n{\"a\":1}");
    }

    #[cfg(unix)]
    #[test]
    fn failures_carry_the_tools_message() {
        let dir = tempfile::tempdir().unwrap();
        let glab = script(
            dir.path(),
            "glab",
            "echo 'run glab auth login first' >&2; exit 1",
        );
        let runner = ProcessRunner {
            glab,
            ..Default::default()
        };
        let err = runner
            .run(ForgeKind::GitLab, &[], None, &CancelToken::new())
            .unwrap_err();
        assert_eq!(
            err,
            Error::Command {
                program: "glab".into(),
                message: "run glab auth login first".into(),
                auth: true
            }
        );
    }

    #[test]
    fn a_missing_tool_says_so() {
        let runner = ProcessRunner {
            gh: "/nonexistent/meld-test-gh".into(),
            ..Default::default()
        };
        let err = runner
            .run(ForgeKind::GitHub, &[], None, &CancelToken::new())
            .unwrap_err();
        assert!(matches!(err, Error::ToolMissing { .. }), "{err:?}");
        assert!(err.to_string().contains("GitHub CLI"));
    }

    #[test]
    fn scripted_runner_answers_and_records() {
        let runner = ScriptedRunner::new().when("pr list", "[]");
        let out = runner
            .run(
                ForgeKind::GitHub,
                &["pr".into(), "list".into()],
                None,
                &CancelToken::new(),
            )
            .unwrap();
        assert_eq!(out, b"[]");
        assert!(
            runner
                .run(
                    ForgeKind::GitHub,
                    &["api".into()],
                    None,
                    &CancelToken::new()
                )
                .is_err()
        );
        assert_eq!(runner.calls().len(), 2);
    }
}
