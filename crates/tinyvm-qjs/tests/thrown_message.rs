//! The String an uncaught throw carried is readable by the host.
//!
//! Before 2026-08-29 the host could tell that a script threw and nothing
//! else: the unwind channel is module globals nothing exports, so the value
//! stayed inside. Every gate script moving from rh reports failure as
//! `throw "gate_id:reason"`, and "the script threw a value" was sending their
//! authors to a manifest on disk to learn which one. The entry epilogue now
//! records the thrown record's address in the fault area's second word when
//! the value is a String, and `guest_thrown_message` follows it.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{GuestFault, Value, compile_qjs_m1, guest_fault, guest_thrown_message};

fn run_and_read(source: &str) -> (Option<GuestFault>, Option<String>) {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let _ = instance.invoke_by_name("main", &Value::args(&[]));
    let memory = instance.memory().expect("guest memory");
    (guest_fault(&memory), guest_thrown_message(&memory))
}

/// The common case, and the one the migration needs.
#[test]
fn a_thrown_string_is_readable_from_the_host() {
    let (fault, message) =
        run_and_read("throw \"artifact_manifest_name_invalid:agenterm-x1.exe\";");
    assert_eq!(fault, Some(GuestFault::UncaughtThrow));
    assert_eq!(
        message.as_deref(),
        Some("artifact_manifest_name_invalid:agenterm-x1.exe")
    );
}

/// A String built at run time, not a literal, so the pointer is a heap record.
#[test]
fn a_built_string_is_readable_too() {
    let (_, message) = run_and_read("const who = \"x1\"; throw \"bad:\" + who + \".exe\";");
    assert_eq!(message.as_deref(), Some("bad:x1.exe"));
}

/// A thrown value that is not a String is reported as a throw with no
/// message -- a stated narrowing, not a silent one.
#[test]
fn a_thrown_number_has_no_message() {
    let (fault, message) = run_and_read("throw 42;");
    assert_eq!(fault, Some(GuestFault::UncaughtThrow));
    assert_eq!(message, None);
}

/// A caught throw records nothing: the fault area describes the call's
/// outcome, and the call finished normally.
#[test]
fn a_caught_throw_leaves_no_message() {
    let (fault, message) = run_and_read("try { throw \"x\"; } catch (e) { } return 1;");
    assert_eq!(fault, None);
    assert_eq!(message, None);
}

/// A program that never throws is byte-identical: the record happens in the
/// epilogue that only a program with an unwind channel has.
#[test]
fn a_program_that_never_throws_pays_nothing() {
    for (source, want) in [
        ("return 1;", 10_007),
        ("let o = {a:1}; o.b = 2; return o.a;", 10_580), /* +23 on 2026-08-29: a program that reads a static property can reach `__obj_get` with a String receiver, and the arm that names the missing property is 23 bytes; see runtime.rs `FAULT_MISSING_STRING_METHOD` */
    ] {
        let n = compile_qjs_m1(source).expect("compiles").len();
        assert_eq!(n, want, "{source:?} is {n} bytes");
    }
}
