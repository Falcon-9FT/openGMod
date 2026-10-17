//! LZMA decoding: old workshop downloads are LZMA streams around a GMA file (`.bin`), and
//! map lumps may be LZMA-compressed too. And encoding, of the "alone" container, as GMod's
//! `util.Compress` makes it: [`compress_alone`].
//!
//! Written from the LZMA specification that comes with the LZMA SDK (public domain). The
//! output is kept in memory as a whole and serves as the dictionary, so no window is needed.
//!
//! The "alone" container (`.lzma`) is 13 bytes of header before the stream: the properties
//! byte (`(pb * 5 + lp) * 9 + lc`), `u32` dictionary size and `u64` uncompressed size, little
//! endian, all ones when unknown (then the stream ends with an end marker). Map lumps and zip
//! entries (method 14) store the 5 property bytes and the sizes their own way and hand the bare
//! stream to [`decompress_raw`].

use std::io;

use crate::io_util::invalid;

const BIT_MODEL_TOTAL_BITS: u32 = 11;
const BIT_MODEL_TOTAL: u16 = 1 << BIT_MODEL_TOTAL_BITS;
const MOVE_BITS: u32 = 5;
const PROB_INIT: u16 = BIT_MODEL_TOTAL / 2;
const TOP_VALUE: u32 = 1 << 24;

const STATES: usize = 12;
const POS_STATES_MAX: usize = 1 << 4;
const LEN_TO_POS_STATES: usize = 4;
const END_POS_MODEL_INDEX: u32 = 14;
const FULL_DISTANCES: usize = 1 << (END_POS_MODEL_INDEX >> 1);
const ALIGN_BITS: u32 = 4;
const MATCH_MIN_LEN: usize = 2;

/// Decompresses an "alone" stream (`.lzma`, and the old workshop `.bin` files), producing at
/// most `max_output` bytes.
pub fn decompress_alone(input: &[u8], max_output: u64) -> io::Result<Vec<u8>> {
    if input.len() < 13 {
        return Err(invalid("too short for an LZMA stream"));
    }
    let size = u64::from_le_bytes(input[5..13].try_into().unwrap());
    let size = (size != u64::MAX).then_some(size);
    decompress_raw(&input[..5], &input[13..], size, max_output)
}

/// Decompresses a bare LZMA stream: `properties` are the 5 bytes of the properties byte and
/// the dictionary size, `size` the unpacked size if known (else the stream ends with an end
/// marker).
pub fn decompress_raw(properties: &[u8], stream: &[u8], size: Option<u64>, max_output: u64) -> io::Result<Vec<u8>> {
    let Some(&byte) = properties.first() else {
        return Err(invalid("LZMA properties missing"));
    };
    if byte >= 9 * 5 * 5 {
        return Err(invalid(format!("LZMA properties byte {byte:#04x} is out of range")));
    }
    let lc = (byte % 9) as u32;
    let lp = ((byte / 9) % 5) as u32;
    let pb = (byte / 45) as u32;
    if let Some(size) = size
        && size > max_output
    {
        return Err(invalid(format!("LZMA stream unpacks to {size} bytes, more than the limit of {max_output}")));
    }
    decode(stream, lc, lp, pb, size, max_output)
}

struct RangeDecoder<'a> {
    input: &'a [u8],
    pos: usize,
    range: u32,
    code: u32,
}

impl<'a> RangeDecoder<'a> {
    fn new(input: &'a [u8]) -> io::Result<RangeDecoder<'a>> {
        if input.len() < 5 || input[0] != 0 {
            return Err(invalid("the LZMA stream does not start like one"));
        }
        let code = u32::from_be_bytes(input[1..5].try_into().unwrap());
        if code == u32::MAX {
            return Err(invalid("the LZMA stream is corrupt"));
        }
        Ok(RangeDecoder { input, pos: 5, range: u32::MAX, code })
    }

    /// Past the end of the input the decoder reads zeros; [`Self::overrun`] reports it.
    fn next_byte(&mut self) -> u8 {
        let byte = self.input.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        byte
    }

    fn overrun(&self) -> bool {
        self.pos > self.input.len()
    }

    fn normalize(&mut self) {
        if self.range < TOP_VALUE {
            self.range <<= 8;
            self.code = (self.code << 8) | self.next_byte() as u32;
        }
    }

    fn bit(&mut self, prob: &mut u16) -> usize {
        let bound = (self.range >> BIT_MODEL_TOTAL_BITS) * *prob as u32;
        let bit = if self.code < bound {
            self.range = bound;
            *prob += (BIT_MODEL_TOTAL - *prob) >> MOVE_BITS;
            0
        } else {
            self.range -= bound;
            self.code -= bound;
            *prob -= *prob >> MOVE_BITS;
            1
        };
        self.normalize();
        bit
    }

    fn direct_bits(&mut self, count: u32) -> u32 {
        let mut result = 0u32;
        for _ in 0..count {
            self.range >>= 1;
            self.code = self.code.wrapping_sub(self.range);
            let mask = 0u32.wrapping_sub(self.code >> 31);
            self.code = self.code.wrapping_add(self.range & mask);
            self.normalize();
            result = (result << 1).wrapping_add(mask.wrapping_add(1));
        }
        result
    }

    /// `bits` bits, most significant first, through the probability tree `probs`.
    fn tree(&mut self, probs: &mut [u16], bits: u32) -> u32 {
        let mut m = 1usize;
        for _ in 0..bits {
            m = (m << 1) + self.bit(&mut probs[m]);
        }
        (m - (1 << bits)) as u32
    }

    /// `bits` bits, least significant first.
    fn tree_reverse(&mut self, probs: &mut [u16], bits: u32) -> u32 {
        let mut m = 1usize;
        let mut symbol = 0u32;
        for i in 0..bits {
            let bit = self.bit(&mut probs[m]);
            m = (m << 1) + bit;
            symbol |= (bit as u32) << i;
        }
        symbol
    }
}

struct LenDecoder {
    choice: u16,
    choice2: u16,
    low: [[u16; 8]; POS_STATES_MAX],
    mid: [[u16; 8]; POS_STATES_MAX],
    high: [u16; 256],
}

impl LenDecoder {
    fn new() -> LenDecoder {
        LenDecoder {
            choice: PROB_INIT,
            choice2: PROB_INIT,
            low: [[PROB_INIT; 8]; POS_STATES_MAX],
            mid: [[PROB_INIT; 8]; POS_STATES_MAX],
            high: [PROB_INIT; 256],
        }
    }

    /// A match length, less [`MATCH_MIN_LEN`].
    fn decode(&mut self, rc: &mut RangeDecoder, pos_state: usize) -> usize {
        if rc.bit(&mut self.choice) == 0 {
            return rc.tree(&mut self.low[pos_state], 3) as usize;
        }
        if rc.bit(&mut self.choice2) == 0 {
            return 8 + rc.tree(&mut self.mid[pos_state], 3) as usize;
        }
        16 + rc.tree(&mut self.high, 8) as usize
    }
}

/// The byte `distance + 1` back from the end of what was decoded so far.
fn back(out: &[u8], distance: u32) -> io::Result<u8> {
    let distance = distance as usize;
    if distance < out.len() { Ok(out[out.len() - 1 - distance]) } else { Err(invalid("the LZMA stream refers to data before its start")) }
}

fn decode(input: &[u8], lc: u32, lp: u32, pb: u32, size: Option<u64>, max_output: u64) -> io::Result<Vec<u8>> {
    let mut rc = RangeDecoder::new(input)?;
    // what the header claims is not trusted for the first allocation; the vector grows as needed
    let capacity = size.unwrap_or(0).min(input.len() as u64 * 16).min(max_output).min(1 << 28);
    let mut out: Vec<u8> = Vec::with_capacity(capacity as usize);

    let mut literal = vec![PROB_INIT; 0x300 << (lc + lp)];
    let mut is_match = [PROB_INIT; STATES * POS_STATES_MAX];
    let mut is_rep = [PROB_INIT; STATES];
    let mut is_rep_g0 = [PROB_INIT; STATES];
    let mut is_rep_g1 = [PROB_INIT; STATES];
    let mut is_rep_g2 = [PROB_INIT; STATES];
    let mut is_rep0_long = [PROB_INIT; STATES * POS_STATES_MAX];
    let mut pos_slot = [[PROB_INIT; 64]; LEN_TO_POS_STATES];
    let mut pos_decoders = [PROB_INIT; 1 + FULL_DISTANCES - END_POS_MODEL_INDEX as usize];
    let mut align = [PROB_INIT; 1 << ALIGN_BITS];
    let mut len_decoder = LenDecoder::new();
    let mut rep_len_decoder = LenDecoder::new();

    let pb_mask = (1usize << pb) - 1;
    let lp_mask = (1usize << lp) - 1;
    let mut state = 0usize;
    let mut reps = [0u32; 4];

    loop {
        if size.is_some_and(|s| out.len() as u64 == s) {
            break;
        }
        if out.len() as u64 >= max_output {
            return Err(invalid(format!("the LZMA stream unpacks to more than the limit of {max_output} bytes")));
        }
        if rc.overrun() {
            return Err(invalid("the LZMA stream is cut short"));
        }
        let pos_state = out.len() & pb_mask;

        if rc.bit(&mut is_match[(state << 4) + pos_state]) == 0 {
            let prev = out.last().copied().unwrap_or(0) as usize;
            let lit_state = ((out.len() & lp_mask) << lc) + (prev >> (8 - lc));
            let probs = &mut literal[lit_state * 0x300..(lit_state + 1) * 0x300];
            let mut symbol = 1usize;
            if state >= 7 {
                let mut match_byte = back(&out, reps[0])? as usize;
                while symbol < 0x100 {
                    let match_bit = (match_byte >> 7) & 1;
                    match_byte <<= 1;
                    let bit = rc.bit(&mut probs[((1 + match_bit) << 8) + symbol]);
                    symbol = (symbol << 1) | bit;
                    if match_bit != bit {
                        break;
                    }
                }
            }
            while symbol < 0x100 {
                symbol = (symbol << 1) | rc.bit(&mut probs[symbol]);
            }
            out.push((symbol - 0x100) as u8);
            state = match state {
                0..=3 => 0,
                4..=9 => state - 3,
                _ => state - 6,
            };
            continue;
        }

        let len;
        if rc.bit(&mut is_rep[state]) != 0 {
            if out.is_empty() {
                return Err(invalid("the LZMA stream repeats data before there is any"));
            }
            if rc.bit(&mut is_rep_g0[state]) == 0 {
                if rc.bit(&mut is_rep0_long[(state << 4) + pos_state]) == 0 {
                    // a single byte from the last distance
                    state = if state < 7 { 9 } else { 11 };
                    let byte = back(&out, reps[0])?;
                    out.push(byte);
                    continue;
                }
            } else {
                let distance;
                if rc.bit(&mut is_rep_g1[state]) == 0 {
                    distance = reps[1];
                } else {
                    if rc.bit(&mut is_rep_g2[state]) == 0 {
                        distance = reps[2];
                    } else {
                        distance = reps[3];
                        reps[3] = reps[2];
                    }
                    reps[2] = reps[1];
                }
                reps[1] = reps[0];
                reps[0] = distance;
            }
            len = rep_len_decoder.decode(&mut rc, pos_state);
            state = if state < 7 { 8 } else { 11 };
        } else {
            reps[3] = reps[2];
            reps[2] = reps[1];
            reps[1] = reps[0];
            len = len_decoder.decode(&mut rc, pos_state);
            state = if state < 7 { 7 } else { 10 };

            // the distance
            let slot = rc.tree(&mut pos_slot[len.min(LEN_TO_POS_STATES - 1)], 6);
            let distance = if slot < 4 {
                slot
            } else {
                let direct = (slot >> 1) - 1;
                let base = (2 | (slot & 1)) << direct;
                if slot < END_POS_MODEL_INDEX {
                    base + rc.tree_reverse(&mut pos_decoders[(base - slot) as usize..], direct)
                } else {
                    let high = rc.direct_bits(direct - ALIGN_BITS) << ALIGN_BITS;
                    base.wrapping_add(high).wrapping_add(rc.tree_reverse(&mut align, ALIGN_BITS))
                }
            };
            if distance == u32::MAX {
                // the end marker
                if size.is_some_and(|s| (out.len() as u64) < s) {
                    return Err(invalid("the LZMA stream ends before its stated size"));
                }
                break;
            }
            if distance as usize >= out.len() {
                return Err(invalid("the LZMA stream refers to data before its start"));
            }
            reps[0] = distance;
        }

        let len = len + MATCH_MIN_LEN;
        if size.is_some_and(|s| out.len() as u64 + len as u64 > s) {
            return Err(invalid("the LZMA stream runs past its stated size"));
        }
        let start = out.len() - 1 - reps[0] as usize;
        // the copy may overlap what it produces (a run), so byte by byte
        for i in 0..len {
            let byte = out[start + i];
            out.push(byte);
        }
    }
    if rc.overrun() {
        return Err(invalid("the LZMA stream is cut short"));
    }
    Ok(out)
}

/// The properties byte of what [`compress_alone`] makes: `lc` 3, `lp` 0, `pb` 2.
const PROPERTIES: u8 = (2 * 5) * 9 + 3;
/// How far back a match may be: the dictionary the header names.
const WINDOW: usize = 1 << 20;
/// The longest match LZMA has.
const MATCH_MAX_LEN: usize = 273;
/// How many earlier places of the same three bytes are tried for a match.
const CHAIN: usize = 48;

/// Compresses data into an "alone" stream (`.lzma`), its size in the header, without an end
/// marker - what GMod's `util.Compress` gives and `util.Decompress` takes. Literals and
/// matches found greedily along chains of three-byte hashes, the last distance tried first.
pub fn compress_alone(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(13 + data.len() / 2);
    out.push(PROPERTIES);
    out.extend_from_slice(&(WINDOW as u32).to_le_bytes());
    out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    let mut encoder = Encoder::new(out);
    encoder.encode(data);
    encoder.rc.finish()
}

struct RangeEncoder {
    out: Vec<u8>,
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
}

impl RangeEncoder {
    fn shift_low(&mut self) {
        if (self.low as u32) < 0xFF00_0000 || (self.low >> 32) != 0 {
            let carry = (self.low >> 32) as u8;
            let mut byte = self.cache;
            loop {
                self.out.push(byte.wrapping_add(carry));
                byte = 0xFF;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = (self.low >> 24) as u8;
        }
        self.cache_size += 1;
        self.low = (self.low & 0x00FF_FFFF) << 8;
    }

    fn bit(&mut self, prob: &mut u16, bit: usize) {
        let bound = (self.range >> BIT_MODEL_TOTAL_BITS) * *prob as u32;
        if bit == 0 {
            self.range = bound;
            *prob += (BIT_MODEL_TOTAL - *prob) >> MOVE_BITS;
        } else {
            self.low += u64::from(bound);
            self.range -= bound;
            *prob -= *prob >> MOVE_BITS;
        }
        while self.range < TOP_VALUE {
            self.range <<= 8;
            self.shift_low();
        }
    }

    fn direct_bits(&mut self, value: u32, count: u32) {
        for i in (0..count).rev() {
            self.range >>= 1;
            if (value >> i) & 1 != 0 {
                self.low += u64::from(self.range);
            }
            while self.range < TOP_VALUE {
                self.range <<= 8;
                self.shift_low();
            }
        }
    }

    fn tree(&mut self, probs: &mut [u16], bits: u32, symbol: u32) {
        let mut m = 1usize;
        for i in (0..bits).rev() {
            let bit = ((symbol >> i) & 1) as usize;
            self.bit(&mut probs[m], bit);
            m = (m << 1) | bit;
        }
    }

    fn tree_reverse(&mut self, probs: &mut [u16], bits: u32, symbol: u32) {
        let mut m = 1usize;
        for i in 0..bits {
            let bit = ((symbol >> i) & 1) as usize;
            self.bit(&mut probs[m], bit);
            m = (m << 1) | bit;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 {
            self.shift_low();
        }
        self.out
    }
}

struct LenEncoder {
    probs: LenDecoder,
}

impl LenEncoder {
    /// A match length, less [`MATCH_MIN_LEN`].
    fn encode(&mut self, rc: &mut RangeEncoder, len: usize, pos_state: usize) {
        let p = &mut self.probs;
        if len < 8 {
            rc.bit(&mut p.choice, 0);
            rc.tree(&mut p.low[pos_state], 3, len as u32);
        } else if len < 16 {
            rc.bit(&mut p.choice, 1);
            rc.bit(&mut p.choice2, 0);
            rc.tree(&mut p.mid[pos_state], 3, (len - 8) as u32);
        } else {
            rc.bit(&mut p.choice, 1);
            rc.bit(&mut p.choice2, 1);
            rc.tree(&mut p.high, 8, (len - 16) as u32);
        }
    }
}

/// The models of the decoder, kept by the encoder as the decoder will.
struct Encoder {
    rc: RangeEncoder,
    literal: Vec<u16>,
    is_match: [u16; STATES * POS_STATES_MAX],
    is_rep: [u16; STATES],
    is_rep_g0: [u16; STATES],
    is_rep0_long: [u16; STATES * POS_STATES_MAX],
    pos_slot: [[u16; 64]; LEN_TO_POS_STATES],
    pos_encoders: [u16; 1 + FULL_DISTANCES - END_POS_MODEL_INDEX as usize],
    align: [u16; 1 << ALIGN_BITS],
    len: LenEncoder,
    rep_len: LenEncoder,
    state: usize,
    reps: [u32; 4],
}

const LC: u32 = 3;
const PB_MASK: usize = 3;

impl Encoder {
    fn new(out: Vec<u8>) -> Encoder {
        Encoder {
            rc: RangeEncoder { out, low: 0, range: u32::MAX, cache: 0, cache_size: 1 },
            literal: vec![PROB_INIT; 0x300 << LC],
            is_match: [PROB_INIT; STATES * POS_STATES_MAX],
            is_rep: [PROB_INIT; STATES],
            is_rep_g0: [PROB_INIT; STATES],
            is_rep0_long: [PROB_INIT; STATES * POS_STATES_MAX],
            pos_slot: [[PROB_INIT; 64]; LEN_TO_POS_STATES],
            pos_encoders: [PROB_INIT; 1 + FULL_DISTANCES - END_POS_MODEL_INDEX as usize],
            align: [PROB_INIT; 1 << ALIGN_BITS],
            len: LenEncoder { probs: LenDecoder::new() },
            rep_len: LenEncoder { probs: LenDecoder::new() },
            state: 0,
            reps: [0; 4],
        }
    }

    fn encode(&mut self, data: &[u8]) {
        const HASH_BITS: u32 = 16;
        let hash = |i: usize| -> usize {
            let v = u32::from(data[i]) | u32::from(data[i + 1]) << 8 | u32::from(data[i + 2]) << 16;
            (v.wrapping_mul(2_654_435_761) >> (32 - HASH_BITS)) as usize
        };
        let mut head = vec![usize::MAX; 1 << HASH_BITS];
        let mut prev = vec![usize::MAX; data.len()];
        let insert = |i: usize, head: &mut [usize], prev: &mut [usize]| {
            if i + 2 < data.len() {
                let h = hash(i);
                prev[i] = head[h];
                head[h] = i;
            }
        };
        let same = |a: usize, b: usize, most: usize| (0..most).take_while(|&k| data[a + k] == data[b + k]).count();
        let mut pos = 0;
        while pos < data.len() {
            let most = (data.len() - pos).min(MATCH_MAX_LEN);
            // the last distance again, then the earlier places of these three bytes
            let rep_at = pos.checked_sub(self.reps[0] as usize + 1);
            let rep_len = rep_at.map_or(0, |at| same(at, pos, most));
            let (mut best_len, mut best_at) = (0, 0);
            if most >= 3 {
                let mut candidate = head[hash(pos)];
                for _ in 0..CHAIN {
                    if candidate == usize::MAX || pos - candidate > WINDOW {
                        break;
                    }
                    let len = same(candidate, pos, most);
                    if len > best_len {
                        (best_len, best_at) = (len, candidate);
                        if len == most {
                            break;
                        }
                    }
                    candidate = prev[candidate];
                }
            }
            let taken = if rep_len >= 2 && rep_len + 1 >= best_len {
                self.rep0(pos, rep_len);
                rep_len
            } else if best_len >= 3 {
                self.matched(pos, best_len, (pos - best_at - 1) as u32);
                best_len
            } else {
                self.literal(data, pos);
                1
            };
            for i in pos..pos + taken {
                insert(i, &mut head, &mut prev);
            }
            pos += taken;
        }
    }

    fn literal(&mut self, data: &[u8], pos: usize) {
        let pos_state = pos & PB_MASK;
        self.rc.bit(&mut self.is_match[(self.state << 4) + pos_state], 0);
        let previous = if pos > 0 { data[pos - 1] as usize } else { 0 };
        let lit_state = previous >> (8 - LC);
        let probs = &mut self.literal[lit_state * 0x300..(lit_state + 1) * 0x300];
        let byte = data[pos] as usize;
        let mut symbol = 1usize;
        // after a match, the byte at the last distance guides the first bits
        let mut matching = self.state >= 7;
        let match_byte = if matching { data[pos - self.reps[0] as usize - 1] as usize } else { 0 };
        for i in (0..8).rev() {
            let bit = (byte >> i) & 1;
            if matching {
                let match_bit = (match_byte >> i) & 1;
                self.rc.bit(&mut probs[((1 + match_bit) << 8) + symbol], bit);
                matching = match_bit == bit;
            } else {
                self.rc.bit(&mut probs[symbol], bit);
            }
            symbol = (symbol << 1) | bit;
        }
        self.state = match self.state {
            0..=3 => 0,
            4..=9 => self.state - 3,
            _ => self.state - 6,
        };
    }

    /// A match at the last distance (`rep0`), `len` long.
    fn rep0(&mut self, pos: usize, len: usize) {
        let pos_state = pos & PB_MASK;
        self.rc.bit(&mut self.is_match[(self.state << 4) + pos_state], 1);
        self.rc.bit(&mut self.is_rep[self.state], 1);
        self.rc.bit(&mut self.is_rep_g0[self.state], 0);
        self.rc.bit(&mut self.is_rep0_long[(self.state << 4) + pos_state], 1);
        self.rep_len.encode(&mut self.rc, len - MATCH_MIN_LEN, pos_state);
        self.state = if self.state < 7 { 8 } else { 11 };
    }

    /// A match `distance + 1` back, `len` long.
    fn matched(&mut self, pos: usize, len: usize, distance: u32) {
        let pos_state = pos & PB_MASK;
        self.rc.bit(&mut self.is_match[(self.state << 4) + pos_state], 1);
        self.rc.bit(&mut self.is_rep[self.state], 0);
        let len = len - MATCH_MIN_LEN;
        self.len.encode(&mut self.rc, len, pos_state);
        self.state = if self.state < 7 { 7 } else { 10 };
        let slot = if distance < 4 {
            distance
        } else {
            let n = 31 - distance.leading_zeros();
            (n << 1) | ((distance >> (n - 1)) & 1)
        };
        self.rc.tree(&mut self.pos_slot[len.min(LEN_TO_POS_STATES - 1)], 6, slot);
        if slot >= 4 {
            let direct = (slot >> 1) - 1;
            let base = (2 | (slot & 1)) << direct;
            let reduced = distance - base;
            if slot < END_POS_MODEL_INDEX {
                self.rc.tree_reverse(&mut self.pos_encoders[(base - slot) as usize..], direct, reduced);
            } else {
                self.rc.direct_bits(reduced >> ALIGN_BITS, direct - ALIGN_BITS);
                self.rc.tree_reverse(&mut self.align, ALIGN_BITS, reduced & ((1 << ALIGN_BITS) - 1));
            }
        }
        self.reps = [distance, self.reps[0], self.reps[1], self.reps[2]];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crc32::crc32;

    /// The text the samples were made from: repeats at short and long distances and some
    /// noise (the same generator as the Python script that compressed it).
    fn sample() -> Vec<u8> {
        let words: [&[u8]; 9] =
            [b"garrys", b"mod ", b"physgun ", b"toolgun ", b"prop_physics ", b"weld ", b"rope ", b"\x00\xff", b"lua/autorun/"];
        let mut out: Vec<u8> = Vec::new();
        let mut x: u64 = 12345;
        for _ in 0..700 {
            x = (x * 1_103_515_245 + 12345) & 0x7fff_ffff;
            if x.is_multiple_of(7) {
                out.extend([3, 11, 19].map(|s| (x >> s) as u8));
            } else if x.is_multiple_of(5) && out.len() > 300 {
                let back = (x >> 4) as usize % (out.len() - 1) + 1;
                let start = out.len() - back;
                let take = 9 + (x % 20) as usize;
                let end = (start + take).min(out.len());
                out.extend_from_within(start..end);
            } else {
                out.extend_from_slice(words[(x % 9) as usize]);
            }
        }
        out
    }

    fn unhex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn what_is_compressed_comes_back() {
        let mut long = Vec::new();
        for i in 0..200_000u32 {
            long.push((i.wrapping_mul(2_654_435_761) >> 13) as u8 % 7);
        }
        let cases: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"a".to_vec(),
            b"hello hello hello".to_vec(),
            vec![0; 5000],
            sample(),
            (0..=255u8).cycle().take(70_000).collect(),
            long,
        ];
        for data in cases {
            let packed = compress_alone(&data);
            assert_eq!(decompress_alone(&packed, 1 << 24).unwrap(), data, "{} bytes", data.len());
        }
        // what repeats becomes small
        let text = sample();
        assert!(compress_alone(&text).len() < text.len() / 2, "{}", compress_alone(&text).len());
        assert!(compress_alone(&[0; 5000]).len() < 100);
    }

    #[test]
    fn small_streams() {
        // made by Python's `lzma.compress(..., format=lzma.FORMAT_ALONE)`
        let hello = unhex("5d00008000ffffffffffffffff00341949ee8de94f7f35c5a3ffff78a40000");
        assert_eq!(decompress_alone(&hello, 1 << 20).unwrap(), b"hello hello hello");
        let empty = unhex(include_str!("../testdata/lzma/empty.hex"));
        assert_eq!(decompress_alone(&empty, 1 << 20).unwrap(), b"");
    }

    #[test]
    fn sample_with_every_kind_of_match() {
        let expected = sample();
        assert_eq!(expected.len(), 5700);
        assert_eq!(crc32(&expected), 0x7c11_f9aa);
        for name in ["default", "lc0lp2pb0", "known"] {
            let stream = unhex(match name {
                "default" => include_str!("../testdata/lzma/default.hex"),
                "lc0lp2pb0" => include_str!("../testdata/lzma/lc0lp2pb0.hex"),
                _ => include_str!("../testdata/lzma/known.hex"),
            });
            assert_eq!(decompress_alone(&stream, 1 << 20).unwrap(), expected, "{name}");
        }
    }

    #[test]
    fn damaged_streams_are_errors() {
        let stream = unhex(include_str!("../testdata/lzma/default.hex"));
        assert!(decompress_alone(&stream[..stream.len() / 2], 1 << 20).is_err(), "cut in half");
        assert!(decompress_alone(&stream, 100).is_err(), "over the limit");
        let mut bad = stream.clone();
        bad[0] = 225;
        assert!(decompress_alone(&bad, 1 << 20).is_err(), "bad properties");
        // a known size larger than the data: the end marker comes too early
        let mut long = unhex(include_str!("../testdata/lzma/known.hex"));
        long[5..13].copy_from_slice(&6000u64.to_le_bytes());
        assert!(decompress_alone(&long, 1 << 20).is_err());
        // flipped bits somewhere in the middle must not panic
        for i in (20..stream.len()).step_by(37) {
            let mut flipped = stream.clone();
            flipped[i] ^= 0x55;
            let _ = decompress_alone(&flipped, 1 << 20);
        }
    }
}
