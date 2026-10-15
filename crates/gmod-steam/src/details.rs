//! `SteamUGCDetails_t`: what the workshop says of an item. The API fills a structure of C,
//! laid out as Windows lays it out (8-byte alignment); it is read from bytes at the offsets
//! of its fields, and what is read is checked by the caller (the item asked for).

use crate::{APP_ID, i32_at, u32_at, u64_at};

/// Room for the structure: more than it takes, a newer library adding fields at its end.
pub(crate) const BUFFER: usize = 16 * 1024;

// the offsets of the fields read
const ID: usize = 0;
const RESULT: usize = 8;
const CONSUMER_APP: usize = 20;
const TITLE: usize = 24;
const TITLE_LEN: usize = 129;
const DESCRIPTION: usize = TITLE + TITLE_LEN;
const DESCRIPTION_LEN: usize = 8000;
const OWNER: usize = 8160;
const CREATED: usize = OWNER + 8;
const UPDATED: usize = CREATED + 4;
const BANNED: usize = UPDATED + 4 + 4 + 4;
const TAGS: usize = BANNED + 3;
const TAGS_LEN: usize = 1025;
const FILE_NAME: usize = 9232;
const FILE_NAME_LEN: usize = 260;
const FILE_SIZE: usize = FILE_NAME + FILE_NAME_LEN;
const VOTES_UP: usize = FILE_SIZE + 4 + 4 + 256;
const VOTES_DOWN: usize = VOTES_UP + 4;
const SCORE: usize = VOTES_DOWN + 4;

/// A workshop item, as `steamworks.FileInfo` gives it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Details {
    pub id: u64,
    /// Steam's `EResult` for the item (1: there).
    pub result: i32,
    pub title: String,
    pub description: String,
    /// The SteamID of who made it.
    pub owner: u64,
    /// Unix time.
    pub created: u32,
    pub updated: u32,
    /// Comma separated.
    pub tags: String,
    pub banned: bool,
    pub size: u64,
    /// Where its preview picture is.
    pub preview_url: String,
    pub votes_up: u32,
    pub votes_down: u32,
    /// From 0 to 1.
    pub score: f32,
}

/// A string of a fixed-size field, up to its 0.
fn text(bytes: &[u8], at: usize, len: usize) -> String {
    let field = bytes.get(at..at + len).unwrap_or_default();
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// Reads the details; none when they are not an item of Garry's Mod (a layout not this one).
pub(crate) fn parse(bytes: &[u8], preview_url: String) -> Option<Details> {
    let id = u64_at(bytes, ID)?;
    let result = i32_at(bytes, RESULT)?;
    // an item Steam has no details of comes back with its ID and a failure only
    if result != 1 {
        return Some(Details { id, result, ..Details::default() });
    }
    if u32_at(bytes, CONSUMER_APP)? != APP_ID {
        return None;
    }
    Some(Details {
        id,
        result,
        title: text(bytes, TITLE, TITLE_LEN),
        description: text(bytes, DESCRIPTION, DESCRIPTION_LEN),
        owner: u64_at(bytes, OWNER)?,
        created: u32_at(bytes, CREATED)?,
        updated: u32_at(bytes, UPDATED)?,
        tags: text(bytes, TAGS, TAGS_LEN),
        banned: *bytes.get(BANNED)? != 0,
        size: u64::from(u32_at(bytes, FILE_SIZE)?),
        preview_url,
        votes_up: u32_at(bytes, VOTES_UP)?,
        votes_down: u32_at(bytes, VOTES_DOWN)?,
        score: f32::from_bits(u32_at(bytes, SCORE)?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(bytes: &mut [u8], at: usize, value: &[u8]) {
        bytes[at..at + value.len()].copy_from_slice(value);
    }

    #[test]
    fn details_are_read_at_the_offsets_of_the_structure() {
        let mut raw = vec![0u8; BUFFER];
        put(&mut raw, ID, &104_607_228u64.to_le_bytes());
        put(&mut raw, RESULT, &1i32.to_le_bytes());
        put(&mut raw, 16, &4000u32.to_le_bytes());
        put(&mut raw, CONSUMER_APP, &4000u32.to_le_bytes());
        put(&mut raw, TITLE, b"Wiremod\0garbage");
        put(&mut raw, DESCRIPTION, "Провода\0".as_bytes());
        put(&mut raw, OWNER, &76_561_197_960_279_927u64.to_le_bytes());
        put(&mut raw, CREATED, &1_350_000_000u32.to_le_bytes());
        put(&mut raw, UPDATED, &1_700_000_000u32.to_le_bytes());
        put(&mut raw, TAGS, b"Addon,tool\0");
        put(&mut raw, FILE_SIZE, &12_345_678u32.to_le_bytes());
        put(&mut raw, VOTES_UP, &900u32.to_le_bytes());
        put(&mut raw, VOTES_DOWN, &100u32.to_le_bytes());
        put(&mut raw, SCORE, &0.9f32.to_le_bytes());
        let item = parse(&raw, "https://images.example/1".into()).unwrap();
        assert_eq!(
            item,
            Details {
                id: 104_607_228,
                result: 1,
                title: "Wiremod".into(),
                description: "Провода".into(),
                owner: 76_561_197_960_279_927,
                created: 1_350_000_000,
                updated: 1_700_000_000,
                tags: "Addon,tool".into(),
                banned: false,
                size: 12_345_678,
                preview_url: "https://images.example/1".into(),
                votes_up: 900,
                votes_down: 100,
                score: 0.9,
            }
        );
        // the offsets of SteamUGCDetails_t, as the SDK's header lays them out
        assert_eq!((DESCRIPTION, OWNER, TAGS, FILE_NAME, FILE_SIZE, VOTES_UP, SCORE), (153, 8160, 8187, 9232, 9492, 9756, 9764));
    }

    #[test]
    fn an_item_of_another_game_is_not_read() {
        let mut raw = vec![0u8; BUFFER];
        put(&mut raw, ID, &5u64.to_le_bytes());
        put(&mut raw, RESULT, &1i32.to_le_bytes());
        put(&mut raw, CONSUMER_APP, &440u32.to_le_bytes());
        assert_eq!(parse(&raw, String::new()), None);
        // one Steam has nothing of: its ID and why
        put(&mut raw, RESULT, &9i32.to_le_bytes());
        assert_eq!(parse(&raw, String::new()).map(|d| (d.id, d.result)), Some((5, 9)));
    }
}
