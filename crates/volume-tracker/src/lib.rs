#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![warn(clippy::unwrap_used, clippy::expect_used)]
#![allow(
    clippy::missing_errors_doc,
    clippy::unreadable_literal,
    clippy::items_after_statements
)]

//! Operating system specific file system notification sources.

use std::{
    fmt::{Debug, Display},
    hash::Hash,
    marker::PhantomData,
    path::PathBuf,
};

use dashmap::DashMap;
use tokio::task::AbortHandle;

#[cfg(windows)]
/// Windows specific file system notification sources.
pub mod windows;

pub(crate) mod mem;

/// A file system identifier.
pub trait FileSystem: Debug + Display {
    /// Get the file system name.
    fn name(&self) -> &str;
}

#[derive(Debug)]
/// A dummy file system identifier.
pub struct UnimplementedFileSystem;

impl FileSystem for UnimplementedFileSystem {
    fn name(&self) -> &str {
        "unknown"
    }
}

impl Display for UnimplementedFileSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown")
    }
}

/// A device identifier.
pub trait Device: Debug {
    /// Get the device name.
    fn name(&self) -> &str;
}

impl Device for () {
    fn name(&self) -> &str {
        "unknown"
    }
}

#[derive(Debug)]
/// A dummy device identifier.
pub struct UnimplementedDevice;

impl Device for UnimplementedDevice {
    fn name(&self) -> &str {
        "unknown"
    }
}

/// A holder for [`AbortHandle`]s, used to cancel tasks whose file systems have been removed.
pub struct AbortHandleHolder<K: Hash + Eq + Display>(
    DashMap<K, (AbortHandle, Option<Box<dyn FnOnce() + Send + Sync>>)>,
);

impl<K: Hash + Eq + Display> Default for AbortHandleHolder<K> {
    fn default() -> Self {
        Self(DashMap::new())
    }
}

#[allow(dead_code)]
impl<K: Hash + Eq + Display> AbortHandleHolder<K> {
    pub(crate) fn insert(
        &self,
        key: K,
        handle: AbortHandle,
        on_remove: Option<Box<dyn FnOnce() + Send + Sync>>,
    ) {
        self.0.insert(key, (handle, on_remove));
    }

    pub(crate) fn gc(&self) {
        self.0.retain(|_, v| !v.0.is_finished());
    }

    pub(crate) fn remove_abort(&self, key: &K) -> Option<K> {
        if let Some((k, (abort, cleanup))) = self.0.remove(key) {
            abort.abort();
            if let Some(cleanup) = cleanup {
                cleanup();
            }
            Some(k)
        } else {
            None
        }
    }

    /// Clear all [`AbortHandle`]s and abort the associated tasks.
    pub fn clear_abort(&self) {
        self.0.iter_mut().for_each(|mut rec| {
            let (key, (abort, cleanup)) = rec.pair_mut();
            if !abort.is_finished() {
                log::info!("Aborting task for volume: {}", key);
                abort.abort();
                if let Some(cleanup) = cleanup.take() {
                    cleanup();
                }
            }
        });

        self.0.clear();
    }
}

impl<K: Hash + Eq + Display> Drop for AbortHandleHolder<K> {
    fn drop(&mut self) {
        self.clear_abort();
    }
}

/// The disposition of a spawner callback.
pub enum SpawnerDisposition {
    /// A task has been spawned to handle the file system.
    Spawned(AbortHandle, Option<Box<dyn FnOnce() + Send + Sync>>),
    /// The file system should be ignored.
    Ignore,
    /// The file system should be skipped but next time a file system change is detected, the callback should be called again.
    Skip,
}

/// A source of notifications for file system changes.
///
/// `F` is a callback that takes a file system and a device,
/// optionally spawning a task to handle the file system.
/// The returned [`tokio::task::AbortHandle`] will be registered
/// and can be used to abort the task when the file system is removed.
pub trait NotificationSource<'a, F>: Sized
where
    F: Fn(Self::FileSystem, Self::Device, Option<PathBuf>) -> SpawnerDisposition + Send + Sync + 'a,
{
    /// The file system type, usually a volume identifier.
    type FileSystem: FileSystem;
    /// The device identifier.
    type Device: Device;
    /// The error type.
    type Error: std::error::Error;

    /// Create a new notification source with the given callback.
    fn new(callback: F) -> Result<Self, Self::Error>;
    /// List all currently present file systems.
    #[allow(clippy::type_complexity)]
    fn list(&self) -> Result<Vec<(Self::FileSystem, Self::Device, Option<PathBuf>)>, Self::Error>;
    /// List all currently present file systems and spawn tasks for each.
    fn list_spawn(&self) -> Result<(), Self::Error>;

    /// Start the notification source and begin spawning tasks for new file systems.
    fn start(&mut self) -> Result<(), Self::Error>;
    /// Stop the notification source but do not abort spawned tasks.
    fn pause(&mut self) -> Result<(), Self::Error>;
    /// Stop the notification source and abort spawned tasks.
    fn reset(&mut self) -> Result<(), Self::Error>;
}

#[derive(Clone)]
/// An error indicating that the platform is not supported.
pub struct NotImplementedError;

impl std::fmt::Display for NotImplementedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Not implemented")
    }
}

impl std::fmt::Debug for NotImplementedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NotImplementedError")
    }
}

impl std::error::Error for NotImplementedError {}

/// A dummy [`NotificationSource`] that does nothing on unimplemented platforms.
pub struct UnimplementedNotifier<'a, F>(PhantomData<&'a F>);

impl<'a, F> NotificationSource<'a, F> for UnimplementedNotifier<'a, F>
where
    F: Fn(UnimplementedFileSystem, UnimplementedDevice, Option<PathBuf>) -> SpawnerDisposition
        + Send
        + Sync
        + 'a,
{
    type FileSystem = UnimplementedFileSystem;
    type Device = UnimplementedDevice;
    type Error = NotImplementedError;

    fn new(_: F) -> Result<Self, Self::Error> {
        Ok(Self(PhantomData))
    }

    fn list(&self) -> Result<Vec<(Self::FileSystem, Self::Device, Option<PathBuf>)>, Self::Error> {
        log::warn!("Platform not supported, no notifications will be received");
        Ok(vec![])
    }

    fn list_spawn(&self) -> Result<(), Self::Error> {
        log::warn!("Platform not supported, no notifications will be received");
        Ok(())
    }

    fn start(&mut self) -> Result<(), Self::Error> {
        log::warn!("Platform not supported, no notifications will be received");
        Ok(())
    }

    fn pause(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn reset(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[cfg(windows)]
/// A platform specific [`NotificationSource`].
pub type PlatformNotifier<'a, F> = windows::HcmNotifier<'a, F>;

#[cfg(not(windows))]
/// A platform specific [`NotificationSource`].
pub type PlatformNotifier<'a, F> = UnimplementedNotifier<'a, F>;

/// Initialize the platform specific components.
pub fn platform_init() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(windows)]
    {
        windows::wmi::init_com()?;
    }

    Ok(())
}
