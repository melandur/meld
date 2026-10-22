//! The crate's error type (replaces `AiLensError` and its subclasses)

/// Why a Claude Code call failed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClaudeErrorReason {
    /// The CLI isn't installed, or not at the configured path
    NotFound,
    Failed,
    Timeout,
    InvalidOutput,
}

impl ClaudeErrorReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not-found",
            Self::Failed => "failed",
            Self::Timeout => "timeout",
            Self::InvalidOutput => "invalid-output",
        }
    }
}

/// How a child process failed to run
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcessErrorKind {
    /// The executable doesn't exist or can't be run
    NotFound,
    Timeout,
    Other,
}

/// Every error has a message fit to show the user (its `Display`)
#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    /// A plain error for the user (Python's `AiLensError`)
    #[error("{0}")]
    Message(String),
    /// The operation was cancelled
    #[error("Cancelled")]
    Cancelled,
    /// git failed, or could not be run
    #[error("{message}")]
    Git {
        message: String,
        command: Vec<String>,
        returncode: Option<i32>,
        stderr: String,
        /// The git executable doesn't exist or can't be run
        not_found: bool,
    },
    /// git printed something the parser doesn't understand
    #[error("{0}")]
    Parse(String),
    /// A child process couldn't be run or timed out
    #[error("{message}")]
    Process {
        message: String,
        command: Vec<String>,
        kind: ProcessErrorKind,
    },
    /// Claude Code failed
    #[error("{message}")]
    Claude {
        message: String,
        reason: ClaudeErrorReason,
    },
    /// The file is generated or in `.ailensignore`, so it isn't sent
    #[error("{0}")]
    IgnoredFile(String),
    /// There are no hunks to explain
    #[error("{0}")]
    NothingToExplain(String),
}

impl Error {
    pub fn msg(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }

    pub fn claude(message: impl Into<String>, reason: ClaudeErrorReason) -> Self {
        Self::Claude {
            message: message.into(),
            reason,
        }
    }

    /// Python's `ClaudeNotFound`
    pub fn claude_not_found(message: impl Into<String>) -> Self {
        Self::claude(message, ClaudeErrorReason::NotFound)
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }

    pub fn is_claude_not_found(&self) -> bool {
        matches!(
            self,
            Self::Claude {
                reason: ClaudeErrorReason::NotFound,
                ..
            }
        )
    }

    pub fn claude_reason(&self) -> Option<ClaudeErrorReason> {
        match self {
            Self::Claude { reason, .. } => Some(*reason),
            _ => None,
        }
    }

    pub fn is_git_not_found(&self) -> bool {
        matches!(
            self,
            Self::Git {
                not_found: true,
                ..
            }
        )
    }

    /// git's stderr, for a git error
    pub fn git_stderr(&self) -> Option<&str> {
        match self {
            Self::Git { stderr, .. } => Some(stderr),
            _ => None,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
