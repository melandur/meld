//! Error type shared by all version control backends.

use std::io;
use std::path::PathBuf;

/// Errors raised by version control backends.
#[derive(Debug, thiserror::Error)]
pub enum VcError {
    /// A path was passed that is outside the repository or not present
    /// (Python's `InvalidVCPath`).
    #[error("{vc}: Path {} is invalid or not present\nError: {reason}", path.display())]
    InvalidPath {
        vc: &'static str,
        path: PathBuf,
        reason: String,
    },

    /// A revision spec the backend can't handle (Python's `InvalidVCRevision`).
    #[error("{vc}: Doesn't understand or have revision {revision}\nError: {reason}")]
    InvalidRevision {
        vc: &'static str,
        revision: String,
        reason: String,
    },

    /// The location is not inside a repository of the requested kind
    /// (Python raises a bare `ValueError` from `Vc.__init__`).
    #[error("{} is not inside a {vc} repository", path.display())]
    NotInRepo { vc: &'static str, path: PathBuf },

    /// The backend does not implement this operation (Python's
    /// `NotImplementedError` or a missing method).
    #[error("{vc} does not support {operation}")]
    Unsupported {
        vc: &'static str,
        operation: &'static str,
    },

    /// The version control program could not be started.
    #[error("failed to run {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: io::Error,
    },

    /// A file can't be marked resolved because it still contains conflict
    /// markers (backends without a "resolved" flag, like CVS and Darcs,
    /// treat a file without markers as resolved).
    #[error("{vc}: {} still contains conflict markers; edit them out before marking it resolved", path.display())]
    ConflictMarkers { vc: &'static str, path: PathBuf },

    /// The version control program's output could not be understood.
    #[error("{vc}: could not parse output: {reason}")]
    Parse { vc: &'static str, reason: String },

    /// A cancellable operation (e.g. [`crate::scan_tree_cancellable`])
    /// was cancelled.
    #[error("cancelled")]
    Cancelled,

    /// Any other I/O error (temporary files, directory scans, ...).
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Result alias for this crate.
pub type Result<T, E = VcError> = std::result::Result<T, E>;
