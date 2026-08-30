//! What the 2026-08-31 array methods cost per element, pinned.
//!
//! `indexOf` / `includes` on an Array, `concat` and `join` landed together
//! because the downstream scripts had spelled each by hand (a `for … of`
//! with `===`, a push loop, a `first ? x : acc + "/" + x` loop). The
//! hand-written forms priced at ~146 steps a loop pass; a prefab has to
//! beat that or it is not worth the bytes.

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

const N: u64 = 1_000;
const NUMBERS: &str = "let a = []; for (let i = 0; i < 1000; i = i + 1) { a.push(i); }";
const STRINGS: &str =
    "let a = []; for (let i = 0; i < 1000; i = i + 1) { a.push(\"abcdefghij\"); }";

#[test]
fn a_miss_costs_a_strict_eq_per_element() {
    let base = steps(&format!("{NUMBERS} return a.length;"));
    let idx = steps(&format!("{NUMBERS} return a.indexOf(-1);"));
    let inc = steps(&format!("{NUMBERS} return a.includes(-1);"));
    let per_idx = (idx - base) as f64 / N as f64;
    let per_inc = (inc - base) as f64 / N as f64;
    println!("array indexOf miss {per_idx:.1}/element, includes miss {per_inc:.1}/element");
    assert!(
        per_idx < 60.0,
        "indexOf miss cost {per_idx:.1} steps an element"
    );
    assert!(
        per_inc < 65.0,
        "includes miss cost {per_inc:.1} steps an element"
    );
    // Strings compare by bytes through `__str_eq` once the tags agree.
    let base = steps(&format!("{STRINGS} return a.length;"));
    let idx = steps(&format!("{STRINGS} return a.indexOf(\"zz\");"));
    let per = (idx - base) as f64 / N as f64;
    println!("array indexOf miss over Strings {per:.1}/element");
    assert!(
        per < 90.0,
        "indexOf miss over Strings cost {per:.1} steps an element"
    );
}

#[test]
fn concat_and_join_are_linear_in_the_elements() {
    let base = steps(&format!("{STRINGS} return a.length;"));
    let cat = steps(&format!("{STRINGS} return a.concat(a).length;"));
    let per = (cat - base) as f64 / (2 * N) as f64;
    println!("concat {per:.1}/element copied");
    assert!(per < 40.0, "concat cost {per:.1} steps an element");
    let join = steps(&format!("{STRINGS} return a.join(\",\").length;"));
    let per = (join - base) as f64 / N as f64;
    println!("join over 10-byte Strings {per:.1}/element (eleven bytes written each)");
    assert!(per < 260.0, "join cost {per:.1} steps an element");
    // A Number element is a `__num_to_string` each: that price is the
    // conversion's (tests/num_to_string_fast.rs), not the join's.
    let base = steps(&format!("{NUMBERS} return a.length;"));
    let join = steps(&format!("{NUMBERS} return a.join(\",\").length;"));
    let per = (join - base) as f64 / N as f64;
    println!("join over Numbers {per:.1}/element");
    assert!(
        per < 900.0,
        "join over Numbers cost {per:.1} steps an element"
    );
}
