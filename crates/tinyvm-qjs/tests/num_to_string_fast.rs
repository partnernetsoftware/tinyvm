//! `"" + n` for an integer below 2^31 takes the digit loop, not Dragon4.
//!
//! Measured through the downstream CLI at 1012da1: ~5 200 steps per
//! conversion, the single most expensive thing a migrated script did per
//! line. An integer in i32 range is steps 6-7 of Number::toString with
//! `k == n`: its digits, a sign, nothing else.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

fn text(source: &str) -> String {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .unwrap_or_else(|e| panic!("trap in {source:?}: {}", e.message()));
    match Value::returned(&vals).unwrap_or_else(|e| panic!("{source:?}: {e}")) {
        Value::String(ptr) => {
            let view = instance.memory().expect("guest memory");
            let bytes: &[u8] = &view;
            let at = ptr as usize;
            let len = u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize;
            String::from_utf8(bytes[at + 4..at + 4 + len].to_vec()).expect("utf-8")
        }
        other => panic!("{source:?}: expected a String, got {other:?}"),
    }
}

fn steps(source: &str) -> u64 {
    let wasm = compile_qjs_m1(source).expect("compiles");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    instance.invoke_by_name("main", &Value::args(&[])).expect("runs");
    instance.last_steps()
}

/// The same digits Rust prints, across the range and its edges.
#[test]
fn integers_print_as_their_digits() {
    for n in [0i64, 1, 7, 9, 10, 42, 99, 100, 12345, 2_147_483_647, -1, -7, -10, -12345, -2_147_483_647, -2_147_483_648] {
        assert_eq!(text(&format!("let n = {n}; return \"\" + n;")), format!("{n}"), "{n}");
    }
}

/// The edge above the fast path still answers through Dragon4, identically.
#[test]
fn the_first_integer_past_i32_and_fractions_take_the_general_path() {
    assert_eq!(text("let n = 2147483648; return \"\" + n;"), "2147483648");
    assert_eq!(text("let n = 1.5; return \"\" + n;"), "1.5");
    assert_eq!(text("let n = -0.25; return \"\" + n;"), "-0.25");
    assert_eq!(text("let n = 1e21; return \"\" + n;"), "1e+21");
    assert_eq!(text("let n = 0 / 0; return \"\" + n;"), "NaN");
}

/// The price: one conversion of a five-digit integer under 800 steps net of
/// the surrounding program (537 measured), where it was ~5 200.
#[test]
fn an_integer_conversion_is_cheap() {
    let base = steps("let n = 12345; return n;");
    let one = steps("let n = 12345; return \"\" + n;");
    let cost = one - base;
    println!("\"\" + 12345: {cost} steps");
    assert!(cost < 800, "\"\" + 12345 cost {cost} steps");
}
