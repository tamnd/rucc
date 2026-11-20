//! The compound file, which is the container an MSI is.
//!
//! Design: `spec/cross-compile/13-distribution.md` section 13.4. An MSI is a small file system in a
//! file: a header, a table saying which sector follows which, a directory of named streams, and the
//! sectors themselves. Microsoft called it structured storage and shipped it in 1992 for OLE, which
//! is why it reads like a floppy disk. What sits inside it, for an MSI, is a relational database,
//! and that is [`crate::msi`] rather than this. This gets the streams out by name.
//!
//! # Two sector sizes and two allocation tables
//!
//! A version 3 file has 512 byte sectors and a version 4 file has 4096 byte sectors, and the header
//! is 512 bytes either way, so sector zero begins one sector in rather than 512 bytes in. Both are
//! read because both are out there, and the SDK's MSIs are version 4.
//!
//! A stream shorter than the cutoff, which is 4096 bytes, is not given sectors of its own. It goes
//! in the mini stream, which is one ordinary stream hanging off the root entry, cut into 64 byte
//! pieces with an allocation table of its own. That is not an optimisation worth skipping: the SDK
//! MSI measured for this has 32 streams and 24 of them are in there, including the string pool and
//! most of the tables, so a reader without it gets almost nothing.
//!
//! # The directory is a tree and is read as one
//!
//! The entries are a red black tree of siblings per storage, and a storage's entry points at the
//! first of its children. Every MSI is flat, with every stream in the root storage, so reading the
//! entries as a plain array would work on every file this compiler will ever open. It is walked as
//! the tree it is anyway, because the cost is one explicit stack, and because the failure of the
//! other way is not an error but a name from one storage answering for a stream in another.
//!
//! # What is not here
//!
//! Writing, and the parts of the format that only a writer cares about: the free sectors, the
//! transaction signature, the class ids and the timestamps on each entry. Nothing here allocates.

use std::fmt;

/// The eight bytes every compound file starts with.
const SIGNATURE: [u8; 8] = [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1];

/// Little endian, which is the only byte order the format was ever used in.
const ORDER: u16 = 0xfffe;

/// The largest sector number that means a sector. Everything above it is a marker: the end of a
/// chain, a free sector, or a sector holding allocation table rather than anything a stream wants.
/// A reader only has to tell a sector from a marker, so it compares against this rather than
/// naming the four of them.
const MAX: u32 = 0xffff_fffa;

/// How large a mini sector is, which the format fixes rather than stores usefully.
const MINI: u16 = 6;

/// The size under which a stream goes in the mini stream instead of getting sectors of its own.
const CUTOFF: u32 = 4096;

/// How many sector numbers the header itself carries before the chain of them starts.
const IN_HEADER: usize = 109;

/// How large one directory entry is.
const ENTRY: usize = 128;

/// A directory entry that is a stream.
const STREAM: u8 = 2;
/// A directory entry that is a storage, which is to say a directory.
const STORAGE: u8 = 1;
/// The root, which is a storage and is also where the mini stream hangs.
const ROOT: u8 = 5;

/// Why a compound file could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CfbError {
    /// It does not start with the signature, so it is not a compound file.
    NotACfb,
    /// A version other than 3 or 4.
    Version {
        /// The major version it claims.
        major: u16,
    },
    /// A header field the format fixes has something else in it.
    Malformed {
        /// Which one.
        what: &'static str,
    },
    /// A sector or a field is not in the file.
    Truncated {
        /// What was being read.
        what: &'static str,
    },
    /// A chain of sectors or a walk of the directory does not end, which a file that was written
    /// correctly cannot do and a file somebody wrote to break a reader can.
    Cycle {
        /// What was being followed.
        what: &'static str,
    },
    /// A name is not the UTF-16 the format says it is.
    Name,
}

impl fmt::Display for CfbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CfbError::NotACfb => {
                write!(f, "it does not begin with the compound file signature")
            }
            CfbError::Version { major } => {
                write!(
                    f,
                    "this says it is a version {major} compound file, and 3 and 4 are the ones read here"
                )
            }
            CfbError::Malformed { what } => write!(f, "{what}"),
            CfbError::Truncated { what } => write!(f, "the file ends in the middle of {what}"),
            CfbError::Cycle { what } => write!(f, "{what} does not end"),
            CfbError::Name => write!(f, "a name is not UTF-16"),
        }
    }
}

impl std::error::Error for CfbError {}

/// One stream in a compound file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    /// The name, with a forward slash between a storage and what is in it. An MSI is flat, so in
    /// practice there is never a slash, and the names are the encoded ones: [`crate::msi`] is what
    /// turns them back into `_StringPool` and `File`.
    pub name: String,
    /// How many bytes it holds.
    pub size: u64,
    /// Where its chain starts, in whichever of the two allocation tables applies to it.
    start: u32,
}

/// A compound file that has been read as far as its directory.
#[derive(Debug)]
pub struct Cfb<'a> {
    bytes: &'a [u8],
    /// How large a sector is, which is 512 or 4096.
    sector: usize,
    /// Which sector follows which, for the streams that have sectors of their own.
    fat: Vec<u32>,
    /// The same for the mini stream, in 64 byte pieces.
    minifat: Vec<u32>,
    /// The mini stream itself, read out once because everything small is a slice of it.
    mini: Vec<u8>,
    streams: Vec<Stream>,
}

impl<'a> Cfb<'a> {
    /// Read a compound file's directory.
    ///
    /// The allocation tables and the mini stream are read here too, because every question a caller
    /// asks afterwards needs all three and none of them is large: the tables are four bytes per
    /// sector of the file, and the mini stream is everything in the file under four kilobytes.
    ///
    /// # Errors
    ///
    /// [`CfbError`] for a file that is not a compound file, is a version this does not read, was
    /// cut short, or has a chain in it that does not end.
    pub fn read(bytes: &'a [u8]) -> Result<Cfb<'a>, CfbError> {
        if bytes.get(..8) != Some(&SIGNATURE) {
            return Err(CfbError::NotACfb);
        }
        let head = |at: usize| -> Result<u16, CfbError> {
            let two = bytes.get(at..at + 2).ok_or(CfbError::Truncated { what: "the header" })?;
            Ok(u16::from_le_bytes([two[0], two[1]]))
        };
        let word = |at: usize| -> Result<u32, CfbError> {
            let four = bytes.get(at..at + 4).ok_or(CfbError::Truncated { what: "the header" })?;
            Ok(u32::from_le_bytes([four[0], four[1], four[2], four[3]]))
        };

        let major = head(26)?;
        if head(28)? != ORDER {
            return Err(CfbError::Malformed { what: "this is not a little endian compound file" });
        }
        let shift = head(30)?;
        // The two go together: a version 3 file has 512 byte sectors and a version 4 file has 4096
        // byte ones, and the format does not allow the pair to be mixed.
        let sector = match (major, shift) {
            (3, 9) | (4, 12) => 1usize << shift,
            (3 | 4, _) => {
                return Err(CfbError::Malformed {
                    what: "the sector size and the version do not go together",
                });
            }
            (major, _) => return Err(CfbError::Version { major }),
        };
        if head(32)? != MINI {
            return Err(CfbError::Malformed { what: "a mini sector is not 64 bytes" });
        }
        if word(56)? != CUTOFF {
            return Err(CfbError::Malformed { what: "the mini stream cutoff is not 4096" });
        }

        let dir_at = word(48)?;
        let minifat_at = word(60)?;
        let difat_at = word(68)?;

        // The sector numbers of the allocation table are in the header as far as 109 of them, and
        // then in a chain of sectors each of which ends with the number of the next.
        let mut which = Vec::with_capacity(IN_HEADER);
        for n in 0..IN_HEADER {
            which.push(word(76 + n * 4)?);
        }
        let per = sector / 4;
        let mut at = difat_at;
        let mut guard = bytes.len() / sector + 2;
        while at <= MAX {
            let it = sector_at(bytes, sector, at, "the sector number table")?;
            for n in 0..per - 1 {
                which.push(le(it, n * 4));
            }
            at = le(it, (per - 1) * 4);
            guard = guard
                .checked_sub(1)
                .ok_or(CfbError::Cycle { what: "the chain of sector number tables" })?;
        }

        let mut fat = Vec::new();
        for at in which.into_iter().take_while(|&at| at <= MAX) {
            let it = sector_at(bytes, sector, at, "the sector table")?;
            for n in 0..per {
                fat.push(le(it, n * 4));
            }
        }

        let mut file =
            Cfb { bytes, sector, fat, minifat: Vec::new(), mini: Vec::new(), streams: Vec::new() };

        // The mini allocation table is an ordinary stream, so it is read the same way as anything
        // else, and it has to come before anything small can be.
        let mini = file.sectors(minifat_at, usize::MAX, "the mini sector table")?;
        file.minifat = (0..mini.len() / 4).map(|n| le(&mini, n * 4)).collect();

        // So is the directory, and the root entry in it is what says where the mini stream is, so
        // the directory has to be read before the mini stream and cannot be a small stream itself.
        let dir = file.sectors(dir_at, usize::MAX, "the directory")?;
        let root = dir.get(..ENTRY).ok_or(CfbError::Truncated { what: "the root entry" })?;
        if root[66] != ROOT {
            return Err(CfbError::Malformed { what: "the first directory entry is not the root" });
        }
        let size = usize::try_from(le64(root, 120)).unwrap_or(usize::MAX);
        file.mini = file.sectors(le(root, 116), size, "the mini stream")?;
        file.streams = walk(&dir)?;
        Ok(file)
    }

    /// Every stream in the file, in the order the directory walk reached them.
    #[must_use]
    pub fn streams(&self) -> &[Stream] {
        &self.streams
    }

    /// The stream with this exact name, if there is one.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&Stream> {
        self.streams.iter().find(|stream| stream.name == name)
    }

    /// One stream's bytes.
    ///
    /// # Errors
    ///
    /// [`CfbError`] for a stream whose chain leaves the file or does not end.
    pub fn contents(&self, stream: &Stream) -> Result<Vec<u8>, CfbError> {
        let size = usize::try_from(stream.size).unwrap_or(usize::MAX);
        if stream.size >= u64::from(CUTOFF) {
            return self.sectors(stream.start, size, "a stream");
        }
        // A small stream is cut into 64 byte pieces of the mini stream, which is already in hand.
        let mut out = Vec::with_capacity(size.min(self.mini.len()));
        let mut at = stream.start;
        let mut guard = self.minifat.len() + 1;
        while at <= MAX && out.len() < size {
            let from = (at as usize) << MINI;
            let piece = self
                .mini
                .get(from..from + (1 << MINI))
                .ok_or(CfbError::Truncated { what: "a small stream" })?;
            out.extend_from_slice(piece);
            at = *self
                .minifat
                .get(at as usize)
                .ok_or(CfbError::Truncated { what: "the mini sector table" })?;
            guard = guard.checked_sub(1).ok_or(CfbError::Cycle { what: "a small stream" })?;
        }
        out.truncate(size);
        Ok(out)
    }

    /// Follow a chain of ordinary sectors and return what it holds, up to `size` bytes.
    ///
    /// [`usize::MAX`] for `size` means everything the chain covers, which is what the directory and
    /// the allocation tables want, since the only thing saying how long those are is the chain.
    fn sectors(&self, start: u32, size: usize, what: &'static str) -> Result<Vec<u8>, CfbError> {
        let mut out = Vec::new();
        let mut at = start;
        let mut guard = self.fat.len() + 1;
        while at <= MAX && out.len() < size {
            out.extend_from_slice(sector_at(self.bytes, self.sector, at, what)?);
            at = *self.fat.get(at as usize).ok_or(CfbError::Truncated { what })?;
            guard = guard.checked_sub(1).ok_or(CfbError::Cycle { what })?;
        }
        if size != usize::MAX {
            if out.len() < size {
                return Err(CfbError::Truncated { what });
            }
            out.truncate(size);
        }
        Ok(out)
    }
}

/// Where a sector is in the file.
///
/// Sector zero begins one whole sector in, not 512 bytes in, so a version 4 file has 3584 bytes of
/// padding after its header. That is the one piece of arithmetic in this format that a reader gets
/// wrong once and only once.
fn sector_at<'a>(
    bytes: &'a [u8],
    sector: usize,
    at: u32,
    what: &'static str,
) -> Result<&'a [u8], CfbError> {
    let from = (at as usize + 1).checked_mul(sector).ok_or(CfbError::Truncated { what })?;
    bytes.get(from..from + sector).ok_or(CfbError::Truncated { what })
}

/// A four byte number, from somewhere that has already been shown to be long enough.
fn le(bytes: &[u8], at: usize) -> u32 {
    let four = bytes.get(at..at + 4).unwrap_or(&[0xff, 0xff, 0xff, 0xff]);
    u32::from_le_bytes([four[0], four[1], four[2], four[3]])
}

/// An eight byte number, the same way. A stream's size is the only one of those in this format, and
/// a short entry gives the largest number there is rather than a panic, which the caller then
/// refuses for being longer than the file.
fn le64(bytes: &[u8], at: usize) -> u64 {
    let eight = bytes.get(at..at + 8).unwrap_or(&[0xff; 8]);
    let mut out = [0u8; 8];
    out.copy_from_slice(eight);
    u64::from_le_bytes(out)
}

/// Walk the directory tree and collect the streams in it with their full names.
///
/// The siblings of one storage are a red black tree ordered by name, and a storage points at the
/// first of its children. What the colours are for is keeping a writer's insertions balanced, and a
/// reader has no use for them at all, so this walks the tree as an ordinary unbalanced one.
fn walk(dir: &[u8]) -> Result<Vec<Stream>, CfbError> {
    let count = dir.len() / ENTRY;
    let entry = |n: u32| -> Option<&[u8]> {
        let n = n as usize;
        (n < count).then(|| &dir[n * ENTRY..(n + 1) * ENTRY])
    };
    let mut out = Vec::new();
    // The root's own child, since the root itself is the storage everything hangs under and its
    // name is not part of any path.
    let mut todo = vec![(le(&dir[..ENTRY], 76), String::new())];
    // Every entry may be reached once. A file whose tree points back at itself is refused rather
    // than walked forever, and the bound is what refuses it.
    let mut guard = count + 1;
    while let Some((n, under)) = todo.pop() {
        let Some(it) = entry(n) else { continue };
        guard = guard.checked_sub(1).ok_or(CfbError::Cycle { what: "the directory" })?;
        let len = usize::from(u16::from_le_bytes([it[64], it[65]]));
        // The length counts the terminating NUL, which is two bytes of it.
        let name = it.get(..len.saturating_sub(2).min(64)).ok_or(CfbError::Name)?;
        let pairs = name.chunks_exact(2);
        let name: Vec<u16> = pairs.map(|two| u16::from_le_bytes([two[0], two[1]])).collect();
        let name = String::from_utf16(&name).map_err(|_| CfbError::Name)?;
        let name = if under.is_empty() { name } else { format!("{under}/{name}") };

        todo.push((le(it, 68), under.clone())); // The left sibling, in this same storage.
        todo.push((le(it, 72), under)); // And the right one.
        match it[66] {
            STREAM => out.push(Stream { size: le64(it, 120), start: le(it, 116), name }),
            STORAGE => todo.push((le(it, 76), name)),
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{CUTOFF, Cfb, CfbError, ENTRY, MINI, ORDER, ROOT, SIGNATURE, STORAGE, STREAM};

    /// How large a sector is in the files these tests build, which is the version 3 size.
    const SECTOR: usize = 512;

    /// The three markers a writer has to put down, which a reader only has to tell from a sector.
    const FREE: u32 = 0xffff_ffff;
    const END: u32 = 0xffff_fffe;
    const FATSECT: u32 = 0xffff_fffd;

    /// Somewhere to put sectors, which hands back the number of the first one of a run.
    struct Room {
        sectors: Vec<[u8; SECTOR]>,
        fat: Vec<u32>,
    }

    impl Room {
        /// Write `data` into as many sectors as it takes and chain them together.
        fn put(&mut self, data: &[u8]) -> u32 {
            if data.is_empty() {
                return END;
            }
            let start = self.sectors.len() as u32;
            for piece in data.chunks(SECTOR) {
                let mut one = [0u8; SECTOR];
                one[..piece.len()].copy_from_slice(piece);
                let at = self.sectors.len();
                self.sectors.push(one);
                self.fat[at] = at as u32 + 1;
            }
            self.fat[self.sectors.len() - 1] = END;
            start
        }
    }

    /// One directory entry.
    fn entry(name: &str, kind: u8, right: u32, child: u32, start: u32, size: u64) -> [u8; ENTRY] {
        let mut it = [0u8; ENTRY];
        let utf16: Vec<u16> = name.encode_utf16().collect();
        for (n, ch) in utf16.iter().enumerate() {
            it[n * 2..n * 2 + 2].copy_from_slice(&ch.to_le_bytes());
        }
        // The length the format wants counts the terminating NUL, which is two bytes of it.
        it[64..66].copy_from_slice(&((utf16.len() as u16 + 1) * 2).to_le_bytes());
        it[66] = kind;
        it[68..72].copy_from_slice(&FREE.to_le_bytes()); // No left sibling, ever, here.
        it[72..76].copy_from_slice(&right.to_le_bytes());
        it[76..80].copy_from_slice(&child.to_le_bytes());
        it[116..120].copy_from_slice(&start.to_le_bytes());
        it[120..128].copy_from_slice(&size.to_le_bytes());
        it
    }

    /// Lay out a version 3 compound file by hand, with the streams given and optionally a storage
    /// to put them in rather than the root.
    ///
    /// The sibling tree of a storage is built as a list down the right, which is a legal tree and a
    /// badly balanced one, and a reader cannot tell the difference because the colours are only
    /// there for the writer. One sector of allocation table, which is 128 entries, so these files
    /// are at most 64 kilobytes.
    fn compound(streams: &[(&str, Vec<u8>)], under: Option<&str>) -> Vec<u8> {
        let mut room = Room { sectors: vec![[0; SECTOR]], fat: vec![FREE; SECTOR / 4] };
        room.fat[0] = FATSECT; // Sector zero is the allocation table itself.

        // The big ones get sectors of their own and the small ones are laid end to end in the mini
        // stream, on a 64 byte boundary each, which is what the format asks for.
        let mut mini = Vec::new();
        let mut placed = Vec::new();
        for (name, bytes) in streams {
            let small = (bytes.len() as u32) < CUTOFF;
            let start = if small {
                let start = mini.len() / 64;
                mini.extend_from_slice(bytes);
                while mini.len() % 64 != 0 {
                    mini.push(0);
                }
                start as u32
            } else {
                room.put(bytes)
            };
            placed.push((*name, start, bytes.len() as u64));
        }

        let mini_at = room.put(&mini);
        let pieces = mini.len() / 64;
        let mut minifat = vec![FREE; SECTOR / 4];
        for (n, slot) in minifat.iter_mut().take(pieces).enumerate() {
            *slot = if n + 1 == pieces { END } else { n as u32 + 1 };
        }
        let mut table = Vec::new();
        for n in &minifat {
            table.extend_from_slice(&n.to_le_bytes());
        }
        let minifat_at = if pieces == 0 { END } else { room.put(&table) };

        // The root, then the storage if there is one, then a stream each.
        let mut dir = Vec::new();
        let first = 1;
        dir.extend_from_slice(&entry("Root Entry", ROOT, FREE, first, mini_at, mini.len() as u64));
        if let Some(storage) = under {
            dir.extend_from_slice(&entry(storage, STORAGE, FREE, 2, 0, 0));
        }
        let base = dir.len() / ENTRY;
        for (n, (name, start, size)) in placed.iter().enumerate() {
            let right = if n + 1 == placed.len() { FREE } else { (base + n + 1) as u32 };
            dir.extend_from_slice(&entry(name, STREAM, right, FREE, *start, *size));
        }
        while dir.len() % SECTOR != 0 {
            dir.push(0);
        }
        let dir_at = room.put(&dir);

        let mut out = vec![0u8; SECTOR];
        out[..8].copy_from_slice(&SIGNATURE);
        out[26..28].copy_from_slice(&3u16.to_le_bytes()); // Version 3.
        out[28..30].copy_from_slice(&ORDER.to_le_bytes());
        out[30..32].copy_from_slice(&9u16.to_le_bytes()); // 512 byte sectors.
        out[32..34].copy_from_slice(&MINI.to_le_bytes());
        out[44..48].copy_from_slice(&1u32.to_le_bytes()); // One allocation table sector.
        out[48..52].copy_from_slice(&dir_at.to_le_bytes());
        out[56..60].copy_from_slice(&CUTOFF.to_le_bytes());
        out[60..64].copy_from_slice(&minifat_at.to_le_bytes());
        out[64..68].copy_from_slice(&1u32.to_le_bytes());
        out[68..72].copy_from_slice(&END.to_le_bytes()); // No chain of table sectors.
        for n in 0..109 {
            let value = if n == 0 { 0 } else { FREE };
            out[76 + n * 4..80 + n * 4].copy_from_slice(&value.to_le_bytes());
        }
        let mut table = [0u8; SECTOR];
        for n in 0..SECTOR / 4 {
            table[n * 4..n * 4 + 4].copy_from_slice(&room.fat[n].to_le_bytes());
        }
        room.sectors[0] = table;
        for one in &room.sectors {
            out.extend_from_slice(one);
        }
        out
    }

    #[test]
    fn a_small_stream_comes_out_of_the_mini_stream() {
        // Which is where the string pool and most of the tables of a real MSI are, so a reader that
        // skipped this would get almost nothing out of one.
        let bytes = compound(&[("one", b"hello".to_vec()), ("two", vec![7; 300])], None);
        let file = Cfb::read(&bytes).expect("a file this laid out itself");
        assert_eq!(file.streams().len(), 2);
        let one = file.find("one").expect("the first");
        assert_eq!(file.contents(one).expect("its bytes"), b"hello");
        let two = file.find("two").expect("the second");
        assert_eq!(file.contents(two).expect("its bytes"), vec![7; 300]);
    }

    #[test]
    fn a_stream_over_the_cutoff_gets_sectors_of_its_own() {
        // Four thousand ninety six is the cutoff and it is not inclusive, so this is the smallest
        // stream that is not a small stream.
        let big: Vec<u8> = (0..CUTOFF).map(|n| (n % 251) as u8).collect();
        let bytes = compound(&[("small", b"x".to_vec()), ("big", big.clone())], None);
        let file = Cfb::read(&bytes).expect("a file this laid out itself");
        assert_eq!(file.contents(file.find("big").expect("it")).expect("its bytes"), big);
        assert_eq!(file.contents(file.find("small").expect("it")).expect("its bytes"), b"x");
    }

    #[test]
    fn a_stream_inside_a_storage_carries_the_storage_in_its_name() {
        // No MSI is shaped this way, which is the reason to test it: the tree is walked as a tree
        // so that a name in one storage cannot answer for a stream in another.
        let bytes = compound(&[("inner", b"deep".to_vec())], Some("Store"));
        let file = Cfb::read(&bytes).expect("a file this laid out itself");
        assert_eq!(file.streams().len(), 1);
        assert!(file.find("inner").is_none(), "the bare name is not the name");
        let it = file.find("Store/inner").expect("the name with its storage on it");
        assert_eq!(file.contents(it).expect("its bytes"), b"deep");
    }

    #[test]
    fn what_is_not_a_compound_file_says_so() {
        assert_eq!(Cfb::read(b"").unwrap_err(), CfbError::NotACfb);
        assert_eq!(Cfb::read(b"MSCF\0\0\0\0").unwrap_err(), CfbError::NotACfb);
    }

    #[test]
    fn a_version_and_a_sector_size_that_do_not_go_together_are_refused() {
        let good = compound(&[("one", b"hello".to_vec())], None);

        let mut wrong = good.clone();
        wrong[26] = 5;
        assert_eq!(Cfb::read(&wrong).unwrap_err(), CfbError::Version { major: 5 });

        // Version 3 with 4096 byte sectors, which the format does not allow even though both
        // numbers are ones it uses.
        let mut mixed = good.clone();
        mixed[30] = 12;
        assert!(matches!(Cfb::read(&mixed), Err(CfbError::Malformed { .. })));

        // And a byte order that was never used by anything. The marker is 0xfffe, so the byte that
        // has to change is the low one.
        let mut order = good;
        order[28] = 0xff;
        assert!(matches!(Cfb::read(&order), Err(CfbError::Malformed { .. })));
    }

    #[test]
    fn a_chain_that_points_at_itself_is_refused_rather_than_followed() {
        // The directory is the chain a reader has to follow to its end rather than to a length,
        // because the only thing saying how long a directory is is the chain itself. So it is the
        // one a file that points back at itself would spin a reader on forever.
        let mut bytes = compound(&[("one", b"hello".to_vec())], None);
        // With one small stream the sectors are the allocation table, the mini stream, the mini
        // allocation table and the directory, so the directory is sector three and the entry that
        // ends its chain is twelve bytes into the table, which is itself one sector in.
        let at = SECTOR + 3 * 4;
        assert_eq!(bytes[at..at + 4], END.to_le_bytes(), "the directory chain ends here");
        bytes[at..at + 4].copy_from_slice(&3u32.to_le_bytes());
        let why = Cfb::read(&bytes).unwrap_err();
        assert!(matches!(why, CfbError::Cycle { .. }), "{why}");
    }

    #[test]
    fn a_file_cut_short_says_so_rather_than_returning_what_it_has() {
        // The directory is the last thing these files lay down, so losing the tail of one loses
        // the list of what is in it, and that is a refusal rather than an empty file.
        let bytes = compound(&[("one", vec![3; 5000])], None);
        let file = Cfb::read(&bytes).expect("a file this laid out itself");
        assert_eq!(file.contents(file.find("one").expect("it")).expect("whole").len(), 5000);
        let short = bytes[..bytes.len() - SECTOR].to_vec();
        assert!(matches!(Cfb::read(&short), Err(CfbError::Truncated { .. })));
    }

    #[test]
    fn a_chain_that_leaves_the_file_is_refused_rather_than_read_past() {
        let bytes = compound(&[("one", vec![3; 5000])], None);
        let file = Cfb::read(&bytes).expect("a file this laid out itself");
        let one = file.find("one").expect("the stream").clone();
        // The allocation table is sector zero, so its second entry is at 512 plus four, and it is
        // what says where this stream carries on after its first sector.
        let mut past = bytes;
        past[SECTOR + 4..SECTOR + 8].copy_from_slice(&200u32.to_le_bytes());
        let file = Cfb::read(&past).expect("the directory is untouched");
        let why = file.contents(&one).unwrap_err();
        assert!(matches!(why, CfbError::Truncated { .. }), "{why}");
    }
}
