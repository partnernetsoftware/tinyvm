//! `Module::from_bytes_explained`: a module refused while validating a
//! function body says which function, by index and -- when the module carries
//! a `name` section -- by name. The plain entry points keep the `Copy`,
//! allocation-free `WasmError`, and answer exactly the same error.
//!
//! Why: "loading wasm: validation: type mismatch" with no location sent a
//! `.qjs` author to bisect a 900-line script (agenterm's first migration wave,
//! tinyvm PRD A7); the compiler had written every function's name into the
//! module all along, and nothing read it.

use tinyvm::{FunctionSite, Limits, WasmError, WasmModule};

const NAMED: &str = r#"
(module
  (import "env" "host" (func $host (param i32) (result i32)))
  (func $fine (result i32) (i32.const 1))
  (func $broken (result i32) (i64.const 1))
  (export "fine" (func $fine))
  (export "broken" (func $broken)))
"#;

#[test]
fn a_body_that_fails_validation_is_named_by_index_and_by_name() {
    // `wat` writes a `name` section for `$`-named functions.
    let wasm = wat::parse_str(NAMED).expect("the text is well-formed");
    let plain = WasmModule::from_bytes_with(&wasm, Limits::default())
        .err()
        .expect("the i64 result is a type mismatch");
    let explained = WasmModule::from_bytes_explained(&wasm, Limits::default())
        .err()
        .expect("the same refusal");
    assert_eq!(explained.error, plain, "the error itself is unchanged");
    assert_eq!(plain, WasmError::Decode("validation: type mismatch"));
    // Index 0 is the import; `fine` is 1; `broken` is 2.
    assert_eq!(
        explained.function,
        Some(FunctionSite {
            index: 2,
            name: Some("broken".to_owned()),
        })
    );
    assert_eq!(
        explained.to_string(),
        "validation: type mismatch in function `broken` (#2)"
    );
}

#[test]
fn without_a_name_section_the_index_still_answers() {
    let wasm = wat::parse_str(NAMED).expect("well-formed");
    // Strip every custom section: keep the header, copy standard sections.
    let mut stripped = wasm[..8].to_vec();
    let mut i = 8;
    while i < wasm.len() {
        let id = wasm[i];
        let mut size = 0u32;
        let mut shift = 0;
        let mut j = i + 1;
        loop {
            let b = wasm[j];
            size |= u32::from(b & 0x7f) << shift;
            shift += 7;
            j += 1;
            if b & 0x80 == 0 {
                break;
            }
        }
        let end = j + size as usize;
        if id != 0 {
            stripped.extend_from_slice(&wasm[i..end]);
        }
        i = end;
    }
    let explained = WasmModule::from_bytes_explained(&stripped, Limits::default())
        .err()
        .expect("still refused");
    assert_eq!(
        explained.function,
        Some(FunctionSite {
            index: 2,
            name: None
        })
    );
    assert_eq!(
        explained.to_string(),
        "validation: type mismatch in function #2"
    );
}

#[test]
fn a_refusal_before_any_body_names_no_function() {
    let explained = WasmModule::from_bytes_explained(b"not wasm at all", Limits::default())
        .err()
        .expect("bad magic");
    assert_eq!(explained.function, None);
    assert_eq!(explained.to_string(), "not a wasm module (bad magic)");
}

#[test]
fn a_well_formed_module_loads_through_either_entry() {
    let wasm = wat::parse_str(r#"(module (func (export "one") (result i32) (i32.const 1)))"#)
        .expect("well-formed");
    assert!(WasmModule::from_bytes_explained(&wasm, Limits::default()).is_ok());
    assert!(WasmModule::from_bytes_with(&wasm, Limits::default()).is_ok());
}
