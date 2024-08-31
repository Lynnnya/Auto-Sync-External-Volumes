use std::marker::PhantomData;

pub struct PzzWSTRIter<'a> {
    ptr: *const u16,
    _marker: PhantomData<&'a u16>,
}

impl PzzWSTRIter<'_> {
    pub unsafe fn new(ptr: *const u16) -> Self {
        Self {
            ptr,
            _marker: PhantomData,
        }
    }
}

impl<'a> Iterator for PzzWSTRIter<'a> {
    type Item = &'a [u16];

    fn next(&mut self) -> Option<Self::Item> {
        if self.ptr.is_null() {
            return None;
        }

        unsafe {
            if (*self.ptr) == 0 {
                return None;
            }

            let mut end_ptr = self.ptr;
            while (*end_ptr) != 0 {
                end_ptr = end_ptr.add(1);
            }

            #[allow(clippy::cast_sign_loss)]
            let slice =
                std::slice::from_raw_parts(self.ptr, end_ptr.offset_from(self.ptr) as usize);
            self.ptr = end_ptr.add(1);

            Some(slice)
        }
    }
}
