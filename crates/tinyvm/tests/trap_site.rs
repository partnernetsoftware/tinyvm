//! `Instance::last_trap_site`: where a failed top-level call stopped.
//!
//! The site locates a failure; it never classifies one. Every test also
//! checks the returned error is the core's own, unchanged.

use tinyvm::{Val, WasmError, WasmModule, WasmTrapSite};

fn instance(wat_source: &str) -> tinyvm::WasmInstance {
    let wasm = wat::parse_str(wat_source).expect("fixture wat");
    WasmModule::from_bytes(&wasm)
        .expect("fixture loads")
        .instantiate()
        .expect("fixture instantiates")
}

const MODULE: &str = r#"
    (module
      (memory 1 1)
      (func $inner (result i32)
        (i32.load (i32.const 65536)))
      (func (export "oob") (result i32)
        (i32.load (i32.const 65536)))
      (func (export "nested") (result i32)
        (i32.const 1)
        (drop)
        (call $inner))
      (func (export "unreachable") (result i32)
        (unreachable))
      (func (export "fill") (result i32)
        (memory.fill (i32.const 65530) (i32.const 0) (i32.const 16))
        (i32.const 0))
      (func (export "ok") (result i32)
        (i32.const 7)))
"#;

#[test]
fn an_out_of_bounds_load_names_its_function() {
    let mut instance = instance(MODULE);
    let error = instance.invoke_by_name("oob", &[]).expect_err("traps");
    assert_eq!(error, WasmError::Trap("memory access out of bounds"));
    // Function 1: `$inner` is 0.
    assert_eq!(
        instance.last_trap_site(),
        Some(WasmTrapSite { function_index: 1 })
    );
}

#[test]
fn a_nested_trap_names_the_innermost_activation() {
    let mut instance = instance(MODULE);
    let error = instance.invoke_by_name("nested", &[]).expect_err("traps");
    assert_eq!(error, WasmError::Trap("memory access out of bounds"));
    assert_eq!(
        instance.last_trap_site(),
        Some(WasmTrapSite { function_index: 0 })
    );
}

#[test]
fn a_site_does_not_turn_another_trap_into_out_of_bounds() {
    let mut instance = instance(MODULE);
    let error = instance
        .invoke_by_name("unreachable", &[])
        .expect_err("traps");
    assert_eq!(error, WasmError::Trap("unreachable executed"));
    assert_eq!(
        instance.last_trap_site().map(|site| site.function_index),
        Some(3)
    );
}

#[test]
fn a_bulk_memory_trap_has_a_site_too() {
    let mut instance = instance(MODULE);
    let error = instance.invoke_by_name("fill", &[]).expect_err("traps");
    assert_eq!(error, WasmError::Trap("bulk memory access out of bounds"));
    assert_eq!(
        instance.last_trap_site().map(|site| site.function_index),
        Some(4)
    );
}

#[test]
fn every_top_level_call_starts_with_no_site() {
    let mut instance = instance(MODULE);
    let _ = instance.invoke_by_name("oob", &[]);
    assert!(instance.last_trap_site().is_some());
    assert_eq!(
        instance.invoke_by_name("ok", &[]).expect("succeeds"),
        vec![Val::I32(7)]
    );
    assert_eq!(instance.last_trap_site(), None);
}
