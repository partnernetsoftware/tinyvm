//! A String property this engine does not have names itself.
//!
//! `"ab".length` is the one String property `__obj_get` answers. Every other
//! one traps on purpose -- `"ab".slice` is a real function in ECMA-262, so
//! `undefined` there would be a wrong answer wearing a right answer's clothes.
//! Until 2026-08-29 that trap was a bare `unreachable` in a program that never
//! said `.length`, and a nameless capability fault in one that did; the
//! scripts moving from rh met it on `slice`, `substr` and `substring` and
//! reported three different bugs. The key was in a local the whole time, and
//! `__obj_get` now writes it where the host can read it.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{GuestFault, Value, compile_qjs_m1, guest_fault, guest_missing_string_method};

fn run_and_read(source: &str) -> (Option<GuestFault>, Option<String>) {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let _ = instance.invoke_by_name("main", &Value::args(&[]));
    let memory = instance.memory().expect("guest memory");
    (guest_fault(&memory), guest_missing_string_method(&memory))
}

/// The case every migrated script hit: a method call this engine lacks
/// (`slice` was the name then; it has since landed, `substring` has not).
#[test]
fn a_missing_string_method_names_itself() {
    let (fault, name) = run_and_read(r#"let s = "abc"; return s.substring(0, 2);"#);
    assert_eq!(fault, Some(GuestFault::MissingStringMethod));
    assert_eq!(name.as_deref(), Some("substring"));
}

/// The same answer whether or not the program reads `.length` somewhere:
/// before this the two programs failed two different ways.
#[test]
fn the_name_does_not_depend_on_length_appearing_elsewhere() {
    let (fault, name) =
        run_and_read(r#"let s = "abc"; let n = s.length; return s.substr(0, n);"#);
    assert_eq!(fault, Some(GuestFault::MissingStringMethod));
    assert_eq!(name.as_deref(), Some("substr"));
}

/// A read, not only a call: `s.foo` as a value is the same boundary.
#[test]
fn a_missing_string_property_read_names_itself_too() {
    let (fault, name) = run_and_read(r#"let s = "abc"; let f = s.toUpperCase; return f;"#);
    assert_eq!(fault, Some(GuestFault::MissingStringMethod));
    assert_eq!(name.as_deref(), Some("toUpperCase"));
}

/// `.length` still answers, and a program that only asks that has no fault.
#[test]
fn length_is_still_the_one_string_property_answered() {
    let (fault, name) = run_and_read(r#"let s = "abc"; return s.length;"#);
    assert_eq!(fault, None);
    assert_eq!(name, None);
}

/// The reader is fault-gated: after an uncaught throw the detail word holds
/// the thrown String, and this reader must not hand it out as a method name.
#[test]
fn the_reader_answers_only_its_own_fault() {
    let (fault, name) = run_and_read(r#"throw "slice";"#);
    assert_eq!(fault, Some(GuestFault::UncaughtThrow));
    assert_eq!(name, None);
}
