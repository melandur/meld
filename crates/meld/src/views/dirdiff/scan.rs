//! Background folder scans (the `_search_recursively_iter()` task).
//!
//! A scan walks the folders depth-first on its own thread and streams
//! batches of scanned folders back, so the tree fills in as it goes and
//! the UI stays responsive. Setting the cancel flag stops the walk
//! promptly, between the entries of a folder.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use iced::futures::Stream;
use iced::futures::channel::{mpsc, oneshot};
use meld_files::dirdiff::{Row, WalkItem};
use meld_files::FolderComparison;

#[derive(Debug, Clone)]
pub enum ScanEvent {
    /// The scanned folder's own row, before its contents.
    Base(Box<Row>),
    /// Scanned folders, in walk order, and the folder being scanned.
    Items(Arc<Vec<WalkItem>>, String),
    /// The walk finished or was cancelled.
    Done { cancelled: bool },
}

const BATCH_INTERVAL: Duration = Duration::from_millis(80);

pub fn start(comparison: FolderComparison, roots: Vec<PathBuf>, cancel: Arc<AtomicBool>) -> impl Stream<Item = ScanEvent> {
    let (tx, rx) = mpsc::unbounded();
    std::thread::spawn(move || {
        let base = comparison.root_row(&roots);
        if tx.unbounded_send(ScanEvent::Base(Box::new(base))).is_err() {
            return;
        }
        let prefix = roots.first().cloned().unwrap_or_default();
        let mut batch = Vec::new();
        let mut last_sent = Instant::now();
        let mut walk = comparison.walk_cancellable(&roots, cancel.clone());
        for item in walk.by_ref() {
            batch.push(item);
            if last_sent.elapsed() >= BATCH_INTERVAL {
                let current = describe(&prefix, batch.last());
                if tx
                    .unbounded_send(ScanEvent::Items(Arc::new(std::mem::take(&mut batch)), current))
                    .is_err()
                {
                    return;
                }
                last_sent = Instant::now();
            }
        }
        let cancelled = walk.cancelled() || cancel.load(Ordering::Relaxed);
        if !batch.is_empty() && !cancelled {
            let current = describe(&prefix, batch.last());
            let _ = tx.unbounded_send(ScanEvent::Items(Arc::new(batch), current));
        }
        let _ = tx.unbounded_send(ScanEvent::Done { cancelled });
    });
    rx
}

fn describe(prefix: &std::path::Path, item: Option<&WalkItem>) -> String {
    item.and_then(|i| i.scan.roots.first())
        .map(|p| p.strip_prefix(prefix).unwrap_or(p).display().to_string())
        .unwrap_or_default()
}

/// Runs blocking work on a thread of its own.
pub fn background<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> impl Future<Output = Option<T>> {
    let (tx, rx) = oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    async move { rx.await.ok() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::futures::StreamExt;
    use meld_files::FolderOptions;

    fn block_on<F: Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
    }

    #[test]
    fn scans_stream_folders_and_finish() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir_all(a.join("sub")).unwrap();
        std::fs::create_dir_all(b.join("sub")).unwrap();
        std::fs::write(a.join("sub/f"), "1").unwrap();
        std::fs::write(b.join("sub/f"), "2").unwrap();
        let comparison = FolderComparison::new(FolderOptions::default(), Vec::new(), Vec::new());
        let events: Vec<ScanEvent> = block_on(
            start(comparison, vec![a, b], Arc::new(AtomicBool::new(false))).collect(),
        );
        assert!(matches!(events.first(), Some(ScanEvent::Base(_))));
        assert!(matches!(events.last(), Some(ScanEvent::Done { cancelled: false })));
        let folders: usize = events
            .iter()
            .map(|e| match e {
                ScanEvent::Items(items, _) => items.len(),
                _ => 0,
            })
            .sum();
        assert_eq!(folders, 2);
    }

    #[test]
    fn cancelled_scans_stop() {
        let dir = tempfile::tempdir().unwrap();
        let comparison = FolderComparison::new(FolderOptions::default(), Vec::new(), Vec::new());
        let events: Vec<ScanEvent> = block_on(
            start(
                comparison,
                vec![dir.path().to_path_buf(), dir.path().to_path_buf()],
                Arc::new(AtomicBool::new(true)),
            )
            .collect(),
        );
        assert!(matches!(events.last(), Some(ScanEvent::Done { cancelled: true })));
    }
}
