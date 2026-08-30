//! Calling a value that is not a function names the callee.
//!
//! ECMA-262 throws a TypeError. Until 2026-08-30 this engine stopped bare
//! (`unbox_function`'s tag check was an `unreachable`), and a downstream
//! lint that wrote `[...].concat(x)` -- a method this engine does not have,
//! read back as `undefined` -- died with no sentence. Now a program with an
//! unwind channel gets the TypeError, catchable, with the callee's name;
//! one without gets fault 8 and `guest_not_a_function` says the name.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{GuestFault, Value, compile_qjs_m1, guest_fault, guest_not_a_function};

fn run_and_read(source: &str) -> (Option<GuestFault>, Option<String>) {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let _ = instance.invoke_by_name("main", &Value::args(&[]));
    let memory = instance.memory().expect("guest memory");
    (guest_fault(&memory), guest_not_a_function(&memory))
}

fn returned_string(source: &str) -> String {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .unwrap_or_else(|e| panic!("{source}: {}", e.message()));
    let Value::String(ptr) = Value::returned(&vals).expect("value") else {
        panic!("{source}: not a string")
    };
    let view = instance.memory().expect("guest memory");
    let bytes: &[u8] = &view;
    let at = ptr as usize;
    let len = u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize;
    String::from_utf8(bytes[at + 4..at + 4 + len].to_vec()).expect("valid UTF-8")
}

#[test]
fn an_uncatchable_program_names_the_callee() {
    for (source, name) in [
        (r#"let f = undefined; return f(1);"#, "f"),
        (r#"let o = { a: 1 }; return o.missing(2);"#, "missing"),
        (r#"let a = [1]; return a.splice(0).length;"#, "splice"),
        // 23.1.3.30 step 1, named for the host as the parameter it is.
        (r#"let a = [2, 1]; return a.sort(1).length;"#, "comparefn"),
        (r#"let n = 3; return n();"#, "n"),
        (r#"let o = { f: 7 }; return o.f();"#, "f"),
        (
            r#"let g = function () { return 1; }; return g()();"#,
            "<expression>",
        ),
    ] {
        let (fault, callee) = run_and_read(source);
        assert_eq!(fault, Some(GuestFault::NotAFunction), "{source}");
        assert_eq!(callee.as_deref(), Some(name), "{source}");
    }
}

#[test]
fn a_program_that_can_catch_gets_the_type_error() {
    for (source, want) in [
        (
            r#"let f = undefined; try { f(1); } catch (e) { return e; } return "ran";"#,
            "TypeError: f is not a function",
        ),
        (
            r#"let a = [1]; try { a.splice(0); } catch (e) { return e; } return "ran";"#,
            "TypeError: splice is not a function",
        ),
        (
            r#"let o = { f: 7 }; try { o.f(); } catch (e) { return e; } return "ran";"#,
            "TypeError: f is not a function",
        ),
        (
            r#"function mk() { return 3; } try { mk()(); } catch (e) { return e; } return "ran";"#,
            "TypeError: <expression> is not a function",
        ),
        (
            r#"let f = function () { return "ok"; }; try { return f(); } catch (e) { return e; }"#,
            "ok",
        ),
    ] {
        assert_eq!(returned_string(source), want, "{source}");
    }
    let (fault, callee) =
        run_and_read(r#"let f = undefined; try { f(1); } catch (e) { return e; } return "ran";"#);
    assert_eq!(fault, None, "a caught TypeError is not a fault");
    assert_eq!(callee, None);
}

#[test]
fn real_calls_and_other_faults_are_untouched() {
    let (fault, callee) = run_and_read(r#"let f = function (x) { return x + 1; }; return f(1);"#);
    assert_eq!(fault, None);
    assert_eq!(callee, None);
    let (fault, callee) = run_and_read(r#"let o = {}; let f = o.missing; return f.name;"#);
    assert_eq!(fault, Some(GuestFault::PropertyOfNonObject));
    assert_eq!(callee, None, "the reader is gated on its own code");
}
