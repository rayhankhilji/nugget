//! Build-time manifest key configuration (see `docs/security/manifest-trust.md`):
//! - `NUGGET_MANIFEST_RELEASE_PUBKEY`: hex ed25519 release public key, baked in with `option_env!`.
//! - `NUGGET_TRUST_DEV_MANIFEST_KEY=1`: trust the development key in a release build.
//!
//! Both are validated here so a typo fails the build instead of shipping a binary that trusts no
//! key.

fn main() {
    println!("cargo:rerun-if-env-changed=NUGGET_MANIFEST_RELEASE_PUBKEY");
    println!("cargo:rerun-if-env-changed=NUGGET_TRUST_DEV_MANIFEST_KEY");

    if let Ok(key) = std::env::var("NUGGET_MANIFEST_RELEASE_PUBKEY") {
        let key = key.trim();
        if !key.is_empty() && (key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit())) {
            panic!(
                "NUGGET_MANIFEST_RELEASE_PUBKEY must be a 32-byte ed25519 public key as 64 hex \
                 characters (got {} characters)",
                key.len()
            );
        }
    }
    if let Ok(v) = std::env::var("NUGGET_TRUST_DEV_MANIFEST_KEY") {
        let v = v.trim();
        if !matches!(v, "" | "0" | "1" | "true" | "false" | "yes" | "no") {
            panic!(
                "NUGGET_TRUST_DEV_MANIFEST_KEY must be 1/true/yes or 0/false/no/empty, got `{v}`"
            );
        }
    }
}
