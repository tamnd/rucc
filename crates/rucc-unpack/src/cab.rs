//! The cabinet container, which is where an MSI keeps what it installs.
//!
//! Design: `spec/cross-compile/13-distribution.md` section 13.4. The Windows SDK arrives as MSIs,
//! and an MSI holds almost none of its own bytes: it is a database saying which cabinet each file
//! is in and what it is called there, and the cabinets sit beside it. So the headers a compiler
//! needs are in here, and the names they are in here under are `fil` followed by a hash, which is
//! why the MSI reader has to come too.
//!
//! # A folder is the unit, not a file
//!
//! A cabinet's folder is not a directory. It is one compressed stream, and the files in it are
//! ranges of what that stream produces, so getting one file means decompressing everything in front
//! of it. There is no way around that and no reader anywhere does better. What this offers instead
//! is [`Cab::folder`], which hands back the whole stream once, so a caller who wants four files out
//! of one folder pays for the folder once rather than four times.
//!
//! # MSZIP is deflate, and its window is the folder
//!
//! A folder's blocks each hold at most thirty two kilobytes and each is its own deflate stream,
//! with the two bytes `CK` in front of it. The catch is that a block's back references reach into
//! the thirty two kilobytes the block before it produced, so the blocks are one stream after all as
//! far as history goes. [`crate::inflate::inflate_into`] appends to a buffer the caller owns and
//! that buffer is the window, which is the whole reason it is shaped that way.
//!
//! Compression type 1, which is MSZIP, is what all four SDK cabinets sampled for this use, from the
//! eighteen kilobyte one up to the fifty five megabyte one. Type 0, which is no compression, is
//! read too because it is four lines. Quantum and LZX are not, and the error says so by name rather
//! than by number.
//!
//! # What is not here
//!
//! Cabinets that span several files. The format can carry a file that starts in one cabinet and
//! finishes in the next, for floppy disks, and a file marked that way is refused rather than
//! guessed at. The SDK does not use it: every payload cabinet stands alone.

use std::fmt;

use crate::inflate::{self, InflateError};

/// The four bytes every cabinet starts with.
const SIGNATURE: [u8; 4] = *b"MSCF";

/// The only version Microsoft has published, major then minor.
const VERSION: (u8, u8) = (1, 3);

/// Bit 0 of the header flags, meaning this cabinet continues one before it.
const PREV: u16 = 0x0001;
/// Bit 1, meaning this cabinet continues into one after it.
const NEXT: u16 = 0x0002;
/// Bit 2, meaning the three reserve sizes are in the header, and with them the reserve areas in
/// every folder entry and every block that those sizes give the length of.
const RESERVE: u16 = 0x0004;

/// The low four bits of a folder's compression field, which is the part that says what it is.
const KIND: u16 = 0x000f;

/// The magic in front of each MSZIP block.
const CK: [u8; 2] = *b"CK";

/// The most one block may produce, which is also how far back the next one can reach.
const BLOCK: usize = 32768;

/// The first of the folder numbers that mean the file is not all in this cabinet. The other two are
/// above it, so the test is a comparison rather than a list.
const CONTINUED: u16 = 0xfffd;

/// Bit 7 of a file's attributes, meaning its name is UTF-8 rather than whatever code page the
/// machine that wrote it was using.
const NAME_IS_UTF: u16 = 0x80;

/// Why a cabinet could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CabError {
    /// It does not start with `MSCF`, so it is not a cabinet.
    NotACab,
    /// A record or a field runs off the end of the file.
    Truncated {
        /// What was being read.
        what: &'static str,
    },
    /// A version other than 1.3, which is the only one there has ever been.
    Version {
        /// The major version it claims.
        major: u8,
        /// The minor version it claims.
        minor: u8,
    },
    /// A folder is compressed with Quantum or LZX, neither of which is read here.
    Compression {
        /// Which folder, counting from zero.
        folder: usize,
        /// The compression type, from the format's own list.
        kind: u16,
    },
    /// A file starts in one cabinet and finishes in another.
    Spanned {
        /// The name of the file.
        name: String,
    },
    /// A file names a folder this cabinet does not have.
    Folder {
        /// The name of the file.
        name: String,
        /// The folder it says it is in.
        which: u16,
    },
    /// A name is not text this host and the machine that wrote it would agree about.
    Name,
    /// A block did not decompress.
    Stream {
        /// Which folder, counting from zero.
        folder: usize,
        /// Which block of that folder, counting from zero.
        block: usize,
        /// What the decompressor said.
        why: InflateError,
    },
    /// A block failed the cabinet's own checksum, or produced a different number of bytes than it
    /// said it would, or had no `CK` in front of it.
    Corrupt {
        /// Which folder, counting from zero.
        folder: usize,
        /// Which block of that folder, counting from zero.
        block: usize,
    },
    /// A file's range runs past the end of what its folder produced.
    Past {
        /// The name of the file.
        name: String,
    },
}

impl fmt::Display for CabError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CabError::NotACab => write!(f, "it does not begin with MSCF, so it is not a cabinet"),
            CabError::Truncated { what } => write!(f, "the file ends in the middle of {what}"),
            CabError::Version { major, minor } => {
                write!(
                    f,
                    "this says it is a version {major}.{minor} cabinet, and 1.3 is the only one read here"
                )
            }
            CabError::Compression { folder, kind } => {
                let name = match kind {
                    2 => "Quantum",
                    3 => "LZX",
                    _ => "something with no name in the format",
                };
                write!(
                    f,
                    "folder {folder} is compressed with {name} ({kind}), and only none and MSZIP are read here"
                )
            }
            CabError::Spanned { name } => {
                write!(
                    f,
                    "{name} continues into another cabinet, and only whole files are read here"
                )
            }
            CabError::Folder { name, which } => {
                write!(f, "{name} says it is in folder {which}, which this cabinet does not have")
            }
            CabError::Name => write!(f, "a name is neither ASCII nor marked as UTF-8"),
            CabError::Stream { folder, block, why } => {
                write!(f, "folder {folder} block {block}: {why}")
            }
            CabError::Corrupt { folder, block } => {
                write!(f, "folder {folder} block {block} did not come out the way its header says")
            }
            CabError::Past { name } => {
                write!(f, "{name} runs past the end of the folder it says it is in")
            }
        }
    }
}

impl std::error::Error for CabError {}

/// One file in a cabinet.
///
/// Called a file rather than a member because that is the word the format uses, and it is
/// [`cab::File`](File) rather than something at the crate root because the crate root already has
/// [`crate::Member`] from the other container and two things called a file up there would be one
/// too many.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// The name, with backslashes, because a cabinet is written by and for Windows. Nothing is
    /// changed about it here: [`crate::under`] is what decides where one may be written.
    pub name: String,
    /// How large it is.
    pub size: u64,
    /// Which folder it is in, counting from zero.
    pub folder: usize,
    /// Where in what that folder produces it starts.
    pub at: u64,
}

/// One folder, which is one compressed stream.
#[derive(Debug)]
struct Folder {
    /// Where its first block is in the file.
    at: usize,
    /// How many blocks it has.
    blocks: u16,
    /// How it is compressed.
    compress: Compress,
}

/// The two ways a folder may be compressed that are read here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compress {
    /// Not at all, which a folder may use for bytes that would not compress.
    None,
    /// Deflate with `CK` in front of each block and the window carried across them.
    Mszip,
}

/// A cabinet that has been read as far as its folder and file tables.
///
/// Holding the bytes rather than copying out of them, the same as [`crate::Zip`], because the
/// caller has the file in memory already.
#[derive(Debug)]
pub struct Cab<'a> {
    bytes: &'a [u8],
    folders: Vec<Folder>,
    files: Vec<File>,
    /// How many bytes of reserve sit in each block's header, which is nothing in every cabinet
    /// Microsoft ships and is read anyway because the header says it may not be.
    per_block: usize,
}

impl<'a> Cab<'a> {
    /// Read a cabinet's tables.
    ///
    /// Nothing is decompressed here. What this produces is the list of what is in the file and
    /// where, which is what a caller needs to decide whether it wants any of it.
    ///
    /// # Errors
    ///
    /// [`CabError`] for a file that is not a cabinet, is a version this does not read, was cut
    /// short, or holds a file that continues into another cabinet.
    pub fn read(bytes: &'a [u8]) -> Result<Cab<'a>, CabError> {
        let mut at = At { bytes, at: 0 };
        if at.take(4, "the signature")? != SIGNATURE {
            return Err(CabError::NotACab);
        }
        at.skip(4, "the header")?; // reserved1, which is always zero.
        let size = at.u32("the header")?;
        at.skip(4, "the header")?; // reserved2.
        let files_at = at.u32("the header")?;
        at.skip(4, "the header")?; // reserved3.
        let minor = at.u8("the header")?;
        let major = at.u8("the header")?;
        if (major, minor) != VERSION {
            return Err(CabError::Version { major, minor });
        }
        let folder_count = at.u16("the header")?;
        let file_count = at.u16("the header")?;
        let flags = at.u16("the header")?;
        at.skip(4, "the header")?; // setID and iCabinet, which only matter across a set.

        // A cabinet is allowed to be shorter than the file it is in, and Microsoft's are: the two
        // sampled for this carry 10,672 and 10,712 bytes past the size the header gives, which is
        // the Authenticode signature appended after the cabinet proper. So this is not a check that
        // the two agree. Longer than the file is the other way round and means it was cut short.
        if size as usize > bytes.len() {
            return Err(CabError::Truncated { what: "the cabinet" });
        }

        let (per_folder, per_block) = if flags & RESERVE == 0 {
            (0, 0)
        } else {
            let header = at.u16("the reserve sizes")? as usize;
            let per_folder = at.u8("the reserve sizes")? as usize;
            let per_block = at.u8("the reserve sizes")? as usize;
            at.skip(header, "the header's reserve area")?;
            (per_folder, per_block)
        };
        // The names of the cabinets on either side, when there are any. They are not used, but they
        // sit between here and the folder table, so they have to be walked past to find it.
        for flag in [PREV, NEXT] {
            if flags & flag != 0 {
                at.name(false, "a neighbouring cabinet's name")?;
                at.name(false, "a neighbouring cabinet's name")?;
            }
        }

        let mut folders = Vec::with_capacity(folder_count as usize);
        for which in 0..folder_count as usize {
            let start = at.u32("a folder entry")? as usize;
            let blocks = at.u16("a folder entry")?;
            let kind = at.u16("a folder entry")?;
            at.skip(per_folder, "a folder's reserve area")?;
            let compress = match kind & KIND {
                0 => Compress::None,
                1 => Compress::Mszip,
                kind => return Err(CabError::Compression { folder: which, kind }),
            };
            folders.push(Folder { at: start, blocks, compress });
        }

        let mut at = At { bytes, at: files_at as usize };
        let mut files = Vec::with_capacity(file_count as usize);
        for _ in 0..file_count {
            let size = at.u32("a file entry")?;
            let start = at.u32("a file entry")?;
            let folder = at.u16("a file entry")?;
            at.skip(4, "a file entry")?; // The date and time it was last written.
            let attribs = at.u16("a file entry")?;
            let name = at.name(attribs & NAME_IS_UTF != 0, "a file's name")?;
            if folder >= CONTINUED {
                return Err(CabError::Spanned { name });
            }
            if folder as usize >= folders.len() {
                return Err(CabError::Folder { name, which: folder });
            }
            files.push(File {
                name,
                size: u64::from(size),
                folder: folder as usize,
                at: u64::from(start),
            });
        }
        Ok(Cab { bytes, folders, files, per_block })
    }

    /// Everything the file table lists, in the order it lists it.
    #[must_use]
    pub fn files(&self) -> &[File] {
        &self.files
    }

    /// The file with this exact name, if there is one.
    ///
    /// Exact, so the caller spells it with backslashes, because that is how a cabinet stores it.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&File> {
        self.files.iter().find(|file| file.name == name)
    }

    /// Every byte of the folder this file is in.
    ///
    /// The whole folder, because a folder is one stream and there is no shorter way to reach a file
    /// in the middle of it. A caller wanting several files out of the same folder should call this
    /// once and take [`File::at`] and [`File::size`] out of the result, rather than call
    /// [`Cab::contents`] for each of them and decompress the folder again every time.
    ///
    /// # Errors
    ///
    /// [`CabError`] for a block that fails its checksum, does not decompress, or produces a
    /// different number of bytes than its header says it will.
    pub fn folder(&self, file: &File) -> Result<Vec<u8>, CabError> {
        let which = file.folder;
        // Read gave every file a folder that is here, so this cannot fail for a file that came out
        // of this cabinet, and a file from another one is a mistake rather than a bad cabinet.
        let folder = self.folders.get(which).ok_or(CabError::Folder {
            name: file.name.clone(),
            which: u16::try_from(which).unwrap_or(u16::MAX),
        })?;

        let mut out = Vec::new();
        let mut at = At { bytes: self.bytes, at: folder.at };
        for block in 0..folder.blocks as usize {
            let wrong = || CabError::Corrupt { folder: which, block };
            let sum = at.u32("a block header")?;
            // Kept as bytes as well as numbers, because the checksum is over these four bytes and
            // taking them from the file is one fewer place to get the order wrong.
            let head = at.take(4, "a block header")?;
            let packed = u16::from_le_bytes([head[0], head[1]]) as usize;
            let plain = u16::from_le_bytes([head[2], head[3]]) as usize;
            at.skip(self.per_block, "a block's reserve area")?;
            let data = at.take(packed, "a block")?;

            // Zero means the writer did not compute one, which is allowed and is what some writers
            // do. Anything else is checked.
            if sum != 0 && checksum(head, checksum(data, 0)) != sum {
                return Err(wrong());
            }
            if plain > BLOCK {
                return Err(wrong());
            }
            let before = out.len();
            match folder.compress {
                Compress::None => out.extend_from_slice(data),
                Compress::Mszip => {
                    let Some((magic, stream)) = data.split_at_checked(2) else {
                        return Err(wrong());
                    };
                    if magic != CK {
                        return Err(wrong());
                    }
                    inflate::inflate_into(stream, &mut out).map_err(|why| CabError::Stream {
                        folder: which,
                        block,
                        why,
                    })?;
                }
            }
            if out.len() - before != plain {
                return Err(wrong());
            }
        }
        Ok(out)
    }

    /// One file's bytes.
    ///
    /// This decompresses the folder the file is in, so see [`Cab::folder`] before calling it in a
    /// loop.
    ///
    /// # Errors
    ///
    /// [`CabError`] for anything [`Cab::folder`] reports, and for a file whose range runs past the
    /// end of what its folder produced.
    pub fn contents(&self, file: &File) -> Result<Vec<u8>, CabError> {
        let folder = self.folder(file)?;
        let past = || CabError::Past { name: file.name.clone() };
        let at = usize::try_from(file.at).map_err(|_| past())?;
        let size = usize::try_from(file.size).map_err(|_| past())?;
        let end = at.checked_add(size).ok_or_else(past)?;
        folder.get(at..end).ok_or_else(past).map(<[u8]>::to_vec)
    }
}

/// The cabinet's own checksum of a block, folded into `sum`.
///
/// A xor of the bytes four at a time and little endian, which is ordinary, and then a tail of one,
/// two or three bytes folded in the other way round, most significant first, which is not. That is
/// what the format's own code does, so it is what a reader has to do.
fn checksum(data: &[u8], sum: u32) -> u32 {
    let words = data.chunks_exact(4);
    let tail = words.remainder();
    let sum =
        words.fold(sum, |sum, four| sum ^ u32::from_le_bytes([four[0], four[1], four[2], four[3]]));
    let mut last = 0u32;
    for (n, &byte) in tail.iter().enumerate() {
        last |= u32::from(byte) << (8 * (tail.len() - 1 - n));
    }
    sum ^ last
}

/// A place in the file, which reads forwards.
struct At<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> At<'a> {
    /// The next `n` bytes.
    ///
    /// The slice borrows the file rather than the cursor, for the same reason as the one in
    /// [`crate::zip`]: a name read out of it outlives the read that produced it.
    fn take(&mut self, n: usize, what: &'static str) -> Result<&'a [u8], CabError> {
        let end = self.at.checked_add(n).ok_or(CabError::Truncated { what })?;
        let got = self.bytes.get(self.at..end).ok_or(CabError::Truncated { what })?;
        self.at = end;
        Ok(got)
    }

    /// The next `n` bytes, thrown away.
    fn skip(&mut self, n: usize, what: &'static str) -> Result<(), CabError> {
        self.take(n, what).map(|_| ())
    }

    fn u8(&mut self, what: &'static str) -> Result<u8, CabError> {
        Ok(self.take(1, what)?[0])
    }

    fn u16(&mut self, what: &'static str) -> Result<u16, CabError> {
        let got = self.take(2, what)?;
        Ok(u16::from_le_bytes([got[0], got[1]]))
    }

    fn u32(&mut self, what: &'static str) -> Result<u32, CabError> {
        let got = self.take(4, what)?;
        Ok(u32::from_le_bytes([got[0], got[1], got[2], got[3]]))
    }

    /// A name, which is the only kind of string a cabinet has and always ends in a NUL.
    ///
    /// A name is UTF-8 when the file that carries it says so and is otherwise in whatever code page
    /// the machine that wrote it was using, which is not written down anywhere in the file. ASCII
    /// is the part of every one of those code pages that agrees, so a byte over 127 with nothing
    /// saying it is UTF-8 is refused rather than turned into a different name on every host. The
    /// SDK's cabinets name everything in hexadecimal, so this costs nothing there.
    fn name(&mut self, utf8: bool, what: &'static str) -> Result<String, CabError> {
        let rest = self.bytes.get(self.at..).ok_or(CabError::Truncated { what })?;
        let len = rest.iter().position(|&byte| byte == 0).ok_or(CabError::Truncated { what })?;
        let name = self.take(len, what)?;
        self.skip(1, what)?;
        if utf8 {
            String::from_utf8(name.to_vec()).map_err(|_| CabError::Name)
        } else if name.is_ascii() {
            Ok(String::from_utf8_lossy(name).into_owned())
        } else {
            Err(CabError::Name)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Cab, CabError, checksum};

    /// A stored deflate block, which is the one an encoder does not have to be written to produce.
    ///
    /// One byte saying this is the last block and that it is stored, then the length twice, the
    /// second time inverted, then the bytes. That is the whole of it, and it means these tests can
    /// hand the reader a real MSZIP block without a deflate encoder anywhere near them.
    fn stored(bytes: &[u8]) -> Vec<u8> {
        let len = u16::try_from(bytes.len()).expect("a test block is small");
        let mut out = vec![b'C', b'K', 0x01];
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(bytes);
        out
    }

    /// The stream from the decompressor's own tests, which copies three bytes from five back and
    /// then ends. Across a block boundary it can only work if the window carried over.
    const COPY_FROM_FIVE_BACK: [u8; 3] = [0x03, 0x12, 0x00];

    /// Lay out a cabinet by hand.
    ///
    /// The files are a name, a size and where in the folder they start. The blocks are the bytes
    /// that go on disk and what each says it produces, kept apart so that a test can hand in a
    /// block that lies about itself. The reserve sizes go in the header when they are given, and
    /// the areas they describe are filled with a byte that is not zero so that a reader which
    /// forgot to skip them reads nonsense rather than nothing.
    fn cabinet(
        files: &[(&str, u32, u32)],
        blocks: &[(Vec<u8>, u16)],
        reserve: Option<(u16, u8, u8)>,
        compress: u16,
    ) -> Vec<u8> {
        let (header, per_folder, per_block) = reserve.unwrap_or((0, 0, 0));
        let mut before = 36;
        if reserve.is_some() {
            before += 4 + header as usize;
        }
        let files_at = before + 8 + per_folder as usize;
        let mut table = Vec::new();
        for &(name, size, start) in files {
            table.extend_from_slice(&size.to_le_bytes());
            table.extend_from_slice(&start.to_le_bytes());
            table.extend_from_slice(&0u16.to_le_bytes()); // Folder zero, the only one here.
            table.extend_from_slice(&0u32.to_le_bytes()); // The date and time.
            table.extend_from_slice(&0x20u16.to_le_bytes()); // Archive, which is what they all are.
            table.extend_from_slice(name.as_bytes());
            table.push(0);
        }
        let folder_at = files_at + table.len();

        let mut data = Vec::new();
        for (bytes, plain) in blocks {
            let packed = u16::try_from(bytes.len()).expect("a test block is small");
            let mut head = Vec::new();
            head.extend_from_slice(&packed.to_le_bytes());
            head.extend_from_slice(&plain.to_le_bytes());
            let sum = checksum(&head, checksum(bytes, 0));
            data.extend_from_slice(&sum.to_le_bytes());
            data.extend_from_slice(&head);
            data.extend(std::iter::repeat_n(0xa5, per_block as usize));
            data.extend_from_slice(bytes);
        }

        let mut out = Vec::new();
        out.extend_from_slice(b"MSCF");
        out.extend_from_slice(&0u32.to_le_bytes());
        let size = u32::try_from(folder_at + data.len()).expect("a test cabinet is small");
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&u32::try_from(files_at).expect("small").to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.push(3); // Minor.
        out.push(1); // Major.
        out.extend_from_slice(&1u16.to_le_bytes()); // One folder.
        out.extend_from_slice(&u16::try_from(files.len()).expect("small").to_le_bytes());
        out.extend_from_slice(&if reserve.is_some() { 0x4u16 } else { 0 }.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // setID and iCabinet.
        if reserve.is_some() {
            out.extend_from_slice(&header.to_le_bytes());
            out.push(per_folder);
            out.push(per_block);
            out.extend(std::iter::repeat_n(0xa5, header as usize));
        }
        out.extend_from_slice(&u32::try_from(folder_at).expect("small").to_le_bytes());
        out.extend_from_slice(&u16::try_from(blocks.len()).expect("small").to_le_bytes());
        out.extend_from_slice(&compress.to_le_bytes());
        out.extend(std::iter::repeat_n(0xa5, per_folder as usize));
        out.extend_from_slice(&table);
        out.extend_from_slice(&data);
        assert_eq!(out.len(), size as usize, "the layout and the header agree");
        out
    }

    /// One MSZIP file in one block, which is the shape of the smallest cabinet the SDK ships.
    fn one_file() -> Vec<u8> {
        let text = b"#define _INC_STDIO\n";
        cabinet(&[("stdio.h", 19, 0)], &[(stored(text), 19)], Some((20, 0, 0)), 1)
    }

    #[test]
    fn the_checksum_folds_four_bytes_at_a_time_and_its_tail_the_other_way_round() {
        assert_eq!(checksum(b"", 0), 0);
        assert_eq!(checksum(b"", 7), 7, "nothing to fold in leaves the seed alone");
        // Four bytes, little endian, which is the ordinary half.
        assert_eq!(checksum(&[1, 2, 3, 4], 0), 0x0403_0201);
        // One, two and three bytes over, most significant first, which is the half that is not.
        assert_eq!(checksum(&[1], 0), 0x0000_0001);
        assert_eq!(checksum(&[1, 2], 0), 0x0000_0102);
        assert_eq!(checksum(&[1, 2, 3], 0), 0x0001_0203);
        // And the two halves together, the second word xored over the first.
        assert_eq!(checksum(&[1, 2, 3, 4, 1, 2, 3, 4], 0), 0);
    }

    #[test]
    fn a_cabinet_with_one_file_in_it_reads() {
        let bytes = one_file();
        let cab = Cab::read(&bytes).expect("a cabinet this laid out itself");
        assert_eq!(cab.files().len(), 1);
        let file = cab.find("stdio.h").expect("the file it was given");
        assert_eq!(file.size, 19);
        assert_eq!(file.folder, 0);
        assert_eq!(cab.contents(file).expect("it comes out"), b"#define _INC_STDIO\n");
    }

    #[test]
    fn two_files_in_one_folder_are_two_ranges_of_the_same_stream() {
        // Which is what a folder is, and the reason `folder` is public: this decompresses once.
        let both = b"one.h\0two.h\0";
        let bytes = cabinet(&[("a.h", 6, 0), ("b.h", 6, 6)], &[(stored(both), 12)], None, 1);
        let cab = Cab::read(&bytes).expect("a cabinet this laid out itself");
        let a = cab.find("a.h").expect("the first");
        let b = cab.find("b.h").expect("the second");
        assert_eq!(a.folder, b.folder);
        assert_eq!(cab.folder(a).expect("the whole folder"), both);
        assert_eq!(cab.contents(a).expect("the first file"), b"one.h\0");
        assert_eq!(cab.contents(b).expect("the second file"), b"two.h\0");
    }

    #[test]
    fn a_block_reaches_back_into_what_the_block_before_it_produced() {
        // The thing that makes MSZIP MSZIP. The second block's stream copies three bytes from five
        // back, and five back is in the first block, so a reader that starts each block with an
        // empty window cannot produce this and a reader that carries the window can.
        let mut second = vec![b'C', b'K'];
        second.extend_from_slice(&COPY_FROM_FIVE_BACK);
        let bytes = cabinet(&[("x", 11, 0)], &[(stored(b"abcdefgh"), 8), (second, 3)], None, 1);
        let cab = Cab::read(&bytes).expect("a cabinet this laid out itself");
        let file = cab.find("x").expect("the file");
        assert_eq!(cab.contents(file).expect("both blocks"), b"abcdefghdef");
    }

    #[test]
    fn the_reserve_areas_are_walked_past_rather_than_read() {
        // All three at once, with something that is not zero in them, which no cabinet Microsoft
        // ships does: the ones measured for this have twenty bytes in the header and none in the
        // folders or the blocks. A reader that skips only the one it has seen fails here.
        let bytes = cabinet(&[("h", 5, 0)], &[(stored(b"hello"), 5)], Some((20, 4, 2)), 1);
        let cab = Cab::read(&bytes).expect("a cabinet with all three reserve areas");
        let file = cab.find("h").expect("the file");
        assert_eq!(cab.contents(file).expect("past the reserve"), b"hello");
    }

    #[test]
    fn a_folder_that_is_not_compressed_is_read_as_it_is() {
        let bytes = cabinet(&[("h", 5, 0)], &[(b"plain".to_vec(), 5)], None, 0);
        let cab = Cab::read(&bytes).expect("an uncompressed cabinet");
        let file = cab.find("h").expect("the file");
        assert_eq!(cab.contents(file).expect("as it is"), b"plain");
    }

    #[test]
    fn what_is_not_a_cabinet_says_so() {
        assert_eq!(Cab::read(b"").unwrap_err(), CabError::Truncated { what: "the signature" });
        assert_eq!(Cab::read(b"MZ\x90\x00").unwrap_err(), CabError::NotACab);
        // The signature and nothing else, which is a cabinet that was cut off at the top.
        assert_eq!(Cab::read(b"MSCF").unwrap_err(), CabError::Truncated { what: "the header" });
    }

    #[test]
    fn a_version_that_is_not_the_one_version_is_refused() {
        let mut bytes = one_file();
        bytes[24] = 4; // The minor, which is at 24 and is 3 in every cabinet there is.
        assert_eq!(Cab::read(&bytes).unwrap_err(), CabError::Version { major: 1, minor: 4 });
    }

    #[test]
    fn quantum_and_lzx_are_refused_by_name() {
        for (kind, want) in [(2u16, "Quantum"), (3, "LZX")] {
            // The high bits carry the window size for LZX, so they are set here to prove the low
            // four are what is looked at.
            let bytes = cabinet(&[("h", 5, 0)], &[(b"plain".to_vec(), 5)], None, kind | 0x1500);
            let why = Cab::read(&bytes).unwrap_err();
            assert_eq!(why, CabError::Compression { folder: 0, kind });
            assert!(why.to_string().contains(want), "{why}");
        }
    }

    #[test]
    fn a_file_that_continues_into_another_cabinet_is_refused() {
        let mut bytes = one_file();
        // The folder number in the one file entry, which the layout puts at 68 plus eight.
        let at = 68 + 8;
        bytes[at] = 0xfe;
        bytes[at + 1] = 0xff;
        assert_eq!(
            Cab::read(&bytes).unwrap_err(),
            CabError::Spanned { name: "stdio.h".to_owned() }
        );
    }

    #[test]
    fn a_file_in_a_folder_that_is_not_there_is_refused() {
        let mut bytes = one_file();
        bytes[68 + 8] = 3;
        assert_eq!(
            Cab::read(&bytes).unwrap_err(),
            CabError::Folder { name: "stdio.h".to_owned(), which: 3 }
        );
    }

    #[test]
    fn a_block_whose_bytes_changed_fails_its_checksum() {
        let mut bytes = one_file();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x20;
        let cab = Cab::read(&bytes).expect("the tables are untouched");
        let file = cab.find("stdio.h").expect("the file");
        assert_eq!(cab.contents(file).unwrap_err(), CabError::Corrupt { folder: 0, block: 0 });
    }

    #[test]
    fn a_block_with_no_ck_in_front_of_it_is_refused() {
        let mut block = stored(b"hello");
        block[1] = b'Z';
        let bytes = cabinet(&[("h", 5, 0)], &[(block, 5)], None, 1);
        let cab = Cab::read(&bytes).expect("the tables are fine");
        let file = cab.find("h").expect("the file");
        assert_eq!(cab.contents(file).unwrap_err(), CabError::Corrupt { folder: 0, block: 0 });
    }

    #[test]
    fn a_block_that_lies_about_what_it_produces_is_refused() {
        // Five bytes of stream and a header saying six, which is the cheapest way a block can be
        // wrong that the checksum cannot see, because the header is what the checksum covers.
        let bytes = cabinet(&[("h", 5, 0)], &[(stored(b"hello"), 6)], None, 1);
        let cab = Cab::read(&bytes).expect("the tables are fine");
        let file = cab.find("h").expect("the file");
        assert_eq!(cab.contents(file).unwrap_err(), CabError::Corrupt { folder: 0, block: 0 });
    }

    #[test]
    fn a_file_that_runs_past_its_folder_is_refused() {
        let bytes = cabinet(&[("h", 50, 0)], &[(stored(b"hello"), 5)], None, 1);
        let cab = Cab::read(&bytes).expect("the tables are fine");
        let file = cab.find("h").expect("the file");
        assert_eq!(cab.contents(file).unwrap_err(), CabError::Past { name: "h".to_owned() });
    }

    #[test]
    fn a_name_is_ascii_unless_it_says_it_is_utf8() {
        let bytes = one_file();
        // The attributes and the name, which the layout puts at 68 plus fourteen and sixteen.
        let attribs = 68 + 14;
        let name = 68 + 16;
        let mut latin = bytes.clone();
        latin[name] = 0xe9; // An e with an accent, in whichever code page you happen to be in.
        assert_eq!(Cab::read(&latin).unwrap_err(), CabError::Name);

        let mut utf8 = latin;
        utf8[attribs] = 0x20 | 0x80;
        // Still not UTF-8, because one byte over 127 on its own is not a character.
        assert_eq!(Cab::read(&utf8).unwrap_err(), CabError::Name);

        // And the same name spelled properly, which is two bytes, so the name is one shorter.
        let mut ok = bytes;
        ok[attribs] = 0x20 | 0x80;
        ok[name] = 0xc3;
        ok[name + 1] = 0xa9;
        let cab = Cab::read(&ok).expect("a UTF-8 name");
        assert_eq!(cab.files()[0].name, "édio.h");
    }
}
