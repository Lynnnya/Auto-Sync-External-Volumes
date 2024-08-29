use super::{DropHandle, Error, ULONG, USHORT};
use std::ffi::c_void;
use windows::{
    core::w,
    Win32::{
        Foundation::*,
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_ALWAYS,
        },
        System::IO::DeviceIoControl,
    },
};

#[repr(C)]
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Default)]
struct MOUNTMGR_MOUNT_POINT {
    symbolic_link_name_offset: ULONG,
    symbolic_link_name_length: USHORT,
    _reserved1: USHORT,
    unique_id_offset: ULONG,
    unique_id_length: USHORT,
    _reserved2: USHORT,
    device_name_offset: ULONG,
    device_name_length: USHORT,
    _reserved3: USHORT,
}

#[repr(C)]
#[allow(non_camel_case_types)]
struct MOUNTMGR_MOUNT_POINTS {
    size: ULONG,
    number_of_mount_points: ULONG,
    points: [MOUNTMGR_MOUNT_POINT; 1],
}

const IOCTL_MOUNTMGR_QUERY_POINTS: u32 = 0x006D0008;

pub struct MountMgr {
    handle: DropHandle,
}

impl MountMgr {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            handle: DropHandle(
                unsafe {
                    CreateFileW(
                        w!(r"\\.\MountPointManager"),
                        0,
                        FILE_SHARE_READ | FILE_SHARE_WRITE,
                        None,
                        OPEN_ALWAYS,
                        FILE_ATTRIBUTE_NORMAL,
                        HANDLE(std::ptr::null_mut()),
                    )
                }
                .map_err(Error::Win32Error)?,
            ),
        })
    }

    pub fn query_points(&self, volume_name: &[u16]) -> Result<Vec<String>, Error> {
        let mut names = Vec::new();

        unsafe {
            let mut attempt = 0;
            let mut buf =
                vec![0u8; std::mem::size_of::<MOUNTMGR_MOUNT_POINT>() + volume_name.len() * 2];

            let input = MOUNTMGR_MOUNT_POINT {
                device_name_offset: std::mem::size_of::<MOUNTMGR_MOUNT_POINT>() as _,
                device_name_length: volume_name.len() as u16 * 2,
                ..Default::default()
            };

            std::ptr::copy_nonoverlapping(
                &input as *const _ as *const u8,
                buf.as_mut_ptr(),
                std::mem::size_of::<MOUNTMGR_MOUNT_POINT>(),
            );

            std::ptr::copy_nonoverlapping(
                volume_name.as_ptr(),
                buf.as_mut_ptr()
                    .byte_add(std::mem::size_of::<MOUNTMGR_MOUNT_POINT>())
                    as *mut u16,
                volume_name.len(),
            );

            let mut out_buf_size = std::mem::size_of::<MOUNTMGR_MOUNT_POINTS>() as u32 + MAX_PATH;

            while attempt < 5 {
                attempt += 1;

                let mut out_buf = vec![0u8; out_buf_size as usize];

                let mut returned = 0u32;

                let ret = DeviceIoControl(
                    self.handle.0,
                    IOCTL_MOUNTMGR_QUERY_POINTS,
                    Some(buf.as_ptr() as *mut c_void),
                    buf.len() as u32,
                    Some(out_buf.as_mut_ptr() as *mut c_void),
                    out_buf_size,
                    Some(&mut returned),
                    None,
                );
                if let Err(e) = ret {
                    if e.code() == ERROR_MORE_DATA.into() {
                        out_buf_size *= 2;
                        continue;
                    }
                    return Err(Error::Win32ErrorOnIoctl("IOCTL_MOUNTMGR_QUERY_POINTS", e));
                }

                let out_ptr = &*(out_buf.as_ptr() as *const MOUNTMGR_MOUNT_POINTS);

                for i in 0..out_ptr.number_of_mount_points {
                    let point = &*out_ptr.points.as_ptr().add(i as usize);
                    if point.symbolic_link_name_offset == 0 {
                        continue;
                    }
                    let name = std::slice::from_raw_parts(
                        out_buf
                            .as_ptr()
                            .add(point.symbolic_link_name_offset as usize)
                            as *const u16,
                        point.symbolic_link_name_length as usize / 2,
                    );
                    names.push(String::from_utf16_lossy(name));
                }

                break;
            }

            if attempt >= 5 {
                return Err(Error::TooManyRetries);
            }
        }

        Ok(names)
    }
}
