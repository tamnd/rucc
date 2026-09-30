//! A zlib stream, which is what `-gz` stores the debug sections as.
//!
//! RFC 1950 around RFC 1951: a two byte header, deflate blocks, and the Adler-32 of what went in.
//! Written here rather than taken from a crate because spec/18-package-layout.md section 18.3 keeps
//! the list of dependencies short, and an encoder is the easy half of deflate. A reader has to
//! accept every stream anybody could write and a writer only has to produce one.
//!
//! The matcher is the one zlib uses at its default level: a hash of the next three bytes, a chain
//! of earlier places with the same hash, and one step of looking ahead before a match is taken.
//! Each block gets its own Huffman codes, or the fixed ones or no coding at all when either of
//! those comes out smaller, so what is written is never more than a few bytes over what went in.

use object::write::{Object as Writer, SectionId};
use object::{AddressSize, SectionFlags, SectionKind, elf};

use crate::section::{Chunk, Compress};

/// Adds one debug section to an object, stored the way `how` says.
///
/// The relocations against the section still count from the front of what it holds uncompressed,
/// which is what gas writes and what a linker expects: it unpacks the section before it applies
/// them.
pub(crate) fn debug_section(obj: &mut Writer<'_>, chunk: &Chunk, how: Compress) -> SectionId {
    let plain = |obj: &mut Writer<'_>| {
        let id = obj.add_section(Vec::new(), chunk.name.clone().into_bytes(), SectionKind::Debug);
        obj.append_section_data(id, &chunk.bytes, 1);
        id
    };
    if how == Compress::None || chunk.bytes.is_empty() {
        return plain(obj);
    }
    let packed = compress(&chunk.bytes);
    let size = chunk.bytes.len() as u64;
    match how {
        Compress::None => plain(obj),
        Compress::Zlib => {
            let wide = obj.architecture().address_size() != Some(AddressSize::U32);
            // `Elf64_Chdr` is the type, four reserved bytes, the size and the alignment, and
            // `Elf32_Chdr` is the same without the reserved word and with four byte fields.
            let mut bytes = Vec::with_capacity(24 + packed.len());
            bytes.extend_from_slice(&elf::ELFCOMPRESS_ZLIB.0.to_le_bytes());
            if wide {
                bytes.extend_from_slice(&0u32.to_le_bytes());
                bytes.extend_from_slice(&size.to_le_bytes());
                bytes.extend_from_slice(&1u64.to_le_bytes());
            } else {
                bytes.extend_from_slice(&(size as u32).to_le_bytes());
                bytes.extend_from_slice(&1u32.to_le_bytes());
            }
            if bytes.len() + packed.len() >= chunk.bytes.len() {
                return plain(obj);
            }
            bytes.extend_from_slice(&packed);
            let name = chunk.name.clone().into_bytes();
            let id = obj.add_section(Vec::new(), name, SectionKind::Debug);
            let section = obj.section_mut(id);
            section.flags =
                SectionFlags::Elf { sh_type: elf::SHT_PROGBITS, sh_flags: elf::SHF_COMPRESSED };
            obj.append_section_data(id, &bytes, if wide { 8 } else { 4 });
            id
        }
        Compress::ZlibGnu => {
            let Some(rest) = chunk.name.strip_prefix(".debug") else {
                return plain(obj);
            };
            if 12 + packed.len() >= chunk.bytes.len() {
                return plain(obj);
            }
            let mut bytes = Vec::with_capacity(12 + packed.len());
            bytes.extend_from_slice(b"ZLIB");
            bytes.extend_from_slice(&size.to_be_bytes());
            bytes.extend_from_slice(&packed);
            let name = format!(".zdebug{rest}").into_bytes();
            let id = obj.add_section(Vec::new(), name, SectionKind::Debug);
            obj.append_section_data(id, &bytes, 1);
            id
        }
    }
}

/// How far back a match may reach, which deflate fixes at 32 KiB.
const WINDOW: usize = 1 << 15;
/// The shortest and longest match deflate can say.
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
/// How many earlier places with the same hash are tried before the best so far is taken.
const CHAIN: usize = 128;
/// A match this long is taken without looking for a longer one.
const NICE: usize = 128;
/// How many symbols go in one block before its codes are worked out and it is written.
const BLOCK: usize = 1 << 14;
const HASH_BITS: u32 = 15;

/// The first length code and the extra bits and base of each of the 29 of them.
const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] =
    [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// The order the code length code's own lengths are written in.
const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

/// One thing a block says: a byte, or a length and a distance back.
#[derive(Clone, Copy)]
enum Sym {
    Lit(u8),
    Copy { len: u16, dist: u16 },
}

/// Those bytes as a zlib stream.
pub(crate) fn compress(data: &[u8]) -> Vec<u8> {
    let mut out = Bits::default();
    // Deflate with a 32 KiB window, and the level field saying default, which is what zlib writes
    // and what makes the header a multiple of 31.
    out.bytes.extend_from_slice(&[0x78, 0x9c]);
    let syms = matches(data);
    let mut from = 0;
    let mut chunks = syms.chunks(BLOCK).peekable();
    if chunks.peek().is_none() {
        block(&mut out, &[], &[], true);
    }
    while let Some(chunk) = chunks.next() {
        let len: usize = chunk
            .iter()
            .map(|sym| match sym {
                Sym::Lit(_) => 1,
                Sym::Copy { len, .. } => usize::from(*len),
            })
            .sum();
        block(&mut out, chunk, &data[from..from + len], chunks.peek().is_none());
        from += len;
    }
    out.flush();
    out.bytes.extend_from_slice(&adler(data).to_be_bytes());
    out.bytes
}

/// The Adler-32 of those bytes, which is what the stream ends with.
fn adler(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    // 5552 is the most bytes the two sums can take before the larger could pass 32 bits.
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

/// Those bytes as literals and matches.
fn matches(data: &[u8]) -> Vec<Sym> {
    let mut syms = Vec::with_capacity(data.len() / 2);
    let mut head = vec![usize::MAX; 1 << HASH_BITS];
    let mut prev = vec![usize::MAX; WINDOW];
    let hash = |at: usize| {
        let word =
            u32::from(data[at]) | u32::from(data[at + 1]) << 8 | u32::from(data[at + 2]) << 16;
        (word.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
    };
    let insert = |at: usize, head: &mut Vec<usize>, prev: &mut Vec<usize>| {
        if at + MIN_MATCH <= data.len() {
            let h = hash(at);
            prev[at % WINDOW] = head[h];
            head[h] = at;
        }
    };
    let longest = |at: usize, head: &Vec<usize>, prev: &Vec<usize>| -> (usize, usize) {
        if at + MIN_MATCH > data.len() {
            return (0, 0);
        }
        let most = (data.len() - at).min(MAX_MATCH);
        let (mut best, mut back) = (0, 0);
        let mut cand = head[hash(at)];
        let mut tries = CHAIN;
        while cand != usize::MAX && cand < at && at - cand <= WINDOW && tries > 0 {
            if data[cand + best.min(most - 1)] == data[at + best.min(most - 1)] {
                let len = data[cand..cand + most]
                    .iter()
                    .zip(&data[at..at + most])
                    .take_while(|(a, b)| a == b)
                    .count();
                if len > best {
                    best = len;
                    back = at - cand;
                    if len >= NICE.min(most) {
                        break;
                    }
                }
            }
            let next = prev[cand % WINDOW];
            // The slot may have been reused by a later place, which would send the chain forward.
            if next == usize::MAX || next >= cand {
                break;
            }
            cand = next;
            tries -= 1;
        }
        if best >= MIN_MATCH { (best, back) } else { (0, 0) }
    };
    let mut at = 0;
    while at < data.len() {
        let (len, back) = longest(at, &head, &prev);
        insert(at, &mut head, &mut prev);
        if len == 0 {
            syms.push(Sym::Lit(data[at]));
            at += 1;
            continue;
        }
        // One byte on may start a longer match, in which case this byte goes out as a literal.
        if len < NICE && at + 1 < data.len() {
            let (next, _) = longest(at + 1, &head, &prev);
            if next > len {
                syms.push(Sym::Lit(data[at]));
                at += 1;
                continue;
            }
        }
        syms.push(Sym::Copy { len: len as u16, dist: back as u16 });
        for skip in at + 1..at + len {
            insert(skip, &mut head, &mut prev);
        }
        at += len;
    }
    syms
}

/// The length code and its extra bits for a match of this length.
fn len_code(len: u16) -> (usize, u16, u8) {
    let at = LEN_BASE.iter().rposition(|&base| base <= len).expect("a length of three or more");
    (257 + at, len - LEN_BASE[at], LEN_EXTRA[at])
}

/// The distance code and its extra bits for a match this far back.
fn dist_code(dist: u16) -> (usize, u16, u8) {
    let at = DIST_BASE.iter().rposition(|&base| base <= dist).expect("a distance of one or more");
    (at, dist - DIST_BASE[at], DIST_EXTRA[at])
}

/// One block, written whichever of the three ways is smallest.
fn block(out: &mut Bits, syms: &[Sym], raw: &[u8], last: bool) {
    let mut lit = [0u32; 286];
    let mut dist = [0u32; 30];
    lit[256] = 1;
    for sym in syms {
        match *sym {
            Sym::Lit(byte) => lit[usize::from(byte)] += 1,
            Sym::Copy { len, dist: back } => {
                lit[len_code(len).0] += 1;
                dist[dist_code(back).0] += 1;
            }
        }
    }
    let lit_lens = lengths(&lit, 15);
    let dist_lens = lengths(&dist, 15);
    let header = Header::new(&lit_lens, &dist_lens);

    let body = |lens_lit: &[u8], lens_dist: &[u8]| -> u64 {
        let mut bits = 0u64;
        for (sym, &count) in lit.iter().enumerate() {
            let extra = if sym > 256 { u64::from(LEN_EXTRA[sym - 257]) } else { 0 };
            bits += u64::from(count) * (u64::from(lens_lit[sym]) + extra);
        }
        for (sym, &count) in dist.iter().enumerate() {
            bits += u64::from(count) * (u64::from(lens_dist[sym]) + u64::from(DIST_EXTRA[sym]));
        }
        bits
    };
    let (fixed_lit, fixed_dist) = fixed();
    let dynamic = 3 + header.bits() + body(&lit_lens, &dist_lens);
    let fixed = 3 + body(&fixed_lit, &fixed_dist);
    // A stored block is its bytes after a byte boundary and a four byte length, in pieces of at
    // most 65535 bytes.
    let stored = if raw.is_empty() {
        u64::MAX
    } else {
        let pieces = raw.len().div_ceil(65535) as u64;
        pieces * (3 + 7 + 32) + raw.len() as u64 * 8
    };

    if stored < dynamic && stored < fixed {
        let pieces: Vec<&[u8]> = raw.chunks(65535).collect();
        for (index, piece) in pieces.iter().enumerate() {
            out.put(u32::from(last && index + 1 == pieces.len()), 1);
            out.put(0, 2);
            out.align();
            let len = piece.len() as u16;
            out.bytes.extend_from_slice(&len.to_le_bytes());
            out.bytes.extend_from_slice(&(!len).to_le_bytes());
            out.bytes.extend_from_slice(piece);
        }
        return;
    }
    out.put(u32::from(last), 1);
    let (lit_lens, dist_lens) = if fixed <= dynamic {
        out.put(1, 2);
        (fixed_lit, fixed_dist)
    } else {
        out.put(2, 2);
        header.write(out);
        (lit_lens, dist_lens)
    };
    let lit_codes = codes(&lit_lens);
    let dist_codes = codes(&dist_lens);
    for sym in syms {
        match *sym {
            Sym::Lit(byte) => out.put(lit_codes[usize::from(byte)], lit_lens[usize::from(byte)]),
            Sym::Copy { len, dist: back } => {
                let (code, extra, bits) = len_code(len);
                out.put(lit_codes[code], lit_lens[code]);
                out.put(u32::from(extra), bits);
                let (code, extra, bits) = dist_code(back);
                out.put(dist_codes[code], dist_lens[code]);
                out.put(u32::from(extra), bits);
            }
        }
    }
    out.put(lit_codes[256], lit_lens[256]);
}

/// The lengths of the fixed codes RFC 1951 section 3.2.6 gives.
fn fixed() -> (Vec<u8>, Vec<u8>) {
    let mut lit = vec![8u8; 288];
    lit[144..256].fill(9);
    lit[256..280].fill(7);
    (lit, vec![5u8; 30])
}

/// The code lengths of a dynamic block's two tables, and how they are written.
struct Header {
    /// How many lengths of each table are written, which leaves off the zeros at the end.
    hlit: usize,
    hdist: usize,
    /// The lengths run length coded: a symbol of the code length code and its extra bits.
    runs: Vec<(u8, u8)>,
    /// The code length code's own lengths.
    lens: Vec<u8>,
    /// How many of those are written, in [`ORDER`].
    hclen: usize,
}

impl Header {
    fn new(lit: &[u8], dist: &[u8]) -> Header {
        let hlit = 257.max(lit.iter().rposition(|&len| len != 0).map_or(0, |at| at + 1));
        let hdist = 1.max(dist.iter().rposition(|&len| len != 0).map_or(0, |at| at + 1));
        let all: Vec<u8> = lit[..hlit].iter().chain(&dist[..hdist]).copied().collect();
        let mut runs = Vec::new();
        let mut at = 0;
        while at < all.len() {
            let len = all[at];
            let run = all[at..].iter().take_while(|&&next| next == len).count();
            if len == 0 && run >= 3 {
                let take = run.min(138);
                if take >= 11 {
                    runs.push((18, (take - 11) as u8));
                } else {
                    runs.push((17, (take - 3) as u8));
                }
                at += take;
            } else if len != 0 && run >= 4 {
                runs.push((len, 0));
                let take = (run - 1).min(6);
                runs.push((16, (take - 3) as u8));
                at += 1 + take;
            } else {
                runs.push((len, 0));
                at += 1;
            }
        }
        let mut freq = [0u32; 19];
        for &(sym, _) in &runs {
            freq[usize::from(sym)] += 1;
        }
        let lens = lengths(&freq, 7);
        let hclen = 4.max(ORDER.iter().rposition(|&sym| lens[sym] != 0).map_or(0, |at| at + 1));
        Header { hlit, hdist, runs, lens, hclen }
    }

    /// How many bits this takes.
    fn bits(&self) -> u64 {
        let mut bits = 5 + 5 + 4 + 3 * self.hclen as u64;
        for &(sym, _) in &self.runs {
            bits += u64::from(self.lens[usize::from(sym)]);
            bits += match sym {
                16 => 2,
                17 => 3,
                18 => 7,
                _ => 0,
            };
        }
        bits
    }

    fn write(&self, out: &mut Bits) {
        out.put((self.hlit - 257) as u32, 5);
        out.put((self.hdist - 1) as u32, 5);
        out.put((self.hclen - 4) as u32, 4);
        for &sym in &ORDER[..self.hclen] {
            out.put(u32::from(self.lens[sym]), 3);
        }
        let codes = codes(&self.lens);
        for &(sym, extra) in &self.runs {
            let sym = usize::from(sym);
            out.put(codes[sym], self.lens[sym]);
            match sym {
                16 => out.put(u32::from(extra), 2),
                17 => out.put(u32::from(extra), 3),
                18 => out.put(u32::from(extra), 7),
                _ => {}
            }
        }
    }
}

/// Huffman code lengths for these counts, none longer than `limit`.
///
/// Every table comes out with at least two codes in it. A table of one code is a set a reader has
/// to take on trust as incomplete and zlib's own reader takes that only for the two main tables,
/// so the second code is given to a symbol that never appears, which costs a bit at most.
fn lengths(freq: &[u32], limit: u8) -> Vec<u8> {
    let mut freq = freq.to_vec();
    for sym in 0..freq.len() {
        if freq.iter().filter(|&&count| count > 0).count() >= 2 {
            break;
        }
        if freq[sym] == 0 {
            freq[sym] = 1;
        }
    }
    // The symbols that appear, least frequent first, and a tree built over them the usual way,
    // two lowest at a time. Depths are read back from the parents.
    let mut syms: Vec<usize> = (0..freq.len()).filter(|&sym| freq[sym] > 0).collect();
    syms.sort_by_key(|&sym| (freq[sym], sym));
    let n = syms.len();
    let mut weight: Vec<u64> = syms.iter().map(|&sym| u64::from(freq[sym])).collect();
    let mut parent = vec![usize::MAX; 2 * n];
    // Two queues, the leaves in order and the joined nodes in the order they are made, which are
    // already in order of weight, so the lowest two are always at the front of one or the other.
    let (mut leaf, mut node) = (0, n);
    let take = |weight: &Vec<u64>, leaf: &mut usize, node: &mut usize| {
        if *leaf < n && (*node >= weight.len() || weight[*leaf] <= weight[*node]) {
            *leaf += 1;
            *leaf - 1
        } else {
            *node += 1;
            *node - 1
        }
    };
    for _ in 0..n - 1 {
        let a = take(&weight, &mut leaf, &mut node);
        let b = take(&weight, &mut leaf, &mut node);
        let joined = weight.len();
        weight.push(weight[a] + weight[b]);
        parent[a] = joined;
        parent[b] = joined;
    }
    let root = weight.len() - 1;
    let mut depth = vec![0u32; weight.len()];
    for at in (0..root).rev() {
        depth[at] = depth[parent[at]] + 1;
    }
    // How many codes of each length, with anything past the limit brought back to it and the sum
    // then made to fit again by pushing a shorter code down a level for each one over, which is
    // the way miniz does it.
    let limit = usize::from(limit);
    let mut count = vec![0u32; limit + 1];
    for &at in depth[..n].iter() {
        count[(at as usize).min(limit)] += 1;
    }
    let full = 1u64 << limit;
    let mut total: u64 = (1..=limit).map(|len| u64::from(count[len]) << (limit - len)).sum();
    while total > full {
        count[limit] -= 1;
        for len in (1..limit).rev() {
            if count[len] > 0 {
                count[len] -= 1;
                count[len + 1] += 2;
                break;
            }
        }
        total -= 1;
    }
    // Shortest codes to the most frequent symbols.
    let mut lens = vec![0u8; freq.len()];
    let mut next = syms.iter().rev();
    for (len, &many) in count.iter().enumerate().skip(1) {
        for _ in 0..many {
            let sym = next.next().expect("a length for every symbol");
            lens[*sym] = len as u8;
        }
    }
    lens
}

/// The canonical codes for these lengths, bit reversed, since deflate sends a code's top bit first
/// and everything else bottom bit first.
fn codes(lens: &[u8]) -> Vec<u32> {
    let mut count = [0u32; 16];
    for &len in lens {
        count[usize::from(len)] += 1;
    }
    count[0] = 0;
    let mut next = [0u32; 16];
    let mut code = 0;
    for len in 1..16 {
        code = (code + count[len - 1]) << 1;
        next[len] = code;
    }
    lens.iter()
        .map(|&len| {
            if len == 0 {
                return 0;
            }
            let code = next[usize::from(len)];
            next[usize::from(len)] += 1;
            code.reverse_bits() >> (32 - u32::from(len))
        })
        .collect()
}

/// Bits packed bottom first into bytes, which is the order deflate reads them in.
#[derive(Default)]
struct Bits {
    bytes: Vec<u8>,
    held: u64,
    count: u32,
}

impl Bits {
    fn put(&mut self, value: u32, bits: u8) {
        self.held |= u64::from(value) << self.count;
        self.count += u32::from(bits);
        while self.count >= 8 {
            self.bytes.push(self.held as u8);
            self.held >>= 8;
            self.count -= 8;
        }
    }

    /// Out to the next byte boundary, which a stored block starts on.
    fn align(&mut self) {
        if self.count > 0 {
            self.bytes.push(self.held as u8);
            self.held = 0;
            self.count = 0;
        }
    }

    fn flush(&mut self) {
        self.align();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader, just enough of one to take back what the writer above wrote.
    ///
    /// Written from RFC 1951 rather than from the writer, so that a mistake in one is not repeated
    /// in the other. The streams were also read back with Python's `zlib` and with `readelf -z` on
    /// a compressed object when this was written.
    fn inflate(stream: &[u8]) -> Vec<u8> {
        assert_eq!(stream[0], 0x78);
        assert_eq!((u16::from(stream[0]) << 8 | u16::from(stream[1])) % 31, 0);
        let mut at = 16usize;
        let bit = |at: &mut usize| {
            let value = (stream[*at / 8] >> (*at % 8)) & 1;
            *at += 1;
            u32::from(value)
        };
        let bits = |at: &mut usize, n: u8| (0..n).fold(0, |acc, i| acc | bit(at) << i);
        // Canonical: the codes of each length are consecutive and follow the shorter ones, so a
        // code is read one bit at a time until it falls inside the run for its length.
        let table = |lens: &[u8]| -> Vec<(u32, Vec<usize>)> {
            let mut first = 0u32;
            (1..=15u8)
                .map(|len| {
                    let syms: Vec<usize> =
                        (0..lens.len()).filter(|&sym| lens[sym] == len).collect();
                    let here = first;
                    first = (first + syms.len() as u32) << 1;
                    (here, syms)
                })
                .collect()
        };
        let decode = |at: &mut usize, table: &[(u32, Vec<usize>)]| -> usize {
            let mut code = 0u32;
            for (first, syms) in table {
                code = code << 1 | bit(at);
                if code >= *first && code - first < syms.len() as u32 {
                    return syms[(code - first) as usize];
                }
            }
            panic!("no code");
        };
        let mut out = Vec::new();
        loop {
            let last = bit(&mut at);
            let kind = bits(&mut at, 2);
            if kind == 0 {
                at = at.div_ceil(8) * 8;
                let len = usize::from(stream[at / 8]) | usize::from(stream[at / 8 + 1]) << 8;
                let byte = at / 8 + 4;
                out.extend_from_slice(&stream[byte..byte + len]);
                at = (byte + len) * 8;
            } else {
                let (lit, dist) = if kind == 1 {
                    fixed()
                } else {
                    assert_eq!(kind, 2);
                    let hlit = bits(&mut at, 5) as usize + 257;
                    let hdist = bits(&mut at, 5) as usize + 1;
                    let hclen = bits(&mut at, 4) as usize + 4;
                    let mut cl = vec![0u8; 19];
                    for &sym in &ORDER[..hclen] {
                        cl[sym] = bits(&mut at, 3) as u8;
                    }
                    let cl = table(&cl);
                    let mut all = Vec::new();
                    while all.len() < hlit + hdist {
                        match decode(&mut at, &cl) {
                            16 => {
                                let prev = *all.last().expect("a length to repeat");
                                for _ in 0..3 + bits(&mut at, 2) {
                                    all.push(prev);
                                }
                            }
                            17 => all.extend(std::iter::repeat_n(0, 3 + bits(&mut at, 3) as usize)),
                            18 => {
                                all.extend(std::iter::repeat_n(0, 11 + bits(&mut at, 7) as usize))
                            }
                            len => all.push(len as u8),
                        }
                    }
                    (all[..hlit].to_vec(), all[hlit..].to_vec())
                };
                let (lit, dist) = (table(&lit), table(&dist));
                loop {
                    let sym = decode(&mut at, &lit);
                    if sym < 256 {
                        out.push(sym as u8);
                    } else if sym == 256 {
                        break;
                    } else {
                        let len = LEN_BASE[sym - 257] as usize
                            + bits(&mut at, LEN_EXTRA[sym - 257]) as usize;
                        let code = decode(&mut at, &dist);
                        let back =
                            DIST_BASE[code] as usize + bits(&mut at, DIST_EXTRA[code]) as usize;
                        for _ in 0..len {
                            out.push(out[out.len() - back]);
                        }
                    }
                }
            }
            if last == 1 {
                break;
            }
        }
        let end = at.div_ceil(8);
        let sum = u32::from_be_bytes(stream[end..end + 4].try_into().expect("four bytes"));
        assert_eq!(sum, adler(&out));
        assert_eq!(end + 4, stream.len());
        out
    }

    fn round_trip(data: &[u8]) -> usize {
        let packed = compress(data);
        assert_eq!(inflate(&packed), data);
        packed.len()
    }

    #[test]
    fn nothing_and_one_byte_come_back() {
        round_trip(b"");
        round_trip(b"a");
        round_trip(b"ab");
    }

    #[test]
    fn a_repeated_string_comes_back_much_smaller() {
        let data = b"int main(void) { return 0; }\n".repeat(400);
        let size = round_trip(&data);
        assert!(size * 20 < data.len(), "{size} of {}", data.len());
    }

    #[test]
    fn bytes_with_no_pattern_come_back_barely_larger() {
        // A small generator rather than a crate, so the bytes are the same on every run.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let data: Vec<u8> = (0..200_000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 24) as u8
            })
            .collect();
        let size = round_trip(&data);
        assert!(size < data.len() + data.len() / 1000 + 64, "{size} of {}", data.len());
    }

    #[test]
    fn text_with_a_skewed_alphabet_uses_codes_of_many_lengths() {
        // Counts that fall off by half each time, which is what drives a Huffman tree past fifteen
        // levels and so exercises the limit.
        let mut data = Vec::new();
        for (index, byte) in (b'a'..=b'z').enumerate() {
            let times = 1usize << (25 - index).min(20);
            data.extend(std::iter::repeat_n(byte, times.min(1 << 20)));
            data.push(b'\n');
        }
        // Shuffle a little so that most of it is literals rather than one long run.
        let mut state = 7u32;
        for at in (1..data.len()).rev() {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let other = (state as usize) % (at + 1);
            data.swap(at, other);
        }
        round_trip(&data);
    }

    #[test]
    fn a_long_run_of_one_byte_is_matched_against_itself() {
        let size = round_trip(&[0u8; 100_000]);
        assert!(size < 200, "{size}");
    }

    #[test]
    fn the_adler_sum_is_the_known_one() {
        assert_eq!(adler(b"Wikipedia"), 0x11E6_0398);
    }
}
