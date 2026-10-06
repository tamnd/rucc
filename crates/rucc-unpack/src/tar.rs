//! The tar archive, which is what a sysroot is packed in.
//!
//! Design: `spec/cross-compile/13-distribution.md` section 13.8 and the WASI notes of WA5
//! (tamnd/rucc#2867). A tar archive is a list of 512-byte headers, each one followed by the data
//! of its member, padded to a multiple of 512 bytes. Two blocks of zeros end it.
//!
//! The reader takes the files and the directories of the ustar format, the long names of GNU tar,
//! and the `path` of a POSIX extended header. Those are what GNU tar and bsdtar write for a tree of
//! ordinary files. A link, a device or any other kind of member is an error that names it, because
//! a sysroot has none and an unpacker that writes them is an unpacker that can be told to write
//! outside its directory. The name of a member is not checked here. [`crate::under`] does that,
//! where the file is written.

use std::fmt;

/// One block of a tar archive.
const BLOCK: usize = 512;

/// Why a tar archive could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TarError {
    /// A header or the data of a member runs off the end of the archive.
    Truncated,
    /// A header has a checksum that is not the sum of its bytes, or a number that is not octal.
    Header {
        /// The offset of the header in the archive.
        at: usize,
    },
    /// A member is of a kind this reader does not write, such as a link.
    Kind {
        /// The name of the member.
        name: String,
        /// The type flag of the header.
        kind: u8,
    },
    /// A name is not UTF-8.
    Name,
}

impl fmt::Display for TarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TarError::Truncated => write!(f, "the tar archive ends in the middle of a member"),
            TarError::Header { at } => write!(f, "the tar header at offset {at} is not valid"),
            TarError::Kind { name, kind } => write!(
                f,
                "{name} is a tar member of type {:?}, and only files and directories are unpacked",
                char::from(*kind)
            ),
            TarError::Name => write!(f, "a tar member has a name that is not UTF-8"),
        }
    }
}

impl std::error::Error for TarError {}

/// A file or a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
}

/// One member of a tar archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry<'a> {
    /// The name, as the archive spells it, with forward slashes.
    pub name: String,
    pub kind: Kind,
    /// The contents of a file. A directory has none.
    pub data: &'a [u8],
}

/// The text of a header field up to its first zero byte.
fn text(field: &[u8]) -> &[u8] {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    &field[..end]
}

/// A number field, in octal with spaces or zeros around it, or in base 256 when its first bit is
/// set, which is how GNU tar writes a size that octal cannot hold.
fn number(field: &[u8]) -> Option<u64> {
    if field.first().is_some_and(|&b| b & 0x80 != 0) {
        let mut value = u64::from(field[0] & 0x7f);
        for &b in &field[1..] {
            value = value.checked_mul(256)?.checked_add(u64::from(b))?;
        }
        return Some(value);
    }
    let digits = text(field);
    let digits = std::str::from_utf8(digits).ok()?.trim_matches(' ');
    if digits.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(digits, 8).ok()
}

fn name(bytes: &[u8]) -> Result<String, TarError> {
    String::from_utf8(bytes.to_vec()).map_err(|_| TarError::Name)
}

/// The value of `path` in the records of a POSIX extended header, if there is one. Each record
/// is `<length> <key>=<value>\n`, and the length counts the whole record.
fn pax_path(mut records: &[u8]) -> Result<Option<String>, TarError> {
    let mut path = None;
    while !records.is_empty() {
        let space = records.iter().position(|&b| b == b' ').ok_or(TarError::Truncated)?;
        let len: usize = std::str::from_utf8(&records[..space])
            .ok()
            .and_then(|len| len.parse().ok())
            .ok_or(TarError::Truncated)?;
        let record = records.get(space + 1..len).ok_or(TarError::Truncated)?;
        let record = record.strip_suffix(b"\n").unwrap_or(record);
        if let Some(value) = record.strip_prefix(b"path=") {
            path = Some(name(value)?);
        }
        records = &records[len..];
    }
    Ok(path)
}

/// Reads the members of a tar archive, in archive order.
///
/// # Errors
///
/// [`TarError`] for an archive that is cut short, a header that is not valid, a member that is
/// not a file or a directory, and a name that is not UTF-8.
pub fn entries(bytes: &[u8]) -> Result<Vec<Entry<'_>>, TarError> {
    let mut out = Vec::new();
    // A name that a GNU long name or an extended header gives for the next member.
    let mut long: Option<String> = None;
    let mut at = 0;
    loop {
        let Some(header) = bytes.get(at..at + BLOCK) else {
            // An archive with no end blocks is cut short, but `tar` itself accepts one that ends
            // on a block boundary, and so does this reader.
            return if at == bytes.len() { Ok(out) } else { Err(TarError::Truncated) };
        };
        if header.iter().all(|&b| b == 0) {
            return Ok(out);
        }
        // The checksum is the sum of the bytes of the header, with its own field taken as spaces.
        let sum: u64 = header
            .iter()
            .enumerate()
            .map(|(i, &b)| if (148..156).contains(&i) { 32 } else { u64::from(b) })
            .sum();
        let size = number(&header[124..136]).and_then(|size| usize::try_from(size).ok());
        let (Some(size), Some(true)) = (size, number(&header[148..156]).map(|c| c == sum)) else {
            return Err(TarError::Header { at });
        };
        let start = at + BLOCK;
        let data = bytes.get(start..start + size).ok_or(TarError::Truncated)?;
        at = start + size.div_ceil(BLOCK) * BLOCK;
        let kind = header[156];
        match kind {
            b'L' => {
                long = Some(name(text(data))?);
                continue;
            }
            b'x' => {
                if let Some(path) = pax_path(data)? {
                    long = Some(path);
                }
                continue;
            }
            // A global extended header sets defaults for the rest of the archive, and none of
            // them changes where a file goes or what is in it.
            b'g' => continue,
            _ => {}
        }
        let name = match long.take() {
            Some(name) => name,
            // The ustar format puts the start of a long name in the prefix field. GNU tar writes
            // other fields there and marks its headers with another magic.
            None if &header[257..263] == b"ustar\0" && header[345] != 0 => {
                let prefix = name(text(&header[345..500]))?;
                format!("{prefix}/{}", name(text(&header[..100]))?)
            }
            None => name(text(&header[..100]))?,
        };
        let kind = match kind {
            b'0' | 0 => Kind::File,
            b'5' => Kind::Dir,
            _ => return Err(TarError::Kind { name, kind }),
        };
        let data = if kind == Kind::Dir { &[][..] } else { data };
        out.push(Entry { name, kind, data });
    }
}

#[cfg(test)]
mod tests {
    use super::{BLOCK, Entry, Kind, TarError, entries};

    /// A header for `name` with the type flag `kind` and the GNU magic, then `data`, padded.
    fn member(out: &mut Vec<u8>, name: &str, kind: u8, data: &[u8]) {
        let mut header = [0u8; BLOCK];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000644\0");
        header[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
        header[156] = kind;
        header[257..265].copy_from_slice(b"ustar  \0");
        header[148..156].fill(b' ');
        let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
    }

    #[test]
    fn files_and_directories_are_read_in_order() {
        let mut tar = Vec::new();
        member(&mut tar, "./", b'5', b"");
        member(&mut tar, "./include/", b'5', b"");
        member(&mut tar, "./include/stdio.h", b'0', b"int printf(const char *, ...);\n");
        tar.extend_from_slice(&[0; 2 * BLOCK]);
        let got = entries(&tar).unwrap();
        let want = [
            Entry { name: "./".to_owned(), kind: Kind::Dir, data: b"" },
            Entry { name: "./include/".to_owned(), kind: Kind::Dir, data: b"" },
            Entry {
                name: "./include/stdio.h".to_owned(),
                kind: Kind::File,
                data: b"int printf(const char *, ...);\n",
            },
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn a_gnu_long_name_and_a_pax_path_name_the_next_member() {
        let long = format!("./{}/a.h", "d".repeat(120));
        let mut tar = Vec::new();
        member(&mut tar, "././@LongLink", b'L', format!("{long}\0").as_bytes());
        member(&mut tar, "./short", b'0', b"a");
        let record = format!(" path={long}\n");
        let record = format!("{}{record}", record.len() + 3);
        member(&mut tar, "./PaxHeaders/b", b'x', record.as_bytes());
        member(&mut tar, "./short", b'0', b"b");
        let got = entries(&tar).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].name.as_str(), got[0].data), (long.as_str(), &b"a"[..]));
        assert_eq!((got[1].name.as_str(), got[1].data), (long.as_str(), &b"b"[..]));
    }

    #[test]
    fn a_link_a_bad_checksum_and_a_short_archive_are_refused() {
        let mut tar = Vec::new();
        member(&mut tar, "lib", b'2', b"");
        assert_eq!(entries(&tar), Err(TarError::Kind { name: "lib".to_owned(), kind: b'2' }));
        let mut tar = Vec::new();
        member(&mut tar, "a", b'0', b"abc");
        tar[0] = b'b';
        assert_eq!(entries(&tar), Err(TarError::Header { at: 0 }));
        let mut tar = Vec::new();
        member(&mut tar, "a", b'0', &[7; 600]);
        tar.truncate(BLOCK + 100);
        assert_eq!(entries(&tar), Err(TarError::Truncated));
    }
}
