//! Dev tool: sign (or verify) a model manifest with a detached ed25519 signature.
//!
//! ```text
//! # create a new key file (hex seed, 0600 on Unix); keep it OUT of the repository
//! cargo run -p nugget-models --example sign-manifest -- --generate-key ~/.config/nugget/manifest-dev.key
//!
//! # sign: writes <manifest>.sig (hex) after validating the manifest
//! NUGGET_MANIFEST_SIGNING_KEY_FILE=~/.config/nugget/manifest-dev.key \
//!   cargo run -p nugget-models --example sign-manifest -- manifests/default.json
//!
//! # verify against the keys embedded in this build (or --pubkey HEX)
//! cargo run -p nugget-models --example sign-manifest -- --verify manifests/default.json
//! ```
//!
//! The signing key is taken from, in order: `--key-file PATH`, `NUGGET_MANIFEST_SIGNING_KEY`
//! (64 hex chars), `NUGGET_MANIFEST_SIGNING_KEY_FILE`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use nugget_models::{
    SigningKey, parse_signing_key, parse_verifying_key, sign_manifest, trusted_keys,
    verify_manifest,
};

const USAGE: &str = "usage:
  sign-manifest [--key-file PATH] [--out SIG] MANIFEST
  sign-manifest --verify MANIFEST [--sig SIG] [--pubkey HEX]
  sign-manifest --generate-key PATH";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    let mut key_file = None;
    let mut out = None;
    let mut sig = None;
    let mut pubkey = None;
    let mut verify = false;
    let mut generate = None;
    let mut manifest = None;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().ok_or(format!("{name} needs a value\n{USAGE}"));
        match a.as_str() {
            "--key-file" => key_file = Some(PathBuf::from(val("--key-file")?)),
            "--out" => out = Some(PathBuf::from(val("--out")?)),
            "--sig" => sig = Some(PathBuf::from(val("--sig")?)),
            "--pubkey" => pubkey = Some(val("--pubkey")?),
            "--generate-key" => generate = Some(PathBuf::from(val("--generate-key")?)),
            "--verify" => verify = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            s if s.starts_with('-') => return Err(format!("unknown flag {s}\n{USAGE}")),
            _ => manifest = Some(PathBuf::from(a)),
        }
    }

    if let Some(path) = generate {
        return generate_key(&path);
    }
    let manifest = manifest.ok_or(USAGE)?;
    let bytes = std::fs::read(&manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let sig_path = sig.or(out).unwrap_or_else(|| sig_path_for(&manifest));

    if verify {
        let sig = std::fs::read(&sig_path).map_err(|e| format!("{}: {e}", sig_path.display()))?;
        let keys = match pubkey {
            Some(hex) => vec![parse_verifying_key(&hex).map_err(|e| e.to_string())?],
            None => trusted_keys().map_err(|e| e.to_string())?,
        };
        let m = verify_manifest(&bytes, &sig, &keys).map_err(|e| e.to_string())?;
        println!(
            "OK: manifest version {} with {} models",
            m.version,
            m.models.len()
        );
        return Ok(());
    }

    let key = load_key(key_file.as_deref())?;
    let vk = key.verifying_key();
    let sig_hex = sign_manifest(&bytes, &key);
    // Refuse to sign something clients would reject anyway.
    let m = verify_manifest(&bytes, sig_hex.as_bytes(), &[vk])
        .map_err(|e| format!("manifest rejected, not signing: {e}"))?;
    std::fs::write(&sig_path, format!("{sig_hex}\n"))
        .map_err(|e| format!("{}: {e}", sig_path.display()))?;
    let pk = hex::encode(vk.to_bytes());
    println!(
        "signed {} (version {}, {} models) -> {}",
        manifest.display(),
        m.version,
        m.models.len(),
        sig_path.display()
    );
    println!("public key: {pk}");
    if pk != nugget_models::DEV_PUBLIC_KEY_HEX {
        println!("note: this key is NOT the dev key embedded in this build");
    }
    Ok(())
}

fn sig_path_for(manifest: &Path) -> PathBuf {
    let mut s = manifest.as_os_str().to_owned();
    s.push(".sig");
    PathBuf::from(s)
}

fn load_key(key_file: Option<&Path>) -> Result<SigningKey, String> {
    let from_file = |p: &Path| {
        std::fs::read_to_string(p)
            .map_err(|e| format!("{}: {e}", p.display()))
            .and_then(|s| parse_signing_key(&s).map_err(|e| e.to_string()))
    };
    if let Some(p) = key_file {
        return from_file(p);
    }
    if let Ok(hex) = std::env::var("NUGGET_MANIFEST_SIGNING_KEY") {
        return parse_signing_key(&hex).map_err(|e| e.to_string());
    }
    if let Some(p) = std::env::var_os("NUGGET_MANIFEST_SIGNING_KEY_FILE") {
        return from_file(Path::new(&p));
    }
    Err(format!(
        "no signing key: pass --key-file, or set NUGGET_MANIFEST_SIGNING_KEY or NUGGET_MANIFEST_SIGNING_KEY_FILE\n{USAGE}"
    ))
}

fn generate_key(path: &Path) -> Result<(), String> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| format!("no OS randomness: {e}"))?;
    let key = SigningKey::from_bytes(&seed);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).map_err(|e| {
        format!(
            "{}: {e} (refusing to overwrite an existing key)",
            path.display()
        )
    })?;
    std::io::Write::write_all(&mut f, format!("{}\n", hex::encode(seed)).as_bytes())
        .map_err(|e| e.to_string())?;
    seed.fill(0);
    println!("wrote private key to {} (never commit it)", path.display());
    println!(
        "public key: {}",
        hex::encode(key.verifying_key().to_bytes())
    );
    Ok(())
}
