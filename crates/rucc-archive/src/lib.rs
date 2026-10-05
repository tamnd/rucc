//! The `ar` archive a linker reads as a static library, written so that two runs agree.
//!
//! Design: `spec/cross-compile/13-distribution.md` section 13.6 for the determinism, and
//! `spec/cross-compile/10-runtime.md` section 10.2 for the file this exists to write. Layer rank 0,
//! see `spec/18-package-layout.md`.
//!
//! # What an archive is for
//!
//! A directory of object files is not a link input. A static library is one file with the objects
//! inside it and a symbol index at the front, and the index is the part that matters: a static link
//! resolves an undefined symbol by looking it up there and pulling in the one member that defines
//! it, rather than by reading every member to find out. An archive with no index parses and links
//! nothing.
//!
//! `librucc_builtins.a` is the first archive rucc has to write, which is why this crate exists, and
//! `rucc-stub` writes import libraries that are archives too, which is why it is a crate rather
//! than a module inside one of them.
//!
//! # What it takes and why
//!
//! A [`Flavour`] rather than a target, and the names each [`Member`] defines rather than the object
//! to read them out of. Both of those fall out of rank 0. `rucc-tuple` is at rank 0 too, so a crate
//! here cannot take a target tuple, and the object readers are blessed for `rucc-object` at rank 9,
//! so a crate here cannot parse the members it is handed. Neither is a loss. The caller is whatever
//! just emitted the object and it already knows both answers, and an interface that is told the
//! names cannot produce an index that disagrees with a member in some way that reading the member
//! would have caught, because there is nothing left to disagree with.
//!
//! # What determinism means here
//!
//! Section 13.6 asks for release artifacts that can be rebuilt and compared, and an archive is a
//! release artifact. So the modification time, the uid and the gid of every member are zero, the
//! members come out in the order they were given, and nothing else varies. The mode is the one
//! numeric field that is not zero, because both of the tools this is compared against write 644
//! there and a member extracted with a mode of zero is a file nobody can open.
//!
//! # Status
//!
//! All three flavours. The System V one, which every ELF target's `ar` writes and which mingw-w64
//! writes too, the COFF one, which is Microsoft's and has two indexes, and the BSD one, which is
//! what Apple's `ld` reads and which is laid out the way `llvm-ar --format=darwin` lays it out.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-archive/0.22.0")]

use core::fmt;

/// The eight bytes every archive starts with.
///
/// An archive with no members is these and nothing else, which is what `ar` produces when it is
/// given nothing and what musl's build writes for each of its empty libraries.
pub const MAGIC: &[u8] = b"!<arch>\n";

/// The size of a member header, which is fixed and has no extension mechanism.
const HEADER: usize = 60;

/// The bytes a header has for a name, one of which the terminator takes.
const NAME: usize = 16;

/// Which of the two indexes a reader of this archive is going to look for.
///
/// The container is the same either way. What differs is the index, and an archive carrying the
/// wrong one is an archive a linker reads as empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavour {
    /// One index, in a member called `/`, with its counts and offsets big-endian.
    ///
    /// What every ELF target's `ar` writes, and what mingw-w64 writes as well, since its toolchain
    /// is a GNU one that happens to emit COFF objects.
    Gnu,
    /// Two indexes, Microsoft's pair.
    ///
    /// The first is the one above. The second is little-endian with its names sorted, so that a
    /// linker can bisect them, and with an index per name saying which member to pull rather than an
    /// offset. Both are present in every library either Windows toolchain produces and a linker may
    /// read either, so writing one and not the other is a file that works until it meets the other
    /// linker.
    Coff,
    /// One index, in a member called `__.SYMDEF`, with a string offset and a member offset per name,
    /// little-endian.
    ///
    /// What Apple's `ar` and `libtool` write and the only index ld64 reads. Every member carries its
    /// name after its header rather than in it, `#1/` and a length being what the header says, and
    /// the name is padded with zero bytes so that the object after it starts on an eight byte
    /// boundary. ld64 reads a 64 bit object in place and refuses one that is not aligned that way.
    Bsd,
}

/// One member of an archive: what it is called, what is in it, and what it answers for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The name the member has inside the archive, which is what `ar t` lists and what a diagnostic
    /// about a member names.
    ///
    /// Any length. A name of 15 bytes or fewer goes in the header and a longer one goes in the
    /// member the format has for exactly that, so a caller does not have to care which.
    pub name: String,
    /// The member's contents, which is a whole object file.
    pub body: Vec<u8>,
    /// The external symbols this member defines, in the order it defines them.
    ///
    /// Definitions only. Indexing a name a member merely refers to would tell the linker this
    /// member answers a question it is only asking, and the link would then either fail with the
    /// name still undefined or pull in a member nothing needed.
    pub defines: Vec<String>,
}

impl Member {
    /// A member with a name and a body and nothing indexed yet.
    #[must_use]
    pub fn new(name: impl Into<String>, body: Vec<u8>) -> Self {
        Member { name: name.into(), body, defines: Vec::new() }
    }

    /// The same member, with the names it defines.
    #[must_use]
    pub fn defining(mut self, defines: Vec<String>) -> Self {
        self.defines = defines;
        self
    }
}

/// Writes an archive.
///
/// The result is a complete file, ready to be written out and put on a link line. Nothing is left
/// to the caller: the index is built, the long names are collected, the members are padded and the
/// offsets are resolved.
///
/// # Errors
///
/// Every failure is the input being unwritable rather than the writing going wrong, and all of them
/// are found before the first byte. See [`Error`].
pub fn write(flavour: Flavour, members: &[Member]) -> Result<Vec<u8>, Error> {
    for one in members {
        if one.name.is_empty() {
            return Err(Error::NoName);
        }
        check(&one.name)?;
        for define in &one.defines {
            check(define)?;
        }
    }
    // The second COFF index holds a member number per name and holds it in two bytes, so this is a
    // real limit of the format rather than a limit of this writer.
    if flavour == Flavour::Coff && members.len() > u16::MAX as usize {
        return Err(Error::TooManyMembers { members: members.len() });
    }
    if members.is_empty() {
        return Ok(MAGIC.to_vec());
    }

    let (names, long) = headers(flavour, members);

    // One entry per name per member, in member order, which is the order both indexes are built in
    // before either is sorted.
    let mut flat: Vec<(&str, usize)> = Vec::new();
    for (at, one) in members.iter().enumerate() {
        for define in &one.defines {
            flat.push((define, at));
        }
    }

    // The members that are not objects, built with zeroes where the offsets go, because the offsets
    // are not known until every length is.
    let mut special: Vec<(&str, Mode, Vec<u8>)> = match flavour {
        Flavour::Gnu => vec![("/", Mode::Zero, sysv(&flat))],
        Flavour::Coff => {
            vec![("/", Mode::Zero, first(&flat)), ("/", Mode::Zero, second(members.len(), &flat))]
        }
        // The BSD index is laid out differently enough that it is written on its own.
        Flavour::Bsd => return bsd(members),
    };
    if let Some(long) = long {
        special.push(("//", Mode::Blank, long));
    }

    let mut at = MAGIC.len();
    for (_, _, body) in &special {
        at += HEADER + even(body.len());
    }
    let mut offsets = Vec::with_capacity(members.len());
    for one in members {
        offsets.push(at);
        at += HEADER + even(one.body.len());
    }
    // A member offset is four bytes in both indexes, so an archive this big could be written and not
    // read. Saying so is better than writing a file whose index wraps around.
    if at > u32::MAX as usize {
        return Err(Error::TooBig { bytes: at });
    }

    resolve(&mut special[0].2, &flat, &offsets);
    if flavour == Flavour::Coff {
        number(&mut special[1].2, &offsets);
    }

    let mut out = Vec::from(MAGIC);
    for (name, mode, body) in &special {
        member(&mut out, name, *mode, body);
    }
    for (name, one) in names.iter().zip(members) {
        member(&mut out, name, Mode::Object, &one.body);
    }
    Ok(out)
}

/// A name that cannot be written, or nothing.
fn check(name: &str) -> Result<(), Error> {
    if name.contains('\0') {
        return Err(Error::NameHasNul { name: name.to_owned() });
    }
    Ok(())
}

/// The header name of every member, and the long names member if any member needed one.
///
/// A name is stored once however many members carry it, which matters because the import libraries
/// `rucc-stub` writes name every member after the DLL: without that, a library with forty records in
/// it would hold forty copies of one string.
fn headers(flavour: Flavour, members: &[Member]) -> (Vec<String>, Option<Vec<u8>>) {
    let mut table: Vec<u8> = Vec::new();
    let mut placed: Vec<(&str, usize)> = Vec::new();
    let mut names = Vec::with_capacity(members.len());
    for one in members {
        if one.name.len() < NAME {
            // The slash is the terminator, and it is there so that a name with trailing spaces in it
            // survives a field that is padded with spaces.
            names.push(format!("{}/", one.name));
            continue;
        }
        let at = match placed.iter().find(|(name, _)| *name == one.name.as_str()) {
            Some((_, at)) => *at,
            None => {
                let at = table.len();
                table.extend_from_slice(one.name.as_bytes());
                match flavour {
                    // The two formats disagree on how a long name ends. GNU terminates with the same
                    // slash a short name carries and a newline so the member reads as text, and COFF
                    // terminates with a zero byte.
                    Flavour::Gnu => table.extend_from_slice(b"/\n"),
                    Flavour::Coff | Flavour::Bsd => table.push(0),
                }
                placed.push((one.name.as_str(), at));
                at
            }
        };
        names.push(format!("/{at}"));
    }
    if table.is_empty() {
        return (names, None);
    }
    pad(&mut table, b'\n');
    (names, Some(table))
}

/// The index `ar` has always had: a count, an offset per name, and the names.
///
/// Big-endian whatever the machine is, which is the one thing about the format that never had a
/// second opinion.
fn sysv(flat: &[(&str, usize)]) -> Vec<u8> {
    let mut out = Vec::new();
    u32be(&mut out, flat.len() as u32);
    for _ in flat {
        u32be(&mut out, 0);
    }
    for (define, _) in flat {
        out.extend_from_slice(define.as_bytes());
        out.push(0);
    }
    out
}

/// The first of the two COFF linker members, which is [`sysv`] with the padding the reference writes.
///
/// The length it declares is the padded one rather than leaving the archive's own padding to do it,
/// which is a distinction no reader notices and is what `llvm-dlltool` writes. The object members
/// take the archive's newline instead.
fn first(flat: &[(&str, usize)]) -> Vec<u8> {
    let mut out = sysv(flat);
    // A zero, which reads as one more empty string and which the count above says nothing points at.
    pad(&mut out, 0);
    out
}

/// The second COFF linker member: an offset per member, then a member number per name, sorted.
///
/// Sorted by the bytes of the name rather than by anything locale-aware, because what reads it is a
/// linker bisecting the table and it compares bytes.
fn second(members: usize, flat: &[(&str, usize)]) -> Vec<u8> {
    let mut sorted: Vec<&(&str, usize)> = flat.iter().collect();
    sorted.sort_by(|one, two| one.0.as_bytes().cmp(two.0.as_bytes()));
    let mut out = Vec::new();
    u32le(&mut out, members as u32);
    for _ in 0..members {
        u32le(&mut out, 0);
    }
    u32le(&mut out, flat.len() as u32);
    for (_, at) in &sorted {
        // One based, and it is a member number rather than an offset, which is the whole difference
        // between this table and the one above.
        u16le(&mut out, *at as u16 + 1);
    }
    for (define, _) in &sorted {
        out.extend_from_slice(define.as_bytes());
        out.push(0);
    }
    pad(&mut out, 0);
    out
}

/// The name of the BSD index, which is a member like any other as far as the container goes.
const SYMDEF: &str = "__.SYMDEF";

/// A whole BSD archive: the index, then every member with its name in front of its body.
///
/// Laid out in one pass, since the index is the only member whose contents depend on where the
/// others land and its own length depends only on the names in it. So its length is worked out
/// first, every offset follows from it, and the bytes go out in order.
fn bsd(members: &[Member]) -> Result<Vec<u8>, Error> {
    // The names in the index, each once per member that defines it, in member order. Unsorted, which
    // is why the member is `__.SYMDEF` and not `__.SYMDEF SORTED`: ld64 reads both and builds its own
    // table either way, and member order is what `llvm-ar` writes.
    let mut strings = Vec::new();
    let mut entries: Vec<(u32, usize)> = Vec::new();
    for (at, one) in members.iter().enumerate() {
        for define in &one.defines {
            entries.push((strings.len() as u32, at));
            strings.extend_from_slice(define.as_bytes());
            strings.push(0);
        }
    }
    // The string table is padded inside its own count so that the index as a whole is a multiple of
    // eight, which keeps the first object aligned without the index needing a pad of its own.
    while strings.len() % 8 != 0 {
        strings.push(0);
    }
    let table = 4 + 8 * entries.len() + 4 + strings.len();

    let mut at = MAGIC.len();
    let index = at;
    at += HEADER + bsd_name(index, SYMDEF).len() + table;
    let mut offsets = Vec::with_capacity(members.len());
    for one in members {
        offsets.push(at);
        at += HEADER + bsd_name(at, &one.name).len() + eight(one.body.len());
    }
    if at > u32::MAX as usize {
        return Err(Error::TooBig { bytes: at });
    }

    let mut out = Vec::from(MAGIC);
    let name = bsd_name(index, SYMDEF);
    header(&mut out, &format!("#1/{}", name.len()), Mode::Zero, name.len() + table);
    out.extend_from_slice(&name);
    u32le(&mut out, (8 * entries.len()) as u32);
    for (string, member) in &entries {
        u32le(&mut out, *string);
        u32le(&mut out, offsets[*member] as u32);
    }
    u32le(&mut out, strings.len() as u32);
    out.extend_from_slice(&strings);
    for (one, at) in members.iter().zip(&offsets) {
        let name = bsd_name(*at, &one.name);
        let body = eight(one.body.len());
        header(&mut out, &format!("#1/{}", name.len()), Mode::Object, name.len() + body);
        out.extend_from_slice(&name);
        out.extend_from_slice(&one.body);
        // Newlines, as `llvm-ar` writes. A Mach-O reader stops where the load commands say the file
        // ends, so what is after that is never read.
        out.resize(out.len() + body - one.body.len(), b'\n');
    }
    Ok(out)
}

/// A member name as a BSD archive stores it after the header, with the zero bytes that bring the
/// member's body to an eight byte boundary when the header starts at `at`.
fn bsd_name(at: usize, name: &str) -> Vec<u8> {
    let mut out = name.as_bytes().to_vec();
    while (at + HEADER + out.len()) % 8 != 0 {
        out.push(0);
    }
    out
}

/// A length rounded up to the eight byte boundary a BSD member keeps.
fn eight(length: usize) -> usize {
    length.next_multiple_of(8)
}

/// Fills in the offsets of the big-endian index, one per name, pointing at the member that defines it.
fn resolve(body: &mut [u8], flat: &[(&str, usize)], offsets: &[usize]) {
    for (index, (_, member)) in flat.iter().enumerate() {
        let to = 4 + 4 * index;
        body[to..to + 4].copy_from_slice(&(offsets[*member] as u32).to_be_bytes());
    }
}

/// Fills in the offsets of the little-endian index, one per member, in member order.
fn number(body: &mut [u8], offsets: &[usize]) {
    for (index, offset) in offsets.iter().enumerate() {
        let to = 4 + 4 * index;
        body[to..to + 4].copy_from_slice(&(*offset as u32).to_le_bytes());
    }
}

/// One byte on the end of a member that needs one to make its length even.
///
/// The byte is the caller's choice because the kinds of member do not agree on it, and neither
/// choice means anything to a reader.
fn pad(body: &mut Vec<u8>, with: u8) {
    if body.len() % 2 == 1 {
        body.push(with);
    }
}

/// What goes in the four numeric fields of a member header.
///
/// Three shapes rather than a number, because the reference writers produce three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// A zero in every field, which the index members carry.
    Zero,
    /// Nothing at all, which the long names member carries.
    Blank,
    /// Zero for the time and the owner, and 644 for the mode, which every object carries.
    Object,
}

/// One member header and its body, padded to an even length.
///
/// Every numeric field but the mode is zero, and that is the determinism requirement rather than
/// laziness: a real modification time or a real uid would put the machine that built the library
/// into the library, and then two builds of the same thing would not be the same file.
fn member(out: &mut Vec<u8>, name: &str, mode: Mode, body: &[u8]) {
    header(out, name, mode, body.len());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        // A newline rather than a zero, which is what `ar` has always written and what keeps a text
        // member readable when somebody looks at the file with a pager.
        out.push(b'\n');
    }
}

/// One member header, saying the member is `size` bytes long.
fn header(out: &mut Vec<u8>, name: &str, mode: Mode, size: usize) {
    let (time, owner, mode) = match mode {
        Mode::Zero => ("0", "0", "0"),
        Mode::Blank => ("", "", ""),
        Mode::Object => ("0", "0", "644"),
    };
    let header =
        format!("{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n", name, time, owner, owner, mode, size);
    out.extend_from_slice(header.as_bytes());
}

/// A length rounded up to the even boundary a member starts on.
fn even(length: usize) -> usize {
    length + length % 2
}

fn u16le(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn u32le(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn u32be(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// Why an archive could not be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A member has no name, so nothing could list it and no diagnostic could name it.
    NoName,
    /// A name contains a zero byte, which is what terminates a name in an index.
    NameHasNul {
        /// The name.
        name: String,
    },
    /// More members than the COFF index can number.
    TooManyMembers {
        /// How many there were.
        members: usize,
    },
    /// An archive too large for a four byte offset to reach the end of.
    TooBig {
        /// How many bytes it came to.
        bytes: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoName => write!(
                f,
                "a member with no name cannot be written, since the name is how anything refers to \
                 it afterwards"
            ),
            Error::NameHasNul { name } => write!(
                f,
                "`{name}` contains a zero byte, which is what ends a name in the symbol index, so \
                 the index would stop there"
            ),
            Error::TooManyMembers { members } => write!(
                f,
                "{members} members is more than the 65535 a COFF symbol index can number, so the \
                 archive would have to be split"
            ),
            Error::TooBig { bytes } => write!(
                f,
                "the archive comes to {bytes} bytes and a symbol index reaches 4294967295, so the \
                 members past that point could not be found"
            ),
        }
    }
}

impl std::error::Error for Error {}

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M7b";

#[cfg(test)]
mod tests {
    use super::*;

    /// A member, with the body taken as the object this crate does not read.
    fn one(name: &str, body: &[u8], defines: &[&str]) -> Member {
        Member::new(name, body.to_vec())
            .defining(defines.iter().map(|define| (*define).to_owned()).collect())
    }

    /// Every member of an archive, as an offset, a name and a body, parsed by hand.
    ///
    /// By hand because the reader that would do it is a dependency this crate does not have, and
    /// because a test that walks the headers itself is a test that notices a header this writer got
    /// half a byte wrong.
    fn members(archive: &[u8]) -> Vec<(usize, String, Vec<u8>)> {
        assert_eq!(&archive[..8], MAGIC);
        let mut at = 8;
        let mut out = Vec::new();
        while at + HEADER <= archive.len() {
            let header = &archive[at..at + HEADER];
            assert_eq!(&header[58..60], b"`\n", "a member header at {at} is not terminated");
            let name =
                String::from_utf8(header[..16].to_vec()).expect("a name").trim_end().to_owned();
            let size: usize = String::from_utf8(header[48..58].to_vec())
                .expect("a size")
                .trim_end()
                .parse()
                .expect("a number");
            let body = archive[at + HEADER..at + HEADER + size].to_vec();
            out.push((at, name, body));
            at += HEADER + even(size);
        }
        assert_eq!(at, archive.len(), "the last member does not end where the file does");
        out
    }

    /// The big-endian index, as the pairs of name and member offset it holds.
    fn sysv_index(body: &[u8]) -> Vec<(String, usize)> {
        let count = u32::from_be_bytes(body[..4].try_into().expect("four bytes")) as usize;
        let mut names = body[4 + 4 * count..].split(|byte| *byte == 0);
        (0..count)
            .map(|index| {
                let at = 4 + 4 * index;
                let offset =
                    u32::from_be_bytes(body[at..at + 4].try_into().expect("four bytes")) as usize;
                let name = names.next().expect("a name");
                (String::from_utf8(name.to_vec()).expect("a name"), offset)
            })
            .collect()
    }

    #[test]
    fn an_archive_with_no_members_is_the_magic_and_nothing_else() {
        for flavour in [Flavour::Gnu, Flavour::Coff, Flavour::Bsd] {
            assert_eq!(write(flavour, &[]).expect("eight bytes"), MAGIC);
        }
    }

    /// A BSD member split into the name stored after its header and the body after that, with the
    /// place in the file the body starts.
    fn bsd_member(at: usize, header: &str, body: &[u8]) -> (String, usize, Vec<u8>) {
        let length: usize =
            header.strip_prefix("#1/").expect("a name after the header").parse().expect("a length");
        let name = body[..length].split(|byte| *byte == 0).next().expect("a name");
        (
            String::from_utf8(name.to_vec()).expect("a name"),
            at + HEADER + length,
            body[length..].to_vec(),
        )
    }

    /// Apple's index: a string offset and a member offset per name, then the strings.
    fn bsd_index(body: &[u8]) -> Vec<(String, usize)> {
        let word = |at: usize| {
            u32::from_le_bytes(body[at..at + 4].try_into().expect("four bytes")) as usize
        };
        let count = word(0) / 8;
        let strings = &body[4 + 8 * count + 4..];
        assert_eq!(word(4 + 8 * count), strings.len());
        (0..count)
            .map(|index| {
                let from = word(4 + 8 * index);
                let name = strings[from..].split(|byte| *byte == 0).next().expect("a name");
                (String::from_utf8(name.to_vec()).expect("a name"), word(8 + 8 * index))
            })
            .collect()
    }

    /// ld64 reads the index from `__.SYMDEF` and each object in place, so every body has to start on
    /// an eight byte boundary and every offset in the index has to be the header of the member that
    /// defines the name.
    #[test]
    fn a_bsd_archive_indexes_each_name_and_aligns_each_object() {
        let long = "a_member_name_longer_than_sixteen_bytes.o";
        let archive = write(
            Flavour::Bsd,
            &[
                one("f.o", b"odd", &["_f", "_g"]),
                one(long, b"twelve bytes", &["_h"]),
                one("empty.o", b"", &[]),
            ],
        )
        .expect("an archive");
        let found = members(&archive);
        assert_eq!(found.len(), 4);
        let parts: Vec<_> =
            found.iter().map(|(at, header, body)| bsd_member(*at, header, body)).collect();
        for (name, starts, _) in &parts {
            assert_eq!(starts % 8, 0, "{name} starts at {starts}");
        }
        assert_eq!(parts[0].0, SYMDEF);
        assert_eq!(parts[1].0, "f.o");
        assert_eq!(&parts[1].2[..3], b"odd");
        assert_eq!(parts[2].0, long);
        assert_eq!(&parts[2].2[..12], b"twelve bytes");
        assert_eq!(parts[3].0, "empty.o");
        assert_eq!(
            bsd_index(&parts[0].2),
            [
                ("_f".to_owned(), found[1].0),
                ("_g".to_owned(), found[1].0),
                ("_h".to_owned(), found[2].0)
            ]
        );
        // The same members twice give the same bytes, which the index is where it would not.
        let again = write(
            Flavour::Bsd,
            &[
                one("f.o", b"odd", &["_f", "_g"]),
                one(long, b"twelve bytes", &["_h"]),
                one("empty.o", b"", &[]),
            ],
        )
        .expect("an archive");
        assert_eq!(archive, again);
    }

    #[test]
    fn the_index_points_at_the_member_that_defines_each_name() {
        let archive = write(
            Flavour::Gnu,
            &[
                one("memcpy.o", b"first", &["memcpy", "memmove"]),
                one("memset.o", b"second", &["memset"]),
            ],
        )
        .expect("an archive");
        let members = members(&archive);
        assert_eq!(
            members.iter().map(|(_, name, _)| name.clone()).collect::<Vec<_>>(),
            ["/", "memcpy.o/", "memset.o/"]
        );
        let at: Vec<usize> = members.iter().map(|(at, _, _)| *at).collect();
        assert_eq!(
            sysv_index(&members[0].2),
            [
                ("memcpy".to_owned(), at[1]),
                ("memmove".to_owned(), at[1]),
                ("memset".to_owned(), at[2]),
            ]
        );
        // And the offsets the index gives really are where those members start, which is the part a
        // linker gets wrong silently if the writer counted a header wrong.
        assert_eq!(members[1].2.as_slice(), b"first");
        assert_eq!(members[2].2.as_slice(), b"second");
    }

    #[test]
    fn a_member_that_defines_nothing_is_in_the_archive_and_not_in_the_index() {
        let archive =
            write(Flavour::Gnu, &[one("empty.o", b"nothing", &[]), one("real.o", b"x", &["main"])])
                .expect("an archive");
        let members = members(&archive);
        assert_eq!(members.len(), 3);
        let index = sysv_index(&members[0].2);
        assert_eq!(index.len(), 1);
        assert_eq!(index[0].0, "main");
        assert_eq!(index[0].1, members[2].0);
    }

    #[test]
    fn a_short_name_is_in_the_header_and_a_long_one_is_in_the_names_member() {
        let long = "123456789012345.o";
        assert!(long.len() >= NAME, "the point of the name is that it does not fit");
        let archive = write(Flavour::Gnu, &[one("fits.o", b"a", &["a"]), one(long, b"b", &["b"])])
            .expect("an archive");
        let members = members(&archive);
        assert_eq!(
            members.iter().map(|(_, name, _)| name.clone()).collect::<Vec<_>>(),
            ["/", "//", "fits.o/", "/0"]
        );
        // The long names member holds the name the way GNU spells it, which is the same slash a
        // short name carries and a newline after it, and then the byte that makes the member even.
        assert_eq!(members[1].2, format!("{long}/\n\n").as_bytes());
    }

    #[test]
    fn the_same_long_name_twice_is_stored_once() {
        let name = "a-name-too-long-for-a-header.o";
        let archive = write(
            Flavour::Coff,
            &[one(name, b"a", &["a"]), one(name, b"b", &["b"]), one(name, b"c", &["c"])],
        )
        .expect("an archive");
        let members = members(&archive);
        let headers: Vec<String> = members.iter().map(|(_, name, _)| name.clone()).collect();
        assert_eq!(headers, ["/", "/", "//", "/0", "/0", "/0"]);
        // One copy of the string, zero terminated, which is how COFF ends a long name, and a newline
        // to make the member even.
        assert_eq!(members[2].2, format!("{name}\0\n").as_bytes());
    }

    #[test]
    fn an_odd_body_is_padded_and_the_size_it_declares_is_the_real_one() {
        let archive = write(Flavour::Gnu, &[one("odd.o", b"abc", &["abc"])]).expect("an archive");
        let members = members(&archive);
        assert_eq!(members[1].2.as_slice(), b"abc");
        // The pad byte is there, after the three bytes the header declared.
        let at = members[1].0 + HEADER + 3;
        assert_eq!(archive[at], b'\n');
        assert_eq!(archive.len(), at + 1);
    }

    #[test]
    fn nothing_in_a_member_header_carries_a_time_or_an_owner() {
        let archive = write(Flavour::Gnu, &[one("a.o", b"x", &["a"])]).expect("an archive");
        for (at, _, _) in members(&archive) {
            let header = &archive[at..at + HEADER];
            let time = String::from_utf8(header[16..28].to_vec()).expect("a time");
            let uid = String::from_utf8(header[28..34].to_vec()).expect("a uid");
            let gid = String::from_utf8(header[34..40].to_vec()).expect("a gid");
            assert_eq!(time.trim_end(), "0", "a header at {at} carries a time");
            assert_eq!(uid.trim_end(), "0", "a header at {at} carries a uid");
            assert_eq!(gid.trim_end(), "0", "a header at {at} carries a gid");
        }
    }

    #[test]
    fn the_same_members_twice_are_the_same_bytes() {
        let input = [one("a.o", b"x", &["a", "b"]), one("b.o", b"yy", &["c"])];
        for flavour in [Flavour::Gnu, Flavour::Coff] {
            let once = write(flavour, &input).expect("an archive");
            let again = write(flavour, &input).expect("an archive");
            assert_eq!(once, again);
        }
    }

    #[test]
    fn the_coff_flavour_writes_two_indexes_and_the_second_is_sorted() {
        let archive = write(
            Flavour::Coff,
            &[one("a.o", b"x", &["zeta", "alpha"]), one("b.o", b"y", &["mu"])],
        )
        .expect("an archive");
        let members = members(&archive);
        assert_eq!(
            members.iter().map(|(_, name, _)| name.clone()).collect::<Vec<_>>(),
            ["/", "/", "a.o/", "b.o/"]
        );
        let at: Vec<usize> = members.iter().map(|(at, _, _)| *at).collect();
        // The first index is in member order, names and all.
        assert_eq!(
            sysv_index(&members[0].2),
            [("zeta".to_owned(), at[2]), ("alpha".to_owned(), at[2]), ("mu".to_owned(), at[3]),]
        );
        // The second is little-endian: a member offset each, then a member number per name with the
        // names sorted, then the names in that order.
        let body = &members[1].2;
        let count = u32::from_le_bytes(body[..4].try_into().expect("four bytes")) as usize;
        assert_eq!(count, 2);
        for (index, want) in [at[2], at[3]].into_iter().enumerate() {
            let from = 4 + 4 * index;
            let got = u32::from_le_bytes(body[from..from + 4].try_into().expect("four bytes"));
            assert_eq!(got as usize, want);
        }
        let names = 4 + 4 * count;
        let symbols = u32::from_le_bytes(body[names..names + 4].try_into().expect("four bytes"));
        assert_eq!(symbols, 3);
        let numbers: Vec<u16> = (0..3)
            .map(|index| {
                let from = names + 4 + 2 * index;
                u16::from_le_bytes(body[from..from + 2].try_into().expect("two bytes"))
            })
            .collect();
        // alpha and zeta are in the first member and mu is in the second, and sorted they come out
        // alpha, mu, zeta.
        assert_eq!(numbers, [1, 2, 1]);
        let strings = &body[names + 4 + 2 * 3..];
        assert_eq!(strings, b"alpha\0mu\0zeta\0");
    }

    #[test]
    fn a_member_with_no_name_is_refused() {
        let members = [Member::new("", vec![1])];
        assert_eq!(write(Flavour::Gnu, &members), Err(Error::NoName));
    }

    #[test]
    fn a_name_with_a_zero_byte_in_it_is_refused() {
        let named = [Member::new("a\0.o", vec![1])];
        assert_eq!(
            write(Flavour::Gnu, &named),
            Err(Error::NameHasNul { name: "a\0.o".to_owned() })
        );
        let defines = [one("a.o", b"x", &["mem\0cpy"])];
        assert_eq!(
            write(Flavour::Gnu, &defines),
            Err(Error::NameHasNul { name: "mem\0cpy".to_owned() })
        );
    }

    #[test]
    fn more_members_than_the_coff_index_can_number_are_refused() {
        let members = vec![Member::new("a.o", Vec::new()); u16::MAX as usize + 1];
        assert_eq!(write(Flavour::Coff, &members), Err(Error::TooManyMembers { members: 65536 }));
        // The GNU index numbers nothing, so the same input is an archive rather than a refusal.
        assert!(write(Flavour::Gnu, &members).is_ok());
    }

    #[test]
    fn every_error_says_something_a_person_can_act_on() {
        let all = [
            Error::NoName,
            Error::NameHasNul { name: "a\0b".to_owned() },
            Error::TooManyMembers { members: 70000 },
            Error::TooBig { bytes: 5_000_000_000 },
        ];
        for error in all {
            let said = error.to_string();
            assert!(said.len() > 40, "{error:?} says too little: {said}");
            assert!(!said.ends_with('.'), "{error:?} ends with a full stop");
            assert!(
                said.chars().next().is_some_and(|first| !first.is_uppercase()),
                "{error:?} starts with a capital"
            );
        }
    }
}
