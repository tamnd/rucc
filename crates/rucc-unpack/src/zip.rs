//! The zip container, which is what a vsix is.
//!
//! Design: `spec/cross-compile/13-distribution.md` section 13.4. The MSVC CRT arrives as vsix
//! files, a vsix is a zip with a manifest in it, and the headers and libraries a compiler needs are
//! ordinary members inside it. So reading one is reading a zip.
//!
//! # Read from the back
//!
//! A zip is read from its end and not its start. The central directory at the back is the list of
//! what is in the file, and the local header in front of each member's data is a copy of part of
//! that entry which an encoder is allowed to leave incomplete: when bit 3 of the flags is set the
//! local header's sizes and checksum are zero and the real ones are in a descriptor after the data,
//! which cannot be found without knowing where the data ends. The central directory always has
//! them. So every number this reader uses comes from there, and the local header is read for one
//! thing only, which is how many bytes of name and extra field sit between it and the data.
//!
//! # Zip64
//!
//! Handled, though nothing Microsoft ships here needs it. A zip stores sizes and offsets in four
//! bytes and spells "this did not fit" as all ones, with the real value in an extra field or in a
//! second directory end record. The largest CRT payload is about fifty megabytes, so the ordinary
//! fields are enough today. It is here because the failure without it is not a refusal but a reader
//! that seeks to 0xffffffff, and because it is a page of code.
//!
//! # What is not here
//!
//! Encryption, and every compression method except the two that matter. Stored and deflate are what
//! a vsix uses and what the format's own appnote calls the only methods that must be supported.

use std::fmt;

use crate::inflate::{self, InflateError};

/// The end of central directory record, which is where reading starts.
const END: u32 = 0x0605_4b50;
/// The zip64 locator, which sits immediately in front of the record above when there is one.
const ZIP64_LOCATOR: u32 = 0x0706_4b50;
/// The zip64 end of central directory record, which the locator points at.
const ZIP64_END: u32 = 0x0606_4b50;
/// A central directory entry.
const ENTRY: u32 = 0x0201_4b50;
/// A local file header.
const LOCAL: u32 = 0x0403_4b50;

/// The value a four byte field takes when the real one did not fit and is in a zip64 field.
const OVERFLOWED: u32 = 0xffff_ffff;

/// The largest a comment can be, which is what bounds the search for the end record.
const COMMENT: usize = 0xffff;

/// Stored, which is no compression at all.
const STORED: u16 = 0;
/// Deflate, which is every other member in practice.
const DEFLATE: u16 = 8;

/// Why a zip could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZipError {
    /// No end of central directory record, so this is not a zip at all, or is a zip that was
    /// truncated at the one end that cannot be recovered from.
    NotAZip,
    /// A record or a field runs off the end of the file.
    Truncated {
        /// What was being read.
        what: &'static str,
    },
    /// A member is compressed with something other than stored or deflate.
    Method {
        /// The name of the member.
        name: String,
        /// The method number, which is from the format's own list.
        method: u16,
    },
    /// A member is encrypted, which nothing here will ever be able to read.
    Encrypted {
        /// The name of the member.
        name: String,
    },
    /// A member's name is not UTF-8, so there is no file name to write it to that both this host
    /// and the machine it came from would agree about.
    Name,
    /// A member did not decompress.
    Stream {
        /// The name of the member.
        name: String,
        /// What the decompressor said.
        why: InflateError,
    },
    /// A member decompressed to something other than what the directory said it would, either the
    /// wrong length or the wrong checksum, which is the format's own end to end check.
    Corrupt {
        /// The name of the member.
        name: String,
    },
    /// The file says it is in more than one piece, which is a floppy disk era feature that nothing
    /// has produced in thirty years and that this will not guess at.
    Split,
}

impl fmt::Display for ZipError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ZipError::NotAZip => write!(f, "no end of central directory record, so not a zip"),
            ZipError::Truncated { what } => write!(f, "the file ends in the middle of {what}"),
            ZipError::Method { name, method } => {
                write!(
                    f,
                    "{name} is compressed with method {method}, and only stored and deflate are read here"
                )
            }
            ZipError::Encrypted { name } => write!(f, "{name} is encrypted"),
            ZipError::Name => write!(f, "a member's name is not UTF-8"),
            ZipError::Stream { name, why } => write!(f, "{name}: {why}"),
            ZipError::Corrupt { name } => {
                write!(f, "{name} came out a different size or checksum than the directory says")
            }
            ZipError::Split => write!(f, "the archive is split across several files"),
        }
    }
}

impl std::error::Error for ZipError {}

/// One member of a zip, as the central directory describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The name, with forward slashes, which is what the format says and what Microsoft's own
    /// tooling writes even though the machine it ran on spells paths the other way.
    pub name: String,
    /// How large it is once decompressed.
    pub size: u64,
    /// How large it is in the file.
    pub packed: u64,
    /// The checksum the directory gives, which [`Zip::contents`] holds the result against.
    pub crc: u32,
    /// How it is compressed, from the format's own list.
    method: u16,
    /// Whether it is encrypted, which is bit 0 of the flags.
    encrypted: bool,
    /// Where its local header is.
    at: u64,
}

impl Member {
    /// Whether this is a directory rather than a file.
    ///
    /// A zip has no type field. A directory is a member whose name ends in a slash and which has no
    /// content, and that is the whole of the convention.
    #[must_use]
    pub fn is_dir(&self) -> bool {
        self.name.ends_with('/')
    }
}

/// A zip that has been read as far as its directory.
///
/// Holding the bytes rather than copying out of them, because the caller has the whole file in
/// memory already and wants a few members out of it.
#[derive(Debug)]
pub struct Zip<'a> {
    bytes: &'a [u8],
    members: Vec<Member>,
}

impl<'a> Zip<'a> {
    /// Read a zip's directory.
    ///
    /// # Errors
    ///
    /// [`ZipError`] for a file that is not a zip, or is one that was cut short.
    pub fn read(bytes: &'a [u8]) -> Result<Zip<'a>, ZipError> {
        let end = find_end(bytes).ok_or(ZipError::NotAZip)?;
        let at = At { bytes, at: end + 4 };
        let (count, start) = directory(at, bytes)?;

        let mut members = Vec::with_capacity(usize::try_from(count).unwrap_or_default().min(4096));
        let mut at = At { bytes, at: usize::try_from(start).map_err(|_| trunc("the directory"))? };
        for _ in 0..count {
            members.push(entry(&mut at)?);
        }
        Ok(Zip { bytes, members })
    }

    /// Everything the directory lists, in the order it lists it.
    #[must_use]
    pub fn members(&self) -> &[Member] {
        &self.members
    }

    /// The member with this exact name, if there is one.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&Member> {
        self.members.iter().find(|member| member.name == name)
    }

    /// Decompress one member and check it against what the directory said it would be.
    ///
    /// # Errors
    ///
    /// [`ZipError`] for a member that is encrypted, is compressed with something this does not
    /// read, does not decompress, or comes out the wrong size or checksum.
    pub fn contents(&self, member: &Member) -> Result<Vec<u8>, ZipError> {
        if member.encrypted {
            return Err(ZipError::Encrypted { name: member.name.clone() });
        }
        let data = self.data(member)?;
        let out = match member.method {
            STORED => data.to_vec(),
            DEFLATE => {
                let mut out = Vec::with_capacity(usize::try_from(member.size).unwrap_or_default());
                inflate::inflate_into(data, &mut out)
                    .map_err(|why| ZipError::Stream { name: member.name.clone(), why })?;
                out
            }
            method => return Err(ZipError::Method { name: member.name.clone(), method }),
        };
        // The format's own end to end check, and the reason this is worth doing rather than
        // trusting the download's hash: that hash says the vsix arrived whole, and this says the
        // member came back out of it as the thing that went in.
        if out.len() as u64 != member.size || crc32(&out) != member.crc {
            return Err(ZipError::Corrupt { name: member.name.clone() });
        }
        Ok(out)
    }

    /// The member's bytes as they sit in the file, still compressed.
    ///
    /// The local header is read for its two length fields and nothing else. Everything else in it
    /// is a copy of the directory entry that an encoder is allowed to leave zeroed, so reading it
    /// for anything would be reading a field that may be a lie.
    fn data(&self, member: &Member) -> Result<&'a [u8], ZipError> {
        let mut at = At {
            bytes: self.bytes,
            at: usize::try_from(member.at).map_err(|_| trunc("a local header"))?,
        };
        if at.u32("a local header")? != LOCAL {
            return Err(trunc("a local header"));
        }
        at.skip(22, "a local header")?;
        let name = u64::from(at.u16("a local header")?);
        let extra = u64::from(at.u16("a local header")?);
        let from = at.at as u64 + name + extra;
        let to = from + member.packed;
        let from = usize::try_from(from).map_err(|_| trunc("a member"))?;
        let to = usize::try_from(to).map_err(|_| trunc("a member"))?;
        self.bytes.get(from..to).ok_or_else(|| trunc("a member"))
    }
}

/// A cursor over the file, which reads little endian because a zip always does whatever the machine
/// it was written on thought.
struct At<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> At<'a> {
    fn u16(&mut self, what: &'static str) -> Result<u16, ZipError> {
        let bytes = self.take(2, what)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self, what: &'static str) -> Result<u32, ZipError> {
        let bytes = self.take(4, what)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self, what: &'static str) -> Result<u64, ZipError> {
        let bytes = self.take(8, what)?;
        let mut eight = [0u8; 8];
        eight.copy_from_slice(bytes);
        Ok(u64::from_le_bytes(eight))
    }

    /// The slice borrows the file rather than the cursor, so reading a name out and then carrying
    /// on past it is one borrow after another rather than two at once.
    fn take(&mut self, n: usize, what: &'static str) -> Result<&'a [u8], ZipError> {
        let to = self.at.checked_add(n).ok_or_else(|| trunc(what))?;
        let bytes = self.bytes.get(self.at..to).ok_or_else(|| trunc(what))?;
        self.at = to;
        Ok(bytes)
    }

    fn skip(&mut self, n: usize, what: &'static str) -> Result<(), ZipError> {
        self.take(n, what).map(|_| ())
    }
}

/// A truncation, said the same way everywhere.
fn trunc(what: &'static str) -> ZipError {
    ZipError::Truncated { what }
}

/// Find the end of central directory record, which is the only thing in a zip with no pointer to
/// it.
///
/// Searched for backwards, because it ends in a comment of up to 65535 bytes that nothing gives the
/// length of from the outside. Backwards rather than forwards so that a member which happens to
/// contain those four bytes, which is an ordinary thing for a zip inside a zip, does not win over
/// the real record.
fn find_end(bytes: &[u8]) -> Option<usize> {
    let signature = END.to_le_bytes();
    let first = bytes.len().saturating_sub(COMMENT + 22);
    let mut at = bytes.len().checked_sub(22)?;
    loop {
        if bytes.get(at..at + 4) == Some(&signature[..]) {
            // The comment length has to agree with where the file ends, which is what tells the
            // real record from four bytes inside a member that look like one.
            let length = usize::from(u16::from_le_bytes([bytes[at + 20], bytes[at + 21]]));
            if at + 22 + length == bytes.len() {
                return Some(at);
            }
        }
        if at == first {
            return None;
        }
        at = at.checked_sub(1)?;
    }
}

/// How many entries the directory has and where it starts, from whichever end record has the real
/// numbers.
fn directory(mut at: At<'_>, bytes: &[u8]) -> Result<(u64, u64), ZipError> {
    let disk = at.u16("the directory end")?;
    let cd_disk = at.u16("the directory end")?;
    at.skip(2, "the directory end")?;
    let count = u64::from(at.u16("the directory end")?);
    at.skip(4, "the directory end")?;
    let start = u64::from(at.u32("the directory end")?);
    if disk != 0 || cd_disk != 0 {
        return Err(ZipError::Split);
    }
    // All ones in either field means the real one is in the zip64 record, which the locator in
    // front of this one points at. Nothing Microsoft ships here is that large, and a reader that
    // seeks to 0xffffffff instead of saying so is the failure worth avoiding.
    if count != 0xffff && start != u64::from(OVERFLOWED) {
        return Ok((count, start));
    }
    zip64(bytes)
}

/// The same two numbers out of the zip64 records.
fn zip64(bytes: &[u8]) -> Result<(u64, u64), ZipError> {
    let end = find_end(bytes).ok_or(ZipError::NotAZip)?;
    let at = end.checked_sub(20).ok_or_else(|| trunc("the zip64 locator"))?;
    let mut at = At { bytes, at };
    if at.u32("the zip64 locator")? != ZIP64_LOCATOR {
        return Err(trunc("the zip64 locator"));
    }
    at.skip(4, "the zip64 locator")?;
    let record = at.u64("the zip64 locator")?;

    let mut at =
        At { bytes, at: usize::try_from(record).map_err(|_| trunc("the zip64 directory end"))? };
    if at.u32("the zip64 directory end")? != ZIP64_END {
        return Err(trunc("the zip64 directory end"));
    }
    at.skip(20, "the zip64 directory end")?;
    let count = at.u64("the zip64 directory end")?;
    at.skip(8, "the zip64 directory end")?;
    let start = at.u64("the zip64 directory end")?;
    Ok((count, start))
}

/// One central directory entry.
fn entry(at: &mut At<'_>) -> Result<Member, ZipError> {
    if at.u32("a directory entry")? != ENTRY {
        return Err(trunc("a directory entry"));
    }
    at.skip(4, "a directory entry")?;
    let flags = at.u16("a directory entry")?;
    let method = at.u16("a directory entry")?;
    at.skip(4, "a directory entry")?;
    let crc = at.u32("a directory entry")?;
    let packed = at.u32("a directory entry")?;
    let size = at.u32("a directory entry")?;
    let name = usize::from(at.u16("a directory entry")?);
    let extra = usize::from(at.u16("a directory entry")?);
    let comment = usize::from(at.u16("a directory entry")?);
    at.skip(8, "a directory entry")?;
    let offset = at.u32("a directory entry")?;

    let name = at.take(name, "a directory entry")?.to_vec();
    // Bit 11 says the name is UTF-8 and its absence says the name is in the code page of whatever
    // machine wrote it, which is a question with no answer here. Every name in every payload
    // Microsoft publishes is ASCII, which is both, so this asks for UTF-8 either way and refuses
    // rather than guessing at a name it would then write to disk.
    let name = String::from_utf8(name).map_err(|_| ZipError::Name)?;
    let extra = at.take(extra, "a directory entry")?;
    let (size, packed, offset) = wider(extra, size, packed, offset)?;
    at.skip(comment, "a directory entry")?;

    Ok(Member { name, size, packed, crc, method, encrypted: flags & 1 != 0, at: offset })
}

/// Replace whichever of the three numbers overflowed with the real one out of the zip64 extra
/// field.
///
/// The field holds only the ones that overflowed, in this order and with no tags on them, so which
/// value is which depends entirely on which of the three were all ones. That is the format's design
/// and not a reading of it.
fn wider(extra: &[u8], size: u32, packed: u32, at: u32) -> Result<(u64, u64, u64), ZipError> {
    let (mut size, mut packed, mut at) = (u64::from(size), u64::from(packed), u64::from(at));
    if size != u64::from(OVERFLOWED)
        && packed != u64::from(OVERFLOWED)
        && at != u64::from(OVERFLOWED)
    {
        return Ok((size, packed, at));
    }
    let mut fields = At { bytes: extra, at: 0 };
    while fields.at + 4 <= extra.len() {
        let tag = fields.u16("a zip64 field")?;
        let length = usize::from(fields.u16("a zip64 field")?);
        if tag != 1 {
            fields.skip(length, "a zip64 field")?;
            continue;
        }
        if size == u64::from(OVERFLOWED) {
            size = fields.u64("a zip64 field")?;
        }
        if packed == u64::from(OVERFLOWED) {
            packed = fields.u64("a zip64 field")?;
        }
        if at == u64::from(OVERFLOWED) {
            at = fields.u64("a zip64 field")?;
        }
        return Ok((size, packed, at));
    }
    Err(trunc("a zip64 field"))
}

/// The table for the checksum below, built once at compile time.
const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            // The polynomial reversed, which is how every zip and every gzip spells it.
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

/// The checksum a zip holds its members against.
pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = OVERFLOWED;
    for &byte in bytes {
        let at = usize::from((crc ^ u32::from(byte)) as u8);
        crc = (crc >> 8) ^ CRC_TABLE[at];
    }
    crc ^ OVERFLOWED
}

#[cfg(test)]
mod tests {
    use super::{Zip, ZipError, crc32};

    #[test]
    fn the_checksum_matches_the_values_everyone_publishes_for_it() {
        // Which is the point of using the one every zip and gzip uses: there are known answers.
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"a"), 0xe8b7_be43);
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b"The quick brown fox jumps over the lazy dog"), 0x414f_a339);
    }

    /// Build a zip with whatever the machine has, for the same reason the decompressor's tests do:
    /// a test that shipped its own encoder would be testing two things this crate wrote.
    ///
    /// `tag` names the directory this one works in, because these tests run at the same time in one
    /// process and a directory named after the process is one they would take turns deleting.
    ///
    /// [`None`] only when there is no `zip` to run. A `zip` that ran and failed fails the test,
    /// rather than turning it into one that asserts nothing and passes.
    fn zipped(tag: &str, files: &[(&str, &[u8])], store: bool) -> Option<Vec<u8>> {
        use std::process::Command;
        let dir = std::env::temp_dir().join(format!("rucc-unpack-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("in")).expect("a directory to work in");
        for (name, bytes) in files {
            let at = dir.join("in").join(name);
            std::fs::create_dir_all(at.parent().expect("a name with a directory above it"))
                .expect("a directory to write into");
            std::fs::write(at, bytes).expect("writes");
        }
        let mut zip = Command::new("zip");
        zip.arg("-q").arg("-r");
        if store {
            zip.arg("-0");
        }
        let ran = zip.arg(dir.join("out.zip")).arg(".").current_dir(dir.join("in")).status();
        let out = match ran {
            Ok(status) => {
                assert!(status.success(), "zip failed on {tag}");
                Some(std::fs::read(dir.join("out.zip")).expect("the zip it said it wrote"))
            }
            // A machine with no zip is one these tests cannot say anything on.
            Err(_) => None,
        };
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    #[test]
    fn what_a_real_encoder_produced_reads_back_as_what_went_in() {
        let long = "a line that repeats and so compresses\n".repeat(3000);
        let files: &[(&str, &[u8])] =
            &[("one.h", b"#define ONE 1\n"), ("deep/two.h", long.as_bytes()), ("empty.h", b"")];
        // Both ways, because stored and deflate are different paths through `contents` and a vsix
        // has members of both kinds in it.
        for store in [false, true] {
            let Some(bytes) = zipped("both", files, store) else {
                // A machine with no zip is one this test cannot say anything on.
                return;
            };
            let zip = Zip::read(&bytes).expect("reads");
            for (name, want) in files {
                let member = zip.find(name).expect(name);
                assert_eq!(zip.contents(member).as_deref(), Ok(*want), "{name} stored {store}");
                assert!(!member.is_dir());
            }
            // And the directories the encoder put in are there and are marked as such.
            assert!(zip.members().iter().any(|member| member.name == "deep/" && member.is_dir()));
        }
    }

    #[test]
    fn a_file_that_is_not_a_zip_says_so_rather_than_reading_something() {
        assert_eq!(Zip::read(b"").unwrap_err(), ZipError::NotAZip);
        assert_eq!(Zip::read(b"MZ\x90\x00 an executable").unwrap_err(), ZipError::NotAZip);
        assert_eq!(Zip::read(&[0u8; 4096]).unwrap_err(), ZipError::NotAZip);
    }

    #[test]
    fn a_zip_that_lost_its_tail_is_refused_and_one_that_lost_its_middle_is_caught_by_the_checksum()
    {
        let files: &[(&str, &[u8])] = &[("one.h", &b"the contents of a header\n".repeat(200))];
        let Some(bytes) = zipped("cut", files, false) else {
            return;
        };
        // The directory is at the back, so a file with its back missing is not a zip any more.
        assert_eq!(Zip::read(&bytes[..bytes.len() / 2]).unwrap_err(), ZipError::NotAZip);

        // Whereas a byte changed in the middle leaves the directory intact and is caught by the
        // check the format carries for exactly this.
        let mut broken = bytes.clone();
        let at = broken.len() / 3;
        broken[at] ^= 0xff;
        let zip = Zip::read(&broken).expect("still has a directory");
        let member = zip.find("one.h").expect("still listed");
        assert!(
            matches!(zip.contents(member), Err(ZipError::Corrupt { .. } | ZipError::Stream { .. })),
            "a changed byte should not read back as the original"
        );
    }

    #[test]
    fn the_end_record_is_found_from_the_back_so_a_zip_inside_a_zip_does_not_win() {
        // Which is what the vsix files are not, but is the ordinary reason to search backwards, and
        // storing one zip inside another with no compression is how to put those four bytes in the
        // middle of a file on purpose.
        let files: &[(&str, &[u8])] = &[("inner.h", b"#define INNER 1\n")];
        let Some(inner) = zipped("inner", files, true) else {
            return;
        };
        let outer: &[(&str, &[u8])] = &[("held.zip", &inner), ("plain.h", b"#define PLAIN 1\n")];
        let Some(bytes) = zipped("outer", outer, true) else {
            return;
        };
        let zip = Zip::read(&bytes).expect("reads the outer one");
        let member = zip.find("held.zip").expect("the inner one is a member");
        assert_eq!(zip.contents(member).as_deref(), Ok(inner.as_slice()));
        // And the inner one reads as a zip in its own right once it is out.
        let held = Zip::read(&inner).expect("reads the inner one");
        assert!(held.find("inner.h").is_some());
    }
}
