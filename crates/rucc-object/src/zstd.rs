//! A zstd frame, which is what `-gz=zstd` stores the debug sections as.
//!
//! RFC 8878. The frame is a header and a run of blocks of at most 128 KiB each. A compressed block
//! holds the literals, coded with Huffman when that is shorter, and the sequences, each a count of
//! literals, a match length and an offset, coded with FSE tables worked out for that block. No
//! checksum is written, since the section header already says how large the section was and the
//! readers that matter, ld and gdb, do not ask for one.
//!
//! The matches come from the same matcher deflate uses in `crate::zlib`, with a 256 KiB window.
//! Repeat offsets are never used, which costs a little on some inputs and keeps the sequence
//! coding plain.
//!
//! Every bitstream in a block is read by the decoder from its last byte back to its first, so each
//! one is built here as the list of fields in the order the decoder reads them and then written
//! out backwards. That keeps the order the format asks for in one place per stream.

use crate::zlib::{Bits, Matcher, Sym, lengths};

/// The window is 2^18 bytes, which is what the frame header says a reader has to keep.
const WINDOW_LOG: u32 = 18;
/// The most a block may hold before it is compressed.
const BLOCK: usize = 1 << 17;
/// The longest match. The match length codes go well past this.
const MOST: usize = 1 << 16;
/// The longest Huffman code a literal may have.
const HUFF_LOG: u8 = 11;

/// The first literal count and match length of each code, and how many extra bits follow it.
const LL_BASE: [u32; 36] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24, 28, 32, 40, 48, 64,
    128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768, 65536,
];
const LL_BITS: [u8; 36] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 6, 7, 8, 9, 10, 11,
    12, 13, 14, 15, 16,
];
const ML_BASE: [u32; 53] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27,
    28, 29, 30, 31, 32, 33, 34, 35, 37, 39, 41, 43, 47, 51, 59, 67, 83, 99, 131, 259, 515, 1027,
    2051, 4099, 8195, 16387, 32771, 65539,
];
const ML_BITS: [u8; 53] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    1, 1, 1, 1, 2, 2, 3, 3, 4, 4, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
];

/// Those bytes as a zstd frame.
pub(crate) fn compress(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 3 + 16);
    out.extend_from_slice(&0xFD2F_B528u32.to_le_bytes());
    // The descriptor: the content size in four or eight bytes, a window descriptor rather than a
    // single segment, no checksum and no dictionary.
    let wide = u32::try_from(data.len()).is_err();
    out.push(if wide { 3 << 6 } else { 2 << 6 });
    out.push(((WINDOW_LOG - 10) << 3) as u8);
    if wide {
        out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    } else {
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    }
    if data.is_empty() {
        // One raw block with nothing in it, marked last.
        out.extend_from_slice(&[1, 0, 0]);
        return out;
    }
    let mut matcher = Matcher::new(data, 1 << WINDOW_LOG, MOST);
    let mut from = 0;
    while from < data.len() {
        let end = (from + BLOCK).min(data.len());
        let raw = &data[from..end];
        let syms = matcher.run(from, end);
        let (kind, body) = match block(&syms) {
            Some(body) if body.len() < raw.len() => (2, body),
            _ => (0, raw.to_vec()),
        };
        let header = u32::from(end == data.len()) | kind << 1 | (body.len() as u32) << 3;
        out.extend_from_slice(&header.to_le_bytes()[..3]);
        out.extend_from_slice(&body);
        from = end;
    }
    out
}

/// One sequence: this many literals, then a copy of `len` bytes from `back` bytes behind.
struct Seq {
    lits: u32,
    len: u32,
    back: u32,
}

/// The body of a compressed block holding these literals and matches, or nothing when some part
/// of it cannot be written this way and the block should go out raw.
fn block(syms: &[Sym]) -> Option<Vec<u8>> {
    let mut lits = Vec::new();
    let mut seqs = Vec::new();
    let mut run = 0;
    for sym in syms {
        match *sym {
            Sym::Lit(byte) => {
                lits.push(byte);
                run += 1;
            }
            Sym::Copy { len, dist } => {
                seqs.push(Seq { lits: run, len, back: dist });
                run = 0;
            }
        }
    }
    // The literals after the last match need no sequence: a reader copies what is left over.
    let mut out = Vec::new();
    literals(&mut out, &lits);
    sequences(&mut out, &seqs)?;
    Some(out)
}

/// The literals section: the bytes as they are, one byte repeated, or Huffman coded, whichever
/// is shortest.
fn literals(out: &mut Vec<u8>, lits: &[u8]) {
    let plain = |out: &mut Vec<u8>, kind: u32, size: usize| {
        let size = size as u32;
        if size < 32 {
            out.push((kind | size << 3) as u8);
        } else if size < 4096 {
            out.extend_from_slice(&(kind | 1 << 2 | size << 4).to_le_bytes()[..2]);
        } else {
            out.extend_from_slice(&(kind | 3 << 2 | size << 4).to_le_bytes()[..3]);
        }
    };
    if !lits.is_empty() && lits.iter().all(|&byte| byte == lits[0]) {
        plain(out, 1, lits.len());
        out.push(lits[0]);
        return;
    }
    if let Some((four, body)) = huffman(lits) {
        let (format, bits) = match lits.len() {
            _ if !four => (0, 10),
            0..1024 => (1, 10),
            1024..16384 => (2, 14),
            _ => (3, 18),
        };
        let header_len = if bits == 10 {
            3
        } else if bits == 14 {
            4
        } else {
            5
        };
        if header_len + body.len() < lits.len() {
            let header =
                2 | format << 2 | (lits.len() as u64) << 4 | (body.len() as u64) << (4 + bits);
            out.extend_from_slice(&header.to_le_bytes()[..header_len]);
            out.extend_from_slice(&body);
            return;
        }
    }
    plain(out, 0, lits.len());
    out.extend_from_slice(lits);
}

/// The literals Huffman coded: the description of the code, then one stream when there are few
/// of them and four with a jump table in front when there are more. Says which along with the
/// bytes, or nothing when the literals do not suit a code.
fn huffman(lits: &[u8]) -> Option<(bool, Vec<u8>)> {
    let mut freq = [0u32; 256];
    for &byte in lits {
        freq[usize::from(byte)] += 1;
    }
    if freq.iter().filter(|&&count| count > 0).count() < 2 {
        return None;
    }
    let last = freq.iter().rposition(|&count| count > 0)?;
    let lens = lengths(&freq[..=last], HUFF_LOG);
    let most = *lens.iter().max()?;
    // A weight is one more than how much shorter than the longest a code is, and nothing for a
    // byte that is not there.
    let weights: Vec<u8> =
        lens.iter().map(|&len| if len == 0 { 0 } else { most + 1 - len }).collect();
    // Codes go out longest first, and within a length by byte, counting up from zero. Counted
    // in units of the longest code, each code takes 2^(weight - 1) of them.
    let mut codes = vec![0u32; weights.len()];
    let mut next = 0u32;
    for weight in 1..=most {
        for (byte, _) in weights.iter().enumerate().filter(|&(_, &w)| w == weight) {
            codes[byte] = next >> (weight - 1);
            next += 1 << (weight - 1);
        }
    }
    let mut out = Vec::new();
    // The last weight is left out, since the others say what it has to be.
    tree(&mut out, &weights[..last])?;
    let stream = |part: &[u8]| {
        let mut bits = Bits::default();
        for &byte in part.iter().rev() {
            bits.put(codes[usize::from(byte)], lens[usize::from(byte)]);
        }
        close(bits)
    };
    if lits.len() < 1024 {
        out.extend_from_slice(&stream(lits));
        return Some((false, out));
    }
    let quarter = lits.len().div_ceil(4);
    let parts: Vec<Vec<u8>> = lits.chunks(quarter).map(stream).collect();
    for part in &parts[..3] {
        out.extend_from_slice(&u16::try_from(part.len()).ok()?.to_le_bytes());
    }
    for part in &parts {
        out.extend_from_slice(part);
    }
    Some((true, out))
}

/// The Huffman weights, FSE coded with two states taking turns, or four bits each when that is
/// shorter or FSE cannot do it.
fn tree(out: &mut Vec<u8>, weights: &[u8]) -> Option<()> {
    let direct = (weights.len() <= 128).then(|| {
        let mut bytes = vec![127 + weights.len() as u8];
        for pair in weights.chunks(2) {
            bytes.push(pair[0] << 4 | pair.get(1).copied().unwrap_or(0));
        }
        bytes
    });
    let fse = fse_weights(weights).filter(|body| body.len() < 128);
    let bytes = match (direct, fse) {
        (Some(direct), Some(fse)) if direct.len() <= fse.len() + 1 => direct,
        (_, Some(fse)) => {
            let mut bytes = vec![fse.len() as u8];
            bytes.extend_from_slice(&fse);
            bytes
        }
        (direct, None) => direct?,
    };
    out.extend_from_slice(&bytes);
    Some(())
}

/// The weights as an FSE table description and a stream read by two states in turn, the first
/// weight from the first state, the second from the second, and so on.
fn fse_weights(weights: &[u8]) -> Option<Vec<u8>> {
    if weights.len() < 2 {
        return None;
    }
    let mut count = [0u32; 13];
    for &weight in weights {
        count[usize::from(weight)] += 1;
    }
    let log = table_log(6, weights.len(), 12);
    let norm = normalize(&count, log)?;
    let table = Table::new(&norm, log);
    // Each state's chain is every other weight. A reader knows it has read the last weight when
    // the step after it runs off the front of the stream, so both chains have to end on a state
    // that takes at least one bit to leave.
    let first: Vec<u8> = weights.iter().copied().step_by(2).collect();
    let second: Vec<u8> = weights.iter().copied().skip(1).step_by(2).collect();
    let (one, two) = (table.states(&first, true)?, table.states(&second, true)?);
    let mut reads = vec![(one[0] as u32, log as u8), (two[0] as u32, log as u8)];
    for at in 0..weights.len() {
        let (chain, i) = if at % 2 == 0 { (&one, at / 2) } else { (&two, at / 2) };
        if let Some(&next) = chain.get(i + 1) {
            reads.push(table.step(chain[i], next));
        }
    }
    let mut out = Vec::new();
    ncount(&mut out, &norm, log);
    out.extend_from_slice(&backwards(&reads));
    Some(out)
}

/// The sequences section: how many, how each of the three kinds of code is coded, the tables,
/// then one stream with all of it.
fn sequences(out: &mut Vec<u8>, seqs: &[Seq]) -> Option<()> {
    let n = seqs.len();
    if n < 128 {
        out.push(n as u8);
    } else if n < 0x7F00 {
        out.extend_from_slice(&[((n >> 8) + 0x80) as u8, n as u8]);
    } else {
        out.push(0xFF);
        out.extend_from_slice(&u16::try_from(n - 0x7F00).ok()?.to_le_bytes());
    }
    if n == 0 {
        return Some(());
    }
    let code = |value: u32, base: &[u32], bits: &[u8]| {
        let at = base.iter().rposition(|&b| b <= value).expect("a code for every value");
        (at as u8, value - base[at], bits[at])
    };
    let ll: Vec<(u8, u32, u8)> = seqs.iter().map(|s| code(s.lits, &LL_BASE, &LL_BITS)).collect();
    let ml: Vec<(u8, u32, u8)> = seqs.iter().map(|s| code(s.len, &ML_BASE, &ML_BITS)).collect();
    // An offset is written three more than it is, so that 1 to 3 can mean the repeat offsets,
    // and its code is how many bits that takes less one.
    let of: Vec<(u8, u32, u8)> = seqs
        .iter()
        .map(|s| {
            let value = s.back + 3;
            let bits = 31 - value.leading_zeros();
            (bits as u8, value - (1 << bits), bits as u8)
        })
        .collect();
    let modes_at = out.len();
    out.push(0);
    let mut modes = 0u8;
    let mut tables = Vec::new();
    for (shift, codes, max_log, alphabet) in [(6, &ll, 9, 36), (4, &of, 8, 32), (2, &ml, 9, 53)] {
        let syms: Vec<u8> = codes.iter().map(|&(sym, _, _)| sym).collect();
        let mut count = vec![0u32; alphabet];
        for &sym in &syms {
            count[usize::from(sym)] += 1;
        }
        let table = if count.iter().filter(|&&c| c > 0).count() == 1 {
            // One code only: say which, and the decoder reads no bits for it at all.
            modes |= 1 << shift;
            out.push(syms[0]);
            Table::one(syms[0], alphabet)
        } else {
            modes |= 2 << shift;
            let log = table_log(max_log, n, alphabet - 1);
            let norm = normalize(&count, log)?;
            ncount(out, &norm, log);
            Table::new(&norm, log)
        };
        let states = table.states(&syms, false)?;
        tables.push((table, states));
    }
    out[modes_at] = modes;
    let [(ll_t, ll_s), (of_t, of_s), (ml_t, ml_s)] = &tables[..] else { unreachable!() };
    let mut reads = vec![
        (ll_s[0] as u32, ll_t.log as u8),
        (of_s[0] as u32, of_t.log as u8),
        (ml_s[0] as u32, ml_t.log as u8),
    ];
    for i in 0..n {
        reads.push((of[i].1, of[i].2));
        reads.push((ml[i].1, ml[i].2));
        reads.push((ll[i].1, ll[i].2));
        if i + 1 < n {
            reads.push(ll_t.step(ll_s[i], ll_s[i + 1]));
            reads.push(ml_t.step(ml_s[i], ml_s[i + 1]));
            reads.push(of_t.step(of_s[i], of_s[i + 1]));
        }
    }
    out.extend_from_slice(&backwards(&reads));
    Some(())
}

/// Fields the decoder reads in this order, written so that it does: the last one first, then a
/// one bit to mark where the stream ends.
fn backwards(reads: &[(u32, u8)]) -> Vec<u8> {
    let mut bits = Bits::default();
    for &(value, width) in reads.iter().rev() {
        bits.put(value, width);
    }
    close(bits)
}

fn close(mut bits: Bits) -> Vec<u8> {
    bits.put(1, 1);
    bits.align();
    bits.bytes
}

/// How many bits of state an FSE table for `n` codes of at most `top` should have, the way the
/// reference encoder picks it: no more than `most`, no fewer than five, and enough for every code
/// to get a state.
fn table_log(most: u32, n: usize, top: usize) -> u32 {
    let high = |x: usize| usize::BITS - 1 - x.max(1).leading_zeros();
    let mut log = most;
    let from_src = high(n - 1).saturating_sub(2);
    if from_src < log {
        log = from_src;
    }
    let least = (high(n - 1) + 1).min(high(top) + 2);
    log.max(least).clamp(5, most)
}

/// The counts scaled to add up to 2^log, with every code that was seen given at least one state.
fn normalize(count: &[u32], log: u32) -> Option<Vec<u32>> {
    let size = 1u64 << log;
    let total: u64 = count.iter().map(|&c| u64::from(c)).sum();
    if count.iter().filter(|&&c| c > 0).count() as u64 > size {
        return None;
    }
    let mut norm: Vec<u32> = count
        .iter()
        .map(|&c| if c == 0 { 0 } else { ((u64::from(c) * size / total) as u32).max(1) })
        .collect();
    let mut sum: u64 = norm.iter().map(|&c| u64::from(c)).sum();
    let largest = (0..count.len()).max_by_key(|&at| count[at])?;
    if sum < size {
        norm[largest] += (size - sum) as u32;
        sum = size;
    }
    while sum > size {
        let at = (0..norm.len()).max_by_key(|&at| norm[at])?;
        norm[at] -= 1;
        sum -= 1;
    }
    Some(norm)
}

/// An FSE table description, as `FSE_writeNCount` in the reference encoder writes one.
fn ncount(out: &mut Vec<u8>, norm: &[u32], log: u32) {
    let mut bits = Bits::default();
    bits.put(log - 5, 4);
    let size = 1i32 << log;
    let mut remaining = size + 1;
    let mut threshold = size;
    let mut width = log + 1;
    let top = norm.iter().rposition(|&c| c > 0).map_or(0, |at| at + 1);
    let mut sym = 0;
    let mut zero_before = false;
    while sym < top && remaining > 1 {
        if zero_before {
            let mut start = sym;
            while norm[sym] == 0 {
                sym += 1;
            }
            while sym >= start + 24 {
                start += 24;
                bits.put(0xFFFF, 16);
            }
            while sym >= start + 3 {
                start += 3;
                bits.put(3, 2);
            }
            bits.put((sym - start) as u32, 2);
        }
        let mut count = norm[sym] as i32;
        sym += 1;
        let max = 2 * threshold - 1 - remaining;
        remaining -= count;
        count += 1;
        if count >= threshold {
            count += max;
        }
        bits.put(count as u32, (width - u32::from(count < max)) as u8);
        zero_before = count == 1;
        while remaining < threshold {
            width -= 1;
            threshold >>= 1;
        }
    }
    bits.align();
    out.extend_from_slice(&bits.bytes);
}

/// An FSE decoding table, and for each code and each state a decoder could go to next, the state
/// it has to leave from to decode that code first.
struct Table {
    log: u32,
    sym: Vec<u8>,
    bits: Vec<u8>,
    base: Vec<u32>,
    from: Vec<Vec<u16>>,
}

impl Table {
    /// The table a decoder builds from this description, following RFC 8878 section 4.1.1.
    fn new(norm: &[u32], log: u32) -> Self {
        let size = 1usize << log;
        let mask = size - 1;
        let step = (size >> 1) + (size >> 3) + 3;
        let mut sym = vec![0u8; size];
        let mut at = 0;
        for (code, &many) in norm.iter().enumerate() {
            for _ in 0..many {
                sym[at] = code as u8;
                at = (at + step) & mask;
            }
        }
        let mut next = norm.to_vec();
        let mut bits = vec![0u8; size];
        let mut base = vec![0u32; size];
        let mut from = vec![Vec::new(); norm.len()];
        for state in 0..size {
            let code = usize::from(sym[state]);
            let x = next[code];
            next[code] += 1;
            let width = log - (31 - x.leading_zeros());
            bits[state] = width as u8;
            base[state] = (x << width) - size as u32;
            let reach = &mut from[code];
            if reach.is_empty() {
                reach.resize(size, 0);
            }
            for to in base[state]..base[state] + (1 << width) {
                reach[to as usize] = state as u16;
            }
        }
        Table { log, sym, bits, base, from }
    }

    /// The table for one code only, which has one state and takes no bits.
    fn one(code: u8, alphabet: usize) -> Self {
        let mut from = vec![Vec::new(); alphabet];
        from[usize::from(code)] = vec![0];
        Table { log: 0, sym: vec![code], bits: vec![0], base: vec![0], from }
    }

    /// The state the decoder is in for each of these codes, which it decodes in order. The last
    /// may be any state of its code, and is taken to be one that needs the most bits to leave, and
    /// that must be at least one when `leave` says so.
    fn states(&self, codes: &[u8], leave: bool) -> Option<Vec<usize>> {
        let last = *codes.last()?;
        let end = (0..self.sym.len())
            .filter(|&state| self.sym[state] == last)
            .max_by_key(|&state| self.bits[state])?;
        if leave && self.bits[end] == 0 {
            return None;
        }
        let mut states = vec![0; codes.len()];
        states[codes.len() - 1] = end;
        for at in (0..codes.len() - 1).rev() {
            states[at] = usize::from(self.from[usize::from(codes[at])][states[at + 1]]);
        }
        Some(states)
    }

    /// The bits that take the decoder from state `at` to state `to`.
    fn step(&self, at: usize, to: usize) -> (u32, u8) {
        (to as u32 - self.base[at], self.bits[at])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A few kilobytes that look like a debug section: names, small numbers, and runs of zeros.
    fn sample(size: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut seed = 7u32;
        while out.len() < size {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            match seed >> 29 {
                0 | 1 => out.extend_from_slice(b"unsigned long "),
                2 => out.extend_from_slice(&(seed >> 8).to_le_bytes()),
                3 => out.extend_from_slice(&[0; 9]),
                4 => out.push((seed >> 16) as u8),
                _ => out.extend_from_slice(b"__kernel_size_t\0"),
            }
        }
        out.truncate(size);
        out
    }

    #[test]
    fn a_frame_starts_with_the_magic_and_says_how_large_its_content_is() {
        let data = sample(5000);
        let frame = compress(&data);
        assert_eq!(frame[..4], [0x28, 0xB5, 0x2F, 0xFD]);
        assert_eq!(frame[4], 2 << 6);
        assert_eq!(frame[5], 8 << 3);
        assert_eq!(u32::from_le_bytes(frame[6..10].try_into().unwrap()), 5000);
        assert!(frame.len() < data.len() / 2, "{} bytes from {}", frame.len(), data.len());
    }

    #[test]
    fn nothing_is_one_empty_raw_block() {
        let frame = compress(&[]);
        assert_eq!(frame[10..], [1, 0, 0]);
    }

    #[test]
    fn bytes_that_do_not_compress_go_out_raw() {
        let mut seed = 1u64;
        let data: Vec<u8> = (0..3000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            })
            .collect();
        let frame = compress(&data);
        let header = u32::from_le_bytes([frame[10], frame[11], frame[12], 0]);
        assert_eq!(header & 1, 1);
        assert_eq!(header >> 1 & 3, 0);
        assert_eq!(frame[13..], data[..]);
    }

    #[test]
    fn an_fse_table_reaches_every_state_from_every_code() {
        let norm = normalize(&[40, 3, 0, 17, 1, 0, 0, 2], 6).unwrap();
        assert_eq!(norm.iter().sum::<u32>(), 64);
        assert!(norm.iter().zip([40, 3, 0, 17, 1, 0, 0, 2]).all(|(&n, c)| (n == 0) == (c == 0)));
        let table = Table::new(&norm, 6);
        for (code, reach) in table.from.iter().enumerate() {
            if norm[code] == 0 {
                continue;
            }
            for (to, &state) in reach.iter().enumerate() {
                let state = usize::from(state);
                assert_eq!(usize::from(table.sym[state]), code);
                let (value, width) = table.step(state, to);
                assert!(value < 1 << width);
            }
        }
    }

    /// The reference decoder, when there is one to run, reads back what went in. A frame it
    /// rejects is one ld and gdb would reject too, so this is the test that counts.
    #[test]
    fn the_reference_decoder_reads_back_what_went_in() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        if Command::new("zstd").arg("--version").output().is_err() {
            return;
        }
        let mut text = Vec::new();
        for path in ["src/zlib.rs", "src/zstd.rs", "src/file.rs", "src/section.rs"] {
            text.extend(std::fs::read(path).unwrap());
        }
        let mut inputs: Vec<Vec<u8>> =
            [0, 1, 2, 31, 32, 500, 1023, 1024, 1025, 5000, 70_000].map(sample).to_vec();
        inputs.push(vec![0; 300_000]);
        inputs.push(sample(600_000));
        inputs.push(text.repeat(3));
        inputs.push((0..=255u8).cycle().take(20_000).collect());
        for data in inputs {
            let frame = compress(&data);
            let mut zstd = Command::new("zstd")
                .args(["-d", "-c", "-q"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let mut stdin = zstd.stdin.take().unwrap();
            let writer = std::thread::spawn(move || stdin.write_all(&frame).unwrap());
            let out = zstd.wait_with_output().unwrap();
            writer.join().unwrap();
            assert!(out.status.success(), "zstd refused the frame for {} bytes", data.len());
            assert!(out.stdout == data, "zstd read back something else for {} bytes", data.len());
        }
    }
}
