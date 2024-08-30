use crate::mem::AlignedBuffer;

use super::{DropHandle, Error, ULONG, USHORT};
use windows::{
    core::w,
    Win32::{
        Foundation::{ERROR_MORE_DATA, HANDLE, MAX_PATH},
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
                .map_err(|e| Error::Win32Error("CreateFileW", e))?,
            ),
        })
    }

    pub fn query_points(&self, volume_name: &[u16]) -> Result<Vec<String>, Error> {
        let mut names = Vec::new();

        unsafe {
            let mut attempt = 0;
            let mut buf = AlignedBuffer::new(
                std::mem::size_of::<MOUNTMGR_MOUNT_POINT>() + volume_name.len() * 2,
                std::mem::align_of::<MOUNTMGR_MOUNT_POINT>(),
            )
            .ok_or(Error::AllocFailed)?;

            #[allow(clippy::cast_possible_truncation)]
            let input = MOUNTMGR_MOUNT_POINT {
                device_name_length: (volume_name.len() * 2)
                    .try_into()
                    .map_err(|_| Error::Overflow)?,
                ..Default::default()
            };

            let input_ptr = buf.write_aligned(&input, 1).ok_or(Error::Overflow)?;

            let volume_name_ptr = buf
                .write_aligned(std::ptr::from_ref(&volume_name), volume_name.len())
                .ok_or(Error::Overflow)?;

            (*input_ptr).device_name_offset = volume_name_ptr
                .byte_offset_from(input_ptr)
                .try_into()
                .map_err(|_| Error::Overflow)?;

            let mut out_buf_size = std::mem::size_of::<MOUNTMGR_MOUNT_POINTS>() + MAX_PATH as usize;

            while attempt < 5 {
                attempt += 1;

                let mut returned = 0u32;

                let out_buf =
                    AlignedBuffer::new(out_buf_size, std::mem::align_of::<MOUNTMGR_MOUNT_POINTS>())
                        .ok_or(Error::AllocFailed)?;

                #[allow(clippy::cast_possible_truncation)]
                let ret = DeviceIoControl(
                    self.handle.0,
                    IOCTL_MOUNTMGR_QUERY_POINTS,
                    Some(buf.as_mut_ptr().cast()),
                    buf.byte_len() as u32,
                    Some(out_buf.as_mut_ptr().cast()),
                    out_buf.byte_len() as u32,
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

                #[allow(clippy::cast_ptr_alignment, clippy::expect_used)]
                let out_ptr = out_buf
                    .cast::<MOUNTMGR_MOUNT_POINTS>()
                    .as_ref()
                    .expect("out_buf is null");

                for i in 0..out_ptr.number_of_mount_points {
                    #[allow(clippy::expect_used)]
                    let point = out_ptr
                        .points
                        .as_ptr()
                        .add(i as usize)
                        .cast::<MOUNTMGR_MOUNT_POINT>()
                        .as_ref()
                        .expect("point is null");
                    if point.symbolic_link_name_offset == 0 {
                        continue;
                    }
                    #[allow(clippy::cast_ptr_alignment)]
                    let name = std::slice::from_raw_parts(
                        out_buf
                            .as_ptr()
                            .add(point.symbolic_link_name_offset as usize)
                            .cast::<u16>(),
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
