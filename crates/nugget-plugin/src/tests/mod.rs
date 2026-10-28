//! Host tests. `manifest`, `package` and `store` need nothing extra; `guest` builds the SDK's
//! example and test plugins for `wasm32-wasip2` (see [`guest_wasm`]).

mod guest;
mod manifest;
mod package;
mod store;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use ed25519_dalek::SigningKey;
use nugget_connector::{Change, ChangeSink, KnownItems, SecretStore};
use nugget_core::Result;
use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::ids::ItemKey;
use serde_json::Value;

use crate::{PluginPackage, PluginStore};

pub(crate) fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// A manifest for `id` with the given capability lines.
pub(crate) fn manifest_toml(id: &str, version: &str, kind: &str, caps: &str) -> String {
    let connector = if kind.contains("connector") {
        "\n[connector]\ndisplay_name = \"Test\"\n"
    } else {
        ""
    };
    format!(
        "id = \"{id}\"\nname = \"Test plugin\"\nversion = \"{version}\"\ndescription = \"test\"\nkind = {kind}\n\n[capabilities]\n{caps}\n{connector}"
    )
}

/// The smallest valid component (no imports, no exports).
pub(crate) const EMPTY_COMPONENT: &[u8] = b"\0asm\x0d\x00\x01\x00";

pub(crate) fn package(manifest: &str, wasm: &[u8], key: &SigningKey) -> PluginPackage {
    PluginPackage::sign(manifest.as_bytes().to_vec(), wasm.to_vec(), key)
}

pub(crate) fn temp_store() -> (tempfile::TempDir, PluginStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = PluginStore::open_at(dir.path().join("plugins")).unwrap();
    (dir, store)
}

/// Build the guest workspace in `sdk/` once per test process and return the path of
/// `<crate>.wasm`. Returns `None` (and the guest tests skip with a warning) when the
/// `wasm32-wasip2` target is not installed, unless `NUGGET_PLUGIN_REQUIRE_GUESTS=1`.
pub(crate) fn guest_wasm(crate_name: &str) -> Option<Vec<u8>> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    let dir = DIR.get_or_init(build_guests).as_ref()?;
    Some(std::fs::read(dir.join(format!("{crate_name}.wasm"))).unwrap())
}

fn build_guests() -> Option<PathBuf> {
    let require = std::env::var("NUGGET_PLUGIN_REQUIRE_GUESTS").is_ok_and(|v| v == "1");
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let libdir = std::process::Command::new(&rustc)
        .args(["--print", "target-libdir", "--target", "wasm32-wasip2"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()));
    if !libdir.is_some_and(|d| d.join("libstd.rlib").exists() || has_std(&d)) {
        let msg = "wasm32-wasip2 is not installed (`rustup target add wasm32-wasip2`); \
                   skipping the plugin guest tests";
        assert!(!require, "{msg}");
        eprintln!("WARNING: {msg}");
        return None;
    }
    let sdk = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("sdk");
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            // .../target/debug/deps/<test binary>
            std::env::current_exe()
                .ok()?
                .ancestors()
                .nth(3)
                .map(PathBuf::from)
        })
        .unwrap()
        .join("plugin-guests");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = std::process::Command::new(cargo)
        .current_dir(&sdk)
        .args([
            "build",
            "--locked",
            "--release",
            "--workspace",
            "--target",
            "wasm32-wasip2",
        ])
        .env("CARGO_TARGET_DIR", &target_dir)
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .status()
        .expect("run cargo for the plugin guests");
    assert!(status.success(), "building the plugin guests failed");
    Some(target_dir.join("wasm32-wasip2").join("release"))
}

fn has_std(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|rd| {
        rd.flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("libstd-"))
    })
}

// ---- fakes ----

#[derive(Default)]
pub(crate) struct VecEgress(pub Mutex<Vec<EgressEvent>>);

impl EgressSink for VecEgress {
    fn record(&self, e: EgressEvent) {
        self.0.lock().unwrap().push(e);
    }
}

#[derive(Default)]
pub(crate) struct MemSecrets(pub Mutex<HashMap<String, String>>);

impl SecretStore for MemSecrets {
    fn get(&self, account: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(account).cloned())
    }
    fn set(&self, account: &str, value: &str) -> Result<()> {
        self.0.lock().unwrap().insert(account.into(), value.into());
        Ok(())
    }
    fn delete(&self, account: &str) -> Result<()> {
        self.0.lock().unwrap().remove(account);
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct FakeSink {
    pub changes: Mutex<Vec<Change>>,
    pub checkpoints: Mutex<Vec<Value>>,
}

#[async_trait]
impl ChangeSink for FakeSink {
    async fn send(&self, change: Change) -> Result<()> {
        self.changes.lock().unwrap().push(change);
        Ok(())
    }
    async fn checkpoint(&self, cursor: Value) -> Result<()> {
        self.checkpoints.lock().unwrap().push(cursor);
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct FakeKnown(pub Mutex<HashMap<ItemKey, Option<String>>>);

#[async_trait]
impl KnownItems for FakeKnown {
    async fn versions(&self) -> Result<HashMap<ItemKey, Option<String>>> {
        Ok(self.0.lock().unwrap().clone())
    }
}

pub(crate) fn _assert_send_sync() {
    fn is<T: Send + Sync>() {}
    is::<Arc<crate::LoadedPlugin>>();
    is::<crate::PluginHost>();
}
