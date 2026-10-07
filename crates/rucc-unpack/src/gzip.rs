//! The gzip member, which is what a sysroot archive is compressed with.
//!
//! Design: `spec/cross-compile/13-distribution.md` section 13.8 and the WASI notes of WA5
//! (tamnd/rucc#2867). A sysroot is a `.tar.gz`, and rucc running as a wasm module cannot start
//! `tar` to unpack one. A gzip member is a short header, a deflate stream, and the checksum and the
//! length of what the stream gives. The deflate stream is the part that [`crate::inflate()`] already
//! reads, so this module reads the header and checks the two numbers at the end.
//!
//! A file can hold more than one member, one after the other, and the result is the members joined.
//! `gzip` writes one member, and `cat a.gz b.gz` makes two. Both are read.

use std::fmt;

use crate::inflate::{InflateError, inflate_into};
use crate::zip::crc32;

/// The two bytes every gzip member starts with.
const MAGIC: [u8; 2] = [0x1f, 0x8b];
/// Deflate, which is the only method the format has.
const DEFLATE: u8 = 8;
/// The header flags, from RFC 1952 section 2.3.1.
const FHCRC: u8 = 2;
const FEXTRA: u8 = 4;
const FNAME: u8 = 8;
const FCOMMENT: u8 = 16;

/// Why a gzip file could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GzipError {
    /// The file does not start with the gzip magic, or a member says a method that is not deflate.
    NotGzip,
    /// The header or the trailer runs off the end of the file.
    Truncated,
    /// The deflate stream did not decompress.
    Stream(InflateError),
    /// The stream gave bytes with another checksum or another length than the trailer says.
    Corrupt,
}

impl fmt::Display for GzipError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GzipError::NotGzip => write!(f, "not a gzip file"),
            GzipError::Truncated => write!(f, "the gzip file ends in the middle of a member"),
            GzipError::Stream(why) => write!(f, "{why}"),
            GzipError::Corrupt => {
                write!(f, "the gzip data has another checksum or length than its trailer says")
            }
        }
    }
}

impl std::error::Error for GzipError {}

/// Decompresses a gzip file, every member of it.
///
/// # Errors
///
/// [`GzipError`] for a file that is not gzip, that is cut short, or whose data does not match its
/// trailer.
pub fn gunzip(bytes: &[u8]) -> Result<Vec<u8>, GzipError> {
    let mut out = Vec::new();
    let mut at = 0;
    loop {
        at = member(bytes, at, &mut out)?;
        if at == bytes.len() {
            return Ok(out);
        }
    }
}

/// Reads the member at `at` onto the end of `out` and gives the offset after it.
fn member(bytes: &[u8], mut at: usize, out: &mut Vec<u8>) -> Result<usize, GzipError> {
    let header = bytes.get(at..at + 10).ok_or(GzipError::Truncated)?;
    if header[..2] != MAGIC || header[2] != DEFLATE {
        return Err(GzipError::NotGzip);
    }
    let flags = header[3];
    at += 10;
    if flags & FEXTRA != 0 {
        let len = bytes.get(at..at + 2).ok_or(GzipError::Truncated)?;
        at += 2 + usize::from(u16::from_le_bytes([len[0], len[1]]));
    }
    // The file name and the comment end with a zero byte.
    for flag in [FNAME, FCOMMENT] {
        if flags & flag != 0 {
            let rest = bytes.get(at..).ok_or(GzipError::Truncated)?;
            at += rest.iter().position(|&b| b == 0).ok_or(GzipError::Truncated)? + 1;
        }
    }
    if flags & FHCRC != 0 {
        at += 2;
    }
    let stream = bytes.get(at..).ok_or(GzipError::Truncated)?;
    // The stream goes into a buffer of its own, because a copy in it must not reach back into the
    // members before it.
    let mut data = Vec::new();
    at += inflate_into(stream, &mut data).map_err(GzipError::Stream)?;
    let trailer = bytes.get(at..at + 8).ok_or(GzipError::Truncated)?;
    let crc = u32::from_le_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    let len = u32::from_le_bytes([trailer[4], trailer[5], trailer[6], trailer[7]]);
    // The length is the length modulo 2^32, as RFC 1952 says.
    if crc32(&data) != crc || data.len() as u32 != len {
        return Err(GzipError::Corrupt);
    }
    out.extend_from_slice(&data);
    Ok(at + 8)
}

#[cfg(test)]
mod tests {
    use super::{GzipError, gunzip};

    /// `printf 'hello\n' | gzip -n`, a member with no name and no time in it.
    const HELLO: &[u8] = &[
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xcb, 0x48, 0xcd, 0xc9, 0xc9,
        0xe7, 0x02, 0x00, 0x20, 0x30, 0x3a, 0x36, 0x06, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn a_member_from_gzip_decompresses() {
        assert_eq!(gunzip(HELLO).as_deref(), Ok(&b"hello\n"[..]));
    }

    #[test]
    fn two_members_join_and_a_name_in_the_header_is_skipped() {
        let mut named = HELLO.to_vec();
        named[3] = 8;
        named.splice(10..10, b"hello.txt\0".iter().copied());
        let mut two = named.clone();
        two.extend_from_slice(HELLO);
        assert_eq!(gunzip(&two).as_deref(), Ok(&b"hello\nhello\n"[..]));
    }

    #[test]
    fn a_bad_magic_a_short_file_and_a_bad_checksum_are_refused() {
        assert_eq!(gunzip(b"hello"), Err(GzipError::Truncated));
        assert_eq!(gunzip(b"PK\x03\x04 and more than ten"), Err(GzipError::NotGzip));
        assert_eq!(gunzip(&HELLO[..HELLO.len() - 3]), Err(GzipError::Truncated));
        let mut bad = HELLO.to_vec();
        bad[18] ^= 1;
        assert_eq!(gunzip(&bad), Err(GzipError::Corrupt));
    }
}
