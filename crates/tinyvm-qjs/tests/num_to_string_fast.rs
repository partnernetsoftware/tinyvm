//! `"" + n` for a safe integer takes the digit loop, not Dragon4.
//!
//! Measured through the downstream CLI at 1012da1: ~5 200 steps per
//! conversion, the single most expensive thing a migrated script did per
//! line. An integer is steps 6-7 of Number::toString with `k == n`: its
//! digits, a sign, nothing else. The first fast path stopped at 2^31, and a
//! 13-digit millisecond timestamp -- `Date.now()`'s shape, in every journal
//! record a downstream journey writes -- still paid 32 786 steps
//! (2026-08-30, agenterm plan/design-host-op-budget.md §7). Now the whole
//! safe range `|x| < 2^53` takes the loop, split into two i32 halves.

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
            let len = u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
                as usize;
            String::from_utf8(bytes[at + 4..at + 4 + len].to_vec()).expect("utf-8")
        }
        other => panic!("{source:?}: expected a String, got {other:?}"),
    }
}

fn steps(source: &str) -> u64 {
    let wasm = compile_qjs_m1(source).expect("compiles");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect("runs");
    instance.last_steps()
}

/// The same digits Rust prints, across the range and its edges.
#[test]
fn integers_print_as_their_digits() {
    for n in [
        0i64,
        1,
        7,
        9,
        10,
        42,
        99,
        100,
        12345,
        2_147_483_647,
        -1,
        -7,
        -10,
        -12345,
        -2_147_483_647,
        -2_147_483_648,
        // Past i32: the split into `hi * 1e9 + lo`, on both sides of a
        // multiple of 1e9 and at the top of the safe range.
        2_147_483_648,
        4_294_967_296,
        999_999_999,
        1_000_000_000,
        1_000_000_001,
        1_999_999_999,
        1_788_101_436_756,
        123_456_789_012_345,
        1_000_000_000_000_000,
        9_006_999_999_999_999,
        9_007_000_000_000_000,
        9_007_199_254_740_991,
        -1_788_101_436_756,
        -9_007_199_254_740_991,
    ] {
        assert_eq!(
            text(&format!("let n = {n}; return \"\" + n;")),
            format!("{n}"),
            "{n}"
        );
    }
}

/// Every integer just under a multiple of 1e9, where the correctly rounded
/// quotient can land one too high and the negative remainder repairs it.
#[test]
fn integers_beside_a_multiple_of_a_billion_print_as_their_digits() {
    for k in [1i64, 2, 7, 10, 4_294, 1_788_101, 9_007_199, 9_007_000] {
        for n in [
            k * 1_000_000_000 - 1,
            k * 1_000_000_000,
            k * 1_000_000_000 + 1,
        ] {
            assert_eq!(
                text(&format!("let n = {n}; return \"\" + n;")),
                format!("{n}"),
                "{n}"
            );
        }
    }
}

/// The edge above the fast path still answers through Dragon4, identically.
#[test]
fn the_first_integer_past_the_safe_range_and_fractions_take_the_general_path() {
    assert_eq!(
        text("let n = 9007199254740992; return \"\" + n;"),
        "9007199254740992"
    );
    assert_eq!(
        text("let n = 1e18; return \"\" + n;"),
        "1000000000000000000"
    );
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

/// A 13-digit millisecond timestamp: 32 786 steps before the split, 797
/// after (2026-08-30). The top of the safe range is 846.
#[test]
fn a_thirteen_digit_integer_conversion_is_cheap() {
    let base = steps("let n = 1788101436756; return n;");
    let one = steps("let n = 1788101436756; return \"\" + n;");
    let cost = one - base;
    println!("\"\" + 1788101436756: {cost} steps");
    assert!(
        cost < 1_200,
        "\"\" + 1788101436756 cost {cost} steps; it was 32 786"
    );
    let base = steps("let n = 9007199254740991; return n;");
    let one = steps("let n = 9007199254740991; return \"\" + n;");
    let cost = one - base;
    assert!(cost < 1_200, "\"\" + 9007199254740991 cost {cost} steps");
}
