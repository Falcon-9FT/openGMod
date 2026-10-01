//! Reading at an offset, and little-endian values out of untrusted bytes.

use std::fs::File;
use std::io::{self, BufRead, Read};

/// The largest single file that is read into memory. A size field in a damaged or hostile
/// archive must not make us allocate gigabytes.
pub(crate) const MAX_FILE: u64 = 1 << 31;

pub(crate) fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// Fills `buf` from `offset` in `file` without moving a shared cursor, so that several
/// threads can read from one handle.
pub(crate) fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            match file.seek_read(&mut buf[done..], offset + done as u64) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
                Ok(n) => done += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    compile_error!("positional file reads are only implemented for Unix and Windows");
}

/// Little-endian values out of a byte slice; reading past the end is an error, not a panic.
pub(crate) struct Bytes<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bytes<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Bytes<'a> {
        Bytes { data, pos: 0 }
    }

    pub(crate) fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&end| end <= self.data.len()).ok_or_else(|| invalid("unexpected end of data"))?;
        let bytes = &self.data[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }

    pub(crate) fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub(crate) fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    /// Bytes up to the next NUL, which is consumed but not returned.
    pub(crate) fn cstr(&mut self) -> io::Result<&'a [u8]> {
        let rest = &self.data[self.pos..];
        let len = rest.iter().position(|&c| c == 0).ok_or_else(|| invalid("string without its terminating NUL"))?;
        self.pos += len + 1;
        Ok(&rest[..len])
    }
}

/// Little-endian values out of a stream.
pub(crate) fn read_array<const N: usize>(r: &mut impl Read) -> io::Result<[u8; N]> {
    let mut buf = [0u8; N];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// Bytes up to the next NUL (consumed, not returned), at most `limit` of them.
pub(crate) fn read_cstr(r: &mut impl BufRead, limit: usize) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    r.take(limit as u64 + 1).read_until(0, &mut out)?;
    match out.pop() {
        Some(0) => Ok(out),
        _ if out.len() >= limit => Err(invalid(format!("string longer than {limit} bytes"))),
        _ => Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
    }
}
