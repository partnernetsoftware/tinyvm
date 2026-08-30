//! What character access costs, pinned: `charCodeAt`, `s[i]` and
//! `substring` walk the string's bytes to find a UTF-16 position, the
//! way `slice` does, so each is linear in the position -- and a loop over
//! every position is quadratic, which a script that spells `for (i <
//! s.length) s[i]` has to know. The walk steps over eight plain-ASCII
//! bytes at a time (`ascii_skip`, shared with `slice`), so the constant
//! is ~2 steps a unit on ASCII text rather than ~40.

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

const BUILD: &str = r#"let s = ""; for (let i = 0; i < 100; i = i + 1) { s = s + "0123456789"; }"#;

#[test]
fn a_position_costs_its_walk() {
    let base = steps(&format!("{BUILD} return s.length;"));
    let code = steps(&format!("{BUILD} return s.charCodeAt(999);"));
    let per = (code - base) as f64 / 1_000.0;
    println!(
        "charCodeAt(999) on 1 000 ASCII characters: {} steps, {per:.1} a unit walked",
        code - base
    );
    assert!(per < 6.0, "charCodeAt walked at {per:.1} steps a unit");
    let index = steps(&format!("{BUILD} return s[999];"));
    let per = (index - base) as f64 / 1_000.0;
    println!(
        "s[999] on 1 000 ASCII characters: {} steps, {per:.1} a unit walked",
        index - base
    );
    assert!(per < 6.0, "s[i] walked at {per:.1} steps a unit");
    let sub = steps(&format!("{BUILD} return s.substring(990).length;"));
    let per = (sub - base) as f64 / 1_000.0;
    println!(
        "substring(990) on 1 000 ASCII characters: {} steps, {per:.1} a unit walked",
        sub - base
    );
    assert!(per < 6.0, "substring walked at {per:.1} steps a unit");
}

#[test]
fn a_loop_over_every_position_is_quadratic_and_says_so() {
    let base = steps(&format!("{BUILD} return s.length;"));
    let all = steps(&format!(
        "{BUILD} let n = 0; for (let i = 0; i < 1000; i = i + 1) {{ n = n + s.charCodeAt(i); }} return n;"
    ));
    println!(
        "charCodeAt at every one of 1 000 positions: {} steps",
        all - base
    );
    // 1 000 walks averaging 500 units, eight ASCII bytes a step, plus the
    // loop: ~3.0M measured on 2026-08-31 (23.3M before the eight-byte step,
    // 39.7M when every character on the way was decoded).
    assert!(all - base < 5_000_000, "the loop cost {} steps", all - base);
}
