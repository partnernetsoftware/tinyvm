//! `JSON` held as a value -- `let j = JSON`, `const s = JSON.stringify` --
//! in a program that also has a closure. `__json_ns` builds two function
//! records, and once any function in the program captures, `__fn_new` takes
//! an environment word; the builder used to hand it none and the module
//! failed the load gate with "type mismatch" (2026-08-30, found by the first
//! GUI journey downstream that did both).

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

fn run(source: &str) -> Value {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("{source}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).unwrap_or_else(|e| panic!("{source}: load gate: {}", e.message()));
    let mut instance = module.instantiate().expect("instantiates");
    let vals = instance.invoke_by_name("main", &Value::args(&[])).expect("runs");
    Value::returned(&vals).expect("value")
}

#[test]
fn json_as_a_value_loads_and_answers_beside_a_closure() {
    for (source, want) in [
        (r#"let j = JSON; return j.parse("[1]").length;"#, 1.0),
        (r#"let j = JSON; function f(x) { return function () { return x; }; } return j.parse("[1]").length + f(1)();"#, 2.0),
        (r#"function f(x) { return function () { return x; }; } let s = JSON.stringify; return s(f(7)()).length;"#, 1.0),
        (r#"function mk() { let n = 3; return function () { return n; }; } const j = JSON; const g = mk(); return j.parse(j.stringify({a: g()})).a;"#, 3.0),
    ] {
        assert_eq!(run(source), Value::Number(want), "{source}");
    }
}
