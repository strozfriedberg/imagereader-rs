#[cfg(feature="capi")]

use std::{
    ffi::{
        CStr,
        CString,
        OsStr,
        c_char
    },
    os::unix::ffi::OsStrExt,
    slice
};

use crate::e01_reader;

#[repr(C)]
pub struct E01Error {
    message: *mut c_char
}

#[unsafe(no_mangle)]
pub extern "C" fn e01_free_error(err: *mut E01Error) {
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

pub struct E01Reader {
    reader: e01_reader::E01Reader
}

fn fill_error<E: ToString>(e: E, err: *mut *mut E01Error) {
    let message = CString::new(e.to_string())
        .expect("impossible")
        .into_raw();
    unsafe { *err = Box::into_raw(Box::new(E01Error { message })); }
}

fn fill_handle(
    r: Result<e01_reader::E01Reader, e01_reader::OpenError>,
    err: *mut *mut E01Error
) -> *mut E01Reader
{
    match r {
        Ok(reader) => Box::into_raw(Box::new(E01Reader { reader })),
        Err(e) => {
            fill_error(e, err);
            std::ptr::null_mut()
        }
    }
}

fn c_str_to_osstr(p: *const c_char) -> &'static OsStr {
    OsStr::from_bytes(unsafe { CStr::from_ptr(p) }.to_bytes())
}

#[unsafe(no_mangle)]
pub extern "C" fn e01_open(
    segment_paths: *const *const c_char,
    segment_paths_len: usize,
    options: *const E01ReaderOptions,
    err: *mut *mut E01Error
) -> *mut E01Reader
{
    let sl = unsafe { slice::from_raw_parts(segment_paths, segment_paths_len) };
    let segment_paths = sl.iter()
        .map(|p| c_str_to_osstr(*p))
        .collect::<Vec<_>>();

    let options = unsafe { (*options).into() };
    fill_handle(e01_reader::E01Reader::open(segment_paths, &options), err)
}

#[unsafe(no_mangle)]
pub extern "C" fn e01_open_glob(
    example_segment_path: *const c_char,
    options: *const E01ReaderOptions,
    err: *mut *mut E01Error
) -> *mut E01Reader
{
    let example_segment_path = c_str_to_osstr(example_segment_path);
    let options = unsafe { (*options).into() };
    fill_handle(
        e01_reader::E01Reader::open_glob(example_segment_path, &options),
        err
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn e01_close(reader: *mut E01Reader) {
    if !reader.is_null() {
        drop(unsafe { Box::from_raw(reader) });
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn e01_read(
    reader: *mut E01Reader,
    offset: usize,
    buf: *mut c_char,
    buflen: usize,
    err: *mut *mut E01Error
) -> usize
{
    let buf = unsafe { slice::from_raw_parts_mut(buf as *mut u8, buflen) };
    match unsafe { &*reader }.reader.read_at_offset(offset, buf) {
        Ok(count) => count,
        Err(e) => {
            fill_error(e, err);
            0
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn e01_chunk_size(reader: *const E01Reader) -> usize {
    unsafe { &*reader }.reader.chunk_size()
}

#[unsafe(no_mangle)]
pub extern "C" fn e01_total_size(reader: *const E01Reader) -> usize {
    unsafe { &*reader }.reader.total_size()
}

#[unsafe(no_mangle)]
pub extern "C" fn e01_stored_md5(reader: *const E01Reader) -> *const u8 {
    match unsafe { &*reader }.reader.get_stored_md5() {
        Some(h) => h.as_ptr(),
        None => std::ptr::null()
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn e01_stored_sha1(reader: *const E01Reader) -> *const u8 {
    match unsafe { &*reader }.reader.get_stored_sha1() {
        Some(h) => h.as_ptr(),
        None => std::ptr::null()
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn e01_segment_count(
    reader: *const E01Reader
) -> usize
{
    unsafe { &*reader }.reader.segment_count()
}

#[unsafe(no_mangle)]
pub extern "C" fn e01_segment_path(
    reader: *const E01Reader,
    usize: index
) -> *const c_char
{
    match unsafe { &*reader }.reader.segment_path(index) {
        Some(p) => p.as_os_str().as_bytes().as_ptr() as *const c_char,
        None => std::ptr::null()
    }
}
