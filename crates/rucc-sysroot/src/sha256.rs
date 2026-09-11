//! sha256, as published in FIPS 180-4.
//!
//! Here rather than behind `spec/18-package-layout.md` section 18.3's dependency wall. The whole
//! of the algorithm is one table of constants and two loops, the test vectors are published with
//! it, and a hash a manifest names itself by is not a reason to take on a crate and everything it
//! depends on. The other direction of that wall matters too: this runs over a manifest, so a
//! change in what it answers would change what every recorded digest means, and a constant in this
//! file cannot change underneath us the way a version resolution can.
//!
//! It hashes a whole slice, and the caller that has a file reads the file. The largest thing it is
//! asked about is a release artifact of a few tens of megabytes, on its way into the cache, which
//! fits in memory on any machine that can run a compiler. A streaming interface would be more code
//! for the same answer and no caller wants one.

/// The initial state: the first thirty two bits of the fractional parts of the square roots of the
/// first eight primes.
const INITIAL: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// One constant per round: the first thirty two bits of the fractional parts of the cube roots of
/// the first sixty four primes.
const ROUND: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// The sha256 of these bytes, as sixty four lowercase hex characters.
///
/// The same number `sha256sum` prints for a file holding them, which is the property that makes a
/// digest worth printing at all: whoever is handed one can check it with a tool they already have
/// rather than with ours.
pub fn hex(message: &[u8]) -> String {
    let mut state = INITIAL;
    let mut blocks = message.chunks_exact(64);
    for block in &mut blocks {
        compress(&mut state, block);
    }

    // The padding is a one bit, then zeros, then the length in bits as a big endian sixty four bit
    // number. That needs a second block when what is left of the message leaves no room for the
    // length, so the tail is two blocks wide and one or both of them are hashed.
    let rest = blocks.remainder();
    let mut tail = [0u8; 128];
    tail[..rest.len()].copy_from_slice(rest);
    tail[rest.len()] = 0x80;
    let width = if rest.len() + 9 > 64 { 128 } else { 64 };
    // In bits, and a message long enough to overflow this does not fit in any machine's memory.
    let bits = (message.len() as u64).wrapping_mul(8);
    tail[width - 8..width].copy_from_slice(&bits.to_be_bytes());
    for block in tail[..width].chunks_exact(64) {
        compress(&mut state, block);
    }

    let mut out = String::with_capacity(64);
    for word in state {
        for byte in word.to_be_bytes() {
            out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
            out.push(char::from_digit(u32::from(byte & 0xf), 16).unwrap_or('0'));
        }
    }
    out
}

/// One block of sixty four bytes into the state.
///
/// # Panics
///
/// A block that is not sixty four bytes, which is a caller that did not chunk its message. The
/// slice is a slice rather than an array reference because `chunks_exact` yields slices and the
/// conversion would be the same assertion one line up.
fn compress(state: &mut [u32; 8], block: &[u8]) {
    assert_eq!(block.len(), 64, "sha256 compresses sixty four bytes at a time");

    // The message schedule: sixteen words read out of the block and forty eight more derived from
    // them, which is what spreads one changed byte across the whole block.
    let mut words = [0u32; 64];
    for (word, bytes) in words.iter_mut().zip(block.chunks_exact(4)) {
        let quad: [u8; 4] = bytes.try_into().expect("four bytes out of a chunk of four");
        *word = u32::from_be_bytes(quad);
    }
    for index in 16..64 {
        let first = words[index - 15];
        let second = words[index - 2];
        let low = first.rotate_right(7) ^ first.rotate_right(18) ^ (first >> 3);
        let high = second.rotate_right(17) ^ second.rotate_right(19) ^ (second >> 10);
        words[index] =
            words[index - 16].wrapping_add(low).wrapping_add(words[index - 7]).wrapping_add(high);
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for (constant, word) in ROUND.iter().zip(words.iter()) {
        let sigma = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let choose = (e & f) ^ (!e & g);
        let one =
            h.wrapping_add(sigma).wrapping_add(choose).wrapping_add(*constant).wrapping_add(*word);
        let sum = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let majority = (a & b) ^ (a & c) ^ (b & c);
        let two = sum.wrapping_add(majority);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(one);
        d = c;
        c = b;
        b = a;
        a = one.wrapping_add(two);
    }

    for (slot, round) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *slot = slot.wrapping_add(round);
    }
}

#[cfg(test)]
mod tests {
    use super::hex;

    #[test]
    fn the_published_vectors() {
        // The three in FIPS 180-4's own examples and in every other implementation's tests. The
        // empty message is the one that exercises the padding on its own, since there is nothing
        // else in the block it pads.
        assert_eq!(hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn a_message_that_fills_its_last_block_takes_a_second_one() {
        // 55 bytes leaves exactly room for the one bit and the length, 56 does not, and the
        // boundary between those two is where an implementation that got the padding wrong stops
        // agreeing with everybody else.
        assert_eq!(
            hex(&b"a".repeat(55)),
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
        );
        assert_eq!(
            hex(&b"a".repeat(56)),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
        assert_eq!(
            hex(&b"a".repeat(64)),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
    }

    #[test]
    fn a_long_message_is_hashed_block_by_block() {
        // A million a's, which is the fourth vector everyone publishes and the only one that goes
        // through the loop enough times for a wrong count of blocks to show.
        assert_eq!(
            hex(&b"a".repeat(1_000_000)),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }
}
