//! The last script-reachable stops have names (tinyvm PRD A11 b/c/d).
//!
//! Two are the script's own doing and say so as `FAULT_INVALID_WRITE`: a key
//! that is not an index on an Array, and a property write on a value that has
//! no properties. Two are boundaries of
//! this engine's representation -- `split("")` and a `slice` boundary inside a
//! surrogate pair -- and stay `CapabilityBoundary`, now with the boundary
//! named. Until 2026-08-30 all four were a bare `unreachable`; the host saw
//! "unreachable executed" and nothing else.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{
    GuestFault, Value, compile_qjs_m1, guest_capability_name, guest_fault, guest_invalid_write,
};

fn run_and_read(source: &str) -> (Option<GuestFault>, Option<String>, Option<String>) {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let outcome = instance.invoke_by_name("main", &Value::args(&[]));
    assert!(outcome.is_err(), "{source}: expected the refusal");
    let memory = instance.memory().expect("guest memory");
    (
        guest_fault(&memory),
        guest_invalid_write(&memory),
        guest_capability_name(&memory),
    )
}

#[test]
fn a_refused_write_says_what_was_written() {
    for (source, reason) in [
        (
            r#"let a = [1]; a["x"] = 2; return a.length;"#,
            "an Array key that is not an index below 16777216",
        ),
        (
            r#"let a = [1]; a[1.5] = 2; return a.length;"#,
            "an Array key that is not an index below 16777216",
        ),
        (
            r#"let a = [1]; a[-1] = 2; return a.length;"#,
            "an Array key that is not an index below 16777216",
        ),
        (
            r#"let a = []; a[16777216] = 1; return a.length;"#,
            "an Array key that is not an index below 16777216",
        ),
        (
            r#"let s = "ab"; s[0] = "x"; return s;"#,
            "a property write on a value that has no properties",
        ),
        (
            r#"let n = 5; n[0] = 1; return n;"#,
            "a property write on a value that has no properties",
        ),
    ] {
        let (fault, reason_read, capability) = run_and_read(source);
        assert_eq!(fault, Some(GuestFault::InvalidWrite), "{source}");
        assert_eq!(reason_read.as_deref(), Some(reason), "{source}");
        assert_eq!(
            capability, None,
            "{source}: the reader is gated on its own code"
        );
    }
}

#[test]
fn a_named_capability_boundary_says_which() {
    for (source, boundary) in [
        (
            r#"return "ab".split("").length;"#,
            "split with an empty separator",
        ),
        (
            r#"let s = "\u{1F600}x"; return s.slice(1);"#,
            "a slice boundary inside a surrogate pair",
        ),
        (
            r#"let s = "a\u{1F600}"; return s.slice(0, 2);"#,
            "a slice boundary inside a surrogate pair",
        ),
    ] {
        let (fault, write, capability) = run_and_read(source);
        assert_eq!(fault, Some(GuestFault::CapabilityBoundary), "{source}");
        assert_eq!(capability.as_deref(), Some(boundary), "{source}");
        assert_eq!(write, None, "{source}: the reader is gated on its own code");
    }
}

/// The older capability arm -- a String property this engine lacks, in a
/// program that never names `.length` -- carries no name, and says so even
/// when an earlier call on the same instance left one behind.
#[test]
fn a_nameless_boundary_does_not_inherit_an_earlier_name() {
    let wasm = compile_qjs_m1(r#"let a = [1]; if (a.length > 5) { a["x"] = 1; } return "ab".foo;"#)
        .expect("compiles");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    assert!(instance.invoke_by_name("main", &Value::args(&[])).is_err());
    let memory = instance.memory().expect("guest memory");
    let fault = guest_fault(&memory);
    assert!(
        fault == Some(GuestFault::CapabilityBoundary)
            || fault == Some(GuestFault::MissingStringMethod),
        "got {fault:?}"
    );
    if fault == Some(GuestFault::CapabilityBoundary) {
        assert_eq!(guest_capability_name(&memory), None);
    }
}

/// Writes that are fine stay fine: an integer index, a String key on an
/// Object, and a push through the length are answers, not refusals.
#[test]
fn ordinary_writes_still_answer() {
    for source in [
        r#"let a = [1]; a[1] = 2; return a.length;"#,
        r#"let a = [1]; a[a.length] = 3; return a.length;"#,
        r#"let o = {}; o["k"] = 4; return o.k;"#,
        r#"let o = {}; o.j = 5; return o.j;"#,
        r#"let a = [1]; a[0] = 9; return a[0];"#,
    ] {
        let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
        let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
        let mut instance = module.instantiate().expect("instantiates");
        let outcome = instance.invoke_by_name("main", &Value::args(&[]));
        let memory = instance.memory().expect("guest memory");
        assert!(
            outcome.is_ok(),
            "{source}: {outcome:?} fault {:?} {:?}",
            guest_fault(&memory),
            guest_invalid_write(&memory)
        );
    }
}
