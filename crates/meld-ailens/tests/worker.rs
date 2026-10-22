//! Port of test/ailens/test_worker.py: the worker, child processes and cards

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use meld_ailens::cards::{Card, CardKind, layout_cards, reflow};
use meld_ailens::error::ProcessErrorKind;
use meld_ailens::process::{CancelToken, ProcessOptions, run_process};
use meld_ailens::worker::{Delivery, Dispatch, Worker};
use meld_ailens::{Error, LineRange, Side};

/// Collects callbacks like GLib.idle_add, run when the test says so
#[derive(Clone)]
struct QueueDispatch {
    pending: Arc<Mutex<Vec<Delivery>>>,
}

impl QueueDispatch {
    fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn dispatch(&self) -> Dispatch {
        let pending = self.pending.clone();
        Arc::new(move |delivery| pending.lock().unwrap().push(delivery))
    }

    fn drain(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let pending: Vec<Delivery> = std::mem::take(&mut *self.pending.lock().unwrap());
            let any = !pending.is_empty();
            for callback in pending {
                callback();
            }
            if any {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

const FIVE: Duration = Duration::from_secs(5);

fn worker() -> (Worker, QueueDispatch) {
    let dispatch = QueueDispatch::new();
    (Worker::new(3, dispatch.dispatch()), dispatch)
}

type Results<T> = Arc<Mutex<Vec<T>>>;

fn results<T>() -> Results<T> {
    Arc::new(Mutex::new(Vec::new()))
}

fn push<T: Send + 'static>(results: &Results<T>) -> impl FnOnce(T) + Send + 'static {
    let results = results.clone();
    move |value| results.lock().unwrap().push(value)
}

fn fail<T>(_: T) {
    panic!("unexpected callback");
}

#[test]
fn worker_delivers_result() {
    let (worker, dispatch) = worker();
    let got = results();
    worker.submit("job", |_| Ok(42), push(&got), fail, true);
    dispatch.drain(FIVE);
    assert_eq!(*got.lock().unwrap(), [42]);
    assert!(!worker.busy());
    worker.shutdown();
}

#[test]
fn worker_delivers_error() {
    let (worker, dispatch) = worker();
    let errors: Results<String> = results();
    let errors_push = errors.clone();
    worker.submit(
        "job",
        |_| -> meld_ailens::Result<()> { Err(Error::msg("nope")) },
        fail,
        move |err| errors_push.lock().unwrap().push(err.to_string()),
        true,
    );
    dispatch.drain(FIVE);
    assert_eq!(*errors.lock().unwrap(), ["nope"]);
    worker.shutdown();
}

#[test]
fn newer_exclusive_job_wins() {
    let (worker, dispatch) = worker();
    let got: Results<&'static str> = results();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let first = worker.submit(
        "job",
        move |_| {
            started_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(FIVE);
            Ok("old")
        },
        push(&got),
        |_| {},
        true,
    );
    started_rx.recv_timeout(FIVE).unwrap();
    worker.submit("job", |_| Ok("new"), push(&got), |_| {}, true);
    assert!(first.is_cancelled());
    release_tx.send(()).unwrap();
    dispatch.drain(FIVE);
    std::thread::sleep(Duration::from_millis(50));
    dispatch.drain(Duration::from_millis(200));
    assert_eq!(*got.lock().unwrap(), ["new"]);
    worker.shutdown();
}

#[test]
fn non_exclusive_jobs_all_deliver() {
    let (worker, dispatch) = worker();
    let got = results();
    for i in 0..3 {
        worker.submit("job", move |_| Ok(i), push(&got), fail, false);
    }
    let deadline = Instant::now() + FIVE;
    while got.lock().unwrap().len() < 3 && Instant::now() < deadline {
        dispatch.drain(Duration::from_millis(100));
    }
    let mut values = got.lock().unwrap().clone();
    values.sort();
    assert_eq!(values, [0, 1, 2]);
    worker.shutdown();
}

#[test]
fn shutdown_drops_pending_callbacks() {
    let (worker, dispatch) = worker();
    let got = results();
    worker.submit("job", |_| Ok(1), push(&got), |_| {}, true);
    std::thread::sleep(Duration::from_millis(100));
    worker.shutdown();
    dispatch.drain(Duration::from_millis(200));
    assert!(got.lock().unwrap().is_empty());
    // And nothing new runs
    let token = worker.submit("job", |_| Ok(2), push(&got), |_| {}, true);
    assert!(token.is_cancelled());
}

#[test]
fn cancel_all_cancels_tokens() {
    let (worker, dispatch) = worker();
    let seen: Results<bool> = results();
    let (started_tx, started_rx) = mpsc::channel();
    let seen_in_job = seen.clone();
    let failed = Arc::new(AtomicBool::new(false));
    let failed_in_callback = failed.clone();
    worker.submit(
        "job",
        move |cancel: &CancelToken| -> meld_ailens::Result<()> {
            started_tx.send(()).unwrap();
            cancel.wait(FIVE);
            seen_in_job.lock().unwrap().push(cancel.is_cancelled());
            cancel.check()
        },
        fail,
        move |_| failed_in_callback.store(true, Ordering::SeqCst),
        true,
    );
    started_rx.recv_timeout(FIVE).unwrap();
    worker.cancel_all();
    std::thread::sleep(Duration::from_millis(100));
    dispatch.drain(Duration::from_millis(200));
    assert_eq!(*seen.lock().unwrap(), [true]);
    // A cancelled job's error isn't delivered
    assert!(!failed.load(Ordering::SeqCst));
    worker.shutdown();
}

#[test]
fn channel_dispatch_delivers_on_the_receiving_thread() {
    let (dispatch, receiver) = meld_ailens::worker::channel_dispatch();
    let worker = Worker::new(1, dispatch);
    let got = results();
    worker.submit("job", |_| Ok("done"), push(&got), fail, true);
    let delivery = receiver.recv_timeout(FIVE).unwrap();
    delivery();
    assert_eq!(*got.lock().unwrap(), ["done"]);
}

// -- process --

fn sh(script: &str) -> Vec<String> {
    vec!["/bin/sh".into(), "-c".into(), script.into()]
}

#[test]
fn run_process_output() {
    let temp = tempfile::tempdir().unwrap();
    let result = run_process(
        &sh("tr a-z A-Z"),
        &ProcessOptions {
            input: Some(b"hello"),
            cwd: Some(temp.path()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.returncode, 0);
    assert_eq!(result.stdout, b"HELLO");
}

#[test]
fn run_process_exit_code_is_not_an_error() {
    let result = run_process(&sh("exit 3"), &ProcessOptions::default()).unwrap();
    assert_eq!(result.returncode, 3);
}

#[test]
fn run_process_missing_binary() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("does-not-exist");
    let err = run_process(&[missing], &ProcessOptions::default()).unwrap_err();
    assert!(matches!(
        err,
        Error::Process {
            kind: ProcessErrorKind::NotFound,
            ..
        }
    ));
}

#[test]
fn run_process_timeout() {
    let started = Instant::now();
    let err = run_process(
        &sh("sleep 10"),
        &ProcessOptions {
            timeout: Some(Duration::from_millis(200)),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(matches!(
        err,
        Error::Process {
            kind: ProcessErrorKind::Timeout,
            ..
        }
    ));
    assert!(started.elapsed() < FIVE);
}

#[test]
fn run_process_cancel_kills() {
    let token = CancelToken::new();
    let canceller = token.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        canceller.cancel();
    });
    let started = Instant::now();
    let err = run_process(
        &sh("sleep 10"),
        &ProcessOptions {
            cancel: Some(&token),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(err.is_cancelled());
    assert!(started.elapsed() < FIVE);
}

#[test]
fn run_process_already_cancelled() {
    let token = CancelToken::new();
    token.cancel();
    let err = run_process(
        &sh("true"),
        &ProcessOptions {
            cancel: Some(&token),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(err.is_cancelled());
}

#[test]
fn cancel_token_callbacks() {
    let token = CancelToken::new();
    let calls: Results<&'static str> = results();
    let a = calls.clone();
    let remove = token.on_cancel(move || a.lock().unwrap().push("a"));
    let b = calls.clone();
    token.on_cancel(move || b.lock().unwrap().push("b"));
    remove.remove();
    token.cancel();
    token.cancel();
    assert_eq!(*calls.lock().unwrap(), ["b"]);
    // Registering after cancel calls at once
    let c = calls.clone();
    token.on_cancel(move || c.lock().unwrap().push("c"));
    assert_eq!(*calls.lock().unwrap(), ["b", "c"]);
}

// -- cards --

#[test]
fn layout_cards_keeps_level_and_avoids_overlap() {
    assert!(layout_cards(&[], &[], 4.0).is_empty());
    assert_eq!(
        layout_cards(&[10.0, 100.0], &[20.0, 20.0], 4.0),
        [10.0, 100.0]
    );
    assert_eq!(
        layout_cards(&[10.0, 15.0, 16.0], &[20.0, 20.0, 5.0], 4.0),
        [10.0, 34.0, 58.0]
    );
    // Cards above the viewport (negative tops) keep their place
    assert_eq!(
        layout_cards(&[-50.0, -40.0], &[20.0, 20.0], 0.0),
        [-50.0, -30.0]
    );
}

#[test]
fn card_anchor() {
    let added = Card {
        key: "a".into(),
        kind: CardKind::Commit,
        added: vec![LineRange::new(5, 7), LineRange::new(2, 2)],
        deleted: vec![LineRange::new(1, 1)],
        ..Default::default()
    };
    assert_eq!(added.anchor(), (Side::New, 2));
    let deleted = Card {
        key: "d".into(),
        kind: CardKind::Commit,
        deleted: vec![LineRange::new(9, 9)],
        ..Default::default()
    };
    assert_eq!(deleted.anchor(), (Side::Old, 9));
    let empty = Card {
        key: "e".into(),
        kind: CardKind::Unknown,
        ..Default::default()
    };
    assert_eq!(empty.anchor(), (Side::New, 1));
}

#[test]
fn reflow_cases() {
    let cases = [
        ("", ""),
        ("one line", "one line"),
        ("wrapped\nprose here", "wrapped prose here"),
        (
            "para one\nwrapped\n\npara two",
            "para one wrapped\n\npara two",
        ),
        (
            "Intro:\n- item one\n  continues\n- item two",
            "Intro:\n- item one continues\n- item two",
        ),
        ("1. first\n2. second", "1. first\n2. second"),
        ("crlf\r\nline", "crlf line"),
        ("  padded  \n\n\n\n  text ", "padded\n\ntext"),
    ];
    for (text, expected) in cases {
        assert_eq!(reflow(text), expected, "{text:?}");
    }
}
