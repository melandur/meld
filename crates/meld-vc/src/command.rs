//! Running version control programs.
//!
//! Actions (commit, add, revert, ...) are returned as [`VcCommand`]
//! descriptions which the application shows in its console
//! ([`VcCommand::console_line`]) and runs ([`VcCommand::run`], or
//! [`VcCommand::to_std_command`] for asynchronous spawning). This replaces
//! the `runner(command, files, refresh, working_dir)` callback protocol and
//! `VcView._command_iter` from `meld/vcview.py`.
//!
//! The private helpers port `_vc.popen`, `_vc.call` and
//! `_vc.call_temp_output`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::{Result, VcError};
use crate::paths;

/// A version control command to run on a set of files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VcCommand {
    /// Program to run (e.g. `git`).
    pub program: String,
    /// Arguments before the file arguments.
    pub args: Vec<String>,
    /// File arguments; passed relative to `working_dir`.
    pub files: Vec<PathBuf>,
    /// Directory the command runs in.
    pub working_dir: PathBuf,
    /// Whether the view should refresh `working_dir` once the command has
    /// finished (`VcView.refresh_partial`).
    pub refresh: bool,
}

/// Result of running a [`VcCommand`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// Exit code, or `None` if terminated by a signal.
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    /// Whether the program exited successfully.
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }
}

impl VcCommand {
    pub(crate) fn new(
        program: impl Into<String>,
        args: &[&str],
        files: &[PathBuf],
        working_dir: &Path,
    ) -> VcCommand {
        VcCommand {
            program: program.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            files: files.to_vec(),
            working_dir: working_dir.to_path_buf(),
            refresh: true,
        }
    }

    /// The file arguments as passed to the program: relative to
    /// `working_dir`.
    pub fn relative_files(&self) -> Vec<PathBuf> {
        self.files
            .iter()
            .map(|f| paths::relpath(f, &self.working_dir))
            .collect()
    }

    /// Full argument vector (excluding the program).
    pub fn argv(&self) -> Vec<String> {
        let mut argv = self.args.clone();
        argv.extend(self.relative_files().iter().map(|f| paths::lossy(f)));
        argv
    }

    /// The line shown in the console before running, e.g.
    /// `git commit -m "fix bug" a.txt (in /repo)\n`. Tokens containing
    /// whitespace are wrapped in double quotes, like Python's `shelljoin`.
    pub fn console_line(&self) -> String {
        fn quote(s: &str) -> String {
            if s.split_whitespace().count() > 1 {
                format!("\"{s}\"")
            } else {
                s.to_string()
            }
        }
        let mut tokens = vec![quote(&self.program)];
        tokens.extend(self.argv().iter().map(|t| quote(t)));
        format!("{} (in {})\n", tokens.join(" "), self.working_dir.display())
    }

    /// A `std::process::Command` with program, arguments and working
    /// directory set, stdin closed and stdout/stderr piped.
    pub fn to_std_command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(self.argv())
            .current_dir(&self.working_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    /// Run the command synchronously, collecting its output.
    pub fn run(&self) -> Result<CommandOutput> {
        let output = self.to_std_command().output().map_err(|e| VcError::Spawn {
            program: self.program.clone(),
            source: e,
        })?;
        Ok(CommandOutput {
            status: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Run all commands in order, stopping at the first one that cannot be
/// started. Returns the commands paired with their outputs.
pub fn run_all(commands: &[VcCommand]) -> Result<Vec<(VcCommand, CommandOutput)>> {
    commands
        .iter()
        .map(|c| c.run().map(|out| (c.clone(), out)))
        .collect()
}

fn spawn_error(program: &str, e: std::io::Error) -> VcError {
    VcError::Spawn {
        program: program.to_string(),
        source: e,
    }
}

/// Run `program args` in `cwd` and return its raw stdout (`_vc.popen`).
/// Stderr is discarded; the exit status is ignored like in Python.
pub(crate) fn output_bytes(program: &str, args: &[&str], cwd: Option<&Path>) -> Result<Vec<u8>> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    log::debug!("running {program} {args:?} in {cwd:?}");
    let out = cmd.output().map_err(|e| spawn_error(program, e))?;
    if !out.stderr.is_empty() {
        log::debug!("{program} stderr: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(out.stdout)
}

/// Like [`output_bytes`] but decoded as (lossy) UTF-8 text.
pub(crate) fn output_text(program: &str, args: &[&str], cwd: Option<&Path>) -> Result<String> {
    output_bytes(program, args, cwd).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Output lines, like Python's `text.split("\n")[:-1]` but tolerant of a
/// missing final newline and `\r\n` endings.
pub(crate) fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

/// Return whether `program args` ran and exited with status 0
/// (`not _vc.call(...)`). Output is discarded.
pub(crate) fn call_ok(program: &str, args: &[&str], cwd: Option<&Path>) -> bool {
    call_status(program, args, cwd) == Some(0)
}

/// Run `program args`, discarding output; `None` if it could not start or
/// was killed.
pub(crate) fn call_status(program: &str, args: &[&str], cwd: Option<&Path>) -> Option<i32> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    cmd.status().ok().and_then(|s| s.code())
}

/// Run a program, waiting at most `timeout` before killing it
/// (`proc.communicate(timeout=5)` followed by `terminate()`).
pub(crate) fn call_with_timeout(
    program: &str,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
) -> Result<()> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| spawn_error(program, e))?;
    let start = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Create a persistent temporary file named `meld-tmp[-<file_id>]…<suffix>`
/// containing `content` and return its path. The caller owns the file.
pub(crate) fn write_temp(content: &[u8], file_id: &str, suffix: &str) -> Result<PathBuf> {
    let prefix = if file_id.is_empty() {
        "meld-tmp".to_string()
    } else {
        format!("meld-tmp-{file_id}")
    };
    let mut file = tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(suffix)
        .tempfile()?;
    file.write_all(content)?;
    file.flush()?;
    let (_, path) = file.keep().map_err(|e| VcError::Io(e.error))?;
    Ok(path)
}

/// Run `program args` in `cwd` and write its stdout to a temporary file
/// (`_vc.call_temp_output`). As in Python, failures of the program itself
/// produce an empty file rather than an error.
pub(crate) fn call_temp_output(
    program: &str,
    args: &[&str],
    cwd: &Path,
    file_id: &str,
    suffix: &str,
) -> Result<PathBuf> {
    let content = output_bytes(program, args, Some(cwd))?;
    write_temp(&content, file_id, suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_line_quotes_and_relativises() {
        let cmd = VcCommand::new(
            "git",
            &["commit", "-m", "fix the bug"],
            &[
                PathBuf::from("/repo/src/a.txt"),
                PathBuf::from("/repo/b c.txt"),
            ],
            Path::new("/repo"),
        );
        assert_eq!(
            cmd.argv(),
            vec!["commit", "-m", "fix the bug", "src/a.txt", "b c.txt"]
        );
        assert_eq!(
            cmd.console_line(),
            "git commit -m \"fix the bug\" src/a.txt \"b c.txt\" (in /repo)\n"
        );
    }

    #[test]
    fn temp_file_naming() {
        let p = write_temp(b"abc", "Local", ".txt").unwrap();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("meld-tmp-Local"));
        assert!(name.ends_with(".txt"));
        assert_eq!(std::fs::read(&p).unwrap(), b"abc");
        std::fs::remove_file(p).unwrap();
    }
}
