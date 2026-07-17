use std::{
    any::Any,
    ffi::{CStr, CString, c_char},
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    slice,
};

use crate::rawdisk_reader::RawdiskReader;

#[repr(C)]
pub struct RawdiskError {
    message: *mut c_char,
}

impl Drop for RawdiskError {
    fn drop(&mut self) {
        unsafe {
            if !self.message.is_null() {
                drop(CString::from_raw(self.message));
            }
        }
    }
}

fn panic_message(p: Box<dyn Any + Send>) -> String {
    let what = p
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown cause".into());

    format!("Panic while reading image: {what}")
}

/// Rust aborts the process when a panic unwinds across an `extern "C"` boundary,
/// so every entry point runs its body here. Malformed images can panic deep in
/// the parser; a C caller must get an error back instead of losing the process.
fn guard<T>(err: *mut *mut RawdiskError, fallback: T, f: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(p) => {
            fill_error(panic_message(p), err);
            fallback
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rawdisk_free_error(err: *mut RawdiskError) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !err.is_null() {
            unsafe {
                drop(Box::from_raw(err));
            }
        }
    }));
}

#[repr(C)]
pub struct RawdiskHandle {
    reader: *mut RawdiskReader,
    pub image_path: *const c_char,
    pub image_size: u64,
}

fn path_to_cstring<'a, P>(path: P) -> Result<CString, String>
where
    P: AsRef<Path> + 'a,
{
    path.as_ref()
        .to_str()
        .ok_or_else(|| "path is not UTF-8".into())
        .and_then(|s| CString::new(s).map_err(|_| "path contains an internal null".into()))
}

impl RawdiskHandle {
    fn new(reader: RawdiskReader) -> Result<Self, String> {
        let image_path = path_to_cstring(&reader.image_path)?.into_raw();

        Ok(Self {
            image_path,
            image_size: reader.image_size,
            reader: Box::into_raw(Box::new(reader)),
        })
    }
}

impl Drop for RawdiskHandle {
    fn drop(&mut self) {
        drop(unsafe { Box::from_raw(self.reader) });
        drop(unsafe { CString::from_raw(self.image_path as *mut c_char) });
    }
}

fn fill_error<E: ToString>(e: E, err: *mut *mut RawdiskError) {
    if !err.is_null() {
        // CString::new doesn't like internal nulls; the error message should
        // not have any, but we must deal with it nonetheless
        let message = CString::new(e.to_string())
            .unwrap_or_else(|_|
                CString::new(
                    format!(
                        "{}. Additionally, the original error message somehow contained an internal null, which should never happen.",
                        e.to_string().replace("\0", "\u{FFFD}")
                    )
                ).expect("inconceivable!")
            )
            .into_raw();

        unsafe {
            *err = Box::into_raw(Box::new(RawdiskError { message }));
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rawdisk_open(
    image_path: *const c_char,
    err: *mut *mut RawdiskError,
) -> *mut RawdiskHandle {
    guard(err, std::ptr::null_mut(), || {
        // convert path
        if image_path.is_null() {
            fill_error("image_path is null", err);
            return std::ptr::null_mut();
        }

        let p = unsafe { CStr::from_ptr(image_path) };

        let Ok(ip) = p.to_str() else {
            fill_error("image_path is not UTF-8", err);
            return std::ptr::null_mut();
        };

        // do the open
        match RawdiskReader::open(ip) {
            Ok(reader) => match RawdiskHandle::new(reader) {
                Ok(handle) => Box::into_raw(Box::new(handle)),
                Err(e) => {
                    fill_error(e, err);
                    std::ptr::null_mut()
                }
            },
            Err(e) => {
                fill_error(e, err);
                std::ptr::null_mut()
            }
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rawdisk_close(reader: *mut RawdiskHandle) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !reader.is_null() {
            drop(unsafe { Box::from_raw(reader) });
        }
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rawdisk_read(
    handle: *mut RawdiskHandle,
    offset: u64,
    buf: *mut c_char,
    buflen: usize,
    err: *mut *mut RawdiskError,
) -> usize {
    guard(err, 0, || {
        if handle.is_null() {
            fill_error("handle is null", err);
            return 0;
        }

        if buf.is_null() {
            fill_error("buf is null", err);
            return 0;
        }

        let buf = unsafe { slice::from_raw_parts_mut(buf as *mut u8, buflen) };
        unsafe { &*(*handle).reader }
            .read_at_offset(offset, buf)
            .unwrap_or_else(|e| {
                fill_error(e, err);
                0
            })
    })
}

#[cfg(test)]
mod test {
    use super::*;

    use crate::{test_data::*, test_helper::do_hash};

    struct Holder<T> {
        ptr: *mut T,
    }

    impl<T> Holder<T> {
        fn new(ptr: *mut T) -> Self {
            Self { ptr }
        }

        fn into_box(mut self) -> Box<T> {
            let ptr = self.ptr;
            self.ptr = std::ptr::null_mut();
            unsafe { Box::from_raw(ptr) }
        }
    }

    impl<T> Drop for Holder<T> {
        fn drop(&mut self) {
            if !self.ptr.is_null() {
                unsafe { drop(Box::from_raw(self.ptr)) }
            }
        }
    }

    #[track_caller]
    fn assert_err(err: *mut RawdiskError, message: &CStr) {
        assert!(!err.is_null());
        let err = unsafe { Box::from_raw(err) };

        assert!(!err.message.is_null());
        assert_eq!(unsafe { CStr::from_ptr(&*err.message) }, message);
    }

    #[track_caller]
    fn assert_err_null(err: *mut RawdiskError) {
        let err = Holder::new(err);
        assert!(err.ptr.is_null());
    }

    #[test]
    fn panic_in_ffi_body_becomes_an_error() {
        let mut err = std::ptr::null_mut();
        let r = guard(&mut err, 0usize, || panic!("boom"));

        assert_eq!(r, 0);
        assert_err_starts_with(err, c"Panic while reading image: boom");
    }

    #[test]
    fn panic_with_null_err_does_not_abort() {
        let r = guard(std::ptr::null_mut(), 0usize, || panic!("boom"));
        assert_eq!(r, 0);
    }

    #[test]
    fn free_error_releases_the_message() {
        let mut err = std::ptr::null_mut();
        fill_error("kaboom", &mut err);
        assert!(!err.is_null());

        // The message is a CString::into_raw pointer; freeing it as a Box
        // deallocates with the wrong layout. Run under Miri/ASan to catch it.
        unsafe { rawdisk_free_error(err) };
    }

    #[track_caller]
    fn assert_err_starts_with(err: *mut RawdiskError, prefix: &CStr) {
        assert!(!err.is_null());
        let err = unsafe { Box::from_raw(err) };

        assert!(!err.message.is_null());

        let message = unsafe { CStr::from_ptr(&*err.message) };

        let msg_b = message.to_bytes();
        let pre_b = prefix.to_bytes();

        assert_eq!(
            &msg_b[..pre_b.len()],
            pre_b,
            "{message:?} does not start with {prefix:?}"
        );
    }

    #[track_caller]
    fn assert_eq_test_data_no_hashing(handle: &RawdiskHandle, exp: &TestData) {
        let image_path = unsafe { CStr::from_ptr(handle.image_path) }
            .to_str()
            .unwrap();

        let act = TestData {
            image_path,
            image_size: handle.image_size,
            sha1: exp.sha1,
        };

        assert_eq!(&act, exp);
    }

    #[track_caller]
    fn assert_eq_test_data(h: *mut RawdiskHandle, exp: &TestData) {
        let handle = unsafe { &*h };

        let sha1 = do_hash(
            |offset, buf: &mut [u8]| {
                let mut err = std::ptr::null_mut();
                let read = unsafe {
                    rawdisk_read(
                        h,
                        offset,
                        buf.as_mut_ptr() as *mut c_char,
                        buf.len(),
                        &mut err,
                    )
                };

                assert_err_null(err);
                read
            },
            handle.image_size,
            false,
        );

        let image_path = unsafe { CStr::from_ptr(handle.image_path) }
            .to_str()
            .unwrap();

        let act = TestData {
            image_path,
            image_size: handle.image_size,
            sha1: &sha1,
        };

        assert_eq!(&act, exp);
    }

    #[test]
    fn test_rawdisk_open_null_path_null_err() {
        let h = Holder::new(unsafe { rawdisk_open(std::ptr::null(), std::ptr::null_mut()) });

        assert!(h.ptr.is_null());
    }

    #[test]
    fn test_rawdisk_open_nonexistent_path_null_err() {
        let path = c"bogus".as_ptr();

        let h = Holder::new(unsafe { rawdisk_open(path, std::ptr::null_mut()) });

        assert!(h.ptr.is_null());
    }

    #[test]
    fn test_rawdisk_open_null_paths() {
        let mut err = std::ptr::null_mut();

        let h = Holder::new(unsafe { rawdisk_open(std::ptr::null(), &mut err) });

        assert_err(err, c"image_path is null");
        assert!(h.ptr.is_null());
    }

    #[test]
    fn test_rawdisk_open_ok() {
        let path = c"data/patterned_4mib.raw".as_ptr();
        let mut err = std::ptr::null_mut();

        let h = Holder::new(unsafe { rawdisk_open(path, &mut err) });

        assert_err_null(err);
        assert!(!h.ptr.is_null());

        let handle = h.into_box();
        assert_eq_test_data_no_hashing(&handle, &PATTERNED_4MIB);

        let handle = Box::into_raw(handle);
        unsafe {
            rawdisk_close(handle);
        }
    }

    #[test]
    fn test_rawdisk_close_null() {
        // nothing to test here other than that it doesn't crash
        unsafe { rawdisk_close(std::ptr::null_mut()) };
    }

    #[test]
    fn test_rawdisk_read_null_handle_null_err() {
        let mut buf: [c_char; 1] = [0];

        let r = unsafe {
            rawdisk_read(
                std::ptr::null_mut(),
                0,
                buf.as_mut_ptr(),
                buf.len(),
                std::ptr::null_mut(),
            )
        };

        assert_eq!(r, 0);
    }

    #[test]
    fn test_rawdisk_read_null_buffer_null_err() {
        let path = c"data/patterned_4mib.raw".as_ptr();

        let h = Holder::new(unsafe { rawdisk_open(path, std::ptr::null_mut()) });

        assert!(!h.ptr.is_null());

        let r = unsafe { rawdisk_read(h.ptr, 0, std::ptr::null_mut(), 1, std::ptr::null_mut()) };

        assert_eq!(r, 0);
    }

    #[test]
    fn test_rawdisk_read_null_handle() {
        let mut buf: [c_char; 1] = [0];
        let mut err = std::ptr::null_mut();

        let r = unsafe {
            rawdisk_read(
                std::ptr::null_mut(),
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut err,
            )
        };

        assert_err(err, c"handle is null");
        assert_eq!(r, 0);
    }

    #[test]
    fn test_rawdisk_read_null_buffer() {
        let path = c"data/patterned_4mib.raw".as_ptr();
        let mut err = std::ptr::null_mut();

        let h = Holder::new(unsafe { rawdisk_open(path, &mut err) });

        assert_err_null(err);
        assert!(!h.ptr.is_null());

        let r = unsafe { rawdisk_read(h.ptr, 0, std::ptr::null_mut(), 1, &mut err) };

        assert_err(err, c"buf is null");
        assert_eq!(r, 0);
    }

    #[test]
    fn test_rawdisk_read_offset_past_end() {
        let path = c"data/patterned_4mib.raw".as_ptr();
        let mut err = std::ptr::null_mut();

        let h = Holder::new(unsafe { rawdisk_open(path, &mut err) });

        assert_err_null(err);
        assert!(!h.ptr.is_null());

        let mut buf: [c_char; 1] = [0];

        let r = unsafe { rawdisk_read(h.ptr, u64::MAX, buf.as_mut_ptr(), buf.len(), &mut err) };

        assert_err_starts_with(
            err,
            c"Requested offset 18446744073709551615 is beyond end of image",
        );
        assert_eq!(r, 0);
    }

    #[test]
    fn test_rawdisk_read_and_hash() {
        let path = c"data/patterned_4mib.raw".as_ptr();
        let mut err = std::ptr::null_mut();

        let h = Holder::new(unsafe { rawdisk_open(path, &mut err) });

        assert_err_null(err);
        assert!(!h.ptr.is_null());

        let mut handle = h.into_box();
        assert_eq_test_data(&mut *handle, &PATTERNED_4MIB);

        let handle = Box::into_raw(handle);
        unsafe {
            rawdisk_close(handle);
        }
    }
}
