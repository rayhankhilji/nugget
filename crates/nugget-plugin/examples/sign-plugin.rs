//! Sign a plugin package directory (`nugget-plugin.toml` + `plugin.wasm`), writing `plugin.sig`,
//! and optionally bundle the signed package into one `.nuggetplugin` file.
//!
//! ```text
//! NUGGET_PLUGIN_SIGNING_KEY=<64 hex chars> cargo run -p nugget-plugin --example sign-plugin -- <dir> [--out <file.nuggetplugin>]
//! ```
//!
//! The key is a hex ed25519 seed read from the environment (or from the file named by
//! `NUGGET_PLUGIN_SIGNING_KEY_FILE`); it is never written anywhere. The public key is printed so
//! it can be added to the host's trusted keys.

use std::path::PathBuf;

use nugget_plugin::{
    MANIFEST_FILE, Manifest, PACKAGE_EXTENSION, PluginPackage, SIGNATURE_FILE, WASM_FILE,
    parse_signing_key, sign_package,
};

const USAGE: &str = "usage: sign-plugin <package dir> [--out <file.nuggetplugin>]";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut dir = None;
    let mut out = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" | "-o" => out = Some(PathBuf::from(args.next().ok_or(USAGE)?)),
            _ if dir.is_none() && !arg.starts_with('-') => dir = Some(PathBuf::from(arg)),
            _ => return Err(USAGE.into()),
        }
    }
    let dir = dir.ok_or(USAGE)?;
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
    let signature = sign_package(&manifest, &wasm, &key);
    std::fs::write(dir.join(SIGNATURE_FILE), &signature)?;
    println!(
        "signed {} {} with public key {}",
        parsed.id,
        parsed.version,
        hex::encode(key.verifying_key().as_bytes())
    );
    if let Some(mut out) = out {
        if out.extension().is_none() {
            out.set_extension(PACKAGE_EXTENSION);
        }
        PluginPackage {
            manifest,
            wasm,
            signature: signature.into_bytes(),
        }
        .write_file(&out)?;
        println!("wrote {}", out.display());
    }
    Ok(())
}
