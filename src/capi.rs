use std::{
    ffi::{
        CStr,
        CString,
        c_char
    },
    mem::ManuallyDrop,
    path::Path,
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
  CSP_ERROR,
  CSP_DAMN_THE_TORPEDOES
}

impl From<CorruptSectionPolicy> for e01_reader::CorruptSectionPolicy {
    fn from(policy: CorruptSectionPolicy) -> e01_reader::CorruptSectionPolicy {
        match policy {
            CorruptSectionPolicy::CSP_ERROR => e01_reader::CorruptSectionPolicy::Error,
            CorruptSectionPolicy::CSP_DAMN_THE_TORPEDOES => e01_reader::CorruptSectionPolicy::DamnTheTorpedoes
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone)]
pub enum CorruptChunkPolicy {
  CCP_ERROR,
  CCP_ZERO,
  CCP_RAW_IF_POSSIBLE
}

impl From<CorruptChunkPolicy> for e01_reader::CorruptChunkPolicy {
    fn from(policy: CorruptChunkPolicy) -> e01_reader::CorruptChunkPolicy {
        match policy {
            CorruptChunkPolicy::CCP_ERROR => e01_reader::CorruptChunkPolicy::Error,
            CorruptChunkPolicy::CCP_ZERO => e01_reader::CorruptChunkPolicy::Zero,
            CorruptChunkPolicy::CCP_RAW_IF_POSSIBLE => e01_reader::CorruptChunkPolicy::RawIfPossible
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
    reader: *mut E01Reader,
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
    // Vec should have been shrunk to fit so length == capacity
    let v = unsafe { Vec::from_raw_parts(ptr, len, len) };

    for s in v {
        drop(unsafe { CString::from_raw(s) });
    }
}

fn paths_to_cstring_vec<'a, P, T>(paths: T) -> Result<Vec<CString>, String>
where
    P: AsRef<Path> + 'a,
    T: IntoIterator<Item = &'a P>
{
    paths.into_iter()
        .enumerate()
        .map(|(i, p)|
            p.as_ref()
                .to_str()
                .ok_or_else(|| format!("path {i} is not UTF-8"))
                .and_then(|s| CString::new(s)
                    .or_else(|_| Err(format!("path {i} contains an internal null")))
                )
        )
        .collect::<Result<Vec<_>, _>>()
}

impl E01Handle {
    fn new(reader: E01Reader) -> Result<Self, String> {
        // convert paths into CStrings, which will be dropped on error
        let segment_paths = paths_to_cstring_vec(&reader.segment_paths)?;

        // convert CStrings into *const c_char, which must be freed by
        // calling e01_close on the handle
        let mut segment_paths = segment_paths.into_iter()
            .map(|sp| sp.into_raw() as *const c_char)
            .collect::<Vec<_>>();

        // ensure that capacity == len, so we don't need to store both
        segment_paths.shrink_to_fit();

        let segment_paths = ManuallyDrop::new(segment_paths).as_ptr();

        Ok(
            Self {
                segment_paths,
                segment_paths_count: reader.segment_paths.len(),
                chunk_size: reader.chunk_size,
                chunk_count: reader.chunk_count,
                sector_count: reader.sector_count,
                sector_size: reader.sector_size,
                image_size: reader.image_size,
                stored_md5: reader.stored_md5.map_or(
                    std::ptr::null_mut(),
                    |h| h.as_ptr()
                ),
                stored_sha1: reader.stored_sha1.map_or(
                    std::ptr::null_mut(),
                    |h| h.as_ptr()
                ),
                reader: Box::into_raw(Box::new(reader))
            }
        )
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
    // convert options
    if options.is_null() {
       fill_error("options is null", err);
       return std::ptr::null_mut();
    }

    let options = unsafe { (*options).into() };

    // convert paths
    if segment_paths.is_null() {
       fill_error("segment_paths is null", err);
       return std::ptr::null_mut();
    }

    let sl = unsafe { slice::from_raw_parts(segment_paths, segment_paths_len) };
    let mut segment_paths = Vec::with_capacity(segment_paths_len);

    for (i, p) in sl.iter().enumerate() {
        if p.is_null() {
            fill_error(format!("segment_paths[{i}] is null"), err);
            return std::ptr::null_mut();
        }

        let p = unsafe { CStr::from_ptr(*p) };

        let Ok(sp) = p.to_str() else {
            fill_error(format!("segment_paths[{i}] is not UTF-8"), err);
            return std::ptr::null_mut();
        };

        segment_paths.push(sp);
    }

    // do the open
    match E01Reader::open(segment_paths, &options) {
        Ok(reader) => match E01Handle::new(reader) {
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
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn e01_open_glob(
    example_segment_path: *const c_char,
    options: *const E01ReaderOptions,
    err: *mut *mut E01Error
) -> *mut E01Handle
{
    // convert options
    if options.is_null() {
       fill_error("options is null", err);
       return std::ptr::null_mut();
    }

    let options = unsafe { (*options).into() };

    // convert path
    if example_segment_path.is_null() {
       fill_error("example_segment_path is null", err);
       return std::ptr::null_mut();
    }

    let p = unsafe { CStr::from_ptr(example_segment_path) };

    let Ok(sp) = p.to_str() else {
        fill_error(format!("example_segment_path is not UTF-8"), err);
        return std::ptr::null_mut();
    };

    // do the open
    match E01Reader::open_glob(sp, &options) {
        Ok(reader) => match E01Handle::new(reader) {
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
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn e01_close(reader: *mut E01Handle) {
    if !reader.is_null() {
        let reader = unsafe { Box::from_raw(reader) };
        free_c_str_array(
            reader.segment_paths as *mut *mut c_char,
            reader.segment_paths_count
        );
        drop(unsafe { Box::from_raw(reader.reader) });
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
    unsafe { &*(*reader).reader }.read_at_offset(offset, buf)
        .unwrap_or_else(|e| { fill_error(e, err); 0 })
}

#[cfg(test)]
mod test {

}
