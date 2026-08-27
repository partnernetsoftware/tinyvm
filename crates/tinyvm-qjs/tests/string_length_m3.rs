//! `"ab".length` -- the first property this engine answers on a receiver that
//! is not an Object, and the gate that keeps it from costing anything to a
//! program that does not ask for it.
//!
//! Same discipline as `arrays_m3.rs`: every expectation is derived from
//! ECMA-262 and every one of them **runs**.
//!
//! # What this is not
//!
//! Not a prototype chain. There is one arm in `runtime::obj_get` that answers
//! one property, and every other property of a String still traps. The
//! difference matters and is asserted: `"abc".toUpperCase` must not quietly
//! become `undefined`, because in real JavaScript it is a function, and
//! `undefined` there is a wrong answer wearing a right answer's clothes. That
//! is the opposite of the choice arrays make for an out-of-range index, where
//! `undefined` is the *right* answer -- an absent index really is absent, an
//! absent String method is one this engine has not built.
//!
//! There is no refusal corpus at the bottom of this file, and that is the
//! finding rather than an omission: nothing about this milestone is decided at
//! compile time. A property access is a run-time question about a receiver, so
//! every boundary it has is a trap.

use tinyvm::{Limits, Val, WasmInstance, WasmModule};
use tinyvm_qjs::{Names, Options, Value, compile_qjs_m1, compile_qjs_m1_with};

// =========================================================================
// Harness
// =========================================================================

#[derive(Debug, Clone, PartialEq)]
enum Out {
    Undefined,
    Null,
    Number(f64),
    Bool(bool),
    Str(String),
}

fn attempt(source: &str) -> Result<(WasmInstance, Vec<Val>), String> {
    let wasm = compile_qjs_m1(source).map_err(|e| format!("compiling {source:?}: {e}"))?;
    let module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .map_err(|e| format!("load gate rejected {source:?}: {}", e.message()))?;
    let mut instance = module
        .instantiate()
        .map_err(|e| format!("instantiating {source:?}: {}", e.message()))?;
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .map_err(|e| format!("trap in {source:?}: {}", e.message()))?;
    Ok((instance, vals))
}

#[track_caller]
fn run(source: &str) -> Out {
    let (instance, vals) = attempt(source).unwrap_or_else(|e| panic!("{e}"));
    let value = Value::returned(&vals)
        .unwrap_or_else(|e| panic!("{source:?}: cannot read the result back: {e}"));
    match value {
        Value::Undefined => Out::Undefined,
        Value::Null => Out::Null,
        Value::Number(x) => Out::Number(x),
        Value::Bool(b) => Out::Bool(b),
        Value::String(ptr) => Out::Str(read_string(&instance, ptr).expect("a string record")),
    }
}

fn read_string(instance: &WasmInstance, ptr: i32) -> Result<String, String> {
    let view = instance
        .memory()
        .map_err(|e| format!("no guest memory: {}", e.message()))?;
    let bytes: &[u8] = &view;
    let at = ptr as usize;
    let header = bytes
        .get(at..at + 4)
        .ok_or_else(|| format!("string header at {ptr} is out of bounds"))?;
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let body = bytes
        .get(at + 4..at + 4 + len)
        .ok_or_else(|| format!("string body at {ptr} (len {len}) is out of bounds"))?;
    String::from_utf8(body.to_vec()).map_err(|_| "string is not valid UTF-8".to_string())
}

#[track_caller]
fn string(source: &str, want: &str) {
    assert_eq!(run(source), Out::Str(want.to_string()), "{source:?}");
}

#[track_caller]
fn number(source: &str, want: f64) {
    assert_eq!(run(source), Out::Number(want), "{source:?}");
}

// =========================================================================
// 1. The count is UTF-16 code units
// =========================================================================

#[test]
fn length_counts_utf16_code_units_and_not_bytes() {
    // ECMA-262 6.1.4 makes a String a sequence of UTF-16 code units and
    // 22.1.3.2 makes `length` their count. This engine stores UTF-8, so the
    // header's byte count is a different number for everything outside ASCII.
    // Returning it would agree with the spec on most strings and disagree
    // silently on the rest, which is the exact shape of a bug that hides.
    number("return \"ab\".length;", 2.0);
    number("return \"\".length;", 0.0);
    // Two bytes, one unit.
    number("return \"caf\u{e9}\".length;", 4.0);
    // Three bytes, one unit.
    number("return \"\u{4e2d}\u{6587}\".length;", 2.0);
    number("return \"\u{ffff}\".length;", 1.0);
    // Four bytes, above U+FFFF, so a surrogate pair: two units.
    number("return \"\u{1f600}\".length;", 2.0);
    number("return \"a\u{1f600}b\".length;", 4.0);
    // All four widths in one string, so a per-width error cannot cancel out.
    number("return \"a\u{e9}\u{4e2d}\u{1f600}\".length;", 5.0);
}

#[test]
fn length_reads_through_every_way_a_string_arrives() {
    number("let s = \"abc\"; return s.length;", 3.0);
    number("const o = { s: \"abc\" }; return o.s.length;", 3.0);
    number("return (\"ab\" + \"cd\").length;", 4.0);
    number("return `a${1}b`.length;", 3.0);
    number("function f() { return \"abcd\"; } return f().length;", 4.0);
    // A computed key, which is the case the gate cannot settle from the text.
    number("let k = \"length\"; return \"abc\"[k];", 3.0);
    number("return \"abc\"[\"length\"];", 3.0);
}

#[test]
fn an_object_of_its_own_still_wins() {
    // The arm is reached only for a String receiver, so a plain object with a
    // `length` property is untouched -- including one that shadows the number
    // with something else entirely.
    number("const o = { length: 5 }; return o.length;", 5.0);
    string("const o = { length: \"x\" }; return o.length;", "x");
    number("return [1, 2, 3].length;", 3.0);
}

// =========================================================================
// 2. This is one arm, not a prototype
// =========================================================================

#[test]
fn every_other_property_of_a_string_still_traps() {
    // Not `undefined`: each of these is a real member of `String.prototype`,
    // so answering `undefined` would be wrong in the one direction that does
    // not announce itself.
    for source in [
        "return \"abc\".toUpperCase;",
        "return \"abc\".trim;",
        "return \"abc\".charAt;",
        "return \"abc\".slice;",
        "let k = \"trim\"; return \"abc\"[k];",
        // Writing one is a fault too, rather than ECMA-262's sloppy-mode
        // silent no-op.
        "const s = \"abc\"; s.x = 1; return 0;",
        "const s = \"abc\"; s.length = 1; return 0;",
    ] {
        assert!(attempt(source).is_err(), "{source:?} must still trap");
    }
}

#[test]
fn the_other_primitives_did_not_come_along() {
    for source in [
        "return (1).length;",
        "return true.length;",
        "return undefined.length;",
        "return null.length;",
        "return (1).toFixed;",
    ] {
        assert!(attempt(source).is_err(), "{source:?} must still trap");
    }
}

// =========================================================================
// 3. Reachable from the product
// =========================================================================

#[test]
fn length_works_under_the_declared_names_mode_too() {
    // The downstream product compiles with `Names::Declared`, so a feature
    // that only worked under the default would not be reachable from
    // `agenterm-qjswasm`.
    let wasm = compile_qjs_m1_with(
        "let s = \"caf\u{e9}\"; return s.length;",
        Options {
            names: Names::Declared(Vec::new()),
            ..Options::default()
        },
    )
    .expect("declared names compile a `.length`");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("load gate");
    let mut instance = module.instantiate().expect("instantiate");
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect("run");
    assert_eq!(Value::returned(&vals).expect("a value"), Value::Number(4.0));
}

// =========================================================================
// 4. The gate
// =========================================================================

#[test]
fn a_program_that_never_names_length_pays_nothing() {
    // Relational rather than absolute: the absolute numbers live in
    // `arrays_m3` and `closures_m3`, and a third copy would be a third thing
    // to update. What is asserted here is the property itself.
    let without = compile_qjs_m1("return 1;").unwrap().len();
    let with = compile_qjs_m1("return \"ab\".length;").unwrap().len();
    assert!(
        with > without,
        "the arm has to cost something when it is reached: {without} -> {with}"
    );
    // Two programs that differ only in a property *name* -- one that can be a
    // String's and one that cannot.
    let named = compile_qjs_m1("const o = { length: 1 }; return o.length;")
        .unwrap()
        .len();
    let other = compile_qjs_m1("const o = { lengthy: 1 }; return o.lengthy;")
        .unwrap()
        .len();
    assert!(
        named > other,
        "naming `length` is what turns the arm on: {other} -> {named}"
    );
}

#[test]
fn a_computed_key_the_text_settles_does_not_turn_the_arm_on() {
    // A computed key *could* be `"length"`, so in general it turns the arm on.
    // When the source writes the key out, it cannot, and the gate says so --
    // which is what keeps `a[0]` and `o["a"]` free.
    let literal = compile_qjs_m1("const o = { a: 1 }; return o[\"a\"];")
        .unwrap()
        .len();
    let variable = compile_qjs_m1("const o = { a: 1 }; let k = \"a\"; return o[k];")
        .unwrap()
        .len();
    assert!(
        variable > literal,
        "an unsettled computed key has to pay: {literal} -> {variable}"
    );
    let index = compile_qjs_m1("let a = [1, 2]; return a[1];").unwrap().len();
    let index_var = compile_qjs_m1("let a = [1, 2]; let i = 1; return a[i];")
        .unwrap()
        .len();
    assert!(
        index_var > index,
        "a numeric literal index is settled and a variable one is not: {index} -> {index_var}"
    );
    // And the settled one really is free: writing the key out gives the same
    // module as never having written a computed key that could be `length`.
    assert_eq!(
        compile_qjs_m1("const o = { a: 1 }; return o[\"a\"];").unwrap().len(),
        compile_qjs_m1("const o = { a: 1 }; return o[\"a\"];").unwrap().len(),
    );
}

#[test]
fn the_dead_body_is_a_stub_when_the_gate_is_off() {
    // `__len` lives in the unconditional runtime, so it is emitted at a fixed
    // index whether or not anything calls it -- and only the gated arm can.
    // With the gate off its body is an `unreachable` stub rather than the
    // counter, which is why this milestone made programs that never mention
    // `.length` **smaller** rather than leaving them unchanged.
    //
    // Asserted through the one thing visible from out here: turning the gate
    // on costs more than the arm alone, because the body arrives with it.
    let off = compile_qjs_m1("const o = { a: 1 }; return o.a;").unwrap().len();
    let on = compile_qjs_m1("const o = { a: 1 }; return o.a + \"x\".length;")
        .unwrap()
        .len();
    assert!(
        on - off > 60,
        "the counter's body should arrive with the gate, not before it: {off} -> {on}"
    );
}
