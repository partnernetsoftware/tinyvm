//! What a bitwise operator costs in steps, pinned: ToInt32 of each side
//! (one call, one NaN test, one range test, one truncation on the common
//! road) and the operator itself. The comparison that matters is against
//! what the scripts spelled by hand while the engine had no `^`:
//! `rh_compat.qjs`'s `xor16`, sixteen turns of `% 2` and a division.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

fn steps(source: &str) -> u64 {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(
        &wasm,
        Limits {
            max_steps: 4_000_000_000,
            ..Limits::default()
        },
    )
    .expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    instance
        .invoke_by_name("main", &Value::args(&[]))
        .unwrap_or_else(|e| panic!("trap in {source:?}: {}", e.message()));
    instance.last_steps()
}

/// `rh_compat.qjs::xor16`, verbatim but for `int_div` inlined.
const XOR16: &str = r#"
function int_div(a, b) { return (a - (a % b)) / b; }
function xor16(a, b) {
  let result = 0;
  let bit = 1;
  let x = a;
  let y = b;
  for (let i = 0; i < 16; i = i + 1) {
    if (x % 2 !== y % 2) { result = result + bit; }
    x = int_div(x, 2);
    y = int_div(y, 2);
    bit = bit * 2;
  }
  return result;
}
"#;

#[test]
fn one_operator_costs_two_conversions_and_the_instruction() {
    let base = steps("let a = 6; let b = 3; return a + b;");
    for (source, cap) in [
        ("let a = 6; let b = 3; return a & b;", 100),
        ("let a = 6; let b = 3; return a | b;", 100),
        ("let a = 6; let b = 3; return a ^ b;", 100),
        ("let a = 6; let b = 3; return a << b;", 100),
        ("let a = 6; let b = 3; return a >> b;", 100),
        ("let a = 6; let b = 3; return a >>> b;", 100),
        ("let a = 6; let b = 3; return ~a + b;", 100),
        // Past 2^31 the modular road is taken: a floor, a multiply, a
        // subtraction and one more compare.
        ("let a = 4294967297; let b = 3; return a & b;", 130),
    ] {
        let cost = steps(source) as i64 - base as i64;
        println!("{source:?}: {cost} steps over `a + b`");
        assert!(cost < cap, "{source:?} cost {cost} steps over `a + b`");
    }
}

#[test]
fn the_operator_beats_the_hand_written_loop() {
    let by_hand = steps(&format!(
        "{XOR16} let h = 0; for (let i = 0; i < 1000; i = i + 1) {{ h = xor16(h, i); }} return h;"
    ));
    let by_operator = steps(
        "let h = 0; for (let i = 0; i < 1000; i = i + 1) { h = (h ^ i) & 0xffff; } return h;",
    );
    println!("1 000 xor16 by hand: {by_hand} steps; by `^` and `&`: {by_operator} steps");
    assert!(
        by_hand > 20 * by_operator,
        "the operator should be well over 20x cheaper: {by_hand} vs {by_operator}"
    );
}
