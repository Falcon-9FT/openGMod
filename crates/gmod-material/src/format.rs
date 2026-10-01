//! Pixel formats of textures, numbered as in the files.
//!
//! The names list the components from the first byte (or the lowest bits) on: `BGRA8888` is
//! the bytes B, G, R, A; `BGR565` keeps blue in bits 0-4 and red in bits 11-15.

/// How the pixels of a texture are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Rgba8888,
    Abgr8888,
    Rgb888,
    Bgr888,
    Rgb565,
    I8,
    Ia88,
    /// Palette indices; textures never carry the palette, so this cannot be shown.
    P8,
    A8,
    /// RGB where pure blue (0, 0, 255) is transparent.
    Rgb888Bluescreen,
    Bgr888Bluescreen,
    Argb8888,
    Bgra8888,
    Dxt1,
    Dxt3,
    Dxt5,
    Bgrx8888,
    Bgr565,
    Bgrx5551,
    Bgra4444,
    Dxt1OneBitAlpha,
    Bgra5551,
    /// Two signed offsets, for refraction (`dudv`) maps.
    Uv88,
    Uvwq8888,
    /// Half floats, for HDR.
    Rgba16161616F,
    Rgba16161616,
    Uvlx8888,
    R32F,
    Rgb323232F,
    Rgba32323232F,
    /// A number this reader does not know.
    Unknown(i32),
}

const KNOWN: [Format; 30] = [
    Format::Rgba8888,
    Format::Abgr8888,
    Format::Rgb888,
    Format::Bgr888,
    Format::Rgb565,
    Format::I8,
    Format::Ia88,
    Format::P8,
    Format::A8,
    Format::Rgb888Bluescreen,
    Format::Bgr888Bluescreen,
    Format::Argb8888,
    Format::Bgra8888,
    Format::Dxt1,
    Format::Dxt3,
    Format::Dxt5,
    Format::Bgrx8888,
    Format::Bgr565,
    Format::Bgrx5551,
    Format::Bgra4444,
    Format::Dxt1OneBitAlpha,
    Format::Bgra5551,
    Format::Uv88,
    Format::Uvwq8888,
    Format::Rgba16161616F,
    Format::Rgba16161616,
    Format::Uvlx8888,
    Format::R32F,
    Format::Rgb323232F,
    Format::Rgba32323232F,
];

const NAMES: [&str; 30] = [
    "RGBA8888",
    "ABGR8888",
    "RGB888",
    "BGR888",
    "RGB565",
    "I8",
    "IA88",
    "P8",
    "A8",
    "RGB888_BLUESCREEN",
    "BGR888_BLUESCREEN",
    "ARGB8888",
    "BGRA8888",
    "DXT1",
    "DXT3",
    "DXT5",
    "BGRX8888",
    "BGR565",
    "BGRX5551",
    "BGRA4444",
    "DXT1_ONEBITALPHA",
    "BGRA5551",
    "UV88",
    "UVWQ8888",
    "RGBA16161616F",
    "RGBA16161616",
    "UVLX8888",
    "R32F",
    "RGB323232F",
    "RGBA32323232F",
];

impl Format {
    /// The format with number `raw`; `None` for -1, which means "no image".
    pub fn from_raw(raw: i32) -> Option<Format> {
        match raw {
            -1 => None,
            0..=29 => Some(KNOWN[raw as usize]),
            _ => Some(Format::Unknown(raw)),
        }
    }

    /// The number in the file.
    pub fn raw(self) -> i32 {
        match self {
            Format::Unknown(raw) => raw,
            known => KNOWN.iter().position(|&f| f == known).unwrap() as i32,
        }
    }

    pub fn name(self) -> String {
        match self {
            Format::Unknown(raw) => format!("format {raw}"),
            known => NAMES[known.raw() as usize].to_string(),
        }
    }

    /// Block-compressed: stored in 4 x 4 blocks of 8 or 16 bytes.
    pub fn is_compressed(self) -> bool {
        matches!(self, Format::Dxt1 | Format::Dxt1OneBitAlpha | Format::Dxt3 | Format::Dxt5)
    }

    /// Bytes per pixel, or per 4 x 4 block of a compressed format; `None` if unknown.
    fn unit(self) -> Option<usize> {
        Some(match self {
            Format::I8 | Format::A8 | Format::P8 => 1,
            Format::Rgb565 | Format::Bgr565 | Format::Bgrx5551 | Format::Bgra5551 | Format::Bgra4444 | Format::Ia88 | Format::Uv88 => 2,
            Format::Rgb888 | Format::Bgr888 | Format::Rgb888Bluescreen | Format::Bgr888Bluescreen => 3,
            Format::Rgba8888
            | Format::Abgr8888
            | Format::Argb8888
            | Format::Bgra8888
            | Format::Bgrx8888
            | Format::Uvwq8888
            | Format::Uvlx8888
            | Format::R32F => 4,
            Format::Rgba16161616F | Format::Rgba16161616 => 8,
            Format::Rgb323232F => 12,
            Format::Rgba32323232F => 16,
            Format::Dxt1 | Format::Dxt1OneBitAlpha => 8,
            Format::Dxt3 | Format::Dxt5 => 16,
            Format::Unknown(_) => return None,
        })
    }

    /// Bytes of one `width` x `height` image; `None` if the format is unknown.
    pub fn image_size(self, width: u32, height: u32) -> Option<usize> {
        let unit = self.unit()?;
        let (w, h) = (width as usize, height as usize);
        Some(if self.is_compressed() { w.div_ceil(4) * h.div_ceil(4) * unit } else { w * h * unit })
    }
}
