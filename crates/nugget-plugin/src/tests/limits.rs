//! The memory cap holds per store (one per plugin call), across every linear memory, not per
//! memory. Plain core modules in WAT exercise the host's limiter directly; no guests needed.

use std::sync::Arc;

use wasmtime::{Engine, Instance, Module, Store};

use super::*;
use crate::host::Limiter;
use crate::{HostConfig, Limits, PluginHost};

const PAGE: usize = 64 * 1024;

/// Three memories of `initial` pages each, and `grow(memory, pages)` for each of them.
fn module(engine: &Engine, initial: u32) -> Module {
    let wat = format!(
        r#"(module
            (memory $a {initial})
            (memory $b {initial})
            (memory $c {initial})
            (func (export "grow_a") (param i32) (result i32) (memory.grow $a (local.get 0)))
            (func (export "grow_b") (param i32) (result i32) (memory.grow $b (local.get 0)))
            (func (export "grow_c") (param i32) (result i32) (memory.grow $c (local.get 0)))
        )"#
    );
    Module::new(engine, wat::parse_str(wat).unwrap()).unwrap()
}

fn store(engine: &Engine, cap_pages: usize) -> Store<Limiter> {
    let mut store = Store::new(engine, Limiter::new(cap_pages * PAGE));
    store.limiter(|l| l);
    store
}

fn grow(
    store: &mut Store<Limiter>,
    instance: &Instance,
    which: &str,
    pages: i32,
) -> wasmtime::Result<i32> {
    instance
        .get_typed_func::<i32, i32>(&mut *store, &format!("grow_{which}"))
        .unwrap()
        .call(&mut *store, pages)
}

#[test]
fn growth_across_memories_counts_against_one_cap() {
    let engine = Engine::default();
    let m = module(&engine, 1);
    // Cap: 8 pages. Three memories of 1 page = 3 pages used.
    let mut s = store(&engine, 8);
    let instance = Instance::new(&mut s, &m, &[]).unwrap();
    // One memory growing within the cap works.
    assert_eq!(grow(&mut s, &instance, "a", 3).unwrap(), 1);
    // 6 used; each further memory is under the cap on its own, but the sum is not.
    assert_eq!(grow(&mut s, &instance, "b", 2).unwrap(), 1);
    assert!(grow(&mut s, &instance, "c", 1).is_err());
    assert!(s.data().memory_exceeded);
}

#[test]
fn initial_memories_count_too() {
    let engine = Engine::default();
    // Three memories of 4 pages each: 12 pages > a cap of 8, though each is 4 <= 8.
    let m = module(&engine, 4);
    let mut s = store(&engine, 8);
    assert!(Instance::new(&mut s, &m, &[]).is_err());
    // A cap that fits all of them instantiates.
    let mut s = store(&engine, 12);
    Instance::new(&mut s, &m, &[]).unwrap();
}

/// A component matching the `plugin` world whose instantiation also creates `bombs` extra core
/// instances, each with a `pages`-page memory that its start function fills (commits) before
/// any export runs. From the PM-1 review PoC.
fn bomb_component(bombs: usize, pages: u32) -> Vec<u8> {
    let bomb_instances: String = (0..bombs)
        .map(|_| "(core instance (instantiate $Bomb))\n".to_string())
        .collect();
    let bytes = u64::from(pages) * PAGE as u64;
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

/// Install `wasm` as a tool plugin and list its tools under a 32 MiB cap.
async fn tools_under_32_mib(wasm: &[u8]) -> nugget_core::Result<Vec<crate::ToolSpec>> {
    let (_d, store) = temp_store();
    let k = key(3);
    let id = "dev.nugget.bomb";
    store
        .install(
            &package(&manifest_toml(id, "1.0.0", "[\"tool\"]", ""), wasm, &k),
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
        allow_private_network: false,
    })
    .unwrap();
    let plugin = host
        .load(&store.load_trusted(id, &[k.verifying_key()]).unwrap())
        .unwrap();
    plugin.tools().await
}

/// The PM-1 review PoC, inverted: 15 extra 32 MiB memories (about 480 MiB committed by start
/// functions) under a 32 MiB cap are refused at instantiation, not just one memory over the cap.
#[tokio::test]
async fn many_memories_in_one_component_share_the_cap() {
    let err = tools_under_32_mib(&bomb_component(15, 512))
        .await
        .expect_err("15 x 32 MiB must not fit a 32 MiB cap");
    assert!(err.to_string().contains("memory limit"), "{err}");
    // Control: one memory over the cap on its own is refused the same way.
    let err = tools_under_32_mib(&bomb_component(1, 600))
        .await
        .expect_err("one 600-page memory is over the cap");
    assert!(err.to_string().contains("memory limit"), "{err}");
    // And a component that fits the cap in total still works.
    tools_under_32_mib(&bomb_component(3, 64)).await.unwrap();
}
