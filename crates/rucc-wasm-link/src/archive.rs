//! The `ar` archive, read as a list of members.
//!
//! `libc.a` from wasi-sdk is a System V archive that `llvm-ar` wrote, with a symbol index and a
//! table of long names. `librucc_builtins.a` is one that `rucc-archive` wrote. This reader takes
//! both, and the BSD layout too, because a user can make an archive with the `ar` of macOS.
//!
//! The reader does not use the symbol index. The linker parses each member anyway to know what it
//! defines and what it needs, and a list made from the members cannot disagree with them. The
//! parse of all 1,200 members of `libc.a` takes a few milliseconds.

use crate::Error;

/// The eight bytes an archive starts with.
pub const MAGIC: &[u8] = b"!<arch>\n";

/// The size of a member header.
const HEADER: usize = 60;

/// A member: its name and its bytes.
#[derive(Debug, Clone)]
pub struct Member<'a> {
    pub name: String,
    pub bytes: &'a [u8],
}

/// Whether `bytes` start as an archive.
#[must_use]
pub fn is_archive(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

/// The members of the archive in `bytes`, in order, without the symbol index and the table of
/// long names.
///
/// # Errors
///
/// When the bytes are not an archive, or a header or a name in it is bad. A thin archive is
/// refused, because its members are other files.
pub fn members(bytes: &[u8]) -> Result<Vec<Member<'_>>, Error> {
    if bytes.starts_with(b"!<thin>\n") {
        return Err(Error::new("a thin archive is not supported".to_owned()));
    }
    if !is_archive(bytes) {
        return Err(Error::new("not an archive".to_owned()));
    }
    let mut members = Vec::new();
    let mut names: &[u8] = &[];
    let mut pos = MAGIC.len();
    while pos < bytes.len() {
        // A member starts at an even offset, so an odd member is followed by one byte of padding.
        if bytes[pos] == b'\n' && pos % 2 == 1 {
            pos += 1;
            continue;
        }
        let header = bytes
            .get(pos..pos + HEADER)
            .ok_or_else(|| Error::new(format!("the member header at offset {pos} is short")))?;
        if &header[58..60] != b"`\n" {
            return Err(Error::new(format!("the member header at offset {pos} is bad")));
        }
        let size = field(&header[48..58])
            .ok_or_else(|| Error::new(format!("the member size at offset {pos} is bad")))?;
        let start = pos + HEADER;
        let data = start
            .checked_add(size)
            .and_then(|end| bytes.get(start..end))
            .ok_or_else(|| Error::new(format!("the member at offset {pos} ends past the file")))?;
        pos = start + size;
        let raw = trim(&header[..16]);
        let (name, data) = if raw == b"/" || raw == b"/SYM64/" || raw.starts_with(b"__.SYMDEF") {
            continue;
        } else if raw == b"//" {
            names = data;
            continue;
        } else if let Some(len) = raw.strip_prefix(b"#1/") {
            // BSD: the name is the first bytes of the data.
            let len = field(len).filter(|&len| len <= data.len());
            let len = len.ok_or_else(|| Error::new(format!("the name at offset {pos} is bad")))?;
            let name = trim_nul(&data[..len]);
            if name.starts_with(b"__.SYMDEF") {
                continue;
            }
            (name, &data[len..])
        } else if let Some(offset) = raw.strip_prefix(b"/") {
            // System V: the name is in the table of long names, ended by `/\n`.
            let offset = field(offset).filter(|&offset| offset < names.len());
            let offset =
                offset.ok_or_else(|| Error::new(format!("the long name at {pos} is bad")))?;
            let rest = &names[offset..];
            let end = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
            let name = &rest[..end];
            (name.strip_suffix(b"/").unwrap_or(name), data)
        } else {
            (raw.strip_suffix(b"/").unwrap_or(raw), data)
        };
        members.push(Member { name: String::from_utf8_lossy(name).into_owned(), bytes: data });
    }
    Ok(members)
}

/// A decimal field of a header, padded with spaces.
fn field(bytes: &[u8]) -> Option<usize> {
    let text = core::str::from_utf8(trim(bytes)).ok()?;
    text.parse().ok()
}

fn trim(bytes: &[u8]) -> &[u8] {
    let end = bytes.iter().rposition(|&b| b != b' ').map_or(0, |end| end + 1);
    &bytes[..end]
}

fn trim_nul(bytes: &[u8]) -> &[u8] {
    let end = bytes.iter().rposition(|&b| b != 0).map_or(0, |end| end + 1);
    &bytes[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(name: &str, size: usize) -> Vec<u8> {
        let mut out = format!("{name:<16}{:<12}{:<6}{:<6}{:<8}{size:<10}`\n", 0, 0, 0, 644);
        out.truncate(HEADER);
        out.into_bytes()
    }

    #[test]
    fn a_system_v_archive_gives_its_members_with_long_names() {
        let long = "a_name_longer_than_sixteen.o/\n";
        let mut bytes = MAGIC.to_vec();
        bytes.extend(header("/", 4));
        bytes.extend([0, 0, 0, 0]);
        bytes.extend(header("//", long.len()));
        bytes.extend(long.as_bytes());
        bytes.extend(header("/0", 3));
        bytes.extend(b"abc\n");
        bytes.extend(header("b.o/", 2));
        bytes.extend(b"de");
        let members = members(&bytes).unwrap();
        let names: Vec<_> = members.iter().map(|m| (m.name.as_str(), m.bytes)).collect();
        assert_eq!(names, [("a_name_longer_than_sixteen.o", &b"abc"[..]), ("b.o", b"de")]);
    }

    #[test]
    fn a_bsd_archive_gives_its_members() {
        let mut bytes = MAGIC.to_vec();
        bytes.extend(header("#1/12", 12));
        bytes.extend(b"__.SYMDEF\0\0\0");
        bytes.extend(header("#1/8", 11));
        bytes.extend(b"c.o\0\0\0\0\0xyz\n");
        let members = members(&bytes).unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!((members[0].name.as_str(), members[0].bytes), ("c.o", &b"xyz"[..]));
    }

    #[test]
    fn a_member_past_the_end_is_an_error() {
        let mut bytes = MAGIC.to_vec();
        bytes.extend(header("a.o/", 100));
        bytes.extend(b"short");
        assert!(members(&bytes).is_err());
        assert!(members(b"!<thin>\n").is_err());
    }
}
