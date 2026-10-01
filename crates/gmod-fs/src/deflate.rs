//! DEFLATE encoding (RFC 1951) for the files openGMOD writes itself, such as PNG previews.
//!
//! One block with the fixed Huffman codes; repeats are found through hash chains over the last
//! 32 KiB, taking the longest match of up to 64 candidates. Simple rather than small: dynamic
//! codes would save another tenth or so.

use crate::inflate::{DISTANCE_BASE, DISTANCE_EXTRA, LENGTH_BASE, LENGTH_EXTRA};

const WINDOW: usize = 32 * 1024;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const HASH_BITS: u32 = 15;
const MAX_CHAIN: usize = 64;
const NONE: usize = usize::MAX;

/// Bits of the output, least significant first.
struct Bits {
    out: Vec<u8>,
    buffer: u64,
    count: u32,
}

impl Bits {
    fn put(&mut self, value: u32, bits: u32) {
        self.buffer |= u64::from(value) << self.count;
        self.count += bits;
        while self.count >= 8 {
            self.out.push(self.buffer as u8);
            self.buffer >>= 8;
            self.count -= 8;
        }
    }

    /// A Huffman code goes out most significant bit first.
    fn code(&mut self, code: u32, bits: u32) {
        self.put(code.reverse_bits() >> (32 - bits), bits);
    }

    fn literal(&mut self, symbol: u32) {
        match symbol {
            0..=143 => self.code(0x30 + symbol, 8),
            144..=255 => self.code(0x190 + symbol - 144, 9),
            256..=279 => self.code(symbol - 256, 7),
            _ => self.code(0xC0 + symbol - 280, 8),
        }
    }

    fn copy(&mut self, length: usize, distance: usize) {
        let i = LENGTH_BASE.iter().rposition(|&base| usize::from(base) <= length).unwrap();
        self.literal(257 + i as u32);
        self.put((length - usize::from(LENGTH_BASE[i])) as u32, u32::from(LENGTH_EXTRA[i]));
        let d = DISTANCE_BASE.iter().rposition(|&base| usize::from(base) <= distance).unwrap();
        self.code(d as u32, 5);
        self.put((distance - usize::from(DISTANCE_BASE[d])) as u32, u32::from(DISTANCE_EXTRA[d]));
    }
}

fn hash(data: &[u8], at: usize) -> usize {
    let key = u32::from(data[at]) << 16 | u32::from(data[at + 1]) << 8 | u32::from(data[at + 2]);
    (key.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
}

/// Earlier positions by the hash of the three bytes there: the newest for each hash, and for
/// each position the one before it with the same hash. A slot of `previous` is reused only
/// once its position has left the window, so a chain followed within the window only goes back.
struct Chains {
    head: Vec<usize>,
    previous: Vec<usize>,
}

impl Chains {
    fn insert(&mut self, data: &[u8], at: usize) {
        if at + MIN_MATCH <= data.len() {
            let h = hash(data, at);
            self.previous[at % WINDOW] = self.head[h];
            self.head[h] = at;
        }
    }

    /// The longest earlier match for the bytes at `at`: (length, distance).
    fn longest_match(&self, data: &[u8], at: usize) -> (usize, usize) {
        let mut best = (0, 0);
        if at + MIN_MATCH > data.len() {
            return best;
        }
        let longest = (data.len() - at).min(MAX_MATCH);
        let mut candidate = self.head[hash(data, at)];
        let mut tries = 0;
        while candidate != NONE && at - candidate <= WINDOW && tries < MAX_CHAIN {
            let length = data[candidate..].iter().zip(&data[at..at + longest]).take_while(|(a, b)| a == b).count();
            if length > best.0 {
                best = (length, at - candidate);
                if length == longest {
                    break;
                }
            }
            candidate = self.previous[candidate % WINDOW];
            tries += 1;
        }
        best
    }
}

/// Compresses `data` into a raw DEFLATE stream.
pub fn deflate(data: &[u8]) -> Vec<u8> {
    let mut bits = Bits { out: Vec::with_capacity(data.len() / 2 + 16), buffer: 0, count: 0 };
    bits.put(1, 1); // the last block
    bits.put(1, 2); // fixed codes
    let mut chains = Chains { head: vec![NONE; 1 << HASH_BITS], previous: vec![NONE; WINDOW] };
    let mut at = 0;
    while at < data.len() {
        let (length, distance) = chains.longest_match(data, at);
        if length >= MIN_MATCH {
            bits.copy(length, distance);
            for p in at..at + length {
                chains.insert(data, p);
            }
            at += length;
        } else {
            bits.literal(u32::from(data[at]));
            chains.insert(data, at);
            at += 1;
        }
    }
    bits.literal(256);
    if bits.count > 0 {
        bits.out.push(bits.buffer as u8);
    }
    bits.out
}

/// The Adler-32 checksum zlib streams end with.
pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    b << 16 | a
}

/// Compresses `data` into a zlib stream (RFC 1950): what PNG files hold.
pub fn zlib(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    out.extend(deflate(data));
    out.extend(adler32(data).to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inflate::inflate;

    fn round_trip(data: &[u8]) -> usize {
        let packed = deflate(data);
        assert_eq!(inflate(&packed, data.len(), data.len() as u64).unwrap(), data);
        packed.len()
    }

    #[test]
    fn inflates_back() {
        round_trip(b"");
        round_trip(b"a");
        round_trip(b"abcabcabcabcabcabcabcabc");
        // runs longer than the longest match, and matches overlapping what they copy
        assert!(round_trip(&[7u8; 100_000]) < 1000);
        // noise does not compress, but must survive
        let mut x = 1u32;
        let noise: Vec<u8> = (0..70_000)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (x >> 24) as u8
            })
            .collect();
        round_trip(&noise);
        // repeats further back than the window must not be used
        let mut far = noise[..40_000].to_vec();
        far.extend_from_slice(&noise[..40_000]);
        round_trip(&far);
        // text-like data shrinks
        let text = "The quick brown fox jumps over the lazy dog. ".repeat(500);
        assert!(round_trip(text.as_bytes()) < text.len() / 20);
    }

    #[test]
    fn zlib_stream() {
        let packed = zlib(b"hello hello hello");
        assert_eq!(&packed[..2], [0x78, 0x01]);
        assert_eq!((0x78u32 * 256 + 0x01) % 31, 0);
        assert_eq!(u32::from_be_bytes(packed[packed.len() - 4..].try_into().unwrap()), adler32(b"hello hello hello"));
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(inflate(&packed[2..packed.len() - 4], 0, 100).unwrap(), b"hello hello hello");
    }
}
