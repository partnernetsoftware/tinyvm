//! A closure that calls -- or holds -- a declared function which itself
//! captures something. The caller builds the callee's environment, so it has
//! to carry those cells too; the parser forwards them to a fixed point.
//! Before 2026-08-30 this shape killed the compiler with "a Res::Captured
//! occurrence is in its function's capture list" (emit.rs `capture_index`).

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

fn run(source: &str) -> Value {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("{source}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let vals = instance.invoke_by_name("main", &Value::args(&[])).expect("runs");
    Value::returned(&vals).expect("value")
}

#[test]
fn a_closure_calling_a_capturing_declaration_carries_its_cells() {
    for (source, want) in [
        (r#"function outer(id) { function ask(op) { return op + id; } return { text: function () { return ask(7); } }; } return outer(1).text();"#, 8.0),
        (r#"function outer(id) { function ask(op) { return op + id; } return { visible: ask(1), text: function () { return ask(2); } }; } let o = outer(10); return o.visible + o.text();"#, 23.0),
        (r#"function outer(id) { const ask = function (op) { return op + id; }; return { text: function () { return ask(5); } }; } return outer(1).text();"#, 6.0),
        // Three levels: the middle closure only forwards.
        (r#"function outer(id) { function ask() { return id; } return function () { return function () { return ask() * 2; }; }; } return outer(21)()();"#, 42.0),
        // The callee calls another capturing declaration: the fixed point.
        (r#"function outer(a, b) { function first() { return a; } function second() { return first() + b; } return { f: function () { return second(); } }; } return outer(3, 4).f();"#, 7.0),
        // The owner itself still calls it without any environment shuffle.
        (r#"function outer(id) { function ask(op) { return op + id; } return ask(1) + ask(2); } return outer(10);"#, 23.0),
        // A captured variable that changes after the closure is made: cells, not copies.
        (r#"function outer() { let n = 1; function ask() { return n; } n = 5; return { f: function () { return ask(); } }; } return outer().f();"#, 5.0),
    ] {
        assert_eq!(run(source), Value::Number(want), "{source}");
    }
}
