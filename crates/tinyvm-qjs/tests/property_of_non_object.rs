//! A property read off `undefined`, `null`, a Number or a Boolean names the key.
//!
//! ECMA-262 throws a TypeError there. This engine stops -- the unwind channel
//! is not a runtime function's to reach -- and until 2026-08-29 stopped bare,
//! which is why every migrated script guards JSON fields with `=== undefined`
//! first. Now `guest_property_of_non_object` says which key.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{GuestFault, Value, compile_qjs_m1, guest_fault, guest_property_of_non_object};

fn run_and_read(source: &str) -> (Option<GuestFault>, Option<String>) {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let _ = instance.invoke_by_name("main", &Value::args(&[]));
    let memory = instance.memory().expect("guest memory");
    (guest_fault(&memory), guest_property_of_non_object(&memory))
}

#[test]
fn undefined_and_null_name_the_key() {
    for (source, key) in [
        (r#"let o = {}; let f = o.missing; return f.name;"#, "name"),
        (r#"let n = null; return n.field;"#, "field"),
        (r#"let x = 1; return x.toFixed;"#, "toFixed"),
        (r#"let b = true; return b.valueOf;"#, "valueOf"),
    ] {
        let (fault, name) = run_and_read(source);
        assert_eq!(fault, Some(GuestFault::PropertyOfNonObject), "{source}");
        assert_eq!(name.as_deref(), Some(key), "{source}");
    }
}

#[test]
fn objects_and_strings_are_not_this_fault() {
    let (fault, _) = run_and_read(r#"let o = { a: 1 }; return o.a;"#);
    assert_eq!(fault, None);
    let (fault, name) = run_and_read(r#"let s = "ab"; return s.substring;"#);
    assert_eq!(fault, Some(GuestFault::MissingStringMethod));
    assert_eq!(name, None, "the reader is gated on its own code");
}
