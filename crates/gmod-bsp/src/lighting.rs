//! Lightmap samples: red, green, blue and a shared power-of-two exponent per luxel
//! (`ColorRGBExp32`), in linear light.

/// The linear colour of a stored sample: `channel * 2^exponent / 255`.
pub fn rgbe_to_linear(sample: [u8; 4]) -> [f32; 3] {
    let scale = 2f32.powi(sample[3] as i8 as i32) / 255.0;
    [sample[0] as f32 * scale, sample[1] as f32 * scale, sample[2] as f32 * scale]
}

/// Luxels of one lightmap page of a face: `(width + 1) * (height + 1)`.
pub fn luxels(size: [i32; 2]) -> usize {
    (size[0].max(0) as usize + 1) * (size[1].max(0) as usize + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples() {
        assert_eq!(rgbe_to_linear([255, 0, 0, 0]), [1.0, 0.0, 0.0]);
        assert_eq!(rgbe_to_linear([255, 255, 255, 1]), [2.0, 2.0, 2.0]);
        let dim = rgbe_to_linear([128, 64, 0, (-2i8) as u8]);
        assert!((dim[0] - 128.0 / 255.0 / 4.0).abs() < 1e-6 && (dim[1] - 64.0 / 255.0 / 4.0).abs() < 1e-6);
        assert_eq!(luxels([3, 1]), 8);
    }
}
