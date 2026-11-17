use std::path::PathBuf;
use std::process::ExitStatus;

use crate::RepoId;

/// `Result` with [`Error`] as the default error type.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors returned by repostore operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The url is not a recognized git remote (or would escape the cache).
    #[error("unsupported repository url: {0}")]
    InvalidUrl(String),

    /// The string is neither a url nor a `host/path` id.
    #[error("invalid repository id: {0}")]
    InvalidId(String),

    /// The repository is not present in the cache.
    #[error("repository not in cache: {0}")]
    NotCached(RepoId),

    /// Clone destination is a non-empty directory or a file.
    #[error("destination already exists and is not empty: {}", .0.display())]
    DestinationExists(PathBuf),

    /// The repository has no such remote.
    #[error("no remote `{remote}` configured in {}", .path.display())]
    NoRemote {
        /// Repository (working tree or bare) that was inspected.
        path: PathBuf,
        /// Name of the missing remote.
        remote: String,
    },

    /// No `REPOSTORE_DIR` and no platform cache directory.
    #[error("could not determine a cache directory; set REPOSTORE_DIR")]
    NoCacheDir,

    /// The `git` executable could not be started.
    #[error("`git` executable not found in PATH")]
    GitNotFound,

    /// A `git` command exited unsuccessfully.
    #[error("`git {args}` failed ({status}){}", fmt_stderr(.stderr))]
    Git {
        /// Arguments passed to git, space separated.
        args: String,
        /// Exit status of the process.
        status: ExitStatus,
        /// Captured standard error, trimmed.
        stderr: String,
    },

    /// Filesystem error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

fn fmt_stderr(stderr: &str) -> String {
    if stderr.is_empty() {
        String::new()
    } else {
        format!(": {stderr}")
    }
}
