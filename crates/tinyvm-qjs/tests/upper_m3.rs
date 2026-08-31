//! `toUpperCase` (ECMA-262 22.1.3.31), landed 2026-08-31 in the fourth
//! A13 batch: the mirror of `toLowerCase` -- the same run table inverted
//! at build time, the same binary search, the ASCII arm subtracting 32.
//!
//! The mirror is the *simple* case mapping, and three divergences from a
//! browser's full mapping are deliberate and pinned here: `ß` stays `ß`
//! (the full mapping is the one-to-two `"SS"`, which a one-to-one table
//! cannot spell; the simple mapping is identity), `µ` and the four
//! titlecase forms stay themselves. `ς` goes to `Σ` by a hand-added run.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

fn string(source: &str, want: &str) {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .unwrap_or_else(|e| panic!("trap in {source:?}: {}", e.message()));
    let Ok(Value::String(p)) = Value::returned(&vals) else {
        panic!("{source:?} did not answer a String");
    };
    let memory = instance.memory().expect("guest memory");
    let bytes: &[u8] = &memory;
    let at = p as usize;
    let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let got = std::str::from_utf8(&bytes[at + 4..at + 4 + len]).expect("UTF-8");
    assert_eq!(got, want, "{source:?}");
}

#[test]
fn ascii_uppercases() {
    string(
        "return \"hello World 123\".toUpperCase();",
        "HELLO WORLD 123",
    );
    string("return \"ABC\".toUpperCase();", "ABC");
    string("return \"\".toUpperCase();", "");
}

#[test]
fn the_inverted_table_reaches_the_same_pairs_backwards() {
    string("return \"caf\u{e9}\".toUpperCase();", "CAF\u{c9}");
    string(
        "return \"\u{43c}\u{438}\u{440}\".toUpperCase();",
        "\u{41c}\u{418}\u{420}",
    );
    string(
        "return \"\u{3c0}\u{3b1}\u{3c1}\u{3ac}\".toUpperCase();",
        "\u{3a0}\u{391}\u{3a1}\u{386}",
    );
    // ǆ -> Ǆ: two uppercase forms share this lowercase and the smallest
    // wins, which is Unicode's own simple mapping.
    string("return \"\u{1c6}\".toUpperCase();", "\u{1c4}");
    // A code point whose uppercase form is one byte wider.
    string("return \"\u{250}\".toUpperCase();", "\u{2c6f}");
    // Astral characters pass through untouched.
    string("return \"a\u{1f600}b\".toUpperCase();", "A\u{1f600}B");
}

#[test]
fn final_sigma_uppercases_and_the_recorded_divergences_stay_put() {
    // ς -> Σ, the hand-added run.
    string(
        "return \"\u{3c4}\u{3ad}\u{3bb}\u{3bf}\u{3c2}\".toUpperCase();",
        "\u{3a4}\u{388}\u{39b}\u{39f}\u{3a3}",
    );
    // ß stays: the full mapping is "SS", the simple one is identity, and
    // a one-to-one table takes the simple one. Recorded, not accidental.
    string("return \"stra\u{df}e\".toUpperCase();", "STRA\u{df}E");
    // µ and a titlecase form are nobody's lowercase in the table.
    string("return \"\u{1c5} \u{b5}\".toUpperCase();", "\u{1c5} \u{b5}");
    // And lowercasing what uppercased still answers the original.
    string(
        "return \"caf\u{e9}\".toUpperCase().toLowerCase();",
        "caf\u{e9}",
    );
}

fn bytes(source: &str) -> usize {
    compile_qjs_m1(source)
        .unwrap_or_else(|e| panic!("compiling {source:?}: {e}"))
        .len()
}

#[test]
fn each_direction_carries_only_its_own_table() {
    let rows = [
        ("return 1;", 10_198),
        ("return \"a\".toLowerCase();", 19_294),
        ("return \"a\".toUpperCase();", 19_194),
        ("return \"a\".toUpperCase().toLowerCase();", 27_669),
    ];
    let got: Vec<usize> = rows.iter().map(|(source, _)| bytes(source)).collect();
    for ((source, _), n) in rows.iter().zip(&got) {
        println!("{source:?} is {n} bytes");
    }
    for ((source, want), n) in rows.iter().zip(&got) {
        assert_eq!(n, want, "{source:?} is {n} bytes");
    }
}
