//! Writing PNG images: previews and, later, screenshots.
//!
//! 8 bits a channel, RGB or RGBA. Each row gets the filter that leaves the smallest sum of
//! absolute differences (the usual choice), then all rows are compressed with zlib.

use gmod_fs::crc32;
use gmod_fs::deflate::zlib;

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
