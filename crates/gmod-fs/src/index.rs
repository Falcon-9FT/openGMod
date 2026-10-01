//! Lookups in an archive's entries kept sorted by normalized path: a file by binary search,
//! a folder's contents as the run of paths that start with it.

/// The entry at `path`.
pub(crate) fn find<'a, T>(items: &'a [T], key: impl Fn(&T) -> &str, path: &str) -> Option<&'a T> {
    items.binary_search_by(|item| key(item).cmp(path)).ok().map(|i| &items[i])
}

/// Whether some entry lies inside the folder `dir` (`""`, the root, if there are any).
pub(crate) fn has_dir<T>(items: &[T], key: impl Fn(&T) -> &str, dir: &str) -> bool {
    if dir.is_empty() {
        return !items.is_empty();
    }
    let prefix = format!("{dir}/");
    let start = items.partition_point(|item| key(item) < prefix.as_str());
    items.get(start).is_some_and(|item| key(item).starts_with(&prefix))
}

/// The names directly inside `dir`, each with whether it is a folder, in the order of the
/// paths (a file `a.txt` comes before a folder `a`).
pub(crate) fn children<T>(items: &[T], key: impl Fn(&T) -> &str, dir: &str) -> Vec<(String, bool)> {
    let prefix = if dir.is_empty() { String::new() } else { format!("{dir}/") };
    let start = items.partition_point(|item| key(item) < prefix.as_str());
    let mut out: Vec<(String, bool)> = Vec::new();
    for item in &items[start..] {
        let Some(rest) = key(item).strip_prefix(prefix.as_str()) else { break };
        match rest.split_once('/') {
            None => out.push((rest.to_string(), false)),
            // all paths below one subfolder are adjacent, so one comparison removes repeats
            Some((sub, _)) => {
                if out.last().is_none_or(|(name, is_dir)| !*is_dir || name != sub) {
                    out.push((sub.to_string(), true));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_from_sorted_paths() {
        let mut paths = vec!["a/b.txt", "a/b/x", "a/b/y", "a/c", "a.txt", "b/z", "a/b0"];
        paths.sort();
        fn key<'a>(p: &'a &str) -> &'a str {
            p
        }
        assert_eq!(
            children(&paths, key, "a"),
            [("b.txt".to_string(), false), ("b".to_string(), true), ("b0".to_string(), false), ("c".to_string(), false)]
        );
        assert_eq!(children(&paths, key, ""), [("a.txt".to_string(), false), ("a".to_string(), true), ("b".to_string(), true)]);
        assert!(children(&paths, key, "q").is_empty());
        assert!(has_dir(&paths, key, "a/b"));
        assert!(!has_dir(&paths, key, "a/c"));
        assert!(!has_dir(&paths, key, "a/"));
        assert_eq!(find(&paths, key, "a/b/y"), Some(&"a/b/y"));
        assert_eq!(find(&paths, key, "a/b"), None);
    }
}
