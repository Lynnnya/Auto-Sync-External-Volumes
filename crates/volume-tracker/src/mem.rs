use std::{
    alloc::{alloc_zeroed, dealloc, Layout},
    ops::Deref,
};

pub struct AlignedBuffer {
    layout: Layout,
    head: *mut u8,
    ptr: *mut u8,
    len_bytes: usize,
}

impl AlignedBuffer {
    pub fn new(len_bytes: usize, align_bytes: usize) -> Option<Self> {
        let layout = Layout::from_size_align(len_bytes, align_bytes).ok()?;

        let head = unsafe { alloc_zeroed(layout) };

        if head.is_null() {
            return None;
        }

        Some(Self {
            layout,
            head,
            ptr: head,
            len_bytes,
        })
    }
    pub fn write_aligned<T>(&mut self, data: *const T, len: usize) -> Option<*mut T> {
        let offset = self.ptr.align_offset(std::mem::align_of::<T>());
        unsafe {
            self.ptr = self.ptr.add(offset);
            let ptr_cast = self.ptr.cast::<T>();
            if ptr_cast.add(len).cast() > self.head.add(self.len_bytes) {
                return None;
            }

            std::ptr::copy_nonoverlapping(data, ptr_cast, len);

            self.ptr = ptr_cast.add(len).cast();

            Some(ptr_cast)
        }
    }
    pub fn as_ptr(&self) -> *const u8 {
        self.head
    }
    pub fn as_mut_ptr(&self) -> *mut u8 {
        self.head
    }
    pub fn byte_len(&self) -> usize {
        self.len_bytes
    }
}

impl Deref for AlignedBuffer {
    type Target = *mut u8;

    fn deref(&self) -> &Self::Target {
        &self.head
    }
}

impl Drop for AlignedBuffer {
    fn drop(&mut self) {
        unsafe {
            dealloc(self.head, self.layout);
        }
    }
}
