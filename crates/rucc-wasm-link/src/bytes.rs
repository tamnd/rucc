//! The byte cursor the readers share, and the LEB128 writers the output needs.
//!
//! Every count, index and size in a wasm file is a LEB128, so every reader in this crate is a loop
//! over this cursor. A read past the end is an error with the offset in it, never a panic, because
//! an input file is something a user gave and a short one is a thing a user can do.

use crate::Error;

/// A position in a byte slice that only moves forward.
#[derive(Debug, Clone)]
pub(crate) struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Cursor { bytes, pos: 0 }
    }

    /// The offset of the next byte from the start of the slice.
    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn short(&self) -> Error {
        Error::new(format!("the input ends at offset {} in the middle of a field", self.pos))
    }

    pub(crate) fn byte(&mut self) -> Result<u8, Error> {
        let byte = *self.bytes.get(self.pos).ok_or_else(|| self.short())?;
        self.pos += 1;
        Ok(byte)
    }

    /// The next `len` bytes.
    pub(crate) fn take(&mut self, len: usize) -> Result<&'a [u8], Error> {
        let end = self.pos.checked_add(len).filter(|&end| end <= self.bytes.len());
        let end = end.ok_or_else(|| self.short())?;
        let bytes = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }

    /// An unsigned LEB128 of at most 32 bits.
    pub(crate) fn u32(&mut self) -> Result<u32, Error> {
        let mut value = 0u64;
        for shift in (0..35).step_by(7) {
            let byte = self.byte()?;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return u32::try_from(value).map_err(|_| {
                    Error::new(format!("a LEB128 at offset {} is too large", self.pos))
                });
            }
        }
        Err(Error::new(format!("a LEB128 at offset {} is longer than 5 bytes", self.pos)))
    }

    /// A signed LEB128 of at most 32 bits.
    pub(crate) fn i32(&mut self) -> Result<i32, Error> {
        let value = self.i64()?;
        i32::try_from(value)
            .map_err(|_| Error::new(format!("a LEB128 at offset {} is too large", self.pos)))
    }

    /// A signed LEB128 of at most 64 bits.
    pub(crate) fn i64(&mut self) -> Result<i64, Error> {
        let mut value = 0i64;
        let mut shift = 0;
        loop {
            let byte = self.byte()?;
            if shift < 64 {
                value |= i64::from(byte & 0x7f) << shift;
            }
            shift += 7;
            if byte & 0x80 == 0 {
                if shift < 64 && byte & 0x40 != 0 {
                    value |= -1i64 << shift;
                }
                return Ok(value);
            }
            if shift >= 70 {
                return Err(Error::new(format!("a LEB128 at offset {} is too long", self.pos)));
            }
        }
    }

    /// A count of entries, checked against the bytes that are left so that a corrupt count cannot
    /// make a reader reserve a vector the size of the address space.
    pub(crate) fn count(&mut self) -> Result<usize, Error> {
        let count = self.u32()? as usize;
        if count > self.bytes.len() - self.pos {
            return Err(Error::new(format!(
                "a count of {count} at offset {} is too large",
                self.pos
            )));
        }
        Ok(count)
    }

    /// A name: a length and that many bytes of UTF-8.
    pub(crate) fn name(&mut self) -> Result<&'a str, Error> {
        let len = self.u32()? as usize;
        let at = self.pos;
        let bytes = self.take(len)?;
        core::str::from_utf8(bytes)
            .map_err(|_| Error::new(format!("the name at offset {at} is not UTF-8")))
    }

    /// The limits of a memory or a table: the flags, the minimum and the maximum when there is one.
    pub(crate) fn limits(&mut self) -> Result<(), Error> {
        let flags = self.byte()?;
        if flags & !0x1 != 0 {
            return Err(Error::new(format!(
                "limits with flags {flags:#x} at offset {} (shared or 64-bit) are not supported",
                self.pos
            )));
        }
        self.u32()?;
        if flags & 0x1 != 0 {
            self.u32()?;
        }
        Ok(())
    }

    /// A constant expression, up to and with its `end`. The ones an object holds are one
    /// constant instruction and `end`, and anything longer is refused.
    pub(crate) fn expr(&mut self) -> Result<&'a [u8], Error> {
        let start = self.pos;
        match self.byte()? {
            0x41 => {
                self.i32()?;
            }
            0x42 => {
                self.i64()?;
            }
            0x43 => {
                self.take(4)?;
            }
            0x44 => {
                self.take(8)?;
            }
            0x23 => {
                self.u32()?;
            }
            0xd2 => {
                self.u32()?;
            }
            op => {
                return Err(Error::new(format!(
                    "a constant expression at offset {start} starts with {op:#04x}"
                )));
            }
        }
        if self.byte()? != 0x0b {
            return Err(Error::new(format!(
                "the constant expression at offset {start} has more than one instruction"
            )));
        }
        Ok(&self.bytes[start..self.pos])
    }
}

/// Appends an unsigned LEB128 at its shortest.
pub(crate) fn uleb(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Appends a signed LEB128 at its shortest.
pub(crate) fn sleb(out: &mut Vec<u8>, mut value: i64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
        if done {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Writes an unsigned LEB128 over the five bytes at `at`, at full width.
pub(crate) fn uleb5(bytes: &mut [u8], at: usize, value: u32) {
    let mut value = value;
    for (i, byte) in bytes[at..at + 5].iter_mut().enumerate() {
        *byte = (value & 0x7f) as u8 | if i < 4 { 0x80 } else { 0 };
        value >>= 7;
    }
}

/// Writes a signed LEB128 over the five bytes at `at`, at full width.
pub(crate) fn sleb5(bytes: &mut [u8], at: usize, value: i32) {
    let mut value = value;
    for (i, byte) in bytes[at..at + 5].iter_mut().enumerate() {
        *byte = (value & 0x7f) as u8 | if i < 4 { 0x80 } else { 0 };
        value >>= 7;
    }
}

/// Appends a name: its length and its bytes.
pub(crate) fn name(out: &mut Vec<u8>, name: &str) {
    uleb(out, name.len() as u64);
    out.extend_from_slice(name.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_written_reads_back() {
        for value in [0u32, 1, 63, 64, 127, 128, 16_383, 16_384, u32::MAX] {
            let mut out = Vec::new();
            uleb(&mut out, u64::from(value));
            assert_eq!(Cursor::new(&out).u32().unwrap(), value);
        }
        for value in [0i32, 1, -1, 63, 64, -64, -65, i32::MIN, i32::MAX] {
            let mut out = Vec::new();
            sleb(&mut out, i64::from(value));
            assert_eq!(Cursor::new(&out).i32().unwrap(), value);
        }
    }

    #[test]
    fn a_padded_field_holds_its_value_in_five_bytes() {
        let mut bytes = [0u8; 5];
        uleb5(&mut bytes, 0, 300);
        assert_eq!(bytes, [0xac, 0x82, 0x80, 0x80, 0x00]);
        assert_eq!(Cursor::new(&bytes).u32().unwrap(), 300);
        sleb5(&mut bytes, 0, -2);
        assert_eq!(bytes, [0xfe, 0xff, 0xff, 0xff, 0x7f]);
        assert_eq!(Cursor::new(&bytes).i32().unwrap(), -2);
    }

    #[test]
    fn a_short_input_is_an_error_and_not_a_panic() {
        assert!(Cursor::new(&[0x80, 0x80]).u32().is_err());
        assert!(Cursor::new(&[0x05, b'a']).name().is_err());
        assert!(Cursor::new(&[0x7f]).count().is_err());
    }
}
