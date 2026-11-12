// Remote paths.
//
// POSIX-shaped and always absolute: a leading slash, no trailing slash except
// at the root, no `.` or `..` left in them. S3 keys are not POSIX paths, but
// presenting them with the same shape is what makes one file browser able to
// show both, so the S3 provider maps between this and its flat keyspace rather
// than the UI learning two path grammars.
//
// `crumbs` returns slices of the input, so drawing a breadcrumb bar every frame
// allocates nothing.

pub const ROOT: &str = "/";

/// Collapse separators and resolve `.` and `..`. Never fails: `..` at the root
/// stays at the root, the way `cd ..` does in `/`.
pub fn normalize(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }

    if segments.is_empty() {
        return ROOT.to_owned();
    }
    let mut out = String::with_capacity(path.len() + 1);
    for segment in segments {
        out.push('/');
        out.push_str(segment);
    }
    out
}

/// Enter `name` from `base`. An absolute `name` replaces `base` entirely.
pub fn join(base: &str, name: &str) -> String {
    if name.starts_with('/') {
        return normalize(name);
    }
    let mut combined = String::with_capacity(base.len() + name.len() + 1);
    combined.push_str(base);
    combined.push('/');
    combined.push_str(name);
    normalize(&combined)
}

/// `None` at the root, which is what disables the "up" button.
pub fn parent(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.rfind('/') {
        Some(0) => Some(ROOT),
        Some(cut) => Some(&trimmed[..cut]),
        None => None,
    }
}

/// The last segment, or `/` at the root.
pub fn file_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(cut) => &trimmed[cut + 1..],
        None if trimmed.is_empty() => ROOT,
        None => trimmed,
    }
}

/// Each segment paired with the absolute path that reaches it, both borrowed
/// from `path`. `/a/b` yields `("a", "/a")` then `("b", "/a/b")`.
pub fn crumbs(path: &str) -> impl Iterator<Item = (&str, &str)> {
    let bytes = path.as_bytes();
    let mut at = 0usize;
    std::iter::from_fn(move || {
        while at < bytes.len() && bytes[at] == b'/' {
            at += 1;
        }
        if at >= bytes.len() {
            return None;
        }
        let end = path[at..]
            .find('/')
            .map_or(path.len(), |offset| at + offset);
        let crumb = (&path[at..end], &path[..end]);
        at = end;
        Some(crumb)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_produces_one_canonical_form() {
        for (input, expected) in [
            ("", "/"),
            ("/", "/"),
            ("//", "/"),
            (".", "/"),
            ("a/b", "/a/b"),
            ("/a/b", "/a/b"),
            ("/a/b/", "/a/b"),
            ("/a//b", "/a/b"),
            ("/a/./b", "/a/b"),
            ("/a/b/..", "/a"),
            ("/a/b/../..", "/"),
            // Escaping past the root is clamped, not an error.
            ("/a/../../..", "/"),
            ("/..", "/"),
        ] {
            assert_eq!(normalize(input), expected, "normalize({input:?})");
        }
    }

    #[test]
    fn normalize_leaves_unusual_but_legal_names_alone() {
        assert_eq!(normalize("/a/...b"), "/a/...b");
        assert_eq!(normalize("/a/.hidden"), "/a/.hidden");
        assert_eq!(normalize("/a/b c"), "/a/b c");
        assert_eq!(normalize("/日本語/ファイル"), "/日本語/ファイル");
    }

    #[test]
    fn join_enters_and_escapes_correctly() {
        assert_eq!(join("/a", "b"), "/a/b");
        assert_eq!(join("/", "a"), "/a");
        assert_eq!(join("/a/b", ".."), "/a");
        assert_eq!(join("/a", "/elsewhere"), "/elsewhere");
        assert_eq!(join("/a", "b/c"), "/a/b/c");
        assert_eq!(join("/a", ""), "/a");
    }

    #[test]
    fn parent_stops_at_the_root() {
        assert_eq!(parent("/a/b/c"), Some("/a/b"));
        assert_eq!(parent("/a/b"), Some("/a"));
        assert_eq!(parent("/a"), Some("/"));
        assert_eq!(parent("/"), None);
        assert_eq!(parent(""), None);
        // A trailing slash must not change the answer.
        assert_eq!(parent("/a/b/"), Some("/a"));
    }

    #[test]
    fn file_name_returns_the_last_segment() {
        assert_eq!(file_name("/a/b/c"), "c");
        assert_eq!(file_name("/a"), "a");
        assert_eq!(file_name("/a/b/"), "b");
        assert_eq!(file_name("/"), "/");
    }

    #[test]
    fn crumbs_pair_each_segment_with_its_absolute_path() {
        let path = "/one/two/three";
        let crumbs: Vec<_> = crumbs(path).collect();
        assert_eq!(
            crumbs,
            [
                ("one", "/one"),
                ("two", "/one/two"),
                ("three", "/one/two/three")
            ]
        );
    }

    #[test]
    fn crumbs_of_the_root_are_empty() {
        assert_eq!(crumbs("/").count(), 0);
        assert_eq!(crumbs("").count(), 0);
    }

    #[test]
    fn crumbs_borrow_rather_than_allocate() {
        let path = String::from("/alpha/beta");
        for (name, full) in crumbs(&path) {
            // Both must point into `path` itself.
            let base = path.as_ptr() as usize;
            let within = |slice: &str| {
                let start = slice.as_ptr() as usize;
                start >= base && start + slice.len() <= base + path.len()
            };
            assert!(within(name) && within(full));
        }
    }

    #[test]
    fn crumbs_survive_repeated_separators() {
        let crumbs: Vec<_> = crumbs("//a//b").map(|(name, _)| name).collect();
        assert_eq!(crumbs, ["a", "b"]);
    }

    #[test]
    fn every_normalized_path_round_trips_through_parent_and_join() {
        for path in ["/a", "/a/b", "/a/b/c"] {
            let parent = parent(path).unwrap();
            assert_eq!(join(parent, file_name(path)), path);
        }
    }
}
