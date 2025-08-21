use std::ffi::{
    CStr,
    CString,
    c_char
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
                Box::from_raw((*err).message);
            }

            Box::from_raw(err);
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone)]
pub enum CorruptSectionPolicy {
  ERROR,
  DAMN_THE_TORPEDOES
}

impl From<CorruptSectionPolicy> for e01_reader::CorruptSectionPolicy {
    fn from(policy: CorruptSectionPolicy) -> e01_reader::CorruptSectionPolicy {
        match policy {
            CorruptSectionPolicy::ERROR => e01_reader::CorruptSectionPolicy::Error,
            CorruptSectionPolicy::DAMN_THE_TORPEDOES => e01_reader::CorruptSectionPolicy::DamnTheTorpedoes
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone)]
pub enum CorruptChunkPolicy {
  ERROR,
  ZERO,
  RAW_IF_POSSIBLE
}

impl From<CorruptChunkPolicy> for e01_reader::CorruptChunkPolicy {
    fn from(policy: CorruptChunkPolicy) -> e01_reader::CorruptChunkPolicy {
        match policy {
            CorruptChunkPolicy::ERROR => e01_reader::CorruptChunkPolicy::Error,
            CorruptChunkPolicy::ZERO => e01_reader::CorruptChunkPolicy::Zero,
            CorruptChunkPolicy::RAW_IF_POSSIBLE => e01_reader::CorruptChunkPolicy::RawIfPossible
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

#[repr(C)]
pub struct E01Reader {
    reader: e01_reader::E01Reader
}

#[unsafe(no_mangle)]
pub extern "C" fn e01_open(
    segment_paths: *const *const c_char,
    segment_paths_len: usize,
    options: *const E01ReaderOptions,
    err: *mut *mut E01Error
) -> *mut E01Reader
{
    let options = unsafe { (*options).into() };

/*
    let pv = vec![];
    for i in 0..segment_paths_len {
        let p = unsafe { CStr::from_ptr(segment_paths[i]) };
        pv.push(p.into());
    }
    let segment_paths = pv;
*/

    match e01_reader::E01Reader::open(segment_paths, &options) {
        Ok(reader) => Box::into_raw(Box::new(E01Reader { reader })),
        Err(e) => {
            let message = CString::new(e.to_string())
                .expect("impossible")
                .into_raw();
            unsafe {
                *err = Box::into_raw(Box::new(E01Error { message }));
            }
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn e01_open_glob(
    example_segment_path: *const c_char,
    options: *const E01ReaderOptions,
    err: *mut *mut E01Error
) -> *mut E01Reader
{
    let options = unsafe { (*options).into() };
    let example_segment_path = match (unsafe {
        CStr::from_ptr(example_segment_path)
    }).to_str()
    {
        Ok(p) => p,
        Err(e) => {
            let message = CString::new(e.to_string())
                .expect("impossible")
                .into_raw();
            unsafe {
                *err = Box::into_raw(Box::new(E01Error { message }));
            }
            return std::ptr::null_mut();
        }
    };

    match e01_reader::E01Reader::open_glob(example_segment_path, &options) {
        Ok(reader) => Box::into_raw(Box::new(E01Reader { reader })),
        Err(e) => {
            let message = CString::new(e.to_string())
                .expect("impossible")
                .into_raw();
            unsafe {
                *err = Box::into_raw(Box::new(E01Error { message }));
            }
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn e01_close(reader: *mut E01Reader) {
    if !reader.is_null() {
        unsafe {
            Box::from_raw(reader);
        }
    }
}

/*
#[unsafe(no_mangle)]
pub extern "C" fn e01_read(
    reader: *mut E01Reader,
    offset: usize,
    buf: 
) -> usize
{

}
*/

#[unsafe(no_mangle)]
pub extern "C" fn e01_chunk_size(reader: *const E01Reader) -> usize {
    unsafe {
        (&*reader).reader.chunk_size()
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn e01_total_size(reader: *const E01Reader) -> usize {
    unsafe {
        (&*reader).reader.total_size()
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn e01_stored_md5(reader: *const E01Reader) -> *const u8 {
    match unsafe { (&*reader).reader.get_stored_md5() } {
        Some(h) => h.as_ptr(),
        None => std::ptr::null()
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn e01_stored_sha1(reader: *const E01Reader) -> *const u8 {
    match unsafe { (&*reader).reader.get_stored_sha1() } {
        Some(h) => h.as_ptr(),
        None => std::ptr::null()
    }
}
