//! `meld --newtab` support: the first Meld listens on a socket in the
//! runtime directory; later invocations with `--newtab` hand their command
//! line to it and wait until their comparison closes, then exit with its
//! status (what GApplication's remote command lines did).
//!
//! Protocol, one request per connection: the client sends its working
//! directory and arguments as NUL-terminated strings followed by an empty
//! string; the server answers with one line, `<status>` or
//! `<status> <error message>`, once the comparison closes.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use iced::Subscription;
use iced::futures::SinkExt;

use crate::cli::{self, Launch};

fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join(format!("meld-{}.sock", whoami()))
}

fn whoami() -> String {
    std::env::var("USER").unwrap_or_else(|_| "user".into())
}

/// Sends the command line to a running Meld. Returns the exit status to
/// use, or `None` when no Meld is running.
pub fn forward(cwd: &Path, args: &[String]) -> Option<i32> {
    let mut stream = UnixStream::connect(socket_path()).ok()?;
    let mut request = Vec::new();
    for part in std::iter::once(cwd.to_string_lossy().into_owned()).chain(args.iter().cloned()) {
        request.extend_from_slice(part.as_bytes());
        request.push(0);
    }
    request.push(0);
    stream.write_all(&request).ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    let line = line.trim_end();
    let (status, message) = line.split_once(' ').unwrap_or((line, ""));
    if !message.is_empty() {
        eprintln!("meld: {message}");
    }
    Some(status.parse().unwrap_or(1))
}

/// A comparison request from another invocation.
#[derive(Debug, Clone)]
pub struct Request {
    pub launch: Launch,
    pub waiter: Waiter,
}

/// The connection of a remote command line, answered when its comparison
/// closes.
#[derive(Debug, Clone)]
pub struct Waiter(Arc<Mutex<Option<UnixStream>>>);

impl Waiter {
    pub fn finish(&self, status: i32, error: Option<&str>) {
        let Some(mut stream) = self.0.lock().ok().and_then(|mut s| s.take()) else {
            return;
        };
        let line = match error {
            Some(e) => format!("{status} {}\n", e.replace('\n', " ")),
            None => format!("{status}\n"),
        };
        let _ = stream.write_all(line.as_bytes());
    }
}

pub fn listen() -> Subscription<Request> {
    Subscription::run(|| {
        iced::stream::channel(16, async |mut output| {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            std::thread::spawn(move || serve(tx));
            while let Some(request) = rx.recv().await {
                if output.send(request).await.is_err() {
                    break;
                }
            }
        })
    })
}

fn serve(tx: tokio::sync::mpsc::UnboundedSender<Request>) {
    let path = socket_path();
    let listener = match std::os::unix::net::UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(_) => {
            // Either another Meld owns the socket, or it is stale.
            if UnixStream::connect(&path).is_ok() {
                return;
            }
            let _ = std::fs::remove_file(&path);
            match std::os::unix::net::UnixListener::bind(&path) {
                Ok(listener) => listener,
                Err(err) => {
                    log::warn!("cannot listen on {}: {err}", path.display());
                    return;
                }
            }
        }
    };
    for stream in listener.incoming().flatten() {
        let Some((cwd, args)) = read_request(&stream) else {
            continue;
        };
        let waiter = Waiter(Arc::new(Mutex::new(Some(stream))));
        let parsed = cli::parse(args)
            .map_err(|e| e.to_string())
            .and_then(|args| cli::launch_from_args(args, &cwd));
        match parsed {
            Ok(launch) => {
                if tx.send(Request { launch, waiter }).is_err() {
                    break;
                }
            }
            Err(err) => waiter.finish(2, Some(&err)),
        }
    }
}

fn read_request(stream: &UnixStream) -> Option<(PathBuf, Vec<String>)> {
    let mut parts = Vec::new();
    let mut current = Vec::new();
    for byte in BufReader::new(stream).bytes() {
        let byte = byte.ok()?;
        if byte != 0 {
            current.push(byte);
            continue;
        }
        if current.is_empty() {
            break;
        }
        parts.push(String::from_utf8_lossy(&current).into_owned());
        current.clear();
    }
    let mut parts = parts.into_iter();
    let cwd = PathBuf::from(parts.next()?);
    Some((cwd, parts.collect()))
}
