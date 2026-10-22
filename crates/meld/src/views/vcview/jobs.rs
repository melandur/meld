//! Work the version control view runs off the UI thread: choosing and
//! opening a backend, scanning the tree, running VC commands, preparing
//! comparisons and dialog contents, and deleting files.
//!
//! The scanning backend instance is shared behind a mutex so that jobs
//! queue up on it; the view keeps a second, idle instance of the same
//! backend for the cheap queries it needs while drawing (valid actions).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use iced::futures::{SinkExt, Stream};
use meld_vc::commit::{COMMIT_HISTORY_ID, DEFAULT_HISTORY_LENGTH};
use meld_vc::{
    Action, DiffOptions, DiffRequest, Entry, MessageHistory, StatusFilters, Vc, VcChoices, VcError, VcKind, VcRow,
};

pub type SharedVc = Arc<Mutex<Box<dyn Vc>>>;

/// Locks the scanning backend, recovering from a panicked job.
pub fn lock(vc: &SharedVc) -> MutexGuard<'_, Box<dyn Vc>> {
    vc.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Hands a value that is neither `Clone` nor shareable through a
/// message, to be taken once by the receiver.
pub struct Slot<T>(Arc<Mutex<Option<T>>>);

impl<T> Slot<T> {
    pub fn new(value: T) -> Self {
        Slot(Arc::new(Mutex::new(Some(value))))
    }

    pub fn take(&self) -> Option<T> {
        self.0.lock().ok()?.take()
    }
}

impl<T> Clone for Slot<T> {
    fn clone(&self) -> Self {
        Slot(self.0.clone())
    }
}

impl<T> std::fmt::Debug for Slot<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Slot(..)")
    }
}

/// Runs blocking work on tokio's blocking thread pool.
pub async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("background task failed: {e}"))
}

/// Runs `f` on a blocking thread, streaming the events it emits.
fn blocking_stream<E: Send + 'static>(
    f: impl FnOnce(&dyn Fn(E)) + Send + 'static,
) -> impl Stream<Item = E> {
    iced::stream::channel(64, async move |mut out: iced::futures::channel::mpsc::Sender<E>| {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let worker = tokio::task::spawn_blocking(move || {
            f(&|event| {
                let _ = tx.send(event);
            })
        });
        while let Some(event) = rx.recv().await {
            if out.send(event).await.is_err() {
                break;
            }
        }
        let _ = worker.await;
    })
}

// ----- Backends -----

/// A backend opened twice: one instance for jobs, one for the UI thread.
#[derive(Debug)]
pub struct OpenedVc {
    pub worker: Box<dyn Vc>,
    pub ui: Box<dyn Vc>,
}

pub fn open_pair(kind: VcKind, location: &Path) -> Result<OpenedVc, String> {
    let open = || kind.open(location).map_err(|e| e.to_string());
    Ok(OpenedVc {
        worker: open()?,
        ui: open()?,
    })
}

#[derive(Debug, Clone)]
pub struct Loaded {
    pub choices: VcChoices,
    pub vc: Result<Slot<OpenedVc>, String>,
}

/// Checks every backend for `location` and opens the default one
/// (`populate_vcs_for_location`).
pub async fn load(location: PathBuf, current: Option<VcKind>) -> Result<Loaded, String> {
    blocking(move || {
        let choices = meld_vc::choose_vcs(Some(&location), current);
        let vc = match choices.choices.get(choices.active) {
            Some(choice) => open_pair(choice.kind, &choices.location).map(Slot::new),
            None => Err("No version control system available".to_string()),
        };
        Loaded { choices, vc }
    })
    .await
}

pub async fn open(kind: VcKind, location: PathBuf) -> Result<Slot<OpenedVc>, String> {
    blocking(move || open_pair(kind, &location).map(Slot::new)).await?
}

// ----- Scanning -----

#[derive(Debug, Clone)]
pub struct ScanOutput {
    pub tree: VcRow,
    /// "2 unpushed commits in 1 branch", shown on the root row.
    pub push_summary: Option<String>,
    /// The folder a partial refresh scanned (`tree` is its row); `None`
    /// for the whole tree.
    pub subtree: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub enum ScanEvent {
    Progress(String),
    Done(Arc<Result<ScanOutput, String>>),
}

/// What a scan covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanScope {
    /// The whole tree, first refreshing all of the backend's cached state
    /// when `refresh`.
    Full { refresh: bool },
    /// One folder (`refresh_partial()`): its cached state is refreshed
    /// and only its rows are rebuilt.
    Folder(PathBuf),
}

const CANCELLED: &str = "cancelled";

/// Forgets the cached state of `path` and everything below it, so that
/// refreshing it also clears entries that went back to normal.
fn forget_below(vc: &mut dyn Vc, path: &Path) {
    let cache = &mut vc.core_mut().cache;
    cache.states.retain(|p, _| !p.starts_with(path));
    cache.meta.retain(|p, _| !p.starts_with(path));
    cache.missing.retain(|folder, _| !folder.starts_with(path));
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
        && let Some(names) = cache.missing.get_mut(parent)
    {
        names.remove(&*name.to_string_lossy());
    }
}

/// Builds the tree (or one folder of it, see [`ScanScope`]), reporting
/// progress at most every 100ms. Setting `cancel` stops the scan before
/// the next folder or entry.
pub fn scan(
    vc: SharedVc,
    filters: StatusFilters,
    scope: ScanScope,
    cancel: Arc<AtomicBool>,
) -> impl Stream<Item = ScanEvent> {
    blocking_stream(move |emit| {
        let result = (|| {
            let mut vc = lock(&vc);
            if cancel.load(Ordering::Relaxed) {
                return Err(CANCELLED.to_string());
            }
            let summary = match vc.get_commits_to_push_summary() {
                Ok(s) if !s.is_empty() => Some(s),
                Ok(_) | Err(VcError::Unsupported { .. }) => None,
                Err(e) => {
                    log::warn!("could not get commits to push: {e}");
                    None
                }
            };
            let (start, subtree) = match &scope {
                ScanScope::Full { refresh } => {
                    if *refresh {
                        emit(ScanEvent::Progress("Scanning repository".into()));
                        vc.refresh_vc_state(None).map_err(|e| e.to_string())?;
                    }
                    (vc.location().to_path_buf(), None)
                }
                ScanScope::Folder(folder) => {
                    emit(ScanEvent::Progress("Scanning repository".into()));
                    forget_below(&mut **vc, folder);
                    vc.refresh_vc_state(Some(folder)).map_err(|e| e.to_string())?;
                    (folder.clone(), Some(folder.clone()))
                }
            };
            if cancel.load(Ordering::Relaxed) {
                return Err(CANCELLED.to_string());
            }
            let mut last = Instant::now() - Duration::from_secs(1);
            let mut tree = meld_vc::scan_tree_cancellable(
                &**vc,
                &start,
                &filters,
                &mut |message| {
                    if last.elapsed() >= Duration::from_millis(100) {
                        last = Instant::now();
                        emit(ScanEvent::Progress(message.to_string()));
                    }
                },
                &cancel,
            )
            .map_err(|e| e.to_string())?;
            // A refreshed folder below the location gets its own state.
            if subtree.is_some()
                && start != vc.location()
                && let VcRow::Entry { entry, .. } = &mut tree
            {
                let fresh = vc.get_entry(&start);
                entry.state = fresh.state;
                entry.options = fresh.options;
            }
            Ok(ScanOutput {
                tree,
                push_summary: summary,
                subtree,
            })
        })();
        if !cancel.load(Ordering::Relaxed) {
            emit(ScanEvent::Done(Arc::new(result)));
        }
    })
}

/// Refreshes the state of one entry (`on_file_changed()`).
pub async fn refresh_entry(vc: SharedVc, path: PathBuf) -> Result<Entry, String> {
    blocking(move || {
        let mut vc = lock(&vc);
        forget_below(&mut **vc, &path);
        vc.refresh_vc_state(Some(&path)).map_err(|e| e.to_string())?;
        Ok(vc.get_entry(&path))
    })
    .await?
}

// ----- Commands -----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleKind {
    Command,
    Output,
    Error,
}

#[derive(Debug, Clone)]
pub enum CommandEvent {
    Console(ConsoleKind, String),
    Finished {
        /// The working folders of commands that asked for a refresh.
        refresh: Vec<PathBuf>,
        failed: bool,
        error: Option<String>,
    },
}

/// Builds the commands for `action` and runs them one after another,
/// echoing each command line and its output to the console
/// (`_command_iter`).
pub fn run_action(vc: SharedVc, action: Action, files: Vec<PathBuf>, message: String) -> impl Stream<Item = CommandEvent> {
    blocking_stream(move |emit| {
        let commands = lock(&vc).action_commands(action, &files, &message);
        let commands = match commands {
            Ok(c) => c,
            Err(e) => {
                emit(CommandEvent::Finished {
                    refresh: Vec::new(),
                    failed: true,
                    error: Some(e.to_string()),
                });
                return;
            }
        };
        let mut refresh: Vec<PathBuf> = Vec::new();
        let mut failed = false;
        let mut error = None;
        for command in &commands {
            let line = command.console_line();
            emit(CommandEvent::Console(ConsoleKind::Command, line.clone()));
            match command.run() {
                Ok(output) => {
                    if !output.stderr.is_empty() {
                        emit(CommandEvent::Console(ConsoleKind::Error, output.stderr.clone()));
                    }
                    emit(CommandEvent::Console(ConsoleKind::Output, format!("{}\n", output.stdout)));
                    failed |= !output.success();
                }
                Err(e) => {
                    failed = true;
                    error = Some(format!("While running '{}'\nError: {e}", line.trim_end()));
                }
            }
            if command.refresh && !refresh.contains(&command.working_dir) {
                refresh.push(command.working_dir.clone());
            }
        }
        emit(CommandEvent::Finished { refresh, failed, error });
    })
}

// ----- Comparisons and dialogs -----

/// Prepares comparisons for `paths` (`run_diff`): base revisions and
/// conflict sides are written to temporary files.
pub async fn prepare_diffs(vc: SharedVc, paths: Vec<PathBuf>, options: DiffOptions) -> Vec<Result<DiffRequest, String>> {
    blocking(move || {
        let vc = lock(&vc);
        paths
            .iter()
            .map(|p| meld_vc::prepare_diff(&**vc, p, &options).map_err(|e| format!("{}: {e}", p.display())))
            .collect()
    })
    .await
    .unwrap_or_else(|e| vec![Err(e)])
}

#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub files: Result<Vec<String>, String>,
    pub prefill: Option<String>,
    pub history: MessageHistory,
}

/// What the commit dialog shows: the files to be committed, a message
/// prefill (e.g. during merges) and the previous messages.
pub async fn commit_info(vc: SharedVc, selected: Vec<PathBuf>) -> CommitInfo {
    blocking(move || {
        let vc = lock(&vc);
        let files = meld_vc::files_to_commit_display(&**vc, &selected).map_err(|e| e.to_string());
        let prefill = vc.get_commit_message_prefill().filter(|m| !m.is_empty());
        drop(vc);
        let history = MessageHistory::default_path()
            .and_then(|p| match MessageHistory::load(&p, COMMIT_HISTORY_ID, DEFAULT_HISTORY_LENGTH) {
                Ok(h) => Some(h),
                Err(e) => {
                    log::warn!("could not read commit message history: {e}");
                    None
                }
            })
            .unwrap_or_default();
        CommitInfo { files, prefill, history }
    })
    .await
    .unwrap_or_else(|e| CommitInfo {
        files: Err(e),
        prefill: None,
        history: MessageHistory::default(),
    })
}

pub async fn save_history(history: MessageHistory) {
    let _ = blocking(move || {
        if let Some(path) = MessageHistory::default_path()
            && let Err(e) = history.save(&path, COMMIT_HISTORY_ID)
        {
            log::warn!("could not save commit message history: {e}");
        }
    })
    .await;
}

/// The push dialog's summary of commits to push; empty if unknown.
pub async fn push_summary(vc: SharedVc) -> Result<String, String> {
    blocking(move || match lock(&vc).get_commits_to_push_summary() {
        Ok(s) => Ok(s),
        Err(VcError::Unsupported { .. }) => Ok(String::new()),
        Err(e) => Err(e.to_string()),
    })
    .await?
}

// ----- Deleting -----

/// Moves `paths` to the trash (`trash_or_confirm`), returning the files
/// trashed, the files that can only be deleted permanently, and error
/// messages.
pub async fn trash(paths: Vec<PathBuf>) -> (Vec<PathBuf>, Vec<PathBuf>, Vec<String>) {
    blocking(move || {
        let mut trashed = Vec::new();
        let mut confirm = Vec::new();
        let mut errors = Vec::new();
        for path in paths {
            match meld_files::trash_or_confirm(&path) {
                Ok(_) => trashed.push(path),
                // Like Meld, folders that can't be trashed are not
                // deleted recursively.
                Err(meld_files::TrashError::NeedsConfirmation { is_dir: true, .. }) => errors.push(format!(
                    "Error deleting {}: Deleting remote folders is not supported",
                    path.display()
                )),
                Err(meld_files::TrashError::NeedsConfirmation { .. }) => confirm.push(path),
                Err(e) => errors.push(format!("Error deleting {}: {e}", path.display())),
            }
        }
        (trashed, confirm, errors)
    })
    .await
    .unwrap_or_else(|e| (Vec::new(), Vec::new(), vec![e]))
}

/// Deletes files that could not be trashed. Returns the files deleted
/// and error messages.
pub async fn delete_permanently(paths: Vec<PathBuf>) -> (Vec<PathBuf>, Vec<String>) {
    blocking(move || {
        let mut deleted = Vec::new();
        let mut errors = Vec::new();
        for path in paths {
            match meld_files::delete_permanently(&path) {
                Ok(()) => deleted.push(path),
                Err(e) => errors.push(format!("Error deleting {}: {e}", path.display())),
            }
        }
        (deleted, errors)
    })
    .await
    .unwrap_or_else(|e| (Vec::new(), vec![e]))
}
