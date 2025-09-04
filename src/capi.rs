use std::{
    ffi::{
        CStr,
        CString,
        OsStr,
        c_char
    },
    mem::ManuallyDrop,
    os::unix::ffi::OsStrExt,
    slice
};

use crate::e01_reader::{
    self,
    E01Reader
};

#[repr(C)]
pub struct E01Error {
    message: *mut c_char
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn e01_free_error(err: *mut E01Error) {
    if !err.is_null() {
        unsafe {
            if !(*err).message.is_null() {
                drop(Box::from_raw((*err).message));
            }

            drop(Box::from_raw(err));
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone)]
pub enum CorruptSectionPolicy {
  CorruptSectionPolicy_ERROR,
  CorruptSectionPolicy_DAMN_THE_TORPEDOES
}

impl From<CorruptSectionPolicy> for e01_reader::CorruptSectionPolicy {
    fn from(policy: CorruptSectionPolicy) -> e01_reader::CorruptSectionPolicy {
        match policy {
            CorruptSectionPolicy::CorruptSectionPolicy_ERROR => e01_reader::CorruptSectionPolicy::Error,
            CorruptSectionPolicy::CorruptSectionPolicy_DAMN_THE_TORPEDOES => e01_reader::CorruptSectionPolicy::DamnTheTorpedoes
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone)]
pub enum CorruptChunkPolicy {
  CorruptChunkPolicy_ERROR,
  CorruptChunkPolicy_ZERO,
  CorruptChunkPolicy_RAW_IF_POSSIBLE
}

impl From<CorruptChunkPolicy> for e01_reader::CorruptChunkPolicy {
    fn from(policy: CorruptChunkPolicy) -> e01_reader::CorruptChunkPolicy {
        match policy {
            CorruptChunkPolicy::CorruptChunkPolicy_ERROR => e01_reader::CorruptChunkPolicy::Error,
            CorruptChunkPolicy::CorruptChunkPolicy_ZERO => e01_reader::CorruptChunkPolicy::Zero,
            CorruptChunkPolicy::CorruptChunkPolicy_RAW_IF_POSSIBLE => e01_reader::CorruptChunkPolicy::RawIfPossible
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct E01ReaderOptions {
  corrupt_section_policy: CorruptSectionPolicy,
  corrupt_chunk_policy: CorruptChunkPolicy
}

impl From<E01ReaderOptions> for e01_reader::E01ReaderOptions {
    fn from(options: E01ReaderOptions) -> e01_reader::E01ReaderOptions {
        e01_reader::E01ReaderOptions {
            corrupt_section_policy: options.corrupt_section_policy.into(),
            corrupt_chunk_policy: options.corrupt_chunk_policy.into()
        }
    }
}

fn fill_error<E: ToString>(e: E, err: *mut *mut E01Error) {
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

        unsafe { *err = Box::into_raw(Box::new(E01Error { message })); }
    }
}

#[repr(C)]
pub struct E01Handle {
    reader: E01Reader,
    pub segment_paths: *const *const c_char,
    pub segment_paths_count: usize,
    pub chunk_size: usize,
    pub chunk_count: usize,
    pub sector_count: usize,
    pub sector_size: usize,
    pub image_size: usize,
    pub stored_md5: *const u8,
    pub stored_sha1: *const u8
}

unsafe fn free_c_str_array(ptr: *mut *mut c_char, len: usize) {
    let v = unsafe { Vec::from_raw_parts(ptr, len, len) };

    for s in v {
        let s = unsafe { CString::from_raw(s) };
        drop(s);
    }
}

impl E01Handle {
    fn new(reader: E01Reader, segment_paths: Vec<*const c_char>) -> Self {
        let mut segment_paths = segment_paths;
        segment_paths.shrink_to_fit();

        let segment_paths = ManuallyDrop::new(segment_paths).as_ptr();

        Self {
            segment_paths,
            segment_paths_count: reader.segment_paths.len(),
            chunk_size: reader.chunk_size,
            chunk_count: reader.chunk_count,
            sector_count: reader.sector_count,
            sector_size: reader.sector_size,
            image_size: reader.image_size,
            stored_md5: match reader.stored_md5 {
                None => std::ptr::null_mut(),
                Some(h) => h.as_ptr()
            },
            stored_sha1: match reader.stored_sha1 {
                None => std::ptr::null_mut(),
                Some(h) => h.as_ptr()
            },
            reader
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn e01_open(
    segment_paths: *const *const c_char,
    segment_paths_len: usize,
    options: *const E01ReaderOptions,
    err: *mut *mut E01Error
) -> *mut E01Handle
{
    if options.is_null() {
       fill_error("options is null", err);
       return std::ptr::null_mut();
    }

    if segment_paths.is_null() {
       fill_error("segment_paths is null", err);
       return std::ptr::null_mut();
    }

    let options = unsafe { (*options).into() };

    let sl = unsafe { slice::from_raw_parts(segment_paths, segment_paths_len) };

    let mut segment_paths = Vec::with_capacity(segment_paths_len);
    let mut cstr_segment_paths = Vec::with_capacity(segment_paths_len);

    for (i, p) in sl.iter().enumerate() {
        if p.is_null() {
            fill_error(format!("segment_paths[{i}] is null"), err);
            return std::ptr::null_mut();
        }

        let p = unsafe { CStr::from_ptr(*p) };

        let cstr = CString::from(p);
        cstr_segment_paths.push(cstr.into_raw() as *const c_char);

        let as_p = OsStr::from_bytes(p.to_bytes());
        segment_paths.push(as_p);
    }

    match E01Reader::open(segment_paths, &options) {
        Ok(reader) => Box::into_raw(Box::new(
            E01Handle::new(reader, cstr_segment_paths)
        )),
        Err(e) => {
            fill_error(e, err);
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn e01_open_glob(
    example_segment_path: *const c_char,
    options: *const E01ReaderOptions,
    err: *mut *mut E01Error
) -> *mut E01Handle
{
    if options.is_null() {
       fill_error("options is null", err);
       return std::ptr::null_mut();
    }

    if example_segment_path.is_null() {
       fill_error("example_segment_path is null", err);
       return std::ptr::null_mut();
    }

    let options = unsafe { (*options).into() };

    let p = unsafe { CStr::from_ptr(example_segment_path) };

    let example_segment_path = OsStr::from_bytes(p.to_bytes());

    let cstr_segment_paths = vec![
        CString::from(p).into_raw() as *const c_char
    ];

    match E01Reader::open_glob(example_segment_path, &options) {
        Ok(reader) => Box::into_raw(Box::new(
            E01Handle::new(reader, cstr_segment_paths)
        )),
        Err(e) => {
            fill_error(e, err);
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn e01_close(reader: *mut E01Handle) {
    if !reader.is_null() {
        let reader = unsafe { Box::from_raw(reader) };
        free_c_str_array(
            reader.segment_paths as *mut *mut c_char,
            reader.segment_paths_count
        );
        drop(reader);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn e01_read(
    reader: *mut E01Handle,
    offset: usize,
    buf: *mut c_char,
    buflen: usize,
    err: *mut *mut E01Error
) -> usize
{
    if reader.is_null() {
       fill_error("reader is null", err);
       return 0;
    }

    if buf.is_null() {
       fill_error("buf is null", err);
       return 0;
    }

    let buf = unsafe { slice::from_raw_parts_mut(buf as *mut u8, buflen) };
    unsafe { &*reader }.reader.read_at_offset(offset, buf)
        .unwrap_or_else(|e| { fill_error(e, err); 0 })
}
