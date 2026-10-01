//! Text files as the engine finds them: UTF-8 with or without a byte order mark, and the
//! UTF-16 of localisation files (`resource/*_english.txt`).

use std::borrow::Cow;

/// Decodes a text file. A UTF-8 byte order mark is dropped; a UTF-16 one (either byte order)
/// selects UTF-16. Everything else is read as UTF-8, with bytes that are not valid UTF-8
/// replaced by U+FFFD (they only occur in comments and strings, never in the structure).
pub fn decode_text(bytes: &[u8]) -> Cow<'_, str> {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return Cow::Owned(utf16(rest, u16::from_le_bytes));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return Cow::Owned(utf16(rest, u16::from_be_bytes));
    }
    String::from_utf8_lossy(bytes)
}

fn utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> String {
    let units = bytes.chunks_exact(2).map(|pair| unit([pair[0], pair[1]]));
    char::decode_utf16(units).map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)).collect()
}
