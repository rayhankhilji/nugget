//! Plugin file-root paths: `nugget_plugin::fs::check_relative` (the `/`-separated path a plugin
//! passes to `nugget:plugin/fs`) and `nugget_plugin::fs::resolve` (that path under a root the user
//! chose).
//!
//! The input is the relative path, as (lossy) UTF-8. Invariants:
//! - an accepted path has no NUL, `\`, `:` or other control character, is not absolute and has
//!   no `..` segment; its parts, none of them empty, `.` or `..`, joined with `/` give the input
//!   back, and only `""` (the root itself) has no parts;
//! - `resolve` refuses whatever `check_relative` refuses, and anything it returns lies under the
//!   canonical root, also after following symlinks. The root is a scratch directory created once
//!   per process with a few files, a folder, and (on unix) a symlink inside the root and one that
//!   leaves it;
//! - under a root that is a single file, only `""` resolves (to the file).
#![no_main]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use nugget_plugin::fs::{check_relative, resolve};

fn root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let base =
            std::env::temp_dir().join(format!("nugget-fuzz-plugin-fs-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("root");
        std::fs::create_dir_all(root.join("sub").join("deeper")).expect("create the scratch root");
        std::fs::write(root.join("a.txt"), b"a").expect("write a.txt");
        std::fs::write(root.join(".hidden"), b"h").expect("write .hidden");
        std::fs::write(root.join("sub").join("b.txt"), b"b").expect("write sub/b.txt");
        // A sibling of the root that a `..` or a symlink could reach.
        std::fs::create_dir_all(base.join("outside")).expect("create outside");
        std::fs::write(base.join("outside").join("secret.txt"), b"s").expect("write secret");
        #[cfg(unix)]
        {
            let _ = std::os::unix::fs::symlink(root.join("sub"), root.join("inner"));
            let _ = std::os::unix::fs::symlink(base.join("outside"), root.join("escape"));
            let _ = std::os::unix::fs::symlink("/", root.join("slash"));
        }
        root.canonicalize().expect("canonical scratch root")
    })
}

fuzz_target!(|data: &[u8]| {
    let rel = String::from_utf8_lossy(data);
    let rel: &str = &rel;
    let root = root();

    let checked = check_relative(rel);
    if let Ok(parts) = &checked {
        assert!(!rel.contains('\0'), "accepted a NUL: {rel:?}");
        assert!(!rel.contains('\\'), "accepted a backslash: {rel:?}");
        assert!(!rel.starts_with('/'), "accepted an absolute path: {rel:?}");
        assert!(
            !Path::new(rel).is_absolute(),
            "accepted an absolute path: {rel:?}"
        );
        assert!(!rel.split('/').any(|s| s == ".."), "accepted `..`: {rel:?}");
        assert!(
            !rel.contains(':') && !rel.chars().any(char::is_control),
            "accepted `:` or a control character: {rel:?}"
        );
        assert_eq!(parts.join("/"), rel, "parts do not rebuild the path");
        assert_eq!(
            parts.is_empty(),
            rel.is_empty(),
            "only `\"\"` names the root itself"
        );
        let mut segments = rel.split('/');
        for part in parts {
            assert!(
                !part.is_empty() && *part != "." && *part != "..",
                "bad part {part:?} from {rel:?}"
            );
            assert!(
                !part.contains(['/', '\\', '\0']),
                "part {part:?} from {rel:?} has a separator or NUL"
            );
            assert!(
                segments.any(|s| s == *part),
                "part {part:?} is not a segment of {rel:?} (in order)"
            );
        }
    }

    if let Ok(path) = resolve(root, rel) {
        assert!(
            checked.is_ok(),
            "resolve accepted {rel:?}, check_relative refused it"
        );
        assert!(
            path.starts_with(root),
            "{rel:?} resolved to {} outside {}",
            path.display(),
            root.display()
        );
        if let Ok(real) = path.canonicalize() {
            assert!(
                real.starts_with(root),
                "{rel:?} resolved to {} which leads to {} outside the root",
                path.display(),
                real.display()
            );
        }
    }

    // A root that is a single file: only `""` resolves, to the file itself.
    let file_root = root.join("a.txt");
    if let Ok(path) = resolve(&file_root, rel) {
        assert!(
            rel.is_empty(),
            "{rel:?} resolved below a file root to {}",
            path.display()
        );
        assert_eq!(path, file_root);
    }
});
