//! What `sort` costs, pinned -- so the two hand-written merge sorts in the
//! downstream corpus (`prune_target_incremental.qjs`, `check.qjs`) can be
//! retired against a number.
//!
//! Each program builds 1 000 elements from a small linear congruential
//! generator, sorts, and then walks the result to prove it is ordered; the
//! walk is priced separately and subtracted, so the number is the sort's.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

fn run(source: &str) -> (Value, u64) {
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
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .unwrap_or_else(|e| panic!("trap in {source:?}: {}", e.message()));
    (
        Value::returned(&vals).expect("a value"),
        instance.last_steps(),
    )
}

const N: f64 = 1_000.0;
const NUMBERS: &str = "let a = []; let x = 7; for (let i = 0; i < 1000; i = i + 1) { x = (x * 1103515245 + 12345) % 2147483648; a.push(x % 100000); }";
const STRINGS: &str = "let a = []; let x = 7; for (let i = 0; i < 1000; i = i + 1) { x = (x * 1103515245 + 12345) % 2147483648; a.push(\"k\" + (x % 100000)); }";
const ORDERED: &str = "let ok = 1; for (let i = 1; i < a.length; i = i + 1) { if (a[i] < a[i - 1]) { ok = 0; } } return ok;";

#[test]
fn a_thousand_numbers_with_a_comparator() {
    let (_, base) = run(&format!("{NUMBERS} {ORDERED}"));
    let (ok, sorted) = run(&format!("{NUMBERS} a.sort((p, q) => p - q); {ORDERED}"));
    assert_eq!(ok, Value::Number(1.0), "the result is ordered");
    let per = (sorted - base) as f64 / (N * N.log2());
    println!(
        "sort of 1 000 Numbers with `(p, q) => p - q`: {} steps, {per:.1} per n log n",
        sorted - base
    );
    assert!(per < 220.0, "sort cost {per:.1} steps per n log n");
}

#[test]
fn a_thousand_strings_by_default_order() {
    let (_, base) = run(&format!("{STRINGS} {ORDERED}"));
    let (ok, sorted) = run(&format!("{STRINGS} a.sort(); {ORDERED}"));
    assert_eq!(ok, Value::Number(1.0), "the result is ordered");
    let per = (sorted - base) as f64 / (N * N.log2());
    println!(
        "sort of 1 000 Strings by default order: {} steps, {per:.1} per n log n",
        sorted - base
    );
    assert!(per < 520.0, "sort cost {per:.1} steps per n log n");
    // And the hand-written shape it replaces, for the record: the same
    // merge `prune_target_incremental.qjs` writes, over the same input.
    let merge = "function sort_range(values, start, end) { if (end - start <= 1) { const one = []; if (end - start === 1) { one.push(values[start]); } return one; } const middle = start + (end - start - (end - start) % 2) / 2; const left = sort_range(values, start, middle); const right = sort_range(values, middle, end); const merged = []; let i = 0; let j = 0; while (i < left.length && j < right.length) { if (right[j] < left[i]) { merged.push(right[j]); j = j + 1; } else { merged.push(left[i]); i = i + 1; } } while (i < left.length) { merged.push(left[i]); i = i + 1; } while (j < right.length) { merged.push(right[j]); j = j + 1; } return merged; }";
    let (ok, hand) = run(&format!(
        "{merge} {STRINGS} a = sort_range(a, 0, a.length); {ORDERED}"
    ));
    assert_eq!(ok, Value::Number(1.0));
    println!(
        "the hand-written merge sort over the same 1 000 Strings: {} steps ({:.1}x)",
        hand - base,
        (hand - base) as f64 / (sorted - base) as f64
    );
}
