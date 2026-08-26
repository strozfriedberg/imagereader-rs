//! Minimal fixed new-style NBD server for read-only exports.
//!
//! Compared to the `nbd` crate server, this avoids flushing after every
//! transmission command and reads export data with `read_at_offset` directly.
//!
//! Generic over the [`NbdImage`] trait so the same protocol code can serve any
//! disk-image reader.

use crate::{IoLog, ReadTimer};
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use std::io::{self, ErrorKind, Read, Write};
use std::sync::Arc;

/// Minimal read-only disk-image interface the NBD server needs.
pub trait NbdImage {
    fn size(&self) -> u64;
    fn read_at_offset(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize>;
}

const NBD_REQ_MAGIC: u32 = 0x2560_9513;
const NBD_SIMPLE_REPLY_MAGIC: u32 = 0x6744_6698;
const NBD_IHAVEOPT: &[u8; 8] = b"IHAVEOPT";
const NBD_CLIENT_OPT_MAGIC: u64 = 0x4948_4156_454F_5054;

const NBD_OPT_EXPORT_NAME: u32 = 1;
const NBD_OPT_ABORT: u32 = 2;
const NBD_OPT_LIST: u32 = 3;
const NBD_OPT_INFO: u32 = 6;
const NBD_OPT_GO: u32 = 7;

const NBD_REP_ACK: u32 = 1;
const NBD_REP_SERVER: u32 = 2;
const NBD_REP_INFO: u32 = 3;
const NBD_REP_FLAG_ERROR: u32 = 1 << 31;
const NBD_REP_ERR_UNSUP: u32 = 1 | NBD_REP_FLAG_ERROR;

const NBD_INFO_EXPORT: u16 = 0;

const NBD_FLAG_FIXED_NEWSTYLE: u16 = 1 << 0;
const NBD_FLAG_NO_ZEROES: u16 = 1 << 1;
const NBD_FLAG_C_FIXED_NEWSTYLE: u32 = NBD_FLAG_FIXED_NEWSTYLE as u32;
const NBD_FLAG_C_NO_ZEROES: u32 = NBD_FLAG_NO_ZEROES as u32;

const NBD_FLAG_HAS_FLAGS: u16 = 1 << 0;
const NBD_FLAG_READ_ONLY: u16 = 1 << 1;

const NBD_CMD_READ: u16 = 0;
const NBD_CMD_WRITE: u16 = 1;
const NBD_CMD_DISC: u16 = 2;
const NBD_CMD_FLUSH: u16 = 3;

const READ_BUF_SIZE: usize = 1024 * 1024;
const MAX_READ_LENGTH: u32 = 32 * 1024 * 1024;

fn read_request_error(offset: u64, length: u32, export_size: u64) -> Option<u32> {
    if length > MAX_READ_LENGTH {
        return Some(22);
    }
    let length = length as u64;
    if offset > export_size || length > export_size.saturating_sub(offset) {
        return Some(22);
    }
    None
}

fn io_err(msg: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, msg)
}

fn write_option_reply(
    stream: &mut impl Write,
    clopt: u32,
    rtype: u32,
    data: &[u8],
) -> io::Result<()> {
    let mut reply = Vec::with_capacity(20 + data.len());
    reply.write_u64::<BigEndian>(0x0003_e889_0455_65a9)?;
    reply.write_u32::<BigEndian>(clopt)?;
    reply.write_u32::<BigEndian>(rtype)?;
    reply.write_u32::<BigEndian>(data.len() as u32)?;
    reply.write_all(data)?;
    stream.write_all(&reply)?;
    stream.flush()
}

// One write call per reply header: on a raw socket with TCP_NODELAY, each
// small write is a syscall and potentially its own packet.
fn write_simple_reply(stream: &mut impl Write, error: u32, handle: u64) -> io::Result<()> {
    let mut header = [0u8; 16];
    header[..4].copy_from_slice(&NBD_SIMPLE_REPLY_MAGIC.to_be_bytes());
    header[4..8].copy_from_slice(&error.to_be_bytes());
    header[8..].copy_from_slice(&handle.to_be_bytes());
    stream.write_all(&header)
}

fn write_simple_error(stream: &mut impl Write, err: io::Error, handle: u64) -> io::Result<()> {
    // Walk the error source chain looking for an OS error code.  Both adapters
    // wrap format-layer errors with io::Error::other(), which carries no OS code,
    // so the fallback EIO (5) fires for most format-level failures.
    let code = err
        .raw_os_error()
        .and_then(|c| u32::try_from(c).ok())
        .filter(|c| *c != 0)
        .or_else(|| {
            use std::error::Error as _;
            let mut src = err.source();
            while let Some(e) = src {
                if let Some(io_e) = e.downcast_ref::<io::Error>()
                    && let Some(c) = io_e
                        .raw_os_error()
                        .and_then(|c| u32::try_from(c).ok())
                        .filter(|&c| c != 0)
                {
                    return Some(c);
                }
                src = e.source();
            }
            None
        })
        .unwrap_or(5); // EIO
    write_simple_reply(stream, code, handle)
}

fn finish_export(stream: &mut impl Write, export_size: u64, client_flags: u32) -> io::Result<()> {
    stream.write_u64::<BigEndian>(export_size)?;
    stream.write_u16::<BigEndian>(NBD_FLAG_HAS_FLAGS | NBD_FLAG_READ_ONLY)?;
    if client_flags & NBD_FLAG_C_NO_ZEROES == 0 {
        stream.write_all(&[0; 124])?;
    }
    stream.flush()
}

fn transmission_flags() -> u16 {
    NBD_FLAG_HAS_FLAGS | NBD_FLAG_READ_ONLY
}

/// `NBD_REP_INFO` / `NBD_INFO_EXPORT` reply (required before `NBD_REP_ACK` for GO/INFO).
fn write_info_export(stream: &mut impl Write, clopt: u32, export_size: u64) -> io::Result<()> {
    let mut data = Vec::with_capacity(12);
    data.write_u16::<BigEndian>(NBD_INFO_EXPORT)?;
    data.write_u64::<BigEndian>(export_size)?;
    data.write_u16::<BigEndian>(transmission_flags())?;
    write_option_reply(stream, clopt, NBD_REP_INFO, &data)
}

/// Reply to `NBD_OPT_GO` / `NBD_OPT_INFO`: INFO_EXPORT then ACK. GO enters transmission.
fn reply_go_or_info(stream: &mut impl Write, clopt: u32, export_size: u64) -> io::Result<bool> {
    write_info_export(stream, clopt, export_size)?;
    write_option_reply(stream, clopt, NBD_REP_ACK, b"")?;
    Ok(clopt == NBD_OPT_GO)
}

pub fn handshake(stream: &mut (impl Read + Write), export_size: u64) -> io::Result<()> {
    let hs_flags = NBD_FLAG_FIXED_NEWSTYLE | NBD_FLAG_NO_ZEROES;

    stream.write_all(b"NBDMAGIC")?;
    stream.write_all(NBD_IHAVEOPT)?;
    stream.write_u16::<BigEndian>(hs_flags)?;
    stream.flush()?;

    let client_flags = stream.read_u32::<BigEndian>()?;
    if client_flags != NBD_FLAG_C_FIXED_NEWSTYLE
        && client_flags != (NBD_FLAG_C_FIXED_NEWSTYLE | NBD_FLAG_C_NO_ZEROES)
    {
        return Err(io_err("invalid client flags"));
    }

    loop {
        if stream.read_u64::<BigEndian>()? != NBD_CLIENT_OPT_MAGIC {
            return Err(io_err("invalid client option magic"));
        }

        let clopt = stream.read_u32::<BigEndian>()?;
        let optlen = stream.read_u32::<BigEndian>()?;
        if optlen > 100_000 {
            return Err(io_err("suspicious option length"));
        }

        let mut opt = vec![0; optlen as usize];
        stream.read_exact(&mut opt)?;

        match clopt {
            NBD_OPT_EXPORT_NAME => {
                finish_export(stream, export_size, client_flags)?;
                return Ok(());
            }
            NBD_OPT_ABORT => {
                write_option_reply(stream, clopt, NBD_REP_ACK, b"")?;
                return Err(io_err("client abort"));
            }
            NBD_OPT_LIST => {
                if optlen != 0 {
                    return Err(io_err("NBD_OPT_LIST with content"));
                }
                write_option_reply(
                    stream,
                    clopt,
                    NBD_REP_SERVER,
                    b"\x00\x00\x00\x0ddiskimage-nbd",
                )?;
                write_option_reply(stream, clopt, NBD_REP_ACK, b"")?;
            }
            NBD_OPT_GO | NBD_OPT_INFO => {
                if reply_go_or_info(stream, clopt, export_size)? {
                    return Ok(());
                }
            }
            // STARTTLS/STRUCTURED_REPLY/EXTENDED_HEADERS and any unknown option
            // (payload already consumed above) get NBD_REP_ERR_UNSUP so
            // negotiation can continue, per the fixed-newstyle spec.
            _ => {
                write_option_reply(stream, clopt, NBD_REP_ERR_UNSUP, b"")?;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)] // 8 distinct protocol-framing params; a struct would obscure the NBD wire layout
fn serve_read(
    stream: &mut impl Write,
    reader: &mut impl NbdImage,
    export_size: u64,
    offset: u64,
    length: u32,
    handle: u64,
    buf: &mut [u8],
    io_log: Option<&Arc<IoLog>>,
) -> io::Result<()> {
    let req_len = length;
    let timer = io_log.map(|_| ReadTimer::start());
    let log_read = |io_log: Option<&Arc<IoLog>>, timer: &Option<ReadTimer>| {
        if let Some(log) = io_log {
            let dur_us = timer.as_ref().map(ReadTimer::elapsed_us).unwrap_or(0);
            log.log_nbd_read(offset, req_len, dur_us);
        }
    };
    if let Some(error) = read_request_error(offset, length, export_size) {
        write_simple_reply(stream, error, handle)?;
        log_read(io_log, &timer);
        return Ok(());
    }

    let length = length as u64;
    let mut remaining = length as usize;
    let mut pos = offset;
    let mut replied = false;

    while remaining > 0 {
        let want = remaining.min(buf.len());
        match reader.read_at_offset(pos, &mut buf[..want]) {
            Ok(0) => {
                log_read(io_log, &timer);
                let err = io_err("unexpected EOF while serving read");
                if replied {
                    return Err(err);
                }
                write_simple_error(stream, err, handle)?;
                return Ok(());
            }
            Ok(n) => {
                if !replied {
                    write_simple_reply(stream, 0, handle)?;
                    replied = true;
                }
                stream.write_all(&buf[..n])?;
                remaining -= n;
                pos += n as u64;
            }
            Err(e) => {
                log_read(io_log, &timer);
                if replied {
                    return Err(e);
                }
                write_simple_error(stream, e, handle)?;
                return Ok(());
            }
        }
    }

    if length == 0 {
        write_simple_reply(stream, 0, handle)?;
    }

    log_read(io_log, &timer);

    Ok(())
}

pub fn transmission(
    stream: &mut (impl Read + Write),
    reader: &mut impl NbdImage,
    export_size: u64,
    io_log: Option<&Arc<IoLog>>,
) -> io::Result<()> {
    let mut buf = vec![0; READ_BUF_SIZE];

    loop {
        // One read call for the whole 28-byte request header, not one per field.
        let mut header = [0u8; 28];
        stream.read_exact(&mut header)?;
        let mut fields = &header[..];
        if fields.read_u32::<BigEndian>()? != NBD_REQ_MAGIC {
            return Err(io_err("invalid request magic"));
        }
        let _flags = fields.read_u16::<BigEndian>()?;
        let typ = fields.read_u16::<BigEndian>()?;
        let handle = fields.read_u64::<BigEndian>()?;
        let offset = fields.read_u64::<BigEndian>()?;
        let length = fields.read_u32::<BigEndian>()?;

        match typ {
            NBD_CMD_READ => {
                // After a successful simple reply header, payload bytes may follow; a
                // second reply would desync the client. Close the session on error.
                serve_read(
                    stream,
                    reader,
                    export_size,
                    offset,
                    length,
                    handle,
                    &mut buf,
                    io_log,
                )?;
            }
            NBD_CMD_DISC => return Ok(()),
            NBD_CMD_FLUSH => {
                write_simple_reply(stream, 0, handle)?;
            }
            NBD_CMD_WRITE => {
                // The write payload must be drained even though the export is
                // read-only; leaving it in the stream would desync request framing.
                drain(stream, length as u64, &mut buf)?;
                write_simple_reply(stream, 1, handle)?; // EPERM
            }
            _ => write_simple_reply(stream, 38, handle)?, // ENOSYS
        }
    }
}

/// Read and discard `remaining` payload bytes from the stream.
fn drain(stream: &mut impl Read, mut remaining: u64, buf: &mut [u8]) -> io::Result<()> {
    while remaining > 0 {
        let want = remaining.min(buf.len() as u64) as usize;
        stream.read_exact(&mut buf[..want])?;
        remaining -= want as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
    use std::io::{Read, Write};
    #[cfg(unix)]
    use std::os::unix::net::UnixStream;

    fn read_client_flags(client: &mut impl Read) {
        let mut ihaveopt = [0u8; 8];
        client.read_exact(&mut ihaveopt).unwrap();
        assert_eq!(&ihaveopt, NBD_IHAVEOPT);
        let _hs_flags = client.read_u16::<BigEndian>().unwrap();
    }

    fn send_export_name(client: &mut impl Write) {
        client
            .write_u32::<BigEndian>(NBD_FLAG_C_FIXED_NEWSTYLE | NBD_FLAG_C_NO_ZEROES)
            .unwrap();
        client.write_u64::<BigEndian>(NBD_CLIENT_OPT_MAGIC).unwrap();
        client.write_u32::<BigEndian>(NBD_OPT_EXPORT_NAME).unwrap();
        client.write_u32::<BigEndian>(0).unwrap();
    }

    fn send_go(client: &mut impl Write, export: &str) {
        client
            .write_u32::<BigEndian>(NBD_FLAG_C_FIXED_NEWSTYLE | NBD_FLAG_C_NO_ZEROES)
            .unwrap();
        send_go_option(client, export);
    }

    /// GO option without the leading client-flags word (for use after the first option).
    fn send_go_option(client: &mut impl Write, export: &str) {
        client.write_u64::<BigEndian>(NBD_CLIENT_OPT_MAGIC).unwrap();
        client.write_u32::<BigEndian>(NBD_OPT_GO).unwrap();
        let mut payload = Vec::new();
        payload.write_u32::<BigEndian>(export.len() as u32).unwrap();
        payload.write_all(export.as_bytes()).unwrap();
        payload.write_u16::<BigEndian>(0).unwrap();
        client.write_u32::<BigEndian>(payload.len() as u32).unwrap();
        client.write_all(&payload).unwrap();
    }

    fn read_reply_magic(client: &mut impl Read) {
        assert_eq!(
            client.read_u64::<BigEndian>().unwrap(),
            0x0003_e889_0455_65a9
        );
    }

    fn read_info_export(client: &mut impl Read, export_size: u64) {
        read_reply_magic(client);
        let _clopt = client.read_u32::<BigEndian>().unwrap();
        assert_eq!(client.read_u32::<BigEndian>().unwrap(), NBD_REP_INFO);
        assert_eq!(client.read_u32::<BigEndian>().unwrap(), 12);
        assert_eq!(client.read_u16::<BigEndian>().unwrap(), NBD_INFO_EXPORT);
        assert_eq!(client.read_u64::<BigEndian>().unwrap(), export_size);
        assert_eq!(
            client.read_u16::<BigEndian>().unwrap(),
            NBD_FLAG_HAS_FLAGS | NBD_FLAG_READ_ONLY
        );
    }

    fn read_go_ack(client: &mut impl Read) {
        read_reply_magic(client);
        let _clopt = client.read_u32::<BigEndian>().unwrap();
        assert_eq!(client.read_u32::<BigEndian>().unwrap(), NBD_REP_ACK);
        assert_eq!(client.read_u32::<BigEndian>().unwrap(), 0);
    }

    fn read_export_header(client: &mut impl Read, export_size: u64) {
        assert_eq!(client.read_u64::<BigEndian>().unwrap(), export_size);
        let flags = client.read_u16::<BigEndian>().unwrap();
        assert_eq!(flags, NBD_FLAG_HAS_FLAGS | NBD_FLAG_READ_ONLY);
    }

    /// In-memory `NbdImage` for protocol tests: serves bytes from a `Vec<u8>`.
    struct MemImage(Vec<u8>);

    impl NbdImage for MemImage {
        fn size(&self) -> u64 {
            self.0.len() as u64
        }

        fn read_at_offset(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
            let off = offset as usize;
            let end = (off + buf.len()).min(self.0.len());
            let n = end.saturating_sub(off);
            buf[..n].copy_from_slice(&self.0[off..off + n]);
            Ok(n)
        }
    }

    #[test]
    #[cfg(unix)]
    fn export_name_handshake_returns_export_size() {
        let (mut client, mut server_io) = UnixStream::pair().unwrap();
        let export_size = 4096;

        let handle = std::thread::spawn(move || handshake(&mut server_io, export_size));

        let mut magic = [0u8; 8];
        client.read_exact(&mut magic).unwrap();
        assert_eq!(&magic, b"NBDMAGIC");
        read_client_flags(&mut client);
        send_export_name(&mut client);
        read_export_header(&mut client, export_size);

        handle.join().unwrap().unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn go_handshake_returns_export_size() {
        let (mut client, mut server_io) = UnixStream::pair().unwrap();
        let export_size = 8192;

        let handle = std::thread::spawn(move || handshake(&mut server_io, export_size));

        let mut magic = [0u8; 8];
        client.read_exact(&mut magic).unwrap();
        assert_eq!(&magic, b"NBDMAGIC");
        read_client_flags(&mut client);
        send_go(&mut client, "vmdk-nbd");
        read_info_export(&mut client, export_size);
        read_go_ack(&mut client);

        handle.join().unwrap().unwrap();
    }

    #[test]
    fn oversized_read_is_rejected_with_einval() {
        assert_eq!(read_request_error(0, MAX_READ_LENGTH + 1, 1024), Some(22));
    }

    /// Full session over a socketpair: GO handshake, then a read whose payload
    /// matches the backing bytes. This is the reliable in-process gate for Phase 1.
    #[test]
    #[cfg(unix)]
    fn full_session_serves_read_payload() {
        let data: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let export_size = data.len() as u64;
        let (mut client, mut server_io) = UnixStream::pair().unwrap();

        let server = std::thread::spawn(move || {
            handshake(&mut server_io, export_size)?;
            let mut img = MemImage(data);
            transmission(&mut server_io, &mut img, export_size, None)
        });

        // Drive the handshake.
        let mut magic = [0u8; 8];
        client.read_exact(&mut magic).unwrap();
        assert_eq!(&magic, b"NBDMAGIC");
        read_client_flags(&mut client);
        send_go(&mut client, "vmdk-nbd");
        read_info_export(&mut client, export_size);
        read_go_ack(&mut client);

        // NBD_CMD_READ: 512 bytes at offset 1024.
        let (off, len) = (1024u64, 512u32);
        client.write_u32::<BigEndian>(NBD_REQ_MAGIC).unwrap();
        client.write_u16::<BigEndian>(0).unwrap(); // flags
        client.write_u16::<BigEndian>(NBD_CMD_READ).unwrap();
        client.write_u64::<BigEndian>(0xABCD).unwrap(); // handle
        client.write_u64::<BigEndian>(off).unwrap();
        client.write_u32::<BigEndian>(len).unwrap();
        client.flush().unwrap();

        // Simple reply header.
        assert_eq!(
            client.read_u32::<BigEndian>().unwrap(),
            NBD_SIMPLE_REPLY_MAGIC
        );
        assert_eq!(client.read_u32::<BigEndian>().unwrap(), 0); // no error
        assert_eq!(client.read_u64::<BigEndian>().unwrap(), 0xABCD); // handle echoed

        let mut payload = vec![0u8; len as usize];
        client.read_exact(&mut payload).unwrap();
        let expected: Vec<u8> = (off..off + len as u64).map(|i| (i % 251) as u8).collect();
        assert_eq!(payload, expected);

        // Disconnect cleanly.
        client.write_u32::<BigEndian>(NBD_REQ_MAGIC).unwrap();
        client.write_u16::<BigEndian>(0).unwrap();
        client.write_u16::<BigEndian>(NBD_CMD_DISC).unwrap();
        client.write_u64::<BigEndian>(0).unwrap();
        client.write_u64::<BigEndian>(0).unwrap();
        client.write_u32::<BigEndian>(0).unwrap();
        client.flush().unwrap();

        server.join().unwrap().unwrap();
    }

    fn drive_go_handshake(client: &mut (impl Read + Write), export_size: u64) {
        let mut magic = [0u8; 8];
        client.read_exact(&mut magic).unwrap();
        assert_eq!(&magic, b"NBDMAGIC");
        read_client_flags(client);
        send_go(client, "diskimage-nbd");
        read_info_export(client, export_size);
        read_go_ack(client);
    }

    fn send_command(client: &mut impl Write, typ: u16, handle: u64, offset: u64, length: u32) {
        client.write_u32::<BigEndian>(NBD_REQ_MAGIC).unwrap();
        client.write_u16::<BigEndian>(0).unwrap();
        client.write_u16::<BigEndian>(typ).unwrap();
        client.write_u64::<BigEndian>(handle).unwrap();
        client.write_u64::<BigEndian>(offset).unwrap();
        client.write_u32::<BigEndian>(length).unwrap();
    }

    fn read_simple_reply_header(client: &mut impl Read, expect_handle: u64) -> u32 {
        assert_eq!(
            client.read_u32::<BigEndian>().unwrap(),
            NBD_SIMPLE_REPLY_MAGIC
        );
        let error = client.read_u32::<BigEndian>().unwrap();
        assert_eq!(client.read_u64::<BigEndian>().unwrap(), expect_handle);
        error
    }

    struct FailImage;

    impl NbdImage for FailImage {
        fn size(&self) -> u64 {
            4096
        }

        fn read_at_offset(&mut self, _offset: u64, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("backing store exploded"))
        }
    }

    /// Reads that fail in the backing store must still appear in the io-log;
    /// the failing reads are exactly the ones workload analysis needs.
    #[test]
    fn failed_reads_are_still_io_logged() {
        let path = std::env::temp_dir().join(format!(
            "diskimage-nbd-test-failed-read-{}.jsonl",
            std::process::id()
        ));
        let log = IoLog::open(&path).unwrap();
        log.begin_serving().unwrap();

        let mut out = Vec::new();
        let mut buf = vec![0u8; 1024];
        serve_read(
            &mut out,
            &mut FailImage,
            4096,
            0,
            512,
            7,
            &mut buf,
            Some(&log),
        )
        .unwrap();
        log.log_summary(); // flushes

        let content = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(
            content.contains(r#""kind":"nbd_read""#),
            "failed read missing from io log: {content}"
        );
        assert!(content.contains(r#""nbd_reads":1"#), "summary: {content}");
    }

    /// A write command carries a payload the server must drain even though the
    /// export is read-only; otherwise the payload bytes desync the request stream.
    #[test]
    #[cfg(unix)]
    fn write_command_is_rejected_and_session_survives() {
        let data: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let export_size = data.len() as u64;
        let (mut client, mut server_io) = UnixStream::pair().unwrap();

        let server = std::thread::spawn(move || {
            handshake(&mut server_io, export_size)?;
            let mut img = MemImage(data);
            transmission(&mut server_io, &mut img, export_size, None)
        });

        drive_go_handshake(&mut client, export_size);

        // NBD_CMD_WRITE (type 1) with a 512-byte payload.
        send_command(&mut client, 1, 0xBEEF, 0, 512);
        client.write_all(&[0xAA; 512]).unwrap();
        client.flush().unwrap();
        let error = read_simple_reply_header(&mut client, 0xBEEF);
        assert_eq!(error, 1); // EPERM: read-only export

        // The session must still serve a valid read afterwards.
        send_command(&mut client, NBD_CMD_READ, 0xCAFE, 1024, 512);
        client.flush().unwrap();
        assert_eq!(read_simple_reply_header(&mut client, 0xCAFE), 0);
        let mut payload = vec![0u8; 512];
        client.read_exact(&mut payload).unwrap();
        let expected: Vec<u8> = (1024u64..1536).map(|i| (i % 251) as u8).collect();
        assert_eq!(payload, expected);

        send_command(&mut client, NBD_CMD_DISC, 0, 0, 0);
        client.flush().unwrap();
        server.join().unwrap().unwrap();
    }

    /// Counts `read`/`write` calls; on a real socket each call is one syscall
    /// (and with TCP_NODELAY, each small write is potentially one packet).
    struct ScriptedStream {
        input: std::io::Cursor<Vec<u8>>,
        reads: usize,
        write_lens: Vec<usize>,
        output: Vec<u8>,
    }

    impl ScriptedStream {
        fn new(input: Vec<u8>) -> Self {
            Self {
                input: std::io::Cursor::new(input),
                reads: 0,
                write_lens: Vec::new(),
                output: Vec::new(),
            }
        }
    }

    impl Read for ScriptedStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            self.input.read(buf)
        }
    }

    impl Write for ScriptedStream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.write_lens.push(buf.len());
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The 28-byte request header must be read with one call and the 16-byte
    /// simple-reply header written with one call — not per-field syscalls.
    #[test]
    fn transmission_uses_single_reads_and_writes_per_request() {
        let data: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();

        // Script: READ 512@1024, then DISC.
        let mut input = Vec::new();
        for (typ, offset, length) in [(NBD_CMD_READ, 1024u64, 512u32), (NBD_CMD_DISC, 0, 0)] {
            input.write_u32::<BigEndian>(NBD_REQ_MAGIC).unwrap();
            input.write_u16::<BigEndian>(0).unwrap();
            input.write_u16::<BigEndian>(typ).unwrap();
            input.write_u64::<BigEndian>(0x1234).unwrap();
            input.write_u64::<BigEndian>(offset).unwrap();
            input.write_u32::<BigEndian>(length).unwrap();
        }

        let mut stream = ScriptedStream::new(input);
        let mut img = MemImage(data);
        transmission(&mut stream, &mut img, 4096, None).unwrap();

        assert_eq!(
            stream.write_lens,
            vec![16, 512],
            "reply must be one 16-byte header write plus one payload write"
        );
        assert_eq!(
            stream.reads, 2,
            "each request header must be read with a single call"
        );
    }

    /// Unknown options must get NBD_REP_ERR_UNSUP and negotiation must continue,
    /// per the fixed-newstyle spec — not a dropped connection.
    #[test]
    #[cfg(unix)]
    fn unknown_option_gets_unsup_reply_and_negotiation_continues() {
        let (mut client, mut server_io) = UnixStream::pair().unwrap();
        let export_size = 4096;

        let handle = std::thread::spawn(move || handshake(&mut server_io, export_size));

        let mut magic = [0u8; 8];
        client.read_exact(&mut magic).unwrap();
        assert_eq!(&magic, b"NBDMAGIC");
        read_client_flags(&mut client);

        // NBD_OPT_LIST_META_CONTEXT (9), which this server does not implement.
        client
            .write_u32::<BigEndian>(NBD_FLAG_C_FIXED_NEWSTYLE | NBD_FLAG_C_NO_ZEROES)
            .unwrap();
        client.write_u64::<BigEndian>(NBD_CLIENT_OPT_MAGIC).unwrap();
        client.write_u32::<BigEndian>(9).unwrap();
        client.write_u32::<BigEndian>(0).unwrap();

        read_reply_magic(&mut client);
        assert_eq!(client.read_u32::<BigEndian>().unwrap(), 9);
        assert_eq!(client.read_u32::<BigEndian>().unwrap(), NBD_REP_ERR_UNSUP);
        assert_eq!(client.read_u32::<BigEndian>().unwrap(), 0);

        // Negotiation continues: GO still completes the handshake.
        send_go_option(&mut client, "diskimage-nbd");
        read_info_export(&mut client, export_size);
        read_go_ack(&mut client);

        handle.join().unwrap().unwrap();
    }
}
