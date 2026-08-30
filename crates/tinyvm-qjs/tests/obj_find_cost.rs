//! What finding a property costs, and that the cheap miss never lies.
//!
//! `__obj_find` walks the entries in order and every one ahead of the answer
//! is a miss. A miss used to go through `__str_eq` -- a call, the length
//! test, then the bytes up to the first that differs -- ~130 steps when the
//! keys shared a prefix. Now it is rejected on the length, the first word and
//! the last word before `__str_eq` is asked (2026-08-31). The prices are
//! pinned here; the correctness half runs every shape the word tests could
//! get wrong -- keys under a word, keys sharing both ends, the empty key,
//! computed keys, and a Number key against its String spelling.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

fn run(source: &str) -> (tinyvm::WasmInstance, Vec<tinyvm::Val>) {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .unwrap_or_else(|e| panic!("trap in {source:?}: {}", e.message()));
    (instance, vals)
}

fn steps(source: &str) -> u64 {
    run(source).0.last_steps()
}

fn text(source: &str) -> String {
    let (instance, vals) = run(source);
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

fn twenty_keys() -> String {
    let props: Vec<String> = (0..20).map(|i| format!("key{i:03}: {i}")).collect();
    format!("let o = {{{}}};", props.join(", "))
}

/// Reading the last of twenty keys that share their first word: 2 834 steps
/// before, 919 after (42 a miss).
#[test]
fn a_miss_on_the_way_to_the_last_key_is_cheap() {
    let build = twenty_keys();
    let base = steps(&format!("{build} return 1;"));
    let last = steps(&format!("{build} return o.key019;"));
    let first = steps(&format!("{build} return o.key000;"));
    println!(
        "20 keys: last {} steps, first {}",
        last - base,
        first - base
    );
    assert!(
        last - base < 1_300,
        "reading the last of 20 keys cost {} steps",
        last - base
    );
    assert!(
        first - base < 200,
        "reading the first key cost {} steps",
        first - base
    );
}

/// Building the literal is the same scan once per property: 27 623 before,
/// 10 203 after for twenty keys.
#[test]
fn building_a_twenty_key_literal_is_cheap() {
    let base = steps("return 1;");
    let lit = steps(&format!("{} return 1;", twenty_keys()));
    println!("20-key literal: {} steps", lit - base);
    assert!(
        lit - base < 14_000,
        "a 20-key literal cost {} steps",
        lit - base
    );
}

/// Every shape the word tests could confuse answers by its bytes.
#[test]
fn the_cheap_miss_never_confuses_two_keys() {
    for (source, want) in [
        // Under a word: the masked first word is the whole key.
        (
            "let o = {a: \"1\", b: \"2\", ab: \"3\", abc: \"4\"}; return o.a + o.b + o.ab + o.abc;",
            "1234",
        ),
        (
            "let o = {ab: \"1\", ac: \"2\", abc: \"3\", abd: \"4\"}; return o.ac + o.abd + o.ab + o.abc;",
            "2413",
        ),
        // The empty key, stored and found.
        (
            "let o = {}; o[\"\"] = \"e\"; o.a = \"a\"; return o[\"\"] + o.a;",
            "ea",
        ),
        (
            "let o = {a: \"a\"}; o[\"\"] = \"e\"; return o[\"\"] + o.a;",
            "ea",
        ),
        // Same first word, same last word, different middle: only the
        // bytes can tell, and they do.
        (
            "let o = {abcdXefgh: \"x\", abcdYefgh: \"y\"}; return o.abcdYefgh + o.abcdXefgh;",
            "yx",
        ),
        (
            "let o = {abcdefgh: \"1\", abcdefghi: \"2\", abcdXefgh: \"3\"}; return o.abcdXefgh + o.abcdefgh + o.abcdefghi;",
            "312",
        ),
        // Exactly a word, and one over.
        (
            "let o = {abcd: \"4\", abcde: \"5\", bbcd: \"b\"}; return o.abcde + o.bbcd + o.abcd;",
            "5b4",
        ),
        // A computed key is a fresh record, never the literal's pointer.
        (
            "let o = {ab: \"1\", abc: \"2\"}; let k = \"a\" + \"b\"; return o[k] + o[k + \"c\"];",
            "12",
        ),
        (
            "let o = {}; let k = \"ke\" + \"y019\"; o[k] = \"c\"; o.key000 = \"a\"; return o.key019 + o[k] + o.key000;",
            "cca",
        ),
        // A Number key and its String spelling are one property.
        (
            "let o = {}; o[1] = \"n\"; o[\"1\"] = \"s\"; return o[1] + o[\"1\"];",
            "ss",
        ),
        (
            "let o = {}; o[\"12\"] = \"s\"; o[12] = \"n\"; return o[12] + o[\"12\"];",
            "nn",
        ),
        // A missing key that shares both words with a present one.
        (
            "let o = {abcdXefgh: \"x\"}; return \"\" + o.abcdYefgh + o.abcdXefgh;",
            "undefinedx",
        ),
        // Overwriting finds the same slot.
        (
            "let o = {key000: \"a\", key001: \"b\"}; o.key001 = \"c\"; o.key000 = \"d\"; return o.key000 + o.key001 + Object.keys(o).length;",
            "dc2",
        ),
    ] {
        assert_eq!(text(source), want, "{source}");
    }
}
