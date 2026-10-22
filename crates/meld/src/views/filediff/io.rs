//! Background work: loading and saving files, comparing, and the version
//! control calls of file comparisons. Everything here blocks and runs
//! on tokio's blocking pool.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use meld_diff::{DiffError, FileComparison, TextBuffer};
use meld_files::{LoadError, LoadOptions, LoadedText, SaveError, SaveOptions};

/// Runs blocking work off the UI thread.
pub async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    match tokio::task::spawn_blocking(f).await {
        Ok(v) => v,
        Err(err) => std::panic::resume_unwind(err.into_panic()),
    }
}

/// A loaded file.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub text: LoadedText,
    pub mtime: Option<SystemTime>,
    pub writable: bool,
}

#[derive(Debug, Clone)]
pub enum LoadFailure {
    LineTooLong(String),
    Other(String),
}

/// Whether `path` can be written (`MeldBufferData.writable`): an
/// existing file must be writable, a missing one counts as writable.
pub fn is_writable(path: &Path) -> bool {
    match std::fs::OpenOptions::new().write(true).open(path) {
        Ok(_) => true,
        Err(err) => err.kind() == std::io::ErrorKind::NotFound,
    }
}

pub fn load(path: PathBuf, target: Option<PathBuf>, options: LoadOptions) -> Result<Loaded, LoadFailure> {
    let text = meld_files::load_text_file(&path, &options).map_err(|err| match err {
        LoadError::LineTooLong { line, length } => LoadFailure::LineTooLong(tr!(
            "Line {line_number} exceeded maximum line length ({line_length} > {LINE_LENGTH_LIMIT}).\n\nLoading files with very long lines may make Meld slow or unresponsive.",
            line_number = line + 1,
            line_length = length,
            LINE_LENGTH_LIMIT = meld_files::text::LINE_LENGTH_LIMIT,
        )),
        LoadError::Io { source, .. } => LoadFailure::Other(source.to_string()),
    })?;
    let mtime = meld_files::file_mtime(&path);
    let writable = is_writable(target.as_deref().unwrap_or(&path));
    Ok(Loaded { text, mtime, writable })
}

#[derive(Debug, Clone)]
pub struct Saved {
    pub mtime: Option<SystemTime>,
}

#[derive(Debug, Clone)]
pub enum SaveFailure {
    /// Changed on disk since it was loaded.
    ExternallyModified(PathBuf),
    Error {
        path: PathBuf,
        message: String,
    },
}

pub fn save(path: PathBuf, text: String, options: SaveOptions) -> Result<Saved, SaveFailure> {
    match meld_files::save_text_file(&path, &text, &options) {
        Ok(mtime) => Ok(Saved { mtime }),
        Err(SaveError::ExternallyModified { path }) => Err(SaveFailure::ExternallyModified(path)),
        Err(SaveError::Encode(err)) => Err(SaveFailure::Error {
            message: tr!(
                "File “{}” contains characters that can’t be encoded using its current encoding “{}”.",
                path.display(),
                err.encoding
            ),
            path,
        }),
        Err(err) => Err(SaveFailure::Error {
            path,
            message: err.to_string(),
        }),
    }
}

/// What a comparison job does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    /// After loading: merge (in auto-merge mode), compare, place the
    /// cursor at the first change and pick languages.
    Initial,
    /// Re-run the comparison of the current text (`refresh_comparison`).
    Refresh,
}

/// Input of a comparison job.
pub struct DiffJob {
    pub kind: DiffKind,
    pub buffers: Vec<TextBuffer>,
    pub filters: Vec<regex::Regex>,
    pub ignore_blanks: bool,
    pub auto_merge: bool,
    pub syncpoints: Vec<Vec<usize>>,
    pub unresolved: Vec<usize>,
    /// For language detection: file paths per pane.
    pub paths: Vec<Option<PathBuf>>,
}

#[derive(Debug, Clone)]
pub struct DiffDone {
    pub kind: DiffKind,
    pub comparison: Box<FileComparison>,
    /// Pane languages (GtkSourceView ids), for initial comparisons.
    pub languages: Option<Vec<Option<&'static str>>>,
    /// A pane showing the same file as an earlier pane.
    pub self_compare: Option<usize>,
}

pub fn run_diff(job: DiffJob, cancel: Arc<AtomicBool>) -> Result<DiffDone, DiffError> {
    let cancelled = || cancel.load(Ordering::Relaxed);
    let merge = job.auto_merge && job.kind == DiffKind::Initial && job.buffers.len() == 3;
    let (mut cmp, _) = if merge {
        FileComparison::new_auto_merge(job.buffers, job.filters, job.ignore_blanks, &cancelled)?
    } else {
        FileComparison::new_cancellable(job.buffers, job.filters, job.ignore_blanks, &cancelled)?
    };
    if !merge && !job.unresolved.is_empty() {
        cmp.differ_mut().set_unresolved(job.unresolved);
    }
    if !job.syncpoints.is_empty() {
        cmp.differ_mut().set_syncpoints(job.syncpoints);
        cmp.rediff_cancellable(&cancelled)?;
    }
    let languages = (job.kind == DiffKind::Initial).then(|| {
        let file_langs: Vec<Option<&'static str>> = job
            .paths
            .iter()
            .map(|p| p.as_deref().and_then(meld_files::language_from_path).map(|l| l.id))
            .collect();
        let texts: Vec<String> = cmp.buffers().iter().map(TextBuffer::text).collect();
        meld_files::choose_pane_languages(&file_langs, |pane| {
            let sample = meld_files::get_language_sample_text(&texts[pane]);
            meld_files::guess_language_from_text(&sample).map(|l| l.id)
        })
    });
    let mut seen = Vec::new();
    let mut self_compare = None;
    for (pane, path) in job.paths.iter().enumerate() {
        let Some(path) = path else {
            continue;
        };
        let id = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        if seen.contains(&id) {
            self_compare = Some(pane);
            break;
        }
        seen.push(id);
    }
    Ok(DiffDone {
        kind: job.kind,
        comparison: Box::new(cmp),
        languages,
        self_compare,
    })
}

/// Opens the version control comparison of a single file (`run_diff` of
/// the VC view for one file).
pub fn prepare_vc_file(path: PathBuf, left_is_local: bool, merge_order_key: &str) -> Result<super::Options, String> {
    use meld_vc::{DiffOptions, DiffRequest, MergeFileOrder};
    let location = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let candidate = meld_vc::get_vcs(&location)
        .into_iter()
        .find(|c| c.enabled())
        .ok_or_else(|| tr!("{} is not in a version controlled folder", path.display()))?;
    let mut vc = meld_vc::open_vc(candidate.kind, &location).map_err(|e| e.to_string())?;
    let _ = vc.refresh_vc_state(Some(&path));
    let options = DiffOptions {
        left_is_local,
        merge_file_order: MergeFileOrder::from_key(merge_order_key),
    };
    match meld_vc::prepare_diff(vc.as_ref(), &path, &options).map_err(|e| e.to_string())? {
        DiffRequest::Folder(_) => Err(tr!("Cannot compare a folder as a file").into()),
        DiffRequest::Files {
            paths,
            labels,
            tab_label,
            merge_output,
            auto_merge,
            prompt_resolve,
            diff_sides,
            temp_files,
        } => {
            let to_side = |s: meld_vc::DiffSide| match s {
                meld_vc::DiffSide::New => meld_config::DiffSide::New,
                meld_vc::DiffSide::Old => meld_config::DiffSide::Old,
            };
            Ok(super::Options {
                paths: paths.into_iter().map(Some).collect(),
                labels: labels.into_iter().map(Option::unwrap_or_default).collect(),
                merge_output,
                auto_merge,
                extras: crate::comparison::FileExtras {
                    tab_label: Some(tab_label),
                    prompt_resolve,
                    diff_sides: diff_sides.map(|[a, b]| [to_side(a), to_side(b)]),
                    temp_files,
                },
            })
        }
    }
}

/// Marks `path` resolved with its version control system.
pub fn resolve(path: PathBuf) -> Result<(), String> {
    let location = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let candidate = meld_vc::get_vcs(&location)
        .into_iter()
        .find(|c| c.enabled())
        .ok_or_else(|| tr!("Not in a version controlled folder").to_string())?;
    let vc = meld_vc::open_vc(candidate.kind, &location).map_err(|e| e.to_string())?;
    let commands = vc
        .action_commands(meld_vc::Action::Resolve, &[path], "")
        .map_err(|e| e.to_string())?;
    for command in commands {
        let output = command.run().map_err(|e| e.to_string())?;
        if !output.success() {
            return Err(format!("{}{}", output.stdout, output.stderr));
        }
    }
    Ok(())
}

/// Current modification times of `paths`.
pub fn mtimes(paths: Vec<Option<PathBuf>>) -> Vec<Option<SystemTime>> {
    paths
        .into_iter()
        .map(|p| p.and_then(|p| meld_files::file_mtime(&p)))
        .collect()
}
