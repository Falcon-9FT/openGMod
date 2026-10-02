//! PNG images: reading the ones addons draw with (`icon16/add.png`, Derma's skin), writing
//! previews and, later, screenshots.
//!
//! Reading takes every kind there is: grey, RGB, palette, with or without alpha, 1 to 16 bits,
//! interlaced or not; what comes out is 8-bit RGBA.
//!
//! Writing is 8 bits a channel, RGB or RGBA. Each row gets the filter that leaves the smallest
//! sum of absolute differences (the usual choice), then all rows are compressed with zlib.

use std::io;

use gmod_fs::crc32;
use gmod_fs::deflate::zlib;
use gmod_fs::inflate::inflate;

/// The largest image read: 16384 pixels a side.
const MAX_SIDE: u32 = 16384;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("PNG: {message}"))
}

/// A decoded image: `width` x `height` pixels of RGBA, rows top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Reads a PNG file into 8-bit RGBA.
pub fn decode(data: &[u8]) -> io::Result<Rgba> {
    if !data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(invalid("not a PNG file"));
    }
    let mut at = 8;
    let mut header: Option<[u8; 13]> = None;
    let mut palette: Vec<[u8; 4]> = Vec::new();
    let mut transparent: Option<Vec<u8>> = None;
    let mut compressed = Vec::new();
    while at + 8 <= data.len() {
        let len = u32::from_be_bytes(data[at..at + 4].try_into().expect("4 bytes")) as usize;
        let kind = &data[at + 4..at + 8];
        let body = data.get(at + 8..at + 8 + len).ok_or_else(|| invalid("a chunk runs past the end"))?;
        match kind {
            b"IHDR" => header = Some(body.try_into().map_err(|_| invalid("IHDR is not 13 bytes"))?),
            b"PLTE" => palette = body.chunks_exact(3).map(|c| [c[0], c[1], c[2], 255]).collect(),
            b"tRNS" => transparent = Some(body.to_vec()),
            b"IDAT" => compressed.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        at += 12 + len;
    }
    let header = header.ok_or_else(|| invalid("no IHDR"))?;
    let width = u32::from_be_bytes(header[0..4].try_into().expect("4 bytes"));
    let height = u32::from_be_bytes(header[4..8].try_into().expect("4 bytes"));
    let (depth, color, interlaced) = (header[8], header[9], header[12] == 1);
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(invalid("size out of range"));
    }
    let channels = match (color, depth) {
        (0, 1 | 2 | 4 | 8 | 16) | (3, 1 | 2 | 4 | 8) => 1,
        (2 | 6 | 4, 8 | 16) => match color {
            2 => 3,
            6 => 4,
            _ => 2,
        },
        _ => return Err(invalid("unknown colour type or bit depth")),
    };
    if let Some(trns) = &transparent
        && color == 3
    {
        for (entry, alpha) in palette.iter_mut().zip(trns) {
            entry[3] = *alpha;
        }
    }
    if compressed.len() < 2 {
        return Err(invalid("no image data"));
    }
    let bits_per_pixel = channels * usize::from(depth);
    let row_bytes = |w: u32| (w as usize * bits_per_pixel).div_ceil(8);
    let expected: usize = if interlaced {
        passes(width, height).iter().filter(|p| p.2 > 0 && p.3 > 0).map(|p| (row_bytes(p.2) + 1) * p.3 as usize).sum()
    } else {
        (row_bytes(width) + 1) * height as usize
    };
    // zlib: two bytes of header before the DEFLATE stream
    let raw = inflate(&compressed[2..], expected, expected as u64 + 1)?;
    if raw.len() < expected {
        return Err(invalid("image data is short"));
    }
    let bpp = bits_per_pixel.div_ceil(8);
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    let mut put = |x: u32, y: u32, rgba: [u8; 4]| {
        let i = (y as usize * width as usize + x as usize) * 4;
        pixels[i..i + 4].copy_from_slice(&rgba);
    };
    let key = |sample: u16| -> bool {
        match (&transparent, color) {
            (Some(t), 0) if t.len() >= 2 => sample == u16::from_be_bytes([t[0], t[1]]),
            _ => false,
        }
    };
    let mut offset = 0;
    let layout: Vec<(u32, u32, u32, u32, u32, u32)> =
        if interlaced { passes(width, height).to_vec() } else { vec![(0, 0, width, height, 1, 1)] };
    for (x0, y0, w, h, dx, dy) in layout {
        if w == 0 || h == 0 {
            continue;
        }
        let stride = row_bytes(w);
        let mut previous = vec![0u8; stride];
        for row in 0..h {
            let filter = raw[offset];
            let mut line = raw[offset + 1..offset + 1 + stride].to_vec();
            offset += stride + 1;
            unfilter(filter, &mut line, &previous, bpp)?;
            for col in 0..w {
                let sample = |index: usize| -> u16 {
                    match depth {
                        16 => u16::from_be_bytes([line[index * 2], line[index * 2 + 1]]),
                        8 => u16::from(line[index]),
                        _ => {
                            let bit = index * usize::from(depth);
                            let byte = line[bit / 8];
                            let shift = 8 - usize::from(depth) - bit % 8;
                            u16::from((byte >> shift) & ((1u8 << depth) - 1))
                        }
                    }
                };
                let to8 = |v: u16| -> u8 {
                    match depth {
                        16 => (v >> 8) as u8,
                        8 => v as u8,
                        _ => (u32::from(v) * 255 / ((1u32 << depth) - 1)) as u8,
                    }
                };
                let base = col as usize * channels;
                let rgba = match color {
                    0 => {
                        let v = sample(base);
                        let g = to8(v);
                        [g, g, g, if key(v) { 0 } else { 255 }]
                    }
                    2 => {
                        let (r, g, b) = (sample(base), sample(base + 1), sample(base + 2));
                        let keyed = match &transparent {
                            Some(t) if t.len() >= 6 => {
                                let k = |i: usize| u16::from_be_bytes([t[i], t[i + 1]]);
                                (r, g, b) == (k(0), k(2), k(4))
                            }
                            _ => false,
                        };
                        [to8(r), to8(g), to8(b), if keyed { 0 } else { 255 }]
                    }
                    3 => palette.get(usize::from(sample(base))).copied().unwrap_or([0, 0, 0, 255]),
                    4 => {
                        let g = to8(sample(base));
                        [g, g, g, to8(sample(base + 1))]
                    }
                    _ => [to8(sample(base)), to8(sample(base + 1)), to8(sample(base + 2)), to8(sample(base + 3))],
                };
                put(x0 + col * dx, y0 + row * dy, rgba);
            }
            previous = line;
        }
    }
    Ok(Rgba { width, height, pixels })
}

/// Adam7: each pass's first pixel, size and step.
fn passes(width: u32, height: u32) -> [(u32, u32, u32, u32, u32, u32); 7] {
    let pass = |x0: u32, y0: u32, dx: u32, dy: u32| {
        let w = if width > x0 { (width - x0).div_ceil(dx) } else { 0 };
        let h = if height > y0 { (height - y0).div_ceil(dy) } else { 0 };
        (x0, y0, w, h, dx, dy)
    };
    [pass(0, 0, 8, 8), pass(4, 0, 8, 8), pass(0, 4, 4, 8), pass(2, 0, 4, 4), pass(0, 2, 2, 4), pass(1, 0, 2, 2), pass(0, 1, 1, 2)]
}

/// Undoes a row's filter, given the row above.
fn unfilter(filter: u8, line: &mut [u8], above: &[u8], bpp: usize) -> io::Result<()> {
    for x in 0..line.len() {
        let a = if x >= bpp { line[x - bpp] } else { 0 };
        let b = above[x];
        let c = if x >= bpp { above[x - bpp] } else { 0 };
        let predicted = match filter {
            0 => 0,
            1 => a,
            2 => b,
            3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
            4 => paeth(a, b, c),
            _ => return Err(invalid("unknown row filter")),
        };
        line[x] = line[x].wrapping_add(predicted);
    }
    Ok(())
}

/// Encodes `pixels` (rows top to bottom, `channels` bytes a pixel: 3 for RGB, 4 for RGBA).
///
/// # Panics
/// If `channels` is not 3 or 4, or `pixels` does not hold `width` x `height` pixels.
pub fn encode(width: u32, height: u32, channels: u8, pixels: &[u8]) -> Vec<u8> {
    assert!(channels == 3 || channels == 4, "PNG: 3 or 4 channels");
    let stride = width as usize * usize::from(channels);
    assert_eq!(pixels.len(), stride * height as usize, "PNG: pixels do not match the size");
    let bpp = usize::from(channels);

    let mut filtered = Vec::with_capacity((stride + 1) * height as usize);
    let zero = vec![0u8; stride];
    let mut candidate = vec![0u8; stride];
    let mut best = vec![0u8; stride];
    for y in 0..height as usize {
        let row = &pixels[y * stride..(y + 1) * stride];
        let above = if y == 0 { &zero[..] } else { &pixels[(y - 1) * stride..y * stride] };
        let mut best_filter = 0;
        let mut best_cost = u64::MAX;
        for filter in 0..5u8 {
            for x in 0..stride {
                let a = if x >= bpp { row[x - bpp] } else { 0 };
                let b = above[x];
                let c = if x >= bpp { above[x - bpp] } else { 0 };
                let predicted = match filter {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                    _ => paeth(a, b, c),
                };
                candidate[x] = row[x].wrapping_sub(predicted);
            }
            let cost = candidate.iter().map(|&v| u64::from((v as i8).unsigned_abs())).sum();
            if cost < best_cost {
                (best_cost, best_filter) = (cost, filter);
                best.copy_from_slice(&candidate);
            }
        }
        filtered.push(best_filter);
        filtered.extend_from_slice(&best);
    }

    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    // 8 bits; colour type 2 (RGB) or 6 (RGBA); deflate; adaptive filters; not interlaced
    header.extend_from_slice(&[8, if channels == 3 { 2 } else { 6 }, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &zlib(&filtered));
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32::update(crc32::crc32(kind), data);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let (pa, pb, pc) = ((p - i16::from(a)).abs(), (p - i16::from(b)).abs(), (p - i16::from(c)).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads back what [`encode`] wrote: chunks, CRCs, zlib and the row filters.
    fn decode(png: &[u8]) -> (u32, u32, u8, Vec<u8>) {
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        let mut at = 8;
        let (mut width, mut height, mut channels, mut data) = (0, 0, 0, Vec::new());
        while at < png.len() {
            let length = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
            let kind = &png[at + 4..at + 8];
            let body = &png[at + 8..at + 8 + length];
            let crc = u32::from_be_bytes(png[at + 8 + length..at + 12 + length].try_into().unwrap());
            assert_eq!(crc, crc32::crc32(&png[at + 4..at + 8 + length]));
            match kind {
                b"IHDR" => {
                    width = u32::from_be_bytes(body[0..4].try_into().unwrap());
                    height = u32::from_be_bytes(body[4..8].try_into().unwrap());
                    channels = if body[9] == 2 { 3 } else { 4 };
                }
                b"IDAT" => data.extend_from_slice(body),
                _ => {}
            }
            at += 12 + length;
        }
        let raw = gmod_fs::inflate::inflate(&data[2..data.len() - 4], 0, 1 << 30).unwrap();
        let bpp = usize::from(channels);
        let stride = width as usize * bpp;
        let mut pixels = vec![0u8; stride * height as usize];
        for y in 0..height as usize {
            let filter = raw[y * (stride + 1)];
            for x in 0..stride {
                let v = raw[y * (stride + 1) + 1 + x];
                let a = if x >= bpp { pixels[y * stride + x - bpp] } else { 0 };
                let b = if y > 0 { pixels[(y - 1) * stride + x] } else { 0 };
                let c = if x >= bpp && y > 0 { pixels[(y - 1) * stride + x - bpp] } else { 0 };
                let predicted = match filter {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                    _ => paeth(a, b, c),
                };
                pixels[y * stride + x] = v.wrapping_add(predicted);
            }
        }
        (width, height, channels, pixels)
    }

    #[test]
    fn round_trip() {
        // a gradient, noise and flat areas, so that every filter gets chosen somewhere
        let (w, h) = (37u32, 23u32);
        let mut x = 7u32;
        let pixels: Vec<u8> = (0..w * h * 4)
            .map(|i| match (i / 4 / w) % 3 {
                0 => (i % 251) as u8,
                1 => {
                    x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    (x >> 24) as u8
                }
                _ => 200,
            })
            .collect();
        assert_eq!(decode(&encode(w, h, 4, &pixels)), (w, h, 4, pixels.clone()));
        let rgb: Vec<u8> = pixels.chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        assert_eq!(decode(&encode(w, h, 3, &rgb)), (w, h, 3, rgb));
    }
}
