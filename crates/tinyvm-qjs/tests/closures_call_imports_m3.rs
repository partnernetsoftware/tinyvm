//! Closures nested in functions that call imported module functions.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Options, Value, compile_qjs_m1_with_modules};

fn go(source: &str) -> String {
    let resolve = |spec: &str| {
        (spec == "lib").then(|| {
            "export function f(x) { return x + 1; }\nexport function g(s) { return s + \"!\"; }"
                .to_string()
        })
    };
    let wasm = match compile_qjs_m1_with_modules(source, Options::default(), &resolve) {
        Ok(w) => w,
        Err(e) => return format!("compile: {e}"),
    };
    let module = match WasmModule::from_bytes_with(&wasm, Limits::default()) {
        Ok(m) => m,
        Err(e) => return format!("load: {}", e.message()),
    };
    let mut instance = module.instantiate().expect("instantiates");
    match instance.invoke_by_name("main", &Value::args(&[])) {
        Ok(v) => format!("ok: {:?}", Value::returned(&v)),
        Err(e) => format!("trap: {}", e.message()),
    }
}

/// A wave-1 migration report said a function nested inside another that
/// captures the outer function's locals and calls an imported function
/// fails wasm validation ("type mismatch"). Eleven shapes of exactly that
/// compile, load and answer; the report's own source was not in the report.
/// Kept as the regression test it would have been.
#[test]
fn nested_closures_call_imported_functions() {
    for (label, src) in [
        (
            "top-level call",
            r#"import * as m from "lib"; return m.f(1);"#,
        ),
        (
            "inside a function",
            r#"import * as m from "lib"; function g(x) { return m.f(x); } return g(1);"#,
        ),
        (
            "nested, no capture",
            r#"import * as m from "lib"; function outer(x) { function inner(y) { return m.f(y); } return inner(x); } return outer(1);"#,
        ),
        (
            "nested, captures outer local",
            r#"import * as m from "lib"; function outer(x) { function inner() { return m.f(x); } return inner(); } return outer(1);"#,
        ),
        (
            "nested closure, no import",
            r#"function outer(x) { function inner() { return x + 1; } return inner(); } return outer(1);"#,
        ),
        (
            "closure returned as value, captures, calls import",
            r#"import * as m from "lib"; function outer(x) { function inner() { return m.f(x); } return inner; } let g = outer(1); return g();"#,
        ),
        (
            "closure passed to map, captures, calls import",
            r#"import * as m from "lib"; function outer(x) { let a = [1, 2]; return a.map(function (v) { return m.f(v + x); }); } return outer(1).length;"#,
        ),
        (
            "inside try, captures, calls import",
            r#"import * as m from "lib"; function outer(x) { try { function inner() { return m.f(x); } return inner(); } catch (e) { return -1; } } return outer(1);"#,
        ),
        (
            "two levels, captures, calls import",
            r#"import * as m from "lib"; function outer(x) { function mid() { function inner() { return m.f(x); } return inner(); } return mid(); } return outer(1);"#,
        ),
        (
            "captures and calls import with a string",
            r#"import * as m from "lib"; function outer(s) { function inner() { return m.g(s); } return inner(); } return outer("ab");"#,
        ),
        (
            "arrow captures, calls import",
            r#"import * as m from "lib"; function outer(x) { const inner = () => m.f(x); return inner(); } return outer(1);"#,
        ),
    ] {
        let got = go(src);
        assert!(got.starts_with("ok: Ok("), "{label}: {got}");
    }
}
