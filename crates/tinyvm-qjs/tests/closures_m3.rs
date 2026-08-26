//! Closures: a nested function reading a binding of an enclosing one.
//!
//! Same discipline as `objects_m3.rs` and `arrays_m3.rs`, which this is the
//! sibling of: every expectation is derived from ECMA-262 rather than from what
//! the implementation happens to do, and every one of them **runs** -- compile
//! -> tinyvm's load gate -> instantiate -> `invoke_by_name("main")`.
//!
//! # The one property everything else rests on
//!
//! ECMA-262 closes over the **binding**, not its value. Assignment is in this
//! subset, so the difference is reachable and a by-value capture would be a
//! fabricated answer rather than a simplification. Half the tests below exist
//! to hold that one line.
//!
//! Designed in `plan/design-closure-milestone.md`.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Boundary, Names, Options, Value, compile_qjs_m1, compile_qjs_m1_with};

#[derive(Debug, Clone, PartialEq)]
enum Out {
    Undefined,
    Number(f64),
    Bool(bool),
    Str(String),
}

#[track_caller]
fn run(source: &str) -> Out {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .unwrap_or_else(|e| panic!("load gate rejected {source:?}: {}", e.message()));
    let mut instance = module
        .instantiate()
        .unwrap_or_else(|e| panic!("instantiating {source:?}: {}", e.message()));
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .unwrap_or_else(|e| panic!("trap in {source:?}: {}", e.message()));
    match Value::returned(&vals).unwrap_or_else(|e| panic!("{source:?}: {e}")) {
        Value::Undefined => Out::Undefined,
        Value::Number(x) => Out::Number(x),
        Value::Bool(b) => Out::Bool(b),
        Value::String(ptr) => {
            let view = instance.memory().expect("guest memory");
            let bytes: &[u8] = &view;
            let at = ptr as usize;
            let len = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4")) as usize;
            Out::Str(String::from_utf8(bytes[at + 4..at + 4 + len].to_vec()).expect("utf-8"))
        }
        other => panic!("{source:?}: unexpected {other:?}"),
    }
}

#[track_caller]
fn number(source: &str, want: f64) {
    assert_eq!(run(source), Out::Number(want), "{source:?}");
}

// =========================================================================
// Capture is by binding
// =========================================================================

#[test]
fn a_nested_function_reads_an_enclosing_local() {
    number(
        "function o() { let a = 1; function i() { return a; } return i(); } return o();",
        1.0,
    );
}

#[test]
fn a_write_after_the_closure_exists_is_visible_through_it() {
    // The line the whole milestone rests on. By-value capture answers 1.
    number(
        "function o() { let a = 1; function i() { return a; } a = 2; return i(); } return o();",
        2.0,
    );
}

#[test]
fn a_write_through_the_closure_is_visible_to_the_declaring_function() {
    // The other direction, and the one a naive implementation gets wrong by
    // leaving the declaring function reading its own wasm local: the two
    // diverge on the first assignment and nothing says so.
    number(
        "function o() { let a = 1; function i() { a = 5; } i(); return a; } return o();",
        5.0,
    );
}

#[test]
fn a_parameter_is_a_binding_like_any_other() {
    // The case every real script hits first, and the one with an order to get
    // right: the argument arrives in the pair of locals whose `i32` half
    // becomes the cell pointer, so it has to be read into the cell before the
    // pointer overwrites it.
    number(
        "function o(n) { function i() { return n; } return i(); } return o(7);",
        7.0,
    );
    number(
        "function o(n) { function i() { n = n + 1; } i(); return n; } return o(1);",
        2.0,
    );
}

#[test]
fn every_kind_of_value_survives_a_capture() {
    // The cell holds a whole V1 pair, so a captured binding is not narrowed to
    // a payload -- it keeps its type.
    // `3 / 2` and not the literal `1.5`: a fractional *literal* is still
    // outside the subset, while the value it denotes is ordinary.
    number(
        "function o() { let a = 3 / 2; function i() { return a; } return i(); } return o();",
        1.5,
    );
    assert_eq!(
        run("function o() { let a = \"x\"; function i() { return a; } return i(); } return o();"),
        Out::Str("x".into())
    );
    assert_eq!(
        run("function o() { let a = true; function i() { return a; } return i(); } return o();"),
        Out::Bool(true)
    );
    assert_eq!(
        run("function o() { let a; function i() { return a; } return i(); } return o();"),
        Out::Undefined
    );
    number(
        "function o() { let a = {v: 3}; function i() { return a.v; } return i(); } return o();",
        3.0,
    );
    number(
        "function o() { let a = [1, 2]; function i() { return a[1]; } return i(); } return o();",
        2.0,
    );
}

// =========================================================================
// Closures as values
// =========================================================================

#[test]
fn a_closure_outlives_the_frame_that_made_it() {
    number(
        "function mk(n) { return function () { return n; }; } let f = mk(7); return f();",
        7.0,
    );
}

#[test]
fn two_instances_of_one_function_expression_have_separate_environments() {
    // **What makes the identity fix observable.** Two evaluations of one
    // function expression are two objects (15.2.5); before capture existed
    // nothing could tell them apart, and now their environments can.
    number(
        "function mk(n) { return function () { return n; }; } \
         let a = mk(1); let b = mk(2); return a() * 10 + b();",
        12.0,
    );
}

#[test]
fn a_closure_reached_through_a_property_still_has_its_environment() {
    // A closure and a namespace table at once, which is the shape
    // `scripts/qjs/lib/fleet.qjs` is built from.
    number(
        "function mk(p) { const o = {}; o.m = function () { return p; }; return o; } \
         return mk(4).m();",
        4.0,
    );
}

#[test]
fn two_closures_over_one_binding_share_it() {
    // Two closures, one cell: a write through either is a write both see. This
    // is the difference between capturing a binding and copying a value, seen
    // from a third angle.
    number(
        "function o() { let a = 1; \
           let get = function () { return a; }; \
           let set = function () { a = 9; }; \
           set(); return get(); } return o();",
        9.0,
    );
}

// =========================================================================
// Depth
// =========================================================================

#[test]
fn a_capture_reaches_through_more_than_one_level() {
    // Flat closures: the innermost function holds the cell directly rather
    // than walking two parents. The middle function captures it too, because
    // it is the one that hands it on.
    number(
        "function a1() { let x = 3; \
           function b() { function c() { return x; } return c(); } \
           return b(); } return a1();",
        3.0,
    );
}

#[test]
fn a_deep_closure_still_sees_a_later_write() {
    number(
        "function a1() { let x = 1; \
           function b() { function c() { return x; } return c; } \
           let c = b(); x = 4; return c(); } return a1();",
        4.0,
    );
}

#[test]
fn a_script_binding_read_from_a_function_is_still_not_a_capture() {
    // Unchanged, and worth a row: the script's bindings are module globals
    // that outlive every frame, so reading one is `Res::Global` and no
    // environment is built for it. A closure milestone must not quietly turn
    // every such read into a capture.
    number("let a = 1; function i() { return a; } return i();", 1.0);
    number("let a = 1; function i() { a = 6; } i(); return a;", 6.0);
}

// =========================================================================
// The gate
// =========================================================================

#[test]
fn a_program_with_no_capture_is_byte_identical_to_what_it_was() {
    // `plan/design-closure-milestone.md` §1.2, as a test rather than a
    // sentence. These are the exact byte counts from before closures landed,
    // measured the same way the array gate's were.
    //
    // The array milestone made this promise and broke it by 11 bytes on the
    // first implementation, because two arms had been appended to the
    // *unconditional* runtime. This one is kept by construction instead: the
    // environment parameter, the record word and the widened uniform signature
    // are each behind `Scan::captures`.
    for (source, want) in [
        ("return 1;", 9_784),
        ("let o = {a:1}; o.b = 2; return o.a;", 9_905),
        (
            "function mk() { return function () { return 1; }; } let f = mk(); return f();",
            9_948,
        ),
        ("return JSON.stringify({a:1});", 15_414),
    ] {
        let n = compile_qjs_m1(source).expect("compiles").len();
        assert_eq!(
            n, want,
            "{source:?} is {n} bytes; a program with no capture must pay nothing for closures"
        );
    }
}

#[test]
fn what_one_closure_costs_is_written_down() {
    // The measurement `plan/design-closure-milestone.md` §4 owed, split the
    // way it asked: the fixed part that arrives with the gate, and the part
    // each capturing function adds.
    //
    // Separating them needs the "one more function" baseline subtracted, which
    // is why there are four programs and not two: a second *capturing*
    // function costs one more function plus one more capture, and only the
    // difference of those two differences is the capture.
    let size = |src: &str| compile_qjs_m1(src).expect("compiles").len();

    let one_value =
        size("function mk() { return function () { return 1; }; } let f = mk(); return f();");
    let two_values = size(
        "function mk() { return function () { return 1; }; } \
         function mk2() { return function () { return 2; }; } \
         let f = mk(); let g = mk2(); return f() + g();",
    );
    let one_closure =
        size("function mk(n) { return function () { return n; }; } let f = mk(1); return f();");
    let two_closures = size(
        "function mk(n) { return function () { return n; }; } \
         function mk2(m) { return function () { return m; }; } \
         let f = mk(1); let g = mk2(2); return f() + g();",
    );

    let per_function = (two_closures - one_closure) as i64 - (two_values - one_value) as i64;
    let fixed = (one_closure - one_value) as i64 - per_function;

    println!("closures cost: fixed {fixed} bytes, {per_function} per capturing function");
    assert_eq!(
        per_function, 99,
        "one capturing function: the environment parameter, the cell prologue and the \
         environment built at each creation site"
    );
    assert_eq!(
        fixed, 21,
        "the gate: one leading slot on the uniform signature, one word on the function \
         record, and `__fn_new`'s extra parameter and store"
    );
}

// =========================================================================
// What is still refused
// =========================================================================

#[test]
fn the_neighbouring_constructs_are_still_refused_by_name() {
    // A closure milestone is the one most likely to be mistaken for having
    // brought these; it did not, and each still says so for itself.
    // The boundary differs by construct and is asserted per row rather than
    // assumed uniform: a template literal is `Subset` -- a shape this engine's
    // own grammar could grow -- while `class` and `async` are `FullJs`.
    for (source, needle, boundary) in [
        (
            "let f = (x) => x; return f(1);",
            "arrow functions",
            Boundary::FullJs,
        ),
        ("return `x`;", "template literals", Boundary::Subset),
        (
            "class A {} return 1;",
            "the `class` keyword",
            Boundary::FullJs,
        ),
        (
            "async function f() {} return 1;",
            "the `async` keyword",
            Boundary::FullJs,
        ),
    ] {
        let e = compile_qjs_m1(source).expect_err("still outside the subset");
        assert!(
            e.message.contains(needle),
            "{source:?}: want {needle:?}, got {}",
            e.message
        );
        assert_eq!(e.boundary, boundary, "{source:?}");
    }
}

#[test]
fn captures_work_under_the_declared_names_mode_too() {
    // The gate is a property of the program, not of the naming mode, and the
    // downstream product uses `Names::Declared`. A milestone that only worked
    // under the default would not be reachable from `agenterm-qjswasm`.
    let wasm = compile_qjs_m1_with(
        "function mk(n) { return function () { return n; }; } let f = mk(5); return f();",
        Options {
            names: Names::Declared(Vec::new()),
        },
    )
    .expect("compiles under Declared");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect("runs");
    assert_eq!(Value::returned(&vals), Ok(Value::Number(5.0)));
}
