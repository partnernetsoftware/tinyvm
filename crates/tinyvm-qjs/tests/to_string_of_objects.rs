//! ToString and ToNumber of a value that is not a primitive are a *named* refusal.
//!
//! ECMA-262 would answer `[object Object]`, `1,2` and the function's source;
//! this engine's standing rule is that an Object, an Array or a function is
//! never quietly converted (`objects_m3`, `arrays_m3`, `function_conformance`,
//! `heap_attack` all pin the stop), because a value silently becoming that
//! text in a command line or a property key is the footgun. Until
//! 2026-08-30 the stop was a bare `unreachable`; now the fault word says
//! which kind, and `JSON.stringify` is the spelling that says what was meant.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{GuestFault, Value, compile_qjs_m1, guest_fault, guest_no_primitive_form};

fn run_and_read(source: &str) -> (Option<GuestFault>, Option<String>) {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let outcome = instance.invoke_by_name("main", &Value::args(&[]));
    assert!(outcome.is_err(), "{source}: expected the refusal");
    let memory = instance.memory().expect("guest memory");
    (guest_fault(&memory), guest_no_primitive_form(&memory))
}

#[test]
fn the_three_kinds_are_named() {
    for (source, kind) in [
        (r#"let o = {}; return "" + o;"#, "an Object"),
        (r#"let o = { a: 1 }; return "x" + o + "y";"#, "an Object"),
        (
            r#"let o = {}; let k = {}; o[k] = 5; return 0;"#,
            "an Object",
        ),
        (r#"return "" + [];"#, "an Array"),
        (r#"return "" + [1, 2, 3];"#, "an Array"),
        (
            r#"let f = function () { return 1; }; return "" + f;"#,
            "a function",
        ),
        (
            r#"let f = function () { return 1; }; return f + 1;"#,
            "a function",
        ),
        (r#"let o = {}; return o * 2;"#, "an Object"),
        (r#"let o = {}; return -o;"#, "an Object"),
        (r#"return [1] * 2;"#, "an Array"),
        (
            r#"let f = function () { return 1; }; return f < 1;"#,
            "a function",
        ),
    ] {
        let (fault, which) = run_and_read(source);
        assert_eq!(fault, Some(GuestFault::NoPrimitiveForm), "{source}");
        assert_eq!(which.as_deref(), Some(kind), "{source}");
    }
}

#[test]
fn primitives_still_convert_and_other_faults_keep_their_own_name() {
    let wasm = compile_qjs_m1(r#"return "" + 1 + true + null + undefined;"#).expect("compiles");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect("primitives convert");
    let (fault, which) = run_and_read(r#"let f = undefined; return f(1);"#);
    assert_eq!(fault, Some(GuestFault::NotAFunction));
    assert_eq!(which, None, "the reader is gated on its own code");
}
