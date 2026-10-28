//! Sign a plugin package directory (`nugget-plugin.toml` + `plugin.wasm`), writing `plugin.sig`.
//!
//! ```text
//! NUGGET_PLUGIN_SIGNING_KEY=<64 hex chars> cargo run -p nugget-plugin --example sign-plugin -- <dir>
//! ```
//!
//! The key is a hex ed25519 seed read from the environment (or from the file named by
//! `NUGGET_PLUGIN_SIGNING_KEY_FILE`); it is never written anywhere. The public key is printed so
//! it can be added to the host's trusted keys.

use std::path::PathBuf;

use nugget_plugin::{
    MANIFEST_FILE, Manifest, SIGNATURE_FILE, WASM_FILE, parse_signing_key, sign_package,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("usage: sign-plugin <package dir>")?,
    );
    let seed = match std::env::var("NUGGET_PLUGIN_SIGNING_KEY") {
        Ok(k) => k,
        Err(_) => std::fs::read_to_string(
            std::env::var("NUGGET_PLUGIN_SIGNING_KEY_FILE")
                .map_err(|_| "set NUGGET_PLUGIN_SIGNING_KEY or NUGGET_PLUGIN_SIGNING_KEY_FILE")?,
        )?,
    };
    let key = parse_signing_key(&seed)?;
    let manifest = std::fs::read(dir.join(MANIFEST_FILE))?;
    let wasm = std::fs::read(dir.join(WASM_FILE))?;
    let parsed = Manifest::parse(&manifest)?;
    std::fs::write(
        dir.join(SIGNATURE_FILE),
        sign_package(&manifest, &wasm, &key),
    )?;
    println!(
        "signed {} {} with public key {}",
        parsed.id,
        parsed.version,
        hex::encode(key.verifying_key().as_bytes())
    );
    Ok(())
}
