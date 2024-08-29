#![deny(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::unwrap_used)]
//! A library for synchronizing files between two directories.

use std::path::PathBuf;

/// File synchronization module.
pub mod sync;

#[derive(Debug, thiserror::Error)]
/// Errors that can occur during synchronization.
pub enum SyncError {
    #[error("Failed to stat {0}")]
    /// Failed to stat a file.
    StatFailed(PathBuf, #[source] std::io::Error),
    #[error("Operation cancelled")]
    /// Operation was cancelled.
    Cancelled,
    #[error("Failed to copy {src} to {dest}")]
    /// Failed to copy a file.
    #[allow(missing_docs)]
    CopyFailed {
        src: PathBuf,
        dest: PathBuf,
        #[source]
        err: tokio::io::Error,
    },
    #[error("An unknown error occurred in a task, this is likely a bug: {0}")]
    /// A panic likely occurred in a task.
    JoinError(#[from] tokio::task::JoinError),
}
