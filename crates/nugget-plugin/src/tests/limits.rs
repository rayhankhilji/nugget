//! The memory cap holds per store (one per plugin call), across every linear memory, not per
//! memory. Plain core modules in WAT exercise the host's limiter directly; no guests needed.

use wasmtime::{Engine, Instance, Module, Store};

use crate::host::Limiter;

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
