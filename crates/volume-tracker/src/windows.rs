use std::{
    ffi::{c_ulong, c_ushort, c_void},
    fmt::{Debug, Display},
    hash::Hash,
    ops::{Deref, DerefMut},
    path::PathBuf,
    pin::Pin,
    sync::Arc,
};

#[allow(clippy::upper_case_acronyms)]
type ULONG = c_ulong;
#[allow(clippy::upper_case_acronyms)]
type USHORT = c_ushort;

use array::PzzWSTRIter;
use dashmap::DashSet;
use mount_mgr::MountMgr;
use windows::{
    core::PCWSTR,
    Win32::{
        Devices::DeviceAndDriverInstallation::{
            CM_Get_Device_Interface_ListW, CM_Get_Device_Interface_List_SizeW,
            CM_Register_Notification, CM_Unregister_Notification,
            CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_NOTIFY_ACTION,
            CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL, CM_NOTIFY_ACTION_DEVICEINTERFACEREMOVAL,
            CM_NOTIFY_EVENT_DATA, CM_NOTIFY_FILTER, CM_NOTIFY_FILTER_0, CM_NOTIFY_FILTER_0_2,
            CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE, CR_BUFFER_SMALL, CR_SUCCESS, HCMNOTIFICATION,
        },
        Foundation::*,
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_ALWAYS,
        },
        System::{Ioctl::GUID_DEVINTERFACE_VOLUME, IO::DeviceIoControl},
    },
};
use wmi::WmiObserver;

use crate::{AbortHandleHolder, Device, FileSystem, NotificationSource, SpawnerDisposition};

pub(crate) mod array;
pub(crate) mod mount_mgr;
pub(crate) mod wmi;

/// The root path name of a volume, like '\\?\Volume{GUID}'.
#[derive(Clone)]
pub struct VolumeName {
    nonpersistent_name: String,
    mount_mgr: Arc<MountMgr>,
}

impl Debug for VolumeName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "VolumeName({})", self.nonpersistent_name)
    }
}

impl Hash for VolumeName {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.nonpersistent_name.hash(state);
    }
}

impl PartialEq for VolumeName {
    fn eq(&self, other: &Self) -> bool {
        self.nonpersistent_name == other.nonpersistent_name
    }
}

impl Eq for VolumeName {}

impl VolumeName {
    /// Get the device name of the volume. Like '\\Device\HarddiskVolume1'.
    pub fn device_name(&self) -> Result<DeviceName, Error> {
        let mut file_name = self.nonpersistent_name.encode_utf16().collect::<Vec<_>>();
        file_name.push(0);

        let handle = DropHandle(unsafe {
            CreateFileW(
                PCWSTR::from_raw(file_name.as_ptr()),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                HANDLE(std::ptr::null_mut()),
            )
            .map_err(Error::Win32Error)?
        });

        #[repr(C)]
        #[allow(non_camel_case_types)]
        struct MOUNTDEV_NAME {
            size: USHORT,
            name: [u16; MAX_PATH as usize],
        }

        const IOCTL_MOUNTDEV_QUERY_DEVICE_NAME: u32 = 0x004D0008;

        let mut buf = MOUNTDEV_NAME {
            size: 0,
            name: [0u16; MAX_PATH as usize],
        };
        let volume_name = unsafe {
            DeviceIoControl(
                *handle,
                IOCTL_MOUNTDEV_QUERY_DEVICE_NAME,
                None,
                0,
                Some(&mut buf as *mut _ as *mut c_void),
                std::mem::size_of_val(&buf) as u32,
                None,
                None,
            )
            .map_err(|e| Error::Win32ErrorOnIoctl("IOCTL_MOUNTDEV_QUERY_DEVICE_NAME", e))?;

            std::slice::from_raw_parts(buf.name.as_ptr(), (buf.size / 2) as usize)
        };

        Ok(DeviceName(
            String::from_utf16(volume_name).map_err(|_| Error::DecodeUtf16Error)?,
        ))
    }

    /// Get the DOS paths of the volume. Like 'C:'.
    pub fn dos_paths(&self) -> Result<Vec<String>, Error> {
        self.device_name()?.dos_paths(&self.mount_mgr)
    }
}

impl Display for VolumeName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.nonpersistent_name)
    }
}

impl FileSystem for VolumeName {
    fn name(&self) -> &str {
        &self.nonpersistent_name
    }
}

/// The resolved device name of a volume, like '\\Device\HarddiskVolume1'.
#[derive(Debug, Clone, PartialEq, Hash, Eq)]
pub struct DeviceName(String);

impl Device for DeviceName {
    fn name(&self) -> &str {
        &self.0
    }
}

impl DeviceName {
    /// Get the DOS paths of the device. Like 'C:'.
    pub fn dos_paths(&self, mount_mgr: &MountMgr) -> Result<Vec<String>, Error> {
        Ok(mount_mgr
            .query_points(&self.0.encode_utf16().collect::<Vec<_>>())?
            .into_iter()
            .filter_map(|s| find_dos_path(&s).map(|s| s.to_string()))
            .collect())
    }
}

pub(crate) struct DropHandle(pub(crate) HANDLE);

unsafe impl Send for DropHandle {}
unsafe impl Sync for DropHandle {}

impl Drop for DropHandle {
    fn drop(&mut self) {
        unsafe {
            if let Err(e) = CloseHandle(self.0) {
                log::error!("Failed to close handle: {}", e);
            }
        }
    }
}

impl Deref for DropHandle {
    type Target = HANDLE;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for DropHandle {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[inline(always)]
/// Try to find a DOS path in a string. Like 'C:'.
pub fn find_dos_path(input: &str) -> Option<&str> {
    input.strip_prefix(r"\DosDevices\")
}

#[derive(Debug, Clone, thiserror::Error)]
/// Errors that can occur in the Windows volume tracker.
#[allow(missing_docs)]
#[non_exhaustive]
pub enum Error {
    #[error("system error: code {0}")]
    SyscallFailed(u32),
    #[error("win32 error: {0}")]
    Win32Error(#[from] windows::core::Error),
    #[error("win32 error on ioctl: {0}: {1}")]
    Win32ErrorOnIoctl(&'static str, windows::core::Error),
    #[error("received invalid utf-16 string")]
    DecodeUtf16Error,
    #[error("Too many retries")]
    TooManyRetries,
}

struct UnsafeSync<T>(T);

impl<T> Deref for UnsafeSync<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for UnsafeSync<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

unsafe impl<T> Sync for UnsafeSync<T> {}

/// A file system notification source for Windows using the Plug and Play manager.
pub struct HcmNotifier<
    F: Fn(VolumeName, DeviceName, Option<PathBuf>) -> SpawnerDisposition
        + Send
        + Sync
        + Clone
        + 'static,
> {
    handle: Option<UnsafeSync<HCMNOTIFICATION>>,
    ctx: Pin<Box<Context>>,
    spawner: Arc<F>,
    _wmi: WmiObserver,
}

struct Context {
    aborter: Arc<AbortHandleHolder<VolumeName>>,
    new_device_queue: Arc<DashSet<VolumeName>>,
    mount_mgr: Arc<MountMgr>,
}

impl<
        F: Fn(VolumeName, DeviceName, Option<PathBuf>) -> SpawnerDisposition
            + Send
            + Sync
            + Clone
            + 'static,
    > NotificationSource<F> for HcmNotifier<F>
{
    type FileSystem = VolumeName;
    type Device = DeviceName;
    type Error = Error;

    fn new(callback: F) -> Result<Self, Self::Error> {
        let queue = Arc::new(DashSet::<VolumeName>::new());
        let queue_clone = queue.clone();
        let aborter = Arc::new(AbortHandleHolder::default());
        let aborter_clone = aborter.clone();
        let callback = Arc::new(callback);
        let callback_clone = callback.clone();

        let inner_cb = Box::new(move || {
            log::debug!("new device callback");
            aborter_clone.gc();

            queue_clone.retain(|mp| {
                let d = match mp.device_name() {
                    Ok(device) => device,
                    Err(e) => {
                        log::error!("Failed to get device name for volume {:?}: {}", *mp, e);
                        return false;
                    }
                };

                let dos_paths = match mp.dos_paths() {
                    Ok(paths) => paths.into_iter().map(PathBuf::from).next(),
                    Err(e) => {
                        log::warn!("Failed to get DOS paths for volume {:?}: {}", *mp, e);
                        None
                    }
                };

                match callback_clone(mp.clone(), d.clone(), dos_paths) {
                    SpawnerDisposition::Spawned(handle) => {
                        aborter_clone.insert(mp.clone(), handle);
                        false
                    }
                    SpawnerDisposition::Ignore => false,
                    SpawnerDisposition::Skip => true,
                }
            })
        });

        Ok(Self {
            handle: None,
            ctx: Box::pin(Context {
                aborter,
                new_device_queue: queue,
                mount_mgr: Arc::new(MountMgr::new()?),
            }),
            spawner: callback,
            _wmi: WmiObserver::new(inner_cb)?,
        })
    }

    fn list(&self) -> Result<Vec<(Self::FileSystem, Self::Device, Option<PathBuf>)>, Self::Error> {
        let mut attempt = 0;

        while attempt < 5 {
            attempt += 1;

            let mut char_count = 0u32;
            let ret = unsafe {
                CM_Get_Device_Interface_List_SizeW(
                    &mut char_count as *mut _,
                    &GUID_DEVINTERFACE_VOLUME as *const _,
                    PCWSTR::null(),
                    CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
                )
            };
            if ret != CR_SUCCESS {
                return Err(Error::SyscallFailed(ret.0));
            }

            let mut buffer = vec![0u16; char_count as usize];

            let ret = unsafe {
                CM_Get_Device_Interface_ListW(
                    &GUID_DEVINTERFACE_VOLUME as *const _,
                    PCWSTR::null(),
                    &mut buffer,
                    CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
                )
            };

            if ret != CR_SUCCESS {
                if ret == CR_BUFFER_SMALL {
                    continue;
                }
                return Err(Error::SyscallFailed(ret.0));
            }

            return Ok(unsafe { PzzWSTRIter::new(buffer.as_ptr()) }
                .filter_map(|s| {
                    let mp = VolumeName {
                        nonpersistent_name: String::from_utf16_lossy(s),
                        mount_mgr: self.ctx.mount_mgr.clone(),
                    };
                    let device = match mp.device_name() {
                        Ok(device) => device,
                        Err(_) => {
                            log::error!("Failed to get device name for volume: {:?}", mp);
                            return None;
                        }
                    };

                    let dos_paths = match mp.dos_paths() {
                        Ok(paths) => paths.into_iter().map(PathBuf::from).next(),
                        Err(e) => {
                            log::warn!("Failed to get DOS paths for volume {:?}: {}", mp, e);
                            None
                        }
                    };

                    Some((mp, device, dos_paths))
                })
                .collect());
        }

        Err(Error::TooManyRetries)
    }

    fn list_spawn(&self) -> Result<(), Self::Error> {
        self.ctx.aborter.clear_abort();
        let list = self.list()?;
        for (mp, d, dos_paths) in list {
            if let SpawnerDisposition::Spawned(handle) =
                (self.spawner)(mp.clone(), d.clone(), dos_paths)
            {
                self.ctx.aborter.insert(mp, handle);
            }
        }

        Ok(())
    }

    fn start(&mut self) -> Result<(), Self::Error> {
        let filter = CM_NOTIFY_FILTER {
            cbSize: std::mem::size_of::<CM_NOTIFY_FILTER>() as u32,
            Flags: 0,
            FilterType: CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE,
            u: CM_NOTIFY_FILTER_0 {
                DeviceInterface: CM_NOTIFY_FILTER_0_2 {
                    ClassGuid: GUID_DEVINTERFACE_VOLUME,
                },
            },
            ..Default::default()
        };

        let mut hnotify = HCMNOTIFICATION::default();

        let ret = unsafe {
            CM_Register_Notification(
                &filter as *const _,
                Some(&*self.ctx as *const Context as *const c_void),
                Some(notify_proc),
                &mut hnotify,
            )
        };
        if ret != CR_SUCCESS {
            return Err(Error::SyscallFailed(ret.0));
        }

        self.handle = Some(UnsafeSync(hnotify));

        Ok(())
    }

    fn pause(&mut self) -> Result<(), Self::Error> {
        if let Some(handle) = self.handle.take() {
            unsafe {
                let ret = CM_Unregister_Notification(*handle);

                if ret != CR_SUCCESS {
                    return Err(Error::SyscallFailed(ret.0));
                }
            }
        }

        Ok(())
    }

    fn reset(&mut self) -> Result<(), Self::Error> {
        self.pause()?;
        self.ctx.aborter.clear_abort();
        Ok(())
    }
}

impl<F> Drop for HcmNotifier<F>
where
    F: Fn(VolumeName, DeviceName, Option<PathBuf>) -> SpawnerDisposition
        + Send
        + Sync
        + Clone
        + 'static,
{
    fn drop(&mut self) {
        if let Err(e) = self.pause() {
            log::error!("Failed to unregister notification: {}", e);
        }
    }
}

unsafe extern "system" fn notify_proc(
    _hnotify: HCMNOTIFICATION,
    ctx: *const c_void,
    action: CM_NOTIFY_ACTION,
    evt_data: *const CM_NOTIFY_EVENT_DATA,
    evt_data_size: u32,
) -> u32 {
    let ctx = &*(ctx as *const Context);
    ctx.aborter.gc();

    match action {
        CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL | CM_NOTIFY_ACTION_DEVICEINTERFACEREMOVAL => {
            let data = &*evt_data;
            let name = data.u.DeviceInterface.SymbolicLink.as_ptr();
            let mut end_ptr = evt_data.byte_add(evt_data_size as usize) as *const u16;
            while end_ptr > name && (*end_ptr.sub(1)) == 0 {
                end_ptr = end_ptr.sub(1);
            }

            let mp = VolumeName {
                nonpersistent_name: String::from_utf16_lossy(std::slice::from_raw_parts(
                    name,
                    end_ptr.offset_from(name) as usize,
                )),
                mount_mgr: ctx.mount_mgr.clone(),
            };

            match action {
                CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL => {
                    log::info!("new device arrival: {:?}", &mp);
                    ctx.new_device_queue.insert(mp);
                }
                CM_NOTIFY_ACTION_DEVICEINTERFACEREMOVAL => {
                    log::info!("device removal: {:?}", &mp);
                    ctx.new_device_queue.remove(&mp);
                    ctx.aborter.remove_abort(&mp);
                }
                _ => unreachable!(),
            }
        }
        _ => {}
    }
    ERROR_SUCCESS.0
}
