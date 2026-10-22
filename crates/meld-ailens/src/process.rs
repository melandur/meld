//! Cancellable child processes, run without a shell (port of `process.py`)

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::error::{Error, ProcessErrorKind, Result};

type Callback = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct TokenState {
    next_id: u64,
    callbacks: Vec<(u64, Callback)>,
}

#[derive(Default)]
struct TokenInner {
    cancelled: AtomicBool,
    state: Mutex<TokenState>,
    condvar: Condvar,
}

/// Thread-safe cancellation flag shared by every call of one operation
///
/// Clones share the flag.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<TokenInner>);

impl std::fmt::Debug for CancelToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl PartialEq for CancelToken {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Unregisters an [`CancelToken::on_cancel`] callback
pub struct CancelRegistration {
    token: Option<CancelToken>,
    id: u64,
}

impl CancelRegistration {
    pub fn remove(self) {
        if let Some(token) = &self.token {
            let mut state = lock(&token.0.state);
            state.callbacks.retain(|(id, _)| *id != self.id);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    pub fn cancel(&self) {
        let callbacks = {
            let mut state = lock(&self.0.state);
            if self.0.cancelled.swap(true, Ordering::SeqCst) {
                return;
            }
            std::mem::take(&mut state.callbacks)
        };
        self.0.condvar.notify_all();
        for (_, callback) in callbacks {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(callback)).is_err() {
                log::error!("Cancel callback failed");
            }
        }
    }

    /// `Err(Error::Cancelled)` once cancelled
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Sleep up to `timeout`; true if cancelled meanwhile
    pub fn wait(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.0.state);
        loop {
            if self.is_cancelled() {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            state = self
                .0
                .condvar
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }

    /// Call `callback` once on cancel (now, if already cancelled)
    pub fn on_cancel(&self, callback: impl FnOnce() + Send + 'static) -> CancelRegistration {
        {
            let mut state = lock(&self.0.state);
            if !self.is_cancelled() {
                state.next_id += 1;
                let id = state.next_id;
                state.callbacks.push((id, Box::new(callback)));
                return CancelRegistration {
                    token: Some(self.clone()),
                    id,
                };
            }
        }
        callback();
        CancelRegistration { token: None, id: 0 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessResult {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// The exit status; `-signal` when a signal killed it
    pub returncode: i32,
}

/// Options of [`run_process`]
#[derive(Debug, Clone, Default)]
pub struct ProcessOptions<'a> {
    pub cwd: Option<&'a Path>,
    /// Written to stdin; stdin is `/dev/null` without it
    pub input: Option<&'a [u8]>,
    /// Added to the environment (or, without `inherit_env`, the whole of it)
    pub env: Option<&'a HashMap<OsString, OsString>>,
    /// With `env`: false runs with exactly `env`
    pub no_inherit_env: bool,
    pub cancel: Option<&'a CancelToken>,
    pub timeout: Option<Duration>,
}

enum Event {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Cancelled,
}

fn kill(child: &mut Child) {
    #[cfg(unix)]
    {
        // The child leads its own process group: kill what it started too
        if let Ok(pid) = libc::pid_t::try_from(child.id()) {
            // SAFETY: the child isn't reaped yet, so the group still exists
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
}

/// Run `args` to completion and return its output
///
/// Never uses a shell. Returns `Error::Cancelled` when the token fires (the
/// process is killed), a `Process` error of kind `Timeout` after the
/// timeout, and of kind `NotFound` when the executable can't be started. A
/// non-zero exit status is not an error here: callers check `returncode`.
pub fn run_process<S: AsRef<std::ffi::OsStr>>(
    args: &[S],
    options: &ProcessOptions<'_>,
) -> Result<ProcessResult> {
    let never = CancelToken::new();
    let cancel = options.cancel.unwrap_or(&never);
    cancel.check()?;
    let display: Vec<String> = args
        .iter()
        .map(|arg| arg.as_ref().to_string_lossy().into_owned())
        .collect();
    let program = display.first().cloned().unwrap_or_default();
    let Some(first) = args.first() else {
        return Err(Error::Process {
            message: "Could not run an empty command".into(),
            command: display,
            kind: ProcessErrorKind::Other,
        });
    };

    let mut command = Command::new(first);
    command.args(&args[1..]);
    if let Some(cwd) = options.cwd {
        command.current_dir(cwd);
    }
    if let Some(env) = options.env {
        if options.no_inherit_env {
            command.env_clear();
        }
        command.envs(env);
    }
    command
        .stdin(if options.input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    // "Text file busy": a script that was just written can't be run while
    // another thread's fork still holds it open; that clears in moments
    let mut spawned = command.spawn();
    for _ in 0..20 {
        match &spawned {
            Err(err) if err.raw_os_error() == Some(26) => {
                std::thread::sleep(Duration::from_millis(10));
                spawned = command.spawn();
            }
            _ => break,
        }
    }
    let mut child = match spawned {
        Ok(child) => child,
        Err(err) => {
            let not_found = matches!(
                err.kind(),
                std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::PermissionDenied
                    | std::io::ErrorKind::NotADirectory
            ) || err.raw_os_error() == Some(20);
            let reason = describe_io_error(&err);
            return Err(Error::Process {
                message: format!("Could not run {program}: {reason}"),
                command: display,
                kind: if not_found {
                    ProcessErrorKind::NotFound
                } else {
                    ProcessErrorKind::Other
                },
            });
        }
    };

    let (sender, receiver) = mpsc::channel::<Event>();
    if let (Some(input), Some(mut stdin)) = (options.input, child.stdin.take()) {
        let input = input.to_vec();
        std::thread::spawn(move || {
            // The child may exit before reading all of it
            let _ = stdin.write_all(&input);
        });
    }
    if let Some(mut stdout) = child.stdout.take() {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let mut data = Vec::new();
            let _ = stdout.read_to_end(&mut data);
            let _ = sender.send(Event::Stdout(data));
        });
    }
    if let Some(mut stderr) = child.stderr.take() {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let mut data = Vec::new();
            let _ = stderr.read_to_end(&mut data);
            let _ = sender.send(Event::Stderr(data));
        });
    }
    let registration = {
        let sender = Mutex::new(sender);
        cancel.on_cancel(move || {
            let _ = lock(&sender).send(Event::Cancelled);
        })
    };

    let deadline = options.timeout.map(|timeout| Instant::now() + timeout);
    let mut stdout = None;
    let mut stderr = None;
    let outcome: Result<()> = loop {
        if stdout.is_some() && stderr.is_some() {
            break Ok(());
        }
        let event = match deadline {
            Some(deadline) => {
                let now = Instant::now();
                if now >= deadline {
                    break Err(timeout_error(&program, options.timeout, &display));
                }
                match receiver.recv_timeout(deadline - now) {
                    Ok(event) => event,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
                }
            }
            None => match receiver.recv() {
                Ok(event) => event,
                Err(_) => break Ok(()),
            },
        };
        match event {
            Event::Stdout(data) => stdout = Some(data),
            Event::Stderr(data) => stderr = Some(data),
            Event::Cancelled => break Err(Error::Cancelled),
        }
    };

    let mut pause = Duration::from_micros(100);
    let status = match outcome {
        Ok(()) => loop {
            // The output is closed; the process is (nearly always) done
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(err) => {
                    registration.remove();
                    return Err(Error::Process {
                        message: format!("Could not run {program}: {err}"),
                        command: display,
                        kind: ProcessErrorKind::Other,
                    });
                }
            }
            if cancel.wait(pause) {
                kill(&mut child);
                let _ = child.wait();
                registration.remove();
                return Err(Error::Cancelled);
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                kill(&mut child);
                let _ = child.wait();
                registration.remove();
                return Err(timeout_error(&program, options.timeout, &display));
            }
            pause = (pause * 2).min(Duration::from_millis(5));
        },
        Err(err) => {
            kill(&mut child);
            let _ = child.wait();
            registration.remove();
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            return Err(err);
        }
    };
    registration.remove();
    cancel.check()?;

    Ok(ProcessResult {
        stdout: stdout.unwrap_or_default(),
        stderr: stderr.unwrap_or_default(),
        returncode: exit_code(status),
    })
}

fn timeout_error(program: &str, timeout: Option<Duration>, command: &[String]) -> Error {
    let seconds = timeout.map(|t| t.as_secs_f64()).unwrap_or_default();
    Error::Process {
        message: format!("{program} took longer than {} seconds", format_g(seconds)),
        command: command.to_vec(),
        kind: ProcessErrorKind::Timeout,
    }
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return -signal;
        }
    }
    -1
}

fn describe_io_error(err: &std::io::Error) -> String {
    // Python shows strerror, without the "(os error N)" suffix
    let text = err.to_string();
    match text.rfind(" (os error ") {
        Some(index) => text[..index].to_string(),
        None => text,
    }
}

/// Python's `{value:g}` for the seconds in messages: `180`, `0.3`
pub(crate) fn format_g(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}
