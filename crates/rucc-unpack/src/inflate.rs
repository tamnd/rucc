//! The one decompressor everything else in this crate is built on.
//!
//! Design: `spec/cross-compile/13-distribution.md` section 13.4, which is the reason the crate
//! exists, and RFC 1951 for the format.
//!
//! # Why this is written here
//!
//! Because both of the containers Microsoft's Windows SDK payloads arrive in hold their contents
//! the same way. A vsix is a zip and a zip's ordinary compression method is deflate. A cabinet's
//! ordinary compression is MSZIP, and MSZIP is deflate with two bytes in front of each block. So
//! one decoder serves the whole of this crate, and writing it is the difference between reading
//! Microsoft's two formats and needing a dependency for each.
//!
//! `spec/18-package-layout.md` section 18.3 is the budget that makes that a decision rather than an
//! accident. Unlike the tar and gzip the sysroot fetch uses, which are programs every host in the
//! support table already has, there is no program a host already has that reads a compound file, so
//! that reader has to be ours. Once it is, the decompressor under it is a few hundred lines with
//! nothing to keep current, and this is a format that was finished in 1996.
//!
//! # Appending rather than returning
//!
//! [`inflate_into`] adds to a buffer the caller owns and does not clear it first, which looks like
//! an inconvenience and is the whole of how MSZIP works. A cabinet folder is a run of blocks, each
//! one a separate deflate stream, and a block's back references reach into the 32 kilobytes the
//! previous block produced. Appending to one buffer is exactly that window, so the cabinet reader
//! gets the behaviour by calling this in a loop rather than by carrying a window of its own.
//!
//! # What is not here
//!
//! Compression, and the zlib and gzip wrappers. Nothing in this compiler produces a zip or a
//! cabinet, and the two wrappers are somebody else's checksum around a stream this already reads.

use std::fmt;

/// The furthest back a copy may reach, which is the format's window.
const WINDOW: usize = 32768;

/// The number of literal and length symbols, the last two of which are never used.
const LITERALS: usize = 288;

/// The number of distance symbols.
const DISTANCES: usize = 30;

/// Why a stream could not be read.
///
/// Each one says what was expected and how far in the reader had got, because the useful question
/// about a stream that will not decode is whether it was truncated or was never this format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InflateError {
    /// How many bytes of the input had been consumed.
    pub at: usize,
    /// What was wrong, in the words of the format.
    pub what: Bad,
}

/// What was wrong with a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bad {
    /// The stream ended in the middle of something, which is a truncated file.
    Truncated,
    /// A block header named a type the format does not have, which is the one value RFC 1951
    /// reserves and has never assigned.
    BlockType,
    /// A stored block's length and its complement disagree, which is the format's own check that
    /// the two bytes were read at the right offset.
    StoredLength,
    /// A code length table that no canonical Huffman code can be built from, either because it
    /// leaves codes unused or because it asks for more than there are.
    Lengths,
    /// A symbol that is not in the code that was being read with.
    Symbol,
    /// A length or distance symbol the format reserves and never assigns.
    Reserved,
    /// A copy that reaches further back than there is output, which is a stream that was decoded
    /// against the wrong history or was never valid.
    TooFarBack,
}

impl fmt::Display for InflateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.what {
            Bad::Truncated => "the stream ends in the middle of a block",
            Bad::BlockType => "a block says it is of the one type the format reserves",
            Bad::StoredLength => "a stored block's length and its complement disagree",
            Bad::Lengths => "the code lengths do not describe a Huffman code",
            Bad::Symbol => "a symbol that the code being read with does not have",
            Bad::Reserved => "a length or distance symbol the format never assigns",
            Bad::TooFarBack => "a copy that reaches back further than anything produced",
        };
        write!(f, "{what}, {} bytes in", self.at)
    }
}

impl std::error::Error for InflateError {}

/// Decompress a deflate stream.
///
/// # Errors
///
/// [`InflateError`] for anything that is not a deflate stream or is one that was cut short.
pub fn inflate(bytes: &[u8]) -> Result<Vec<u8>, InflateError> {
    let mut out = Vec::new();
    inflate_into(bytes, &mut out)?;
    Ok(out)
}

/// Decompress a deflate stream onto the end of `out`, leaving what is already there.
///
/// What is already in `out` is history the stream may copy from, which is what MSZIP wants and what
/// nothing else should rely on. A caller with no history passes an empty buffer.
///
/// The number of input bytes the stream used, which is not always all of them: a zip member's data
/// is followed by the next member's header, and a cabinet block's stream is followed by the next
/// block's.
///
/// # Errors
///
/// [`InflateError`] for anything that is not a deflate stream or is one that was cut short.
pub fn inflate_into(bytes: &[u8], out: &mut Vec<u8>) -> Result<usize, InflateError> {
    // Where the history ends and this stream's output begins. A copy may reach back into the
    // history, so the two are one buffer, and the distance check below is against the whole of it.
    let mut bits = Bits { bytes, at: 0, held: 0, count: 0 };
    loop {
        let last = bits.bits(1)?;
        match bits.bits(2)? {
            0 => stored(&mut bits, out)?,
            1 => {
                let (lit, dist) = fixed();
                block(&mut bits, out, &lit, &dist)?;
            }
            2 => {
                let (lit, dist) = dynamic(&mut bits)?;
                block(&mut bits, out, &lit, &dist)?;
            }
            // The fourth value is the one RFC 1951 reserves and nobody has ever assigned, so a
            // stream that has it is not a deflate stream that was read from the right offset.
            _ => return Err(bits.bad(Bad::BlockType)),
        }
        if last == 1 {
            // The bits left over in the byte the last block ended in are padding, and the caller
            // wants the offset of the byte after them.
            return Ok(bits.at);
        }
    }
}

/// A stream of bits, least significant first, which is the order deflate packs them in.
struct Bits<'a> {
    bytes: &'a [u8],
    /// The next byte to take, which is also how far in an error is reported at.
    at: usize,
    /// Bits taken from bytes and not yet handed out.
    held: u32,
    /// How many of `held` are real.
    count: u32,
}

impl Bits<'_> {
    /// An error at wherever the reader is.
    fn bad(&self, what: Bad) -> InflateError {
        InflateError { at: self.at, what }
    }

    /// The next `want` bits as a number, with the first bit read as the least significant.
    fn bits(&mut self, want: u32) -> Result<u32, InflateError> {
        while self.count < want {
            let byte = *self.bytes.get(self.at).ok_or_else(|| self.bad(Bad::Truncated))?;
            self.at += 1;
            self.held |= u32::from(byte) << self.count;
            self.count += 8;
        }
        let got = self.held & ((1 << want) - 1);
        self.held >>= want;
        self.count -= want;
        Ok(got)
    }

    /// Throw away the rest of the byte being read, which is what a stored block starts with.
    fn align(&mut self) {
        self.held = 0;
        self.count = 0;
    }
}

/// A block that was not compressed at all, which is what deflate does with data that would grow.
fn stored(bits: &mut Bits<'_>, out: &mut Vec<u8>) -> Result<(), InflateError> {
    bits.align();
    let len = usize::from(u16::try_from(bits.bits(16)?).unwrap_or_default());
    let not = usize::from(u16::try_from(bits.bits(16)?).unwrap_or_default());
    // The format's own check that these four bytes were read at the right offset, and the reason a
    // stored block is the one place a truncation is caught before the data runs out.
    if len != !not & 0xffff {
        return Err(bits.bad(Bad::StoredLength));
    }
    let from = bits.at;
    let to = from.checked_add(len).ok_or_else(|| bits.bad(Bad::Truncated))?;
    let data = bits.bytes.get(from..to).ok_or_else(|| bits.bad(Bad::Truncated))?;
    out.extend_from_slice(data);
    bits.at = to;
    Ok(())
}

/// A canonical Huffman code, as the count of codes of each length and the symbols in order.
///
/// Not a lookup table. A table is how a decompressor that has to be fast is written and this one
/// does not: it is reading a few hundred megabytes once, when somebody asked for an SDK, and the
/// version that can be read against RFC 1951 line by line is worth more here than the version that
/// is three times its speed.
struct Code {
    /// How many codes there are of each length, indexed by length, with index 0 unused.
    counts: [u16; 16],
    /// Every symbol that has a code, shortest code first and in symbol order within a length.
    symbols: Vec<u16>,
}

impl Code {
    /// Build the code a list of code lengths describes, one length per symbol and 0 for a symbol
    /// that has no code.
    fn new(lengths: &[u8]) -> Option<Code> {
        let mut counts = [0u16; 16];
        for &length in lengths {
            counts[usize::from(length)] += 1;
        }
        // A code with one symbol in it is legal and is what a distance code looks like when every
        // match in the block is at the same distance, so the check below has to allow it.
        if usize::from(counts[0]) == lengths.len() {
            return Some(Code { counts, symbols: Vec::new() });
        }
        // Kraft's inequality, which is the whole of whether these lengths are a code. Left is how
        // many codes of the current length are still unassigned, and it may not go negative, and at
        // the end it may only be positive for the one symbol case above.
        let mut left = 1i32;
        for &count in counts.iter().skip(1) {
            left <<= 1;
            left -= i32::from(count);
            if left < 0 {
                return None;
            }
        }
        let mut offsets = [0u16; 16];
        for length in 1..15 {
            offsets[length + 1] = offsets[length] + counts[length];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (symbol, &length) in lengths.iter().enumerate() {
            if length != 0 {
                let at = usize::from(offsets[usize::from(length)]);
                symbols[at] = u16::try_from(symbol).unwrap_or_default();
                offsets[usize::from(length)] += 1;
            }
        }
        Some(Code { counts, symbols })
    }

    /// Read one symbol.
    ///
    /// A bit at a time, walking down the lengths: at each length, the codes of that length are a
    /// contiguous run of numbers, so the symbol is found by asking whether the number read so far
    /// falls inside the run. That is the whole of what canonical means.
    fn read(&self, bits: &mut Bits<'_>) -> Result<u16, InflateError> {
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;
        for length in 1..16 {
            code |= i32::try_from(bits.bits(1)?).unwrap_or_default();
            let count = i32::from(self.counts[length]);
            if code - first < count {
                let at = usize::try_from(index + (code - first)).unwrap_or_default();
                return self.symbols.get(at).copied().ok_or_else(|| bits.bad(Bad::Symbol));
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(bits.bad(Bad::Symbol))
    }
}

/// The code every deflate stream may use without describing it, from RFC 1951 section 3.2.6.
fn fixed() -> (Code, Code) {
    let mut lengths = [0u8; LITERALS];
    for (symbol, length) in lengths.iter_mut().enumerate() {
        *length = match symbol {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    // Neither can fail: these are the lengths the document gives.
    let lit = Code::new(&lengths).unwrap_or(Code { counts: [0; 16], symbols: Vec::new() });
    let dist =
        Code::new(&[5u8; DISTANCES]).unwrap_or(Code { counts: [0; 16], symbols: Vec::new() });
    (lit, dist)
}

/// The order the code lengths of the code-length code are written in, which is chosen so that the
/// lengths that are usually zero are at the end and can be left out.
const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

/// Read the two codes a dynamic block describes before its data.
fn dynamic(bits: &mut Bits<'_>) -> Result<(Code, Code), InflateError> {
    let nlit = bits.bits(5)? as usize + 257;
    let ndist = bits.bits(5)? as usize + 1;
    let ncode = bits.bits(4)? as usize + 4;
    if nlit > LITERALS || ndist > DISTANCES + 2 {
        return Err(bits.bad(Bad::Lengths));
    }

    // The code that the two real codes' lengths are themselves written with.
    let mut meta = [0u8; 19];
    for &at in ORDER.iter().take(ncode) {
        meta[at] = u8::try_from(bits.bits(3)?).unwrap_or_default();
    }
    let meta = Code::new(&meta).ok_or_else(|| bits.bad(Bad::Lengths))?;

    // Both codes' lengths are written as one run, so a repeat may carry across the boundary between
    // them, which is why this is one loop and not two.
    let mut lengths = vec![0u8; nlit + ndist];
    let mut at = 0;
    while at < lengths.len() {
        let symbol = meta.read(bits)?;
        match symbol {
            0..=15 => {
                lengths[at] = u8::try_from(symbol).unwrap_or_default();
                at += 1;
            }
            // Repeat the length before this one. There has to be one, and a stream that asks to
            // repeat nothing is not a stream.
            16 => {
                let last = at.checked_sub(1).ok_or_else(|| bits.bad(Bad::Lengths))?;
                let last = lengths[last];
                let times = bits.bits(2)? as usize + 3;
                fill(&mut lengths, &mut at, last, times).ok_or_else(|| bits.bad(Bad::Lengths))?;
            }
            17 => {
                let times = bits.bits(3)? as usize + 3;
                fill(&mut lengths, &mut at, 0, times).ok_or_else(|| bits.bad(Bad::Lengths))?;
            }
            18 => {
                let times = bits.bits(7)? as usize + 11;
                fill(&mut lengths, &mut at, 0, times).ok_or_else(|| bits.bad(Bad::Lengths))?;
            }
            _ => return Err(bits.bad(Bad::Lengths)),
        }
    }

    let lit = Code::new(&lengths[..nlit]).ok_or_else(|| bits.bad(Bad::Lengths))?;
    let dist = Code::new(&lengths[nlit..]).ok_or_else(|| bits.bad(Bad::Lengths))?;
    Ok((lit, dist))
}

/// Write `value` `times` over, refusing to run off the end rather than growing the list.
///
/// A run that would not fit is a stream describing more symbols than it said it had, which is a
/// different thing from a stream that is merely unusual and is worth refusing outright.
fn fill(lengths: &mut [u8], at: &mut usize, value: u8, times: usize) -> Option<()> {
    let end = at.checked_add(times)?;
    lengths.get_mut(*at..end)?.fill(value);
    *at = end;
    Some(())
}

/// The extra bits and the base for each length symbol, from RFC 1951 section 3.2.5.
const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u32; 29] =
    [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];

/// The same for each distance symbol.
const DISTANCE_BASE: [u16; DISTANCES] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DISTANCE_EXTRA: [u32; DISTANCES] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Read one block's worth of literals and copies with the two codes it was written with.
fn block(
    bits: &mut Bits<'_>,
    out: &mut Vec<u8>,
    lit: &Code,
    dist: &Code,
) -> Result<(), InflateError> {
    loop {
        let symbol = lit.read(bits)?;
        match symbol {
            0..=255 => out.push(u8::try_from(symbol).unwrap_or_default()),
            256 => return Ok(()),
            257..=285 => {
                let at = usize::from(symbol) - 257;
                let length = usize::from(LENGTH_BASE[at]) + bits.bits(LENGTH_EXTRA[at])? as usize;
                let symbol = usize::from(dist.read(bits)?);
                // 30 and 31 have codes in the fixed distance code and no meaning, so a stream that
                // reaches one was not written by a deflate encoder.
                if symbol >= DISTANCES {
                    return Err(bits.bad(Bad::Reserved));
                }
                let distance = usize::from(DISTANCE_BASE[symbol])
                    + bits.bits(DISTANCE_EXTRA[symbol])? as usize;
                copy(bits, out, length, distance)?;
            }
            // 286 and 287 have codes in the fixed literal code and no meaning either.
            _ => return Err(bits.bad(Bad::Reserved)),
        }
    }
}

/// Copy `length` bytes from `distance` back, which may overlap what is being written.
///
/// A byte at a time on purpose. A copy of 10 bytes from 1 back is how deflate spells a run of one
/// byte repeated, so the source moves as the destination does and a block copy would be wrong.
fn copy(
    bits: &Bits<'_>,
    out: &mut Vec<u8>,
    length: usize,
    distance: usize,
) -> Result<(), InflateError> {
    // Against the whole buffer rather than against this stream's share of it, because the history a
    // caller passed in is exactly what a cabinet block is allowed to reach into.
    let from = out.len().checked_sub(distance).ok_or_else(|| bits.bad(Bad::TooFarBack))?;
    // A distance is at most 32768 by the tables above, so this cannot be the reason a stream is
    // refused. It is here because the window is the format's rule and not an accident of them.
    if distance > WINDOW {
        return Err(bits.bad(Bad::TooFarBack));
    }
    out.reserve(length);
    for at in from..from + length {
        let byte = out[at];
        out.push(byte);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Bad, inflate, inflate_into};

    /// The smallest thing there is: one stored block holding nothing.
    ///
    /// Bit 0 is the last-block flag and bits 1 and 2 are the type, so `0x01` is a final stored
    /// block, then four bytes of length and its complement.
    #[test]
    fn an_empty_stored_block_is_an_empty_stream() {
        assert_eq!(inflate(&[0x01, 0x00, 0x00, 0xff, 0xff]), Ok(Vec::new()));
    }

    #[test]
    fn a_stored_block_is_the_bytes_as_they_stand() {
        let mut stream = vec![0x01, 0x05, 0x00, 0xfa, 0xff];
        stream.extend_from_slice(b"hello");
        assert_eq!(inflate(&stream), Ok(b"hello".to_vec()));
    }

    #[test]
    fn a_stored_length_that_disagrees_with_its_complement_is_refused() {
        // Which is the format's own check that these four bytes were read at the right offset, and
        // is what catches a stream being decoded from the wrong place before it produces anything.
        let mut stream = vec![0x01, 0x05, 0x00, 0x00, 0x00];
        stream.extend_from_slice(b"hello");
        assert_eq!(inflate(&stream).unwrap_err().what, Bad::StoredLength);
    }

    /// Round trips against a real encoder, which is what says the decoder is right rather than
    /// self-consistent.
    ///
    /// The encoder is whatever the machine has, since a test that shipped its own would be testing
    /// two things it wrote. `gzip` is on every host in the support table and its member header is a
    /// fixed ten bytes when it is fed on standard input, so the deflate stream starts at byte ten.
    /// [`None`] only when there is no `gzip` to run, which is a machine these tests cannot say
    /// anything on rather than a failure of this crate. A `gzip` that ran and did not produce a
    /// member is a failure, and it fails here rather than quietly turning the test into one that
    /// asserts nothing.
    fn gzipped(bytes: &[u8], level: &str) -> Option<Vec<u8>> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let gzip = Command::new("gzip")
            .arg(level)
            .arg("--no-name")
            .arg("--stdout")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut gzip) = gzip else {
            return None;
        };
        gzip.stdin.take().expect("a pipe that was asked for").write_all(bytes).expect("writes");
        let out = gzip.wait_with_output().expect("gzip finishes");
        assert!(out.status.success(), "gzip {level} failed");
        // Ten bytes of header, then the deflate stream, then four bytes of crc and four of length.
        let body = out.stdout.get(10..out.stdout.len() - 8).expect("a member with a header on it");
        Some(body.to_vec())
    }

    #[test]
    fn what_a_real_encoder_produced_decodes_back_to_what_it_was_given() {
        // Text that repeats, so there are matches at a spread of distances, some of them long.
        let mut source = Vec::new();
        for i in 0..4000u32 {
            source.extend_from_slice(
                format!("line {i} of a file that says much the same\n").as_bytes(),
            );
        }
        // Both ends of the range, because the fast level and the slow one make different blocks and
        // the slow one is where the long matches and the deep codes are.
        for level in ["-1", "-9"] {
            let Some(stream) = gzipped(&source, level) else {
                // A machine with no gzip is not a failure of this crate.
                return;
            };
            assert_eq!(inflate(&stream).as_deref(), Ok(source.as_slice()), "at {level}");
        }
    }

    #[test]
    fn bytes_that_do_not_compress_come_back_through_the_stored_blocks_they_end_up_in() {
        // Deflate falls back to storing when compressing would grow the data, so this is how the
        // stored path gets exercised by a real encoder rather than by a stream written by hand.
        let mut source = Vec::new();
        let mut state = 0x243f_6a88_85a3_08d3u64;
        for _ in 0..200_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            source.push(u8::try_from(state & 0xff).unwrap_or_default());
        }
        let Some(stream) = gzipped(&source, "-9") else {
            return;
        };
        assert_eq!(inflate(&stream).as_deref(), Ok(source.as_slice()));
    }

    #[test]
    fn a_short_run_of_one_byte_is_a_copy_that_overlaps_itself() {
        // Which is how deflate spells a repeat, and is why the copy is a byte at a time.
        let source = vec![b'z'; 5000];
        let Some(stream) = gzipped(&source, "-9") else {
            return;
        };
        assert_eq!(inflate(&stream).as_deref(), Ok(source.as_slice()));
    }

    /// A stream that copies three bytes from five back and then ends, written out by hand.
    ///
    /// No encoder produces a stream whose first act is a copy, because there would be nothing to
    /// copy from, which is the whole reason MSZIP is worth a test of its own and why this one is
    /// three bytes from RFC 1951 rather than something `gzip` was asked for. Bit by bit, least
    /// significant first: `1` for the last block and `01` for the fixed code, then 0000001 which is
    /// the fixed code for symbol 257 and means a length of three, then 00100 which is distance
    /// symbol 4 and one extra bit of 0, which together mean five back, then 0000000 which is symbol
    /// 256 and ends the block. Twenty three bits, so the last one is padding.
    const COPY_FROM_FIVE_BACK: [u8; 3] = [0x03, 0x12, 0x00];

    #[test]
    fn a_stream_decoded_onto_history_copies_out_of_it() {
        // Which is the whole of MSZIP: each block of a folder is its own stream and reaches back
        // into the thirty two kilobytes the block before it produced.
        let mut out = b"abcdefgh".to_vec();
        assert_eq!(inflate_into(&COPY_FROM_FIVE_BACK, &mut out), Ok(3));
        assert_eq!(out, b"abcdefghdef");
    }

    #[test]
    fn a_stream_decoded_onto_nothing_cannot_copy_out_of_it() {
        // The same three bytes with no history under them. A copy that reaches back before anything
        // there is gets refused, rather than wrapping round or reading whatever is next to the
        // buffer.
        let mut out = Vec::new();
        let why = inflate_into(&COPY_FROM_FIVE_BACK, &mut out).unwrap_err();
        assert_eq!(why.what, Bad::TooFarBack);
        assert!(out.is_empty());
    }

    #[test]
    fn a_truncated_stream_says_so_rather_than_returning_what_it_managed() {
        let source =
            b"a string long enough that the encoder has something to do with it".repeat(40);
        let Some(stream) = gzipped(&source, "-9") else {
            return;
        };
        let cut = &stream[..stream.len() / 2];
        assert_eq!(inflate(cut).unwrap_err().what, Bad::Truncated);
    }

    #[test]
    fn the_reserved_block_type_is_refused() {
        // Bits 1 and 2 are the type and 3 is the value RFC 1951 reserves and has never assigned.
        assert_eq!(inflate(&[0x07]).unwrap_err().what, Bad::BlockType);
    }

    #[test]
    fn the_number_of_bytes_used_is_returned_so_a_caller_can_find_what_follows() {
        // A zip member's data is followed by the next member's header and a cabinet block's stream
        // by the next block's, so where this stream ended is a thing the caller needs.
        let mut stream = vec![0x01, 0x03, 0x00, 0xfc, 0xff];
        stream.extend_from_slice(b"abc");
        stream.extend_from_slice(b"and then something else entirely");
        let mut out = Vec::new();
        assert_eq!(inflate_into(&stream, &mut out), Ok(8));
        assert_eq!(out, b"abc");
    }

    #[test]
    fn an_error_says_how_far_in_it_happened() {
        let mut stream = vec![0x00, 0x03, 0x00, 0xfc, 0xff];
        stream.extend_from_slice(b"abc");
        // Not the last block, and there is no block after it.
        let why = inflate(&stream).unwrap_err();
        assert_eq!(why.what, Bad::Truncated);
        assert_eq!(why.at, 8);
        assert!(why.to_string().contains("8 bytes in"), "{why}");
    }
}
