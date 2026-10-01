//! DEFLATE decoding (RFC 1951): zip entries stored with method 8, in map pakfiles among others.
//!
//! Written from the RFC. Huffman codes are decoded through one table indexed by the next
//! bits of input (as many as the longest code has), built per block.

use std::io;

use crate::io_util::invalid;

const LENGTH_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LENGTH_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DISTANCE_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385,
    24577,
];
const DISTANCE_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
/// The order in which a dynamic block lists the lengths of the code length code.
const CODE_LENGTH_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

/// Bits of the input, least significant first.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buffer: u64,
    count: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Bits<'a> {
        Bits { data, pos: 0, buffer: 0, count: 0 }
    }

    /// Fills the buffer; past the end of the input it reads zeros, which [`Self::overrun`]
    /// notices.
    fn refill(&mut self) {
        while self.count <= 56 {
            let byte = self.data.get(self.pos).copied().unwrap_or(0);
            self.pos += 1;
            self.buffer |= (byte as u64) << self.count;
            self.count += 8;
        }
    }

    fn overrun(&self) -> bool {
        // bits handed out so far, against the bits there are
        (self.pos as u64 * 8).saturating_sub(self.count as u64) > self.data.len() as u64 * 8
    }

    fn take(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        if self.count < n {
            self.refill();
        }
        let value = (self.buffer & ((1u64 << n) - 1)) as u32;
        self.buffer >>= n;
        self.count -= n;
        value
    }

    fn align_to_byte(&mut self) {
        let skip = self.count % 8;
        self.take(skip);
    }
}

/// A Huffman code as a table: entry = symbol << 4 | code length (0: no such code).
struct Huffman {
    table: Vec<u16>,
    bits: u32,
}

impl Huffman {
    fn new(lengths: &[u8]) -> io::Result<Huffman> {
        let bits = lengths.iter().copied().max().unwrap_or(0) as u32;
        let mut count = [0u32; 16];
        for &length in lengths {
            count[length as usize] += 1;
        }
        count[0] = 0;
        let mut next = [0u32; 16];
        let mut code = 0u32;
        for length in 1..16 {
            code = (code + count[length - 1]) << 1;
            next[length] = code;
            if next[length] + count[length] > 1 << length {
                return Err(invalid("deflate: over-subscribed Huffman code"));
            }
        }
        let mut table = vec![0u16; 1 << bits];
        for (symbol, &length) in lengths.iter().enumerate() {
            if length == 0 {
                continue;
            }
            let length = length as u32;
            let code = next[length as usize];
            next[length as usize] += 1;
            // codes are sent most significant bit first, so the table is indexed by them reversed
            let reversed = code.reverse_bits() >> (32 - length);
            let mut index = reversed as usize;
            while index < table.len() {
                table[index] = (symbol as u16) << 4 | length as u16;
                index += 1 << length;
            }
        }
        Ok(Huffman { table, bits })
    }

    fn decode(&self, input: &mut Bits) -> io::Result<u16> {
        if input.count < self.bits {
            input.refill();
        }
        let entry = self.table.get((input.buffer & ((1u64 << self.bits) - 1)) as usize).copied().unwrap_or(0);
        let length = (entry & 15) as u32;
        if length == 0 {
            return Err(invalid("deflate: invalid Huffman code"));
        }
        input.buffer >>= length;
        input.count -= length;
        Ok(entry >> 4)
    }
}

fn fixed_codes() -> io::Result<(Huffman, Huffman)> {
    let mut literal = [0u8; 288];
    literal[..144].fill(8);
    literal[144..256].fill(9);
    literal[256..280].fill(7);
    literal[280..].fill(8);
    Ok((Huffman::new(&literal)?, Huffman::new(&[5u8; 30])?))
}

fn dynamic_codes(input: &mut Bits) -> io::Result<(Huffman, Huffman)> {
    let literals = input.take(5) as usize + 257;
    let distances = input.take(5) as usize + 1;
    let code_lengths = input.take(4) as usize + 4;
    let mut lengths = [0u8; 19];
    for &position in &CODE_LENGTH_ORDER[..code_lengths] {
        lengths[position] = input.take(3) as u8;
    }
    let code_length_code = Huffman::new(&lengths)?;
    let mut all = vec![0u8; literals + distances];
    let mut i = 0;
    while i < all.len() {
        let symbol = code_length_code.decode(input)?;
        let (value, repeat) = match symbol {
            0..=15 => (symbol as u8, 1),
            16 => {
                let previous = *all[..i].last().ok_or_else(|| invalid("deflate: a repeat before any length"))?;
                (previous, 3 + input.take(2) as usize)
            }
            17 => (0, 3 + input.take(3) as usize),
            _ => (0, 11 + input.take(7) as usize),
        };
        if i + repeat > all.len() {
            return Err(invalid("deflate: code lengths run past their count"));
        }
        all[i..i + repeat].fill(value);
        i += repeat;
    }
    if all[256] == 0 {
        return Err(invalid("deflate: no end-of-block code"));
    }
    Ok((Huffman::new(&all[..literals])?, Huffman::new(&all[literals..])?))
}

/// Decompresses a raw DEFLATE stream into at most `max_output` bytes. `size_hint` (the size
/// the container states, if any) only sizes the first allocation.
pub fn inflate(input: &[u8], size_hint: usize, max_output: u64) -> io::Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::with_capacity(size_hint.min(1 << 28));
    let mut bits = Bits::new(input);
    let mut fixed: Option<(Huffman, Huffman)> = None;
    loop {
        let last = bits.take(1) == 1;
        match bits.take(2) {
            0 => {
                bits.align_to_byte();
                let length = bits.take(16) as usize;
                let complement = bits.take(16) as usize;
                if length != !complement & 0xFFFF {
                    return Err(invalid("deflate: stored block length does not match its complement"));
                }
                for _ in 0..length {
                    out.push(bits.take(8) as u8);
                }
            }
            kind @ (1 | 2) => {
                let dynamic;
                let (literal, distance) = if kind == 1 {
                    if fixed.is_none() {
                        fixed = Some(fixed_codes()?);
                    }
                    let (l, d) = fixed.as_ref().unwrap();
                    (l, d)
                } else {
                    dynamic = dynamic_codes(&mut bits)?;
                    (&dynamic.0, &dynamic.1)
                };
                loop {
                    let symbol = literal.decode(&mut bits)? as usize;
                    if symbol < 256 {
                        out.push(symbol as u8);
                        continue;
                    }
                    if symbol == 256 {
                        break;
                    }
                    let index = symbol - 257;
                    if index >= LENGTH_BASE.len() {
                        return Err(invalid("deflate: invalid length symbol"));
                    }
                    let length = LENGTH_BASE[index] as usize + bits.take(LENGTH_EXTRA[index] as u32) as usize;
                    let code = distance.decode(&mut bits)? as usize;
                    if code >= DISTANCE_BASE.len() {
                        return Err(invalid("deflate: invalid distance symbol"));
                    }
                    let back = DISTANCE_BASE[code] as usize + bits.take(DISTANCE_EXTRA[code] as u32) as usize;
                    if back > out.len() {
                        return Err(invalid("deflate: a distance reaches before the start"));
                    }
                    let start = out.len() - back;
                    if back >= length {
                        out.extend_from_within(start..start + length);
                    } else {
                        // the copy overlaps what it produces: a run
                        for i in 0..length {
                            let byte = out[start + i];
                            out.push(byte);
                        }
                    }
                    if out.len() as u64 > max_output {
                        return Err(invalid(format!("deflate: unpacks to more than the limit of {max_output} bytes")));
                    }
                }
            }
            _ => return Err(invalid("deflate: invalid block type")),
        }
        if out.len() as u64 > max_output {
            return Err(invalid(format!("deflate: unpacks to more than the limit of {max_output} bytes")));
        }
        if bits.overrun() {
            return Err(invalid("deflate: the stream is cut short"));
        }
        if last {
            return Ok(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crc32::crc32;

    fn unhex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// Streams made by Python's zlib (raw deflate, `wbits=-15`) from the same 5700-byte sample
    /// as the LZMA tests: level 0 gives stored blocks, level 1 and 9 dynamic Huffman blocks.
    #[test]
    fn python_zlib_streams() {
        for (name, stream) in [
            ("stored", include_str!("../testdata/deflate/stored.hex")),
            ("fast", include_str!("../testdata/deflate/fast.hex")),
            ("best", include_str!("../testdata/deflate/best.hex")),
        ] {
            let out = inflate(&unhex(stream), 0, 1 << 20).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!((out.len(), crc32(&out)), (5700, 0x7c11_f9aa), "{name}");
        }
        // short input: zlib uses the fixed code
        let hello = unhex(include_str!("../testdata/deflate/fixed.hex"));
        assert_eq!(inflate(&hello, 0, 1 << 20).unwrap(), b"hello hello hello hello");
    }

    #[test]
    fn damaged_streams_are_errors() {
        let stream = unhex(include_str!("../testdata/deflate/best.hex"));
        assert!(inflate(&stream[..stream.len() / 2], 0, 1 << 20).is_err());
        assert!(inflate(&stream, 0, 100).is_err());
        assert!(inflate(&[0x07], 0, 1 << 20).is_err(), "block type 3");
        for i in (0..stream.len()).step_by(29) {
            let mut flipped = stream.clone();
            flipped[i] ^= 0x5A;
            let _ = inflate(&flipped, 0, 1 << 20);
        }
    }
}
