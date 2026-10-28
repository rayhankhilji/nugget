//! Item keys and URLs. Keys are OS-independent: `<root index>:<relative path with / separators>`.

use std::path::{Component, Path, PathBuf};

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

/// The separator convention of a relative path string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathStyle {
    /// Only `/` separates components (a `\` is part of a file name).
    Posix,
    /// Both `\` and `/` separate components.
    Windows,
}

impl PathStyle {
    /// The convention of the OS this was compiled for.
    pub const NATIVE: PathStyle = if cfg!(windows) {
        PathStyle::Windows
    } else {
        PathStyle::Posix
    };
}

/// The item key of a file at `rel` (relative to root number `root_index`).
///
/// Only normal components are kept (`.` and prefixes are dropped) and they are joined with `/`,
/// so the same tree yields the same keys on every OS. Non-UTF-8 names are converted lossily.
pub fn item_key(root_index: usize, rel: &Path) -> String {
    let mut key = format!("{root_index}:");
    key.push_str(&rel_key(rel));
    key
}

/// The `/`-joined form of a relative path (the part of a key after `<root index>:`).
pub fn rel_key(rel: &Path) -> String {
    let mut out = String::new();
    for c in rel.components() {
        if let Component::Normal(s) = c {
            if !out.is_empty() {
                out.push('/');
            }
            out.push_str(&s.to_string_lossy());
        }
    }
    out
}

/// [`item_key`] for a relative path given as a string in a known separator convention. Empty and
/// `.` components are dropped, so `a\\b\\c.md` (Windows) and `a/b/c.md` give the same key.
pub fn item_key_from_str(root_index: usize, rel: &str, style: PathStyle) -> String {
    let parts: Vec<&str> = rel
        .split(|c| c == '/' || (style == PathStyle::Windows && c == '\\'))
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    format!("{root_index}:{}", parts.join("/"))
}

/// Split a key into its root index and relative part.
pub fn parse_key(key: &str) -> Option<(usize, &str)> {
    let (idx, rel) = key.split_once(':')?;
    Some((idx.parse().ok()?, rel))
}

/// Re-attach a key's relative part to its root.
pub(crate) fn join_rel(root: &Path, rel: &str) -> PathBuf {
    rel.split('/')
        .filter(|p| !p.is_empty())
        .fold(root.to_path_buf(), |p, c| p.join(c))
}

/// `file://` URL of an absolute path (`None` for a relative one).
pub fn file_url(path: &Path) -> Option<String> {
    url::Url::from_file_path(path).ok().map(String::from)
}

/// JavaScript's `encodeURIComponent` set, which Obsidian's URI handler decodes.
const URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

/// `obsidian://open?vault=…&file=…` for a note at vault-relative `rel` (with `/` separators).
pub fn obsidian_url(vault: &str, rel: &str) -> String {
    format!(
        "obsidian://open?vault={}&file={}",
        utf8_percent_encode(vault, URI_COMPONENT),
        utf8_percent_encode(rel, URI_COMPONENT)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_use_forward_slashes_on_every_os() {
        let rel = Path::new("a").join("b").join("c.md");
        assert_eq!(item_key(0, &rel), "0:a/b/c.md");
        assert_eq!(item_key(3, Path::new("./x.txt")), "3:x.txt");
        assert_eq!(item_key(1, Path::new("")), "1:");
        // `/` is a separator on Windows too.
        assert_eq!(item_key(0, Path::new("a/b/c.md")), "0:a/b/c.md");
    }

    #[test]
    fn windows_style_separators_give_the_same_key() {
        let posix = item_key_from_str(0, "Docs/Notes/plan v2.md", PathStyle::Posix);
        let windows = item_key_from_str(0, "Docs\\Notes\\plan v2.md", PathStyle::Windows);
        let mixed = item_key_from_str(0, ".\\Docs/Notes\\\\plan v2.md", PathStyle::Windows);
        assert_eq!(posix, "0:Docs/Notes/plan v2.md");
        assert_eq!(windows, posix);
        assert_eq!(mixed, posix);
        // On a POSIX system a backslash is an ordinary file-name character.
        assert_eq!(
            item_key_from_str(0, "odd\\name.txt", PathStyle::Posix),
            "0:odd\\name.txt"
        );
        // The native Path-based key agrees with the string form.
        let native = Path::new("Docs").join("Notes").join("plan v2.md");
        assert_eq!(item_key(0, &native), posix);
    }

    #[cfg(windows)]
    #[test]
    fn native_windows_paths_normalise() {
        assert_eq!(item_key(2, Path::new(r"a\b\c.txt")), "2:a/b/c.txt");
        assert_eq!(item_key(2, Path::new(r"a\b/c.txt")), "2:a/b/c.txt");
    }

    #[test]
    fn parse_and_join_round_trip() {
        assert_eq!(parse_key("12:a/b.md"), Some((12, "a/b.md")));
        assert_eq!(parse_key("0:"), Some((0, "")));
        assert_eq!(parse_key("x:a"), None);
        assert_eq!(parse_key("nocolon"), None);
        let root = std::env::temp_dir();
        assert_eq!(
            join_rel(&root, "a/b.md"),
            root.join("a").join("b.md"),
            "joined with native separators"
        );
    }

    #[test]
    fn obsidian_urls_are_component_encoded() {
        assert_eq!(
            obsidian_url("My Vault", "Folder/Note One.md"),
            "obsidian://open?vault=My%20Vault&file=Folder%2FNote%20One.md"
        );
        assert_eq!(
            obsidian_url("v", "a&b=c#d?.md"),
            "obsidian://open?vault=v&file=a%26b%3Dc%23d%3F.md"
        );
        assert_eq!(
            obsidian_url("Ünï", "é (1).md"),
            "obsidian://open?vault=%C3%9Cn%C3%AF&file=%C3%A9%20(1).md"
        );
    }

    #[test]
    fn file_urls_are_absolute_and_encoded() {
        let dir = std::env::temp_dir().join("a b").join("c#d.md");
        let url = file_url(&dir).unwrap();
        assert!(url.starts_with("file:///"), "{url}");
        assert!(url.ends_with("/a%20b/c%23d.md"), "{url}");
        assert_eq!(file_url(Path::new("relative.md")), None);
    }
}
