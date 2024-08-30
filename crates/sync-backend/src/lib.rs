#![deny(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::unwrap_used)]
//! A library for synchronizing files between two directories.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// File synchronization module.
pub mod sync;

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Configuration for the synchronization.
pub struct Config {
    /// Pairs of directories to synchronize.
    pub pairs: Vec<SyncPairs>,
}

impl Config {
    /// Validate the configuration.
    pub fn validate(&self) -> Result<(), String> {
        for (i, pair) in self.pairs.iter().enumerate() {
            pair.validate().map_err(|e| format!("Pair {}: {}", i, e))?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// A pair of directories to synchronize.
pub struct SyncPairs {
    /// Source directory.
    pub src: SyncPairSource,
    /// Destination directory.
    pub dest: SyncPairDest,
    /// Number of concurrent file operations.
    pub concurrency: usize,
}

impl SyncPairs {
    /// Validate the configuration.
    pub fn validate(&self) -> Result<(), String> {
        if self.concurrency == 0 {
            return Err("Concurrency must be greater than 0".to_string());
        }

        self.src
            .r#match
            .validate()
            .map_err(|e| format!("Source: {}", e))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Source directory to synchronize.
pub struct SyncPairSource {
    /// Device match configuration.
    pub r#match: DeviceMatchConfig,
    /// Path to synchronize.
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Device match configuration.
pub struct DeviceMatchConfig {
    /// Volume name.
    pub volume: Option<String>,
    /// Device name.
    pub device: Option<String>,
}

impl DeviceMatchConfig {
    /// Check if the volume and/or device names match.
    pub fn matches(&self, volume_name: &str, device_name: &str) -> bool {
        if let Some(ref volume) = self.volume {
            if volume != volume_name {
                return false;
            }
        }
        if let Some(ref device) = self.device {
            if device != device_name {
                return false;
            }
        }
        true
    }
    /// Validate the configuration.
    pub fn validate(&self) -> Result<(), String> {
        if self.volume.is_none() && self.device.is_none() {
            return Err("At least one of volume or device must be specified".to_string());
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Destination directory to synchronize.
pub struct SyncPairDest {
    /// Path to synchronize (absolute).
    pub path: PathBuf,
}

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
    #[error("Short copy from {src} to {dest}, copied {copied} bytes, expected {expected}")]
    /// A copy operation was short, maybe a file was modified during the copy or a file system error
    #[allow(missing_docs)]
    ShortCopy {
        src: PathBuf,
        dest: PathBuf,
        copied: u64,
        expected: u64,
    },
    #[error("An unknown error occurred in a task, this is likely a bug: {0}")]
    /// A panic likely occurred in a task.
    JoinError(#[from] tokio::task::JoinError),
}
