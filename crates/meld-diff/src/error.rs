//! Error type for the crate.

/// Errors returned by `meld-diff`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DiffError {
    /// A cancellable comparison was cancelled by its callback.
    #[error("the comparison was cancelled")]
    Cancelled,
    /// A differ compares two or three sequences (at most three).
    #[error("a comparison holds at most three sequences, got {0}")]
    TooManySequences(usize),
    /// A pane index outside the comparison was used.
    #[error("pane {0} is not part of this comparison")]
    InvalidPane(usize),
    /// A text filter matched across a line break, which would change the
    /// number of lines and break the line mapping.
    #[error("a text filter changed the number of lines, which is unsupported")]
    FilterChangedLineCount,
}
