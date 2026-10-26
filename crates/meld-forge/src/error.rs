//! Errors of the forge client; every message is fit to show the user

/// What went wrong talking to the forge or fetching a pull request
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// `gh` or `glab` isn't installed (or can't be run)
    #[error("“{program}” isn’t installed. Install the {cli} to see {nouns} here.")]
    ToolMissing {
        program: String,
        /// e.g. "GitHub CLI"
        cli: String,
        /// e.g. "pull requests"
        nouns: String,
    },
    /// The tool ran and failed, e.g. not logged in or the request was refused
    #[error("{program} failed: {message}")]
    Command {
        program: String,
        message: String,
        /// The tool says it isn't logged in
        auth: bool,
    },
    /// The tool answered with something that isn't the expected JSON
    #[error("Unexpected answer from {program}: {message}")]
    Parse { program: String, message: String },
    /// The forge can't do this (e.g. request changes on GitLab)
    #[error("{0}")]
    Unsupported(String),
    /// What the user asked to post isn't complete
    #[error("{0}")]
    Invalid(String),
    /// git failed fetching or comparing the pull request
    #[error("{0}")]
    Git(String),
    /// The operation was cancelled
    #[error("Cancelled")]
    Cancelled,
}

impl Error {
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Error::Cancelled)
    }
}

impl From<meld_ailens::Error> for Error {
    fn from(err: meld_ailens::Error) -> Self {
        if err.is_cancelled() {
            Error::Cancelled
        } else {
            Error::Git(err.to_string())
        }
    }
}

impl From<Error> for meld_ailens::Error {
    fn from(err: Error) -> Self {
        match err {
            Error::Cancelled => meld_ailens::Error::Cancelled,
            other => meld_ailens::Error::msg(other.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
