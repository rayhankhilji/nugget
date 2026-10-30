//! `nugget:plugin/fs` path resolution, without guests: the syntax check, containment (including
//! symlink escapes on Unix) and root checks.

use std::path::Path;

use nugget_core::Error;

use crate::fs::{canonical_root, check_relative, is_valid_segment, resolve};

#[test]
fn relative_paths_are_split_into_segments() {
    assert!(check_relative("").unwrap().is_empty());
    assert_eq!(check_relative("a").unwrap(), ["a"]);
    assert_eq!(
        check_relative("notes/2026/.hidden.md").unwrap(),
        ["notes", "2026", ".hidden.md"]
    );
    assert_eq!(check_relative("a b/ü.txt").unwrap(), ["a b", "ü.txt"]);
    assert!(is_valid_segment(".hidden"));
    assert!(!is_valid_segment("a/b"));
}

#[test]
fn bad_relative_paths_are_refused() {
    let long_segment = "a".repeat(256);
    let long_path = vec!["a"; 2100].join("/");
    let many = vec!["a"; 129].join("/");
    for bad in [
        "/etc/passwd",
        "/",
        "..",
        "../x",
        "a/../../x",
        "a/..",
        ".",
        "./a",
        "a/./b",
        "a//b",
        "a/",
        "a\\b",
        "..\\x",
        "C:",
        "C:/Windows",
        "C:\\Windows",
        "c:x",
        "\\\\server\\share",
        "//server/share",
        "\\\\?\\C:\\x",
        "a\0b",
        "a\nb",
        "file.txt:stream",
        "con",
        "NUL.txt",
        "a/Com1.log",
        "lpt9",
        "trailing.",
        "trailing ",
        long_segment.as_str(),
        long_path.as_str(),
        many.as_str(),
    ] {
        match check_relative(bad) {
            Err(Error::Invalid(_)) => {}
            other => panic!("{bad:?}: expected Invalid, got {other:?}"),
        }
        assert!(
            bad.contains('/') || !is_valid_segment(bad),
            "{bad:?} must not be a valid segment"
        );
    }
    // Lookalikes that are fine.
    for ok in [
        "..a",
        "a..b",
        ".a",
        "console.log",
        "nul_x",
        "com10",
        "a.b.c",
    ] {
        check_relative(ok).unwrap();
    }
}

#[test]
fn resolve_stays_inside_the_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("vault");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub").join("note.md"), "hi").unwrap();
    std::fs::write(dir.path().join("outside.txt"), "secret").unwrap();

    let canon_root = canonical_root(root.to_str().unwrap()).unwrap();
    assert_eq!(resolve(&canon_root, "").unwrap(), canon_root);
    let note = resolve(&canon_root, "sub/note.md").unwrap();
    assert!(note.starts_with(&canon_root));
    assert_eq!(std::fs::read_to_string(&note).unwrap(), "hi");
    // A non-canonical root works the same (it is canonicalized).
    assert_eq!(resolve(&root, "sub/note.md").unwrap(), note);

    assert!(matches!(
        resolve(&canon_root, "missing.md"),
        Err(Error::NotFound(_))
    ));
    // Joining below a file is "not found", not an escape.
    assert!(matches!(
        resolve(&canon_root, "sub/note.md/x"),
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        resolve(&canon_root, "../outside.txt"),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        resolve(Path::new("relative/root"), ""),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn a_single_file_can_be_a_root() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("export.json");
    std::fs::write(&file, "{}").unwrap();
    let root = canonical_root(file.to_str().unwrap()).unwrap();
    assert_eq!(resolve(&root, "").unwrap(), root);
    assert!(matches!(resolve(&root, "x"), Err(Error::NotFound(_))));
}

#[test]
fn root_paths_must_be_absolute_and_exist() {
    for bad in ["", "relative/dir", "./x", "a\0b", "~/notes"] {
        assert!(
            matches!(canonical_root(bad), Err(Error::Invalid(_))),
            "{bad:?}"
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope");
    assert!(matches!(
        canonical_root(missing.to_str().unwrap()),
        Err(Error::NotFound(_))
    ));
}

#[cfg(unix)]
#[test]
fn symlinks_out_of_the_root_are_refused() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("vault");
    std::fs::create_dir_all(root.join("inner")).unwrap();
    std::fs::write(root.join("inner").join("ok.md"), "ok").unwrap();
    std::fs::create_dir_all(dir.path().join("private")).unwrap();
    std::fs::write(dir.path().join("private").join("key"), "secret").unwrap();
    // Escapes: to a file outside, a folder outside, `/`, and a sibling whose name shares the
    // root's prefix (`vault-evil` starts with `vault` as a string, not as a path).
    std::fs::create_dir_all(dir.path().join("vault-evil")).unwrap();
    std::fs::write(dir.path().join("vault-evil").join("x"), "x").unwrap();
    symlink(dir.path().join("private").join("key"), root.join("key")).unwrap();
    symlink(dir.path().join("private"), root.join("priv")).unwrap();
    symlink("/", root.join("slash")).unwrap();
    symlink(dir.path().join("vault-evil"), root.join("evil")).unwrap();
    symlink("../private/key", root.join("rel")).unwrap();
    // Inside: a link to a file and to a folder in the root.
    symlink(root.join("inner").join("ok.md"), root.join("alias.md")).unwrap();
    symlink("inner", root.join("inner-link")).unwrap();

    let root = canonical_root(root.to_str().unwrap()).unwrap();
    for escape in [
        "key",
        "priv",
        "priv/key",
        "slash",
        "slash/etc",
        "evil/x",
        "rel",
    ] {
        match resolve(&root, escape) {
            Err(Error::Unauthorized(_)) | Err(Error::NotFound(_)) => {}
            other => panic!("{escape}: expected a refusal, got {other:?}"),
        }
    }
    // Every escape that exists is an explicit refusal.
    for escape in ["key", "priv", "priv/key", "slash", "evil/x", "rel"] {
        assert!(
            matches!(resolve(&root, escape), Err(Error::Unauthorized(_))),
            "{escape}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(resolve(&root, "alias.md").unwrap()).unwrap(),
        "ok"
    );
    assert_eq!(
        std::fs::read_to_string(resolve(&root, "inner-link/ok.md").unwrap()).unwrap(),
        "ok"
    );
    // A root that is itself a symlink resolves to its target, and containment is checked
    // against the target.
    let link_root = dir.path().join("link-root");
    symlink(&root, &link_root).unwrap();
    let canon = canonical_root(link_root.to_str().unwrap()).unwrap();
    assert_eq!(canon, root);
    assert!(matches!(
        resolve(&link_root, "key"),
        Err(Error::Unauthorized(_))
    ));
}

#[cfg(windows)]
#[test]
fn windows_verbatim_roots_compare_consistently() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    let root = canonical_root(dir.path().to_str().unwrap()).unwrap();
    // `canonicalize` yields `\\?\` paths on Windows; both sides of the check use it.
    assert!(root.to_string_lossy().starts_with(r"\\?\"));
    let p = resolve(&root, "a.txt").unwrap();
    assert!(p.starts_with(&root));
    assert!(matches!(resolve(&root, "C:"), Err(Error::Invalid(_))));
}
