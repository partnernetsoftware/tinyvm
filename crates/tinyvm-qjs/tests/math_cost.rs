//! What a `Math` call costs in steps: the fold makes it a direct prefab
//! call with no property lookup and no receiver test, so `floor` is a
//! ToNumber, one instruction and the boxing. The demand's comparison:
//! `int_div` (rh_compat.qjs, 68 call lines downstream) against
//! `Math.trunc(a / b)`.

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

#[test]
fn one_call_costs_a_conversion_and_the_instruction() {
    let base = steps("let a = 6.5; let b = 3; return a + b;");
    for (source, cap) in [
        ("let a = 6.5; let b = 3; return Math.floor(a) + b;", 40),
        ("let a = 6.5; let b = 3; return Math.round(a) + b;", 60),
        ("let a = 6.5; let b = 3; return Math.sign(a) + b;", 50),
        ("let a = 6.5; let b = 3; return Math.min(a, b);", 50),
        ("let a = 2.0; let b = 10; return Math.pow(a, b);", 200),
    ] {
        let cost = steps(source) as i64 - base as i64;
        println!("{source:?}: {cost} steps over `a + b`");
        assert!(cost < cap, "{source:?} cost {cost} steps over `a + b`");
    }
}

/// `int_div` spelled by hand against the engine's `Math.trunc(a / b)`.
#[test]
fn trunc_of_a_quotient_beats_the_hand_written_int_div() {
    let by_hand = steps(
        "function int_div(a, b) { return (a - (a % b)) / b; }
         let n = 0; for (let i = 1; i < 1000; i = i + 1) { n = n + int_div(1000000, i); } return n;",
    );
    let by_math = steps(
        "let n = 0; for (let i = 1; i < 1000; i = i + 1) { n = n + Math.trunc(1000000 / i); } return n;",
    );
    println!("1 000 int_div by hand: {by_hand} steps; by Math.trunc: {by_math} steps");
    assert!(
        by_math * 2 < by_hand,
        "Math.trunc should be at least 2x cheaper: {by_hand} vs {by_math}"
    );
}
