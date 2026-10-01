//! Paths inside the game file system.
//!
//! Every mount indexes its files by the *normalized* path: lower case, `/` between
//! components, no leading or trailing `/`. The engine treats paths without regard to case
//! on every system, and so must openGMOD on Linux, where the disk does not.

/// Brings a game path into normalized form. `\` counts as `/`; empty and `.` components are
/// dropped and `..` removes the component before it.
///
/// Returns `None` for a path that must not be looked up at all: one that climbs above the
/// root with `..`, or one with `:` or NUL in it (drive letters, alternate data streams).
/// Game paths come from Lua code and from servers; this keeps them inside their mounts.
pub fn normalize(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p if p.contains([':', '\0']) => return None,
            p => parts.push(p),
        }
    }
    Some(parts.join("/").to_lowercase())
}

/// Whether `path` is already in normalized form.
pub fn is_normalized(path: &str) -> bool {
    normalize(path).is_some_and(|n| n == path)
}

/// Splits a normalized path into its folder (`""` for the root) and its last component.
pub fn split(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(slash) => (&path[..slash], &path[slash + 1..]),
        None => ("", path),
    }
}

/// Joins a normalized folder (`""` for the root) and a name.
pub fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") }
}

/// The extension of the last component, without the dot (`""` if there is none).
pub fn extension(path: &str) -> &str {
    let name = split(path).1;
    match name.rfind('.') {
        Some(dot) if dot > 0 => &name[dot + 1..],
        _ => "",
    }
}

/// Whether `name` matches `pattern`, where `*` stands for any run of characters and `?` for
/// exactly one. Both should be normalized (lower case). `*.*` matches every name, as it does
/// in the Windows file APIs the engine's `FindFirst` grew up with.
pub fn wildcard_match(pattern: &str, name: &str) -> bool {
    if pattern == "*.*" || pattern == "*" {
        return true;
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut p, mut n) = (0, 0);
    // position of the last `*` and of the name character it is currently taken to end at
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, n));
            p += 1;
        } else if let Some((star_p, star_n)) = star {
            p = star_p + 1;
            n = star_n + 1;
            star = Some((star_p, star_n + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizing() {
        assert_eq!(normalize("Materials\\Models/Props_C17//chair.VMT").as_deref(), Some("materials/models/props_c17/chair.vmt"));
        assert_eq!(normalize("/lua/./autorun/").as_deref(), Some("lua/autorun"));
        assert_eq!(normalize("lua/includes/../autorun/x.lua").as_deref(), Some("lua/autorun/x.lua"));
        assert_eq!(normalize("").as_deref(), Some(""));
        assert_eq!(normalize("../cfg/server.cfg"), None);
        assert_eq!(normalize("data/../../gameinfo.txt"), None);
        assert_eq!(normalize("C:/Windows/system.ini"), None);
        assert_eq!(normalize("data/a.txt:stream"), None);
        assert_eq!(normalize("ДАННЫЕ/Файл.txt").as_deref(), Some("данные/файл.txt"));
        assert!(is_normalized("lua/autorun/x.lua"));
        assert!(!is_normalized("Lua/x.lua"));
        assert!(!is_normalized("/lua"));
    }

    #[test]
    fn splitting() {
        assert_eq!(split("a/b/c.txt"), ("a/b", "c.txt"));
        assert_eq!(split("c.txt"), ("", "c.txt"));
        assert_eq!(join("", "x"), "x");
        assert_eq!(join("a/b", "x"), "a/b/x");
        assert_eq!(extension("maps/gm_construct.bsp"), "bsp");
        assert_eq!(extension("a.b/readme"), "");
        assert_eq!(extension(".hidden"), "");
    }

    #[test]
    fn wildcards() {
        assert!(wildcard_match("*.lua", "init.lua"));
        assert!(!wildcard_match("*.lua", "init.luac"));
        assert!(wildcard_match("weapon_*", "weapon_crowbar"));
        assert!(wildcard_match("gm_*.bsp", "gm_construct.bsp"));
        assert!(wildcard_match("?.txt", "a.txt"));
        assert!(!wildcard_match("?.txt", "ab.txt"));
        assert!(wildcard_match("*a*b*", "xxaxxbxx"));
        assert!(!wildcard_match("*a*b*", "xxbxxaxx"));
        assert!(wildcard_match("*.*", "readme"));
        assert!(wildcard_match("exact", "exact"));
        assert!(!wildcard_match("exact", "exactly"));
        assert!(wildcard_match("", ""));
        assert!(!wildcard_match("", "a"));
    }
}
