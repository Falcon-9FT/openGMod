//! CRC-32 (IEEE 802.3, the zlib one), as VPK and GMA files store it for every entry.

const TABLE: [u32; 256] = table();

const fn table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut bit = 0;
        while bit < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            bit += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

/// The CRC-32 of `data`.
pub fn crc32(data: &[u8]) -> u32 {
    update(0, data)
}

/// Continues a CRC-32 computed over earlier data with `data`.
pub fn update(crc: u32, data: &[u8]) -> u32 {
    let mut c = !crc;
    for &byte in data {
        c = TABLE[((c ^ byte as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

#[cfg(test)]
mod tests {
    #[test]
    fn known_values() {
        assert_eq!(super::crc32(b""), 0);
        assert_eq!(super::crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(super::update(super::crc32(b"1234"), b"56789"), 0xCBF4_3926);
    }
}
