//! PM review PoC (temporary).

use std::sync::Arc;

use super::*;
use crate::{HostConfig, Limits, PluginHost};

fn component(bombs: usize, pages: u32) -> Vec<u8> {
    let bomb_instances: String = (0..bombs)
        .map(|_| "(core instance (instantiate $Bomb))\n".to_string())
        .collect();
    let bytes = pages as u64 * 65536;
    let wat = format!(
        r#"(component
  (core module $M
    (memory (export "mem") 1)
    (func (export "realloc") (param i32 i32 i32 i32) (result i32) i32.const 1024)
    (func (export "validate") (param i32 i32) (result i32) i32.const 2048)
    (func (export "sync") (param i32 i32 i32 i32 i32) (result i32) i32.const 2048)
    (func (export "list") (result i32) i32.const 2048)
    (func (export "call") (param i32 i32 i32 i32) (result i32) i32.const 2048))
  (core instance $m (instantiate $M))
  (core module $Bomb
    (memory {pages})
    (func $s i32.const 0 i32.const 1 i32.const {bytes} memory.fill)
    (start $s))
  {bomb_instances}
  (type $err (variant (case "invalid" string) (case "unauthorized" string) (case "network" string)
    (case "rate-limited" (option u64)) (case "not-found" string) (case "denied" string)
    (case "cancelled") (case "other" string)))
  (type $spec (record (field "name" string) (field "description" string) (field "input-schema" string)))
  (func $validate (param "settings" string) (result (result (error $err)))
    (canon lift (core func $m "validate") (memory (core memory $m "mem")) (realloc (core func $m "realloc"))))
  (func $sync (param "settings" string) (param "cursor" (option string)) (result (result string (error $err)))
    (canon lift (core func $m "sync") (memory (core memory $m "mem")) (realloc (core func $m "realloc"))))
  (func $list (result (list $spec))
    (canon lift (core func $m "list") (memory (core memory $m "mem")) (realloc (core func $m "realloc"))))
  (func $call (param "name" string) (param "input" string) (result (result string (error $err)))
    (canon lift (core func $m "call") (memory (core memory $m "mem")) (realloc (core func $m "realloc"))))
  (instance $conn (export "plugin-error" (type $err)) (export "validate" (func $validate)) (export "sync" (func $sync)))
  (export "nugget:plugin/connector@0.1.0" (instance $conn))
  (instance $tools (export "plugin-error" (type $err)) (export "tool-spec" (type $spec)) (export "list-tools" (func $list)) (export "call" (func $call)))
  (export "nugget:plugin/tools@0.1.0" (instance $tools))
)"#
    );
    wat::parse_str(&wat).unwrap()
}

#[tokio::test]
async fn pm_poc_memory_cap_is_per_memory_not_per_instance() {
    // Cap: 32 MiB. The component instantiates 15 extra core instances, each with a 32 MiB memory
    // that its start function fills (committing it), i.e. ~480 MiB per call.
    let wasm = component(15, 512);
    let (_d, store) = temp_store();
    let k = key(3);
    store
        .install(
            &package(
                &manifest_toml("dev.nugget.bomb", "1.0.0", "[\"tool\"]", ""),
                &wasm,
                &k,
            ),
            &[k.verifying_key()],
            &mut |_| true,
        )
        .unwrap();
    let host = PluginHost::new(HostConfig {
        limits: Limits {
            memory_bytes: 32 * 1024 * 1024,
            ..Limits::default()
        },
        state_dir: store.state_root(),
        egress: Arc::new(VecEgress::default()),
        secrets: None,
        http_client: None,
    })
    .unwrap();
    let plugin = host.load(&store.load("dev.nugget.bomb").unwrap()).unwrap();
    let r = plugin.tools().await;
    // Expected (if the cap were per plugin): an error mentioning the memory limit.
    assert!(r.is_ok(), "{r:?}");
}

#[tokio::test]
async fn pm_poc_control_single_memory_over_cap_fails() {
    let wasm = component(1, 600);
    let (_d, store) = temp_store();
    let k = key(3);
    store
        .install(
            &package(&manifest_toml("dev.nugget.bomb", "1.0.0", "[\"tool\"]", ""), &wasm, &k),
            &[k.verifying_key()],
            &mut |_| true,
        )
        .unwrap();
    let host = PluginHost::new(HostConfig {
        limits: Limits { memory_bytes: 32 * 1024 * 1024, ..Limits::default() },
        state_dir: store.state_root(),
        egress: Arc::new(VecEgress::default()),
        secrets: None,
        http_client: None,
    })
    .unwrap();
    let plugin = host.load(&store.load("dev.nugget.bomb").unwrap()).unwrap();
    let r = plugin.tools().await;
    assert!(r.unwrap_err().to_string().contains("memory limit"));
}

#[test]
fn pm_poc_load_does_not_check_signer_is_trusted() {
    use sha2::Digest;
    let (_d, store) = temp_store();
    let trusted = key(1);
    let m = manifest_toml("dev.nugget.x", "1.0.0", "[\"tool\"]", "");
    store
        .install(&package(&m, EMPTY_COMPONENT, &trusted), &[trusted.verifying_key()], &mut |_| true)
        .unwrap();
    // Someone who can write the data dir (e.g. a synced folder) swaps in their own package.
    let evil = key(66);
    let m2 = manifest_toml("dev.nugget.x", "9.0.0", "[\"tool\"]", "hosts = [\"evil.example\"]\nsecrets = [\"token\"]");
    let pkg = package(&m2, EMPTY_COMPONENT, &evil);
    let h = |b: &[u8]| hex::encode(sha2::Sha256::digest(b));
    let blobs = store.root().join("blobs/sha256");
    std::fs::write(blobs.join(h(&pkg.manifest)), &pkg.manifest).unwrap();
    let idx_path = store.root().join("index.json");
    let mut idx: serde_json::Value = serde_json::from_slice(&std::fs::read(&idx_path).unwrap()).unwrap();
    let rec = &mut idx["plugins"]["dev.nugget.x"];
    rec["manifest_sha256"] = h(&pkg.manifest).into();
    rec["signature"] = String::from_utf8(pkg.signature.clone()).unwrap().into();
    rec["signer"] = hex::encode(evil.verifying_key().as_bytes()).into();
    rec["approved"] = serde_json::json!({"hosts": ["evil.example"], "secrets": ["token"], "kv_max_bytes": 0});
    std::fs::write(&idx_path, serde_json::to_vec(&idx).unwrap()).unwrap();
    let loaded = store.load("dev.nugget.x").expect("loads although signed by an untrusted key");
    assert_eq!(loaded.installed.approved.hosts, vec!["evil.example".to_string()]);
}
