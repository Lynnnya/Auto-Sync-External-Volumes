#![warn(missing_docs)]
#![warn(clippy::unwrap_used)]
#![warn(clippy::expect_used)]
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

#[derive(Debug)]
/// A holder for [`AbortHandle`]s, used to cancel tasks whose file systems have been removed.
pub struct AbortHandleHolder<K: Hash + Eq + Display>(DashMap<K, AbortHandle>);

impl<K: Hash + Eq + Display> Default for AbortHandleHolder<K> {
    fn default() -> Self {
        Self(DashMap::new())
    }
}

#[allow(dead_code)]
impl<K: Hash + Eq + Display> AbortHandleHolder<K> {
    pub(crate) fn insert(&self, key: K, handle: AbortHandle) {
        self.0.insert(key, handle);
    }

    pub(crate) fn gc(&self) {
        self.0.retain(|_, v| !v.is_finished());
    }

    pub(crate) fn remove_abort(&self, key: &K) -> Option<K> {
        if let Some((k, handle)) = self.0.remove(key) {
            handle.abort();
            Some(k)
        } else {
            None
        }
    }

    /// Clear all [`AbortHandle`]s and abort the associated tasks.
    pub fn clear_abort(&self) {
        self.0.iter().for_each(|rec| {
            if !rec.value().is_finished() {
                log::info!("Aborting task for volume: {}", rec.key());
            }
            rec.value().abort();
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
    Spawned(AbortHandle),
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
pub trait NotificationSource<F>: Sized
where
    F: Fn(Self::FileSystem, Self::Device, Option<PathBuf>) -> SpawnerDisposition
        + Send
        + Clone
        + 'static,
{
    /// The file system type, usually a volume identifier.
    type FileSystem: FileSystem;
    /// The device identifier.
    type Device: Device;
    /// The error type.
    type Error;

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

/// A dummy [`NotificationSource`] that does nothing on unimplemented platforms.
pub struct UnimplementedNotifier<F>(PhantomData<F>);

impl<F> NotificationSource<F> for UnimplementedNotifier<F>
where
    F: Fn(UnimplementedFileSystem, UnimplementedDevice, Option<PathBuf>) -> SpawnerDisposition
        + Send
        + Clone
        + 'static,
{
    type FileSystem = UnimplementedFileSystem;
    type Device = UnimplementedDevice;
    type Error = &'static str;

    fn new(_: F) -> Result<Self, Self::Error> {
        Err("Unimplemented")
    }

    fn list(&self) -> Result<Vec<(Self::FileSystem, Self::Device, Option<PathBuf>)>, Self::Error> {
        Err("Unimplemented")
    }

    fn list_spawn(&self) -> Result<(), Self::Error> {
        Err("Unimplemented")
    }

    fn start(&mut self) -> Result<(), Self::Error> {
        Err("Unimplemented")
    }

    fn pause(&mut self) -> Result<(), Self::Error> {
        Err("Unimplemented")
    }

    fn reset(&mut self) -> Result<(), Self::Error> {
        Err("Unimplemented")
    }
}

#[cfg(windows)]
/// A platform specific [`NotificationSource`].
pub type PlatformNotifier<F> = windows::HcmNotifier<F>;

#[cfg(not(windows))]
/// A platform specific [`NotificationSource`].
pub type PlatformNotifier<F> = UnimplementedNotifier<F>;

/// Initialize the platform specific components.
pub fn platform_init() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(windows)]
    {
        windows::wmi::init_com()?;
    }

    Ok(())
}
