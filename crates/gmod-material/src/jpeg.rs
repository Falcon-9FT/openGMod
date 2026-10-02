//! JPEG images, as GMod reads them for `Material("x.jpg")`: the main menu's backgrounds,
//! screenshots, workshop previews. Baseline and progressive, grey, RGB and CMYK.

use std::io;

use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

use crate::png::Rgba;

/// The largest image read, in pixels a side.
const MAX_SIDE: usize = 16384;

/// Reads a JPEG into 8-bit RGBA.
pub fn decode(data: &[u8]) -> io::Result<Rgba> {
    let invalid = |e: String| io::Error::new(io::ErrorKind::InvalidData, format!("JPEG: {e}"));
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA).set_max_width(MAX_SIDE).set_max_height(MAX_SIDE);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(data), options);
    let pixels = decoder.decode().map_err(|e| invalid(format!("{e:?}")))?;
    let (width, height) = decoder.dimensions().ok_or_else(|| invalid("no size".into()))?;
    if pixels.len() != width * height * 4 {
        return Err(invalid("not the pixels its size says".into()));
    }
    Ok(Rgba { width: width as u32, height: height as u32, pixels })
}

/// Reads a PNG or a JPEG, by what the data starts with.
pub fn decode_image(data: &[u8]) -> io::Result<Rgba> {
    if data.starts_with(&[0xFF, 0xD8]) { decode(data) } else { crate::png::decode(data) }
}
