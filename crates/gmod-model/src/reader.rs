//! Little-endian fields at offsets of a file, every read checked against its end.

use std::io;

pub(crate) fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// The bytes of one file and its name, for messages.
#[derive(Clone, Copy)]
pub(crate) struct Reader<'a> {
    pub data: &'a [u8],
    pub what: &'a str,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8], what: &'a str) -> Reader<'a> {
        Reader { data, what }
    }

    pub fn bytes(&self, at: usize, len: usize) -> io::Result<&'a [u8]> {
        at.checked_add(len)
            .and_then(|end| self.data.get(at..end))
            .ok_or_else(|| invalid(format!("{}: {len} bytes at {at} run past the end ({} bytes)", self.what, self.data.len())))
    }

    fn array<const N: usize>(&self, at: usize) -> io::Result<[u8; N]> {
        Ok(self.bytes(at, N)?.try_into().unwrap())
    }

    pub fn u8(&self, at: usize) -> io::Result<u8> {
        Ok(self.array::<1>(at)?[0])
    }

    pub fn u16(&self, at: usize) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.array(at)?))
    }

    pub fn i16(&self, at: usize) -> io::Result<i16> {
        Ok(i16::from_le_bytes(self.array(at)?))
    }

    pub fn i32(&self, at: usize) -> io::Result<i32> {
        Ok(i32::from_le_bytes(self.array(at)?))
    }

    pub fn f32(&self, at: usize) -> io::Result<f32> {
        Ok(f32::from_le_bytes(self.array(at)?))
    }

    pub fn vec3(&self, at: usize) -> io::Result<[f32; 3]> {
        Ok([self.f32(at)?, self.f32(at + 4)?, self.f32(at + 8)?])
    }

    /// A count: not negative.
    pub fn count(&self, at: usize, what: &str) -> io::Result<usize> {
        let n = self.i32(at)?;
        usize::try_from(n).map_err(|_| invalid(format!("{}: {what} count is {n}", self.what)))
    }

    /// An offset from `base`, as the files store them (they may point backwards).
    pub fn offset(&self, base: usize, at: usize) -> io::Result<usize> {
        let relative = i64::from(self.i32(at)?);
        usize::try_from(base as i64 + relative)
            .map_err(|_| invalid(format!("{}: an offset points before the start of the file", self.what)))
    }

    /// Checks that `count` records of `size` bytes fit from `at` on: what bounds every
    /// allocation by the size of the file.
    pub fn table(&self, at: usize, count: usize, size: usize, what: &str) -> io::Result<()> {
        let fits = count.checked_mul(size).and_then(|len| at.checked_add(len)).is_some_and(|end| end <= self.data.len());
        if fits { Ok(()) } else { Err(invalid(format!("{}: {count} {what} at {at} run past the end", self.what))) }
    }

    /// A string ending with NUL (or the file).
    pub fn string(&self, at: usize) -> io::Result<String> {
        let rest = self.data.get(at..).ok_or_else(|| invalid(format!("{}: a name at {at} is past the end", self.what)))?;
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        Ok(String::from_utf8_lossy(&rest[..end]).into_owned())
    }
}
