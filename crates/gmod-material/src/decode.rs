//! Turning the pixels of any [`Format`] into 8-bit RGBA.
//!
//! DXT1, DXT3 and DXT5 are the block compression formats of S3TC (BC1, BC2 and BC3 in
//! Direct3D 10 terms): the image is cut into 4 x 4 blocks; each holds two RGB565 end colours,
//! the two colours between them, and a 2-bit choice of the four per pixel. DXT1 with the first
//! end colour not above the second has one colour in the middle and transparent black. DXT3
//! adds 4 bits of alpha per pixel; DXT5 two end alphas, six between them (or four, 0 and
//! 255) and a 3-bit choice per pixel.
//!
//! Float formats are clamped to 0..1, not tone-mapped.

use crate::format::Format;

/// Decodes one `width` x `height` image to RGBA, 4 bytes a pixel, rows top to bottom.
/// `None` if `data` is too short or the format cannot be shown (P8, unknown formats).
pub fn to_rgba8(format: Format, width: u32, height: u32, data: &[u8]) -> Option<Vec<u8>> {
    let (w, h) = (width as usize, height as usize);
    let count = w * h;
    match format {
        Format::Dxt1 | Format::Dxt1OneBitAlpha => blocks::<8>(w, h, data, |block, out| colour_block(block, false, out)),
        Format::Dxt3 => blocks::<16>(w, h, data, |block, out| {
            colour_block(&block[8..], true, out);
            let alpha = u64::from_le_bytes(block[..8].try_into().unwrap());
            for (i, texel) in out.iter_mut().enumerate() {
                texel[3] = ((alpha >> (4 * i)) & 0xF) as u8 * 17;
            }
        }),
        Format::Dxt5 => blocks::<16>(w, h, data, |block, out| {
            colour_block(&block[8..], true, out);
            let alpha = alpha_block(block[..8].try_into().unwrap());
            for (texel, a) in out.iter_mut().zip(alpha) {
                texel[3] = a;
            }
        }),
        Format::Rgba8888 => pixels::<4>(data, count, |p| *p),
        Format::Abgr8888 => pixels::<4>(data, count, |&[a, b, g, r]| [r, g, b, a]),
        Format::Argb8888 => pixels::<4>(data, count, |&[a, r, g, b]| [r, g, b, a]),
        Format::Bgra8888 => pixels::<4>(data, count, |&[b, g, r, a]| [r, g, b, a]),
        Format::Bgrx8888 => pixels::<4>(data, count, |&[b, g, r, _]| [r, g, b, 255]),
        Format::Uvwq8888 | Format::Uvlx8888 => pixels::<4>(data, count, |p| *p),
        Format::Rgb888 => pixels::<3>(data, count, |&[r, g, b]| [r, g, b, 255]),
        Format::Bgr888 => pixels::<3>(data, count, |&[b, g, r]| [r, g, b, 255]),
        Format::Rgb888Bluescreen => pixels::<3>(data, count, |&[r, g, b]| bluescreen(r, g, b)),
        Format::Bgr888Bluescreen => pixels::<3>(data, count, |&[b, g, r]| bluescreen(r, g, b)),
        Format::Rgb565 => pixels::<2>(data, count, |p| {
            let v = u16::from_le_bytes(*p);
            [bits(v, 0, 5), bits(v, 5, 6), bits(v, 11, 5), 255]
        }),
        Format::Bgr565 => pixels::<2>(data, count, |p| {
            let v = u16::from_le_bytes(*p);
            [bits(v, 11, 5), bits(v, 5, 6), bits(v, 0, 5), 255]
        }),
        Format::Bgrx5551 => pixels::<2>(data, count, |p| {
            let v = u16::from_le_bytes(*p);
            [bits(v, 10, 5), bits(v, 5, 5), bits(v, 0, 5), 255]
        }),
        Format::Bgra5551 => pixels::<2>(data, count, |p| {
            let v = u16::from_le_bytes(*p);
            [bits(v, 10, 5), bits(v, 5, 5), bits(v, 0, 5), bits(v, 15, 1)]
        }),
        Format::Bgra4444 => pixels::<2>(data, count, |p| {
            let v = u16::from_le_bytes(*p);
            [bits(v, 8, 4), bits(v, 4, 4), bits(v, 0, 4), bits(v, 12, 4)]
        }),
        Format::I8 => pixels::<1>(data, count, |&[l]| [l, l, l, 255]),
        Format::A8 => pixels::<1>(data, count, |&[a]| [0, 0, 0, a]),
        Format::Ia88 => pixels::<2>(data, count, |&[l, a]| [l, l, l, a]),
        Format::Uv88 => pixels::<2>(data, count, |&[u, v]| [u, v, 0, 255]),
        Format::Rgba16161616F => pixels::<8>(data, count, |p| {
            let c = |i: usize| unit_float(half(u16::from_le_bytes([p[2 * i], p[2 * i + 1]])));
            [c(0), c(1), c(2), c(3)]
        }),
        Format::Rgba16161616 => pixels::<8>(data, count, |p| [p[1], p[3], p[5], p[7]]),
        Format::R32F => pixels::<4>(data, count, |p| {
            let l = unit_float(f32::from_le_bytes(*p));
            [l, l, l, 255]
        }),
        Format::Rgb323232F => pixels::<12>(data, count, |p| {
            let c = |i: usize| unit_float(f32::from_le_bytes(p[4 * i..4 * i + 4].try_into().unwrap()));
            [c(0), c(1), c(2), 255]
        }),
        Format::Rgba32323232F => pixels::<16>(data, count, |p| {
            let c = |i: usize| unit_float(f32::from_le_bytes(p[4 * i..4 * i + 4].try_into().unwrap()));
            [c(0), c(1), c(2), c(3)]
        }),
        Format::P8 | Format::Unknown(_) => None,
    }
}

/// A value from an IEEE 754 half float.
pub fn half(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = i32::from((bits >> 10) & 0x1F);
    let fraction = f32::from(bits & 0x3FF);
    match exponent {
        0 => sign * fraction * (-24f32).exp2(),
        31 if fraction == 0.0 => sign * f32::INFINITY,
        31 => f32::NAN,
        _ => sign * (1.0 + fraction / 1024.0) * ((exponent - 15) as f32).exp2(),
    }
}

fn unit_float(v: f32) -> u8 {
    // NaN turns into 0
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// `width` bits of `v` from bit `shift` on, widened to 8 bits by repeating the top bits.
fn bits(v: u16, shift: u32, width: u32) -> u8 {
    let x = (u32::from(v) >> shift) & ((1 << width) - 1);
    match width {
        1 => (x * 255) as u8,
        _ => ((x << (8 - width)) | (x >> (2 * width).saturating_sub(8))) as u8,
    }
}

fn bluescreen(r: u8, g: u8, b: u8) -> [u8; 4] {
    if (r, g, b) == (0, 0, 255) { [0, 0, 0, 0] } else { [r, g, b, 255] }
}

fn pixels<const N: usize>(data: &[u8], count: usize, convert: impl Fn(&[u8; N]) -> [u8; 4]) -> Option<Vec<u8>> {
    let (pixels, _) = data.get(..count * N)?.as_chunks::<N>();
    Some(pixels.iter().flat_map(convert).collect())
}

/// Decodes 4 x 4 blocks of `N` bytes into a `width` x `height` image; `decode` fills in the 16
/// texels of a block, row by row.
fn blocks<const N: usize>(width: usize, height: usize, data: &[u8], decode: impl Fn(&[u8; N], &mut [[u8; 4]; 16])) -> Option<Vec<u8>> {
    let (across, down) = (width.div_ceil(4), height.div_ceil(4));
    let (blocks, _) = data.get(..across * down * N)?.as_chunks::<N>();
    let mut out = vec![0u8; width * height * 4];
    let mut texels = [[0u8; 4]; 16];
    for (i, block) in blocks.iter().enumerate() {
        decode(block, &mut texels);
        let (bx, by) = (i % across * 4, i / across * 4);
        for (j, texel) in texels.iter().enumerate() {
            let (x, y) = (bx + j % 4, by + j / 4);
            if x < width && y < height {
                let at = (y * width + x) * 4;
                out[at..at + 4].copy_from_slice(texel);
            }
        }
    }
    Some(out)
}

fn rgb565(v: u16) -> [u8; 3] {
    [bits(v, 11, 5), bits(v, 5, 6), bits(v, 0, 5)]
}

/// `(a·wa + b·wb) / (wa + wb)`, rounded, per channel.
fn mix(a: [u8; 3], b: [u8; 3], wa: u32, wb: u32) -> [u8; 4] {
    let c = |i: usize| ((u32::from(a[i]) * wa + u32::from(b[i]) * wb + (wa + wb) / 2) / (wa + wb)) as u8;
    [c(0), c(1), c(2), 255]
}

/// The colour half of a block: two RGB565 end colours and 2-bit indices. DXT3 and DXT5 always
/// use four colours; DXT1 has three and transparent black when the first colour is not above
/// the second.
fn colour_block(block: &[u8], always_four: bool, out: &mut [[u8; 4]; 16]) {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let (a, b) = (rgb565(c0), rgb565(c1));
    let palette = if c0 > c1 || always_four {
        [mix(a, b, 1, 0), mix(a, b, 0, 1), mix(a, b, 2, 1), mix(a, b, 1, 2)]
    } else {
        [mix(a, b, 1, 0), mix(a, b, 0, 1), mix(a, b, 1, 1), [0, 0, 0, 0]]
    };
    let indices = u32::from_le_bytes(block[4..8].try_into().unwrap());
    for (i, texel) in out.iter_mut().enumerate() {
        *texel = palette[(indices >> (2 * i)) as usize & 3];
    }
}

/// The alpha half of a DXT5 block: two end values and 3-bit indices.
fn alpha_block(block: [u8; 8]) -> [u8; 16] {
    let (a0, a1) = (u32::from(block[0]), u32::from(block[1]));
    let lerp = |wa: u32, wb: u32| ((a0 * wa + a1 * wb + (wa + wb) / 2) / (wa + wb)) as u8;
    let palette = if a0 > a1 {
        [a0 as u8, a1 as u8, lerp(6, 1), lerp(5, 2), lerp(4, 3), lerp(3, 4), lerp(2, 5), lerp(1, 6)]
    } else {
        [a0 as u8, a1 as u8, lerp(4, 1), lerp(3, 2), lerp(2, 3), lerp(1, 4), 0, 255]
    };
    let mut indices = [0u8; 8];
    indices[..6].copy_from_slice(&block[2..8]);
    let indices = u64::from_le_bytes(indices);
    std::array::from_fn(|i| palette[(indices >> (3 * i)) as usize & 7])
}
