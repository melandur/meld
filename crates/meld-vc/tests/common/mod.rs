//! Helpers shared by the integration tests of the non-Git backends.
//!
//! The tools are found through `PATH`; a test returns early (and prints
//! why) when its tool is not installed, so the suite passes on machines
//! that only have some of them.
//!
//! Every test binary runs the tools with a private, empty `HOME` (and no
//! Mercurial user configuration) so the user's configuration is neither
//! read nor written.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

use meld_vc::{VcCommand, run_all};

/// Whether `program` can be found on `PATH`.
pub fn on_path(program: &str) -> bool {
    meld_vc::paths::which(program).is_some()
}

/// Return early from a test if none of the given programs is on `PATH`.
#[macro_export]
macro_rules! require_tool {
    ($($program:expr),+) => {
        if ![$($program),+].iter().any(|p| common::on_path(p)) {
            eprintln!("skipping: none of {:?} found on PATH", [$($program),+]);
            return;
        }
        common::isolate_environment();
    };
}

/// Point `HOME` (and the per-tool configuration variables) at a private
/// folder for this test process, before any test spawns a program.
pub fn isolate_environment() {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let home = tempfile::Builder::new()
            .prefix("meld-vc-test-home")
            .tempdir()
            .expect("temporary home")
            .keep();
        // SAFETY: called once, under the `OnceLock`, before this test
        // process spawns any program; every test calls this first and
        // concurrent callers block until the initialisation has finished.
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
            std::env::set_var("XDG_CACHE_HOME", home.join(".cache"));
            std::env::set_var("HGRCPATH", "");
            std::env::set_var("HGUSER", "Meld Test <meld@example.com>");
            std::env::set_var("BRZ_EMAIL", "Meld Test <meld@example.com>");
            std::env::set_var("DARCS_EMAIL", "Meld Test <meld@example.com>");
            std::env::set_var("EMAIL", "meld@example.com");
            std::env::set_var("LC_ALL", "C.UTF-8");
            std::env::remove_var("CVSROOT");
            std::env::remove_var("CVS_RSH");
        }
        home
    });
}

/// Run `program args` in `dir`, panicking on failure unless `allow_fail`.
pub fn run_in(dir: &Path, program: &str, args: &[&str], allow_fail: bool) -> Output {
    let out = Command::new(program)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("{program} runs: {e}"));
    assert!(
        allow_fail || out.status.success(),
        "{program} {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// Run `program args` in `dir` and return its stdout; panics on failure.
pub fn tool(dir: &Path, program: &str, args: &[&str]) -> String {
    String::from_utf8_lossy(&run_in(dir, program, args, false).stdout).into_owned()
}

/// The first non-empty line of `program --version`, for diagnostics.
pub fn version(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or_default()
                .to_string()
        })
        .unwrap_or_default()
}

pub fn write(root: &Path, name: &str, content: &str) {
    let p = root.join(name);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(p, content).unwrap();
}

/// Run backend commands, asserting that each one succeeds.
pub fn run_ok(cmds: Vec<VcCommand>) {
    for (cmd, out) in run_all(&cmds).unwrap() {
        assert!(
            out.success(),
            "{} failed: {}{}",
            cmd.console_line(),
            out.stdout,
            out.stderr
        );
    }
}

/// Read and delete a temporary file returned by a backend.
pub fn take(path: &Path) -> String {
    let content = fs::read_to_string(path).unwrap();
    fs::remove_file(path).unwrap();
    content
}

/// A temporary folder holding a repository (or working copy) at `root`.
pub struct Fixture {
    pub _tmp: tempfile::TempDir,
    pub root: PathBuf,
}

impl Fixture {
    pub fn new(name: &str) -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(name);
        fs::create_dir(&root).unwrap();
        Fixture { _tmp: tmp, root }
    }

    /// Another path inside the temporary folder (next to `root`).
    pub fn sibling(&self, name: &str) -> PathBuf {
        self._tmp.path().join(name)
    }
}
