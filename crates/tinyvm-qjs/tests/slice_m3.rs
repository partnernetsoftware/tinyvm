//! `s.slice(start[, end])` -- ECMA-262 22.1.3.21, on UTF-16 code-unit positions.
//!
//! Every wave-1 migration group asked for it by name (rh's `sub_string`
//! became `slice` in the mapping) and `test_harness.bounded_record_text`
//! truncates evidence with it. Until this landed a call was a bare trap, then
//! (d2e66b3) a named refusal; now it answers.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

fn text(source: &str) -> String {
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

fn traps(source: &str) -> bool {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    instance.invoke_by_name("main", &Value::args(&[])).is_err()
}

#[test]
fn both_indices_given() {
    assert_eq!(text(r#"return "abcdef".slice(1, 3);"#), "bc");
    assert_eq!(text(r#"return "abcdef".slice(0, 6);"#), "abcdef");
    assert_eq!(text(r#"return "abcdef".slice(0, 0);"#), "");
}

#[test]
fn end_defaults_to_the_length() {
    assert_eq!(text(r#"return "abcdef".slice(2);"#), "cdef");
    assert_eq!(text(r#"return "abcdef".slice(0);"#), "abcdef");
    assert_eq!(text(r#"return "".slice(0);"#), "");
}

#[test]
fn negative_indices_count_from_the_end() {
    assert_eq!(text(r#"return "abcdef".slice(-2);"#), "ef");
    assert_eq!(text(r#"return "abcdef".slice(1, -1);"#), "bcde");
    assert_eq!(text(r#"return "abcdef".slice(-4, -2);"#), "cd");
}

#[test]
fn out_of_range_indices_clamp_and_crossed_ones_are_empty() {
    assert_eq!(text(r#"return "abcdef".slice(0, 100);"#), "abcdef");
    assert_eq!(text(r#"return "abcdef".slice(-100, 2);"#), "ab");
    assert_eq!(text(r#"return "abcdef".slice(3, 1);"#), "");
    assert_eq!(text(r#"return "abcdef".slice(9);"#), "");
}

/// Positions are code units, so a two-byte `é` is one unit and an astral
/// character is two, as `length` says.
#[test]
fn positions_are_utf16_code_units() {
    assert_eq!(text("return \"caf\u{e9}\".slice(3);"), "\u{e9}");
    assert_eq!(text("return \"caf\u{e9}x\".slice(3, 4);"), "\u{e9}");
    assert_eq!(text("return \"\u{1f600}ab\".slice(2);"), "ab");
    assert_eq!(text("return \"a\u{1f600}b\".slice(1, 3);"), "\u{1f600}");
    assert_eq!(text("return \"a\u{1f600}b\".slice(-1);"), "b");
}

/// A boundary inside a surrogate pair would be a lone surrogate, which UTF-8
/// cannot carry: unrepresentable, so it traps rather than answer wrongly.
#[test]
fn a_boundary_inside_a_surrogate_pair_traps() {
    assert!(traps("return \"\u{1f600}\".slice(0, 1);"));
    assert!(traps("return \"\u{1f600}ab\".slice(1);"));
}

/// NaN is 0 and a fraction truncates, per ToIntegerOrInfinity.
#[test]
fn nan_is_zero_and_fractions_truncate() {
    assert_eq!(text(r#"return "abcdef".slice(0 / 0, 2);"#), "ab");
    assert_eq!(text(r#"return "abcdef".slice(1.9, 3.2);"#), "bc");
    assert_eq!(text(r#"return "abcdef".slice(-0.5);"#), "abcdef");
}

/// The migration's use: truncate a long record and append a marker.
#[test]
fn it_reads_the_way_the_harness_uses_it() {
    assert_eq!(
        text(r#"let t = "0123456789"; let m = 4; if (t.length <= m) { return t; } return t.slice(0, m) + "...";"#),
        "0123..."
    );
}

/// Gated: a program that never slices is byte-identical to what it was, and
/// the two forms share one core so naming both costs one body.
#[test]
fn what_slice_costs_is_written_down() {
    let base = compile_qjs_m1("return \"abcdef\".length;").expect("compiles").len();
    let two = compile_qjs_m1("return \"abcdef\".slice(1, 3).length;").expect("compiles").len();
    let one = compile_qjs_m1("return \"abcdef\".slice(1).length;").expect("compiles").len();
    let both = compile_qjs_m1("return \"abcdef\".slice(1, 3).length + \"abcdef\".slice(1).length;")
        .expect("compiles")
        .len();
    println!(
        "slice(a, b): {} bytes over a length-only program; slice(a): {}; both: {}",
        two - base,
        one - base,
        both - base
    );
    assert!(both - base < (two - base) + (one - base), "the two forms must share the core");
    assert_eq!(
        compile_qjs_m1("return 1;").expect("compiles").len(),
        9_940,
        "a program that never slices pays nothing"
    );
}

/// A non-negative index never walks past itself: `slice(0, 10)` on a
/// 1000-character string fits in a budget that the whole-string walk did
/// not (78 000 steps measured through the CLI at 1012da1).
#[test]
fn a_non_negative_slice_does_not_walk_the_whole_string() {
    let source = r#"let s = ""; for (let i = 0; i < 100; i = i + 1) { s = s + "0123456789"; } return s.slice(0, 10);"#;
    let wasm = compile_qjs_m1(source).expect("compiles");
    // Enough for the 100-iteration build (~880 000 steps at 8 800 each) but
    // not for 78 000 more on top: the slice itself must be a few hundred.
    // The baseline returns the string itself: `.length` would walk it too.
    let build_only = compile_qjs_m1(r#"let s = ""; for (let i = 0; i < 100; i = i + 1) { s = s + "0123456789"; } return s;"#).expect("compiles");
    let steps = |wasm: &[u8]| -> u64 {
        let module = WasmModule::from_bytes_with(wasm, Limits::default()).expect("loads");
        let mut instance = module.instantiate().expect("instantiates");
        instance.invoke_by_name("main", &Value::args(&[])).expect("runs");
        instance.last_steps()
    };
    let slice_cost = steps(&wasm) - steps(&build_only);
    assert!(slice_cost < 3_000, "slice(0, 10) cost {slice_cost} steps");
}
