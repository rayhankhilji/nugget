//! Filesystem helpers.

use std::path::Path;

/// Create `dir` (and missing parents) readable only by the current user on Unix (0700).
/// Existing directories are left as they are; Windows relies on the per-user profile ACLs.
pub(crate) fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn creates_private_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a/b");
        create_private_dir(&dir).unwrap();
        for d in [tmp.path().join("a"), dir] {
            let mode = std::fs::metadata(&d).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "{}", d.display());
        }
    }
}
