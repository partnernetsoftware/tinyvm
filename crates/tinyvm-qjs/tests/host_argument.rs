//! A host argument of the wrong type names the call and the position.
//!
//! `print(1)` is refused at compile time; `print(s.length)` cannot be, because
//! a receiver's type is a run-time fact, and until 2026-08-29 it reached a
//! bare `unreachable` in the call site's argument unwrapping. Every script
//! author met it on their first `print(n)`. The guest now records
//! `"<host>#<n>"` before trapping; a literal String argument skips the test.

use tinyvm::{Limits, Val, WasmModule};
use tinyvm_qjs::{
    GuestFault, HostFn, HostParam, HostResult, Names, Options, Value, compile_qjs_m1_with,
    guest_fault, guest_host_argument,
};

fn log_host() -> Vec<HostFn> {
    vec![HostFn {
        name: "log".to_string(),
        module: "sys".to_string(),
        field: "log".to_string(),
        params: vec![HostParam::StrPtrLen, HostParam::StrPtrLen],
        result: HostResult::Void,
    }]
}

fn compile(source: &str) -> Vec<u8> {
    compile_qjs_m1_with(
        source,
        Options {
            names: Names::Declared(log_host()),
            ..Options::default()
        },
    )
    .unwrap_or_else(|e| panic!("compiling {source:?}: {e}"))
}

fn run(source: &str) -> (bool, Option<GuestFault>, Option<String>) {
    let wasm = compile(source);
    let mut module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    // A program that never calls `log` imports nothing; binding is then
    // nothing to do rather than a failure.
    let _ = module.bind_import_typed("sys", "log", |_args: &[Val], _memory: &mut [u8]| Ok(Vec::new()));
    let mut instance = module.instantiate().expect("instantiates");
    let ok = instance.invoke_by_name("main", &Value::args(&[])).is_ok();
    let memory = instance.memory().expect("guest memory");
    (ok, guest_fault(&memory), guest_host_argument(&memory))
}

#[test]
fn a_number_where_a_string_is_declared_names_the_call_and_the_argument() {
    let (ok, fault, detail) = run(r#"let s = "abc"; log("n", s.length);"#);
    assert!(!ok, "it must stop");
    assert_eq!(fault, Some(GuestFault::HostArgument));
    assert_eq!(detail.as_deref(), Some("log#2"));
}

#[test]
fn the_first_argument_is_numbered_one() {
    let (ok, fault, detail) = run(r#"let n = 2; log(n, "x");"#);
    assert!(!ok);
    assert_eq!(fault, Some(GuestFault::HostArgument));
    assert_eq!(detail.as_deref(), Some("log#1"));
}

#[test]
fn strings_built_at_run_time_pass() {
    let (ok, fault, detail) = run(r#"let n = 2; log("n=" + n, "" + n);"#);
    assert!(ok, "{fault:?} {detail:?}");
    assert_eq!(fault, None);
}

#[test]
fn literal_strings_pass_without_a_test() {
    let (ok, fault, _) = run(r#"log("a", "b");"#);
    assert!(ok);
    assert_eq!(fault, None);
    // The literal form carries no tag test: it is smaller than the same
    // call with one argument built at run time.
    let literal = compile(r#"log("a", "b");"#).len();
    let built = compile(r#"let n = 1; log("a", "" + n);"#).len();
    assert!(literal < built, "literal {literal} vs built {built}");
}

#[test]
fn the_reader_answers_only_its_own_fault() {
    let (_, fault, detail) = run(r#"throw "log#9";"#);
    assert_eq!(fault, Some(GuestFault::UncaughtThrow));
    assert_eq!(detail, None);
}
