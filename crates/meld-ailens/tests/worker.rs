//! Port of test/ailens/test_worker.py: child processes and cards

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use meld_ailens::cards::{Card, CardKind, layout_cards};
use meld_ailens::error::ProcessErrorKind;
use meld_ailens::process::{CancelToken, ProcessOptions, run_process};
use meld_ailens::{Error, LineRange, Side};

const FIVE: Duration = Duration::from_secs(5);

type Results<T> = Arc<Mutex<Vec<T>>>;

fn results<T>() -> Results<T> {
    Arc::new(Mutex::new(Vec::new()))
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
