//! The entity lump: every entity of the map as `{ "key" "value" ... }` blocks of text.

use std::io;

use crate::lumps::invalid;

/// One entity: its key-value pairs in file order (keys may repeat: outputs, for one).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entity {
    pub pairs: Vec<(String, String)>,
}

impl Entity {
    /// The first value of `key` (ASCII case ignored, as in the engine).
    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str())
    }

    pub fn class_name(&self) -> &str {
        self.get("classname").unwrap_or("")
    }

    /// A value of three numbers (`origin`, `angles`), if it is one.
    pub fn vec3(&self, key: &str) -> Option<[f32; 3]> {
        let mut parts = self.get(key)?.split_whitespace().map(|p| p.parse::<f32>().ok());
        let v = [parts.next()??, parts.next()??, parts.next()??];
        Some(v)
    }
}

fn token<'a>(text: &'a str, pos: &mut usize) -> Option<&'a str> {
    let bytes = text.as_bytes();
    while *pos < bytes.len() && (bytes[*pos].is_ascii_whitespace() || bytes[*pos] == 0) {
        *pos += 1;
    }
    let start = *pos;
    match bytes.get(start)? {
        b'{' | b'}' => {
            *pos += 1;
            Some(&text[start..start + 1])
        }
        b'"' => {
            let end = text[start + 1..].find('"').map_or(text.len(), |e| start + 1 + e);
            *pos = (end + 1).min(text.len());
            Some(&text[start + 1..end])
        }
        _ => {
            let end =
                text[start..].find(|c: char| c.is_ascii_whitespace() || c == '"' || c == '{' || c == '}').map_or(text.len(), |e| start + e);
            *pos = end;
            Some(&text[start..end])
        }
    }
}

/// Reads the entity lump. Bytes that are not UTF-8 (Windows-1252 text in values) become
/// U+FFFD.
pub fn parse_entities(lump: &[u8]) -> io::Result<Vec<Entity>> {
    let text = String::from_utf8_lossy(lump);
    let mut pos = 0;
    let mut out = Vec::new();
    while let Some(open) = token(&text, &mut pos) {
        if open != "{" {
            return Err(invalid(format!("entities: `{open}` where an entity should start")));
        }
        let mut entity = Entity::default();
        loop {
            match token(&text, &mut pos) {
                Some("}") => break,
                Some("{") | None => return Err(invalid("entities: an entity is not closed")),
                Some(key) => {
                    let value = token(&text, &mut pos).ok_or_else(|| invalid("entities: a key without a value"))?;
                    entity.pairs.push((key.to_string(), value.to_string()));
                }
            }
        }
        out.push(entity);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entities() {
        let lump = b"{\n\"world_maxs\" \"1 2 3\"\n\"classname\" \"worldspawn\"\n}\n{\n\"origin\" \"-64 128.5 16\"\n\"ClassName\" \"info_player_start\"\n\"OnTrigger\" \"a\"\n\"OnTrigger\" \"b\"\n}\n\0";
        let entities = parse_entities(lump).unwrap();
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[0].class_name(), "worldspawn");
        assert_eq!(entities[1].class_name(), "info_player_start");
        assert_eq!(entities[1].vec3("origin"), Some([-64.0, 128.5, 16.0]));
        assert_eq!(entities[1].pairs.iter().filter(|(k, _)| k == "OnTrigger").count(), 2);
        assert!(parse_entities(b"{ \"a\" \"b\" ").is_err());
        assert!(parse_entities(b"\"a\" \"b\"").is_err());
        assert_eq!(parse_entities(b"  \0\0").unwrap(), []);
    }
}
