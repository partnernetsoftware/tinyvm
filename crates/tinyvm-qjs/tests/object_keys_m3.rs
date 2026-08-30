//! `Object.keys(o)` -- ECMA-262 20.1.2.17, folded at the parser into a gated
//! method call.
//!
//! Every expectation runs: compile -> tinyvm's load gate -> instantiate ->
//! `invoke_by_name("main")`.
//!
//! # Why this and not `for … in`
//!
//! The downstream migration corpus (71 rh scripts moving to `.qjs`) iterates
//! with `for x in <expr>` 458 times. Classified by what `<expr>` is: 352 are
//! arrays reached by name, 172 are `0..N` ranges, 88 are arrays from
//! split/read_dir/args, and **12** go through `.keys()`. So `for … of`
//! already covers the bulk, ranges are a mechanical rewrite, and key
//! enumeration is this one function -- not a `for … in` statement, which
//! would be a second loop lowering for twelve sites.
//!
//! # Why a fold
//!
//! Same reason as `Number(x)`: the method layer already gates per name and
//! emits nothing for a program that never writes it. `Object.keys(x)` becomes
//! `x.__keys()` before resolution, so `Object` is never looked up, and a
//! script that declares its own `Object` keeps it.

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
            let len = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4")) as usize;
            String::from_utf8(bytes[at + 4..at + 4 + len].to_vec()).expect("utf-8")
        }
        Value::Number(x) => format!("{x}"),
        other => panic!("{source:?}: unexpected {other:?}"),
    }
}

/// Keys come back in insertion order, which for a record of own string keys
/// is the whole of 20.1.2.17's ordering rule.
#[test]
fn keys_come_back_in_insertion_order() {
    let source = "const o = { b: 1, a: 2, c: 3 };
    let s = \"\";
    for (const k of Object.keys(o)) { s = s + k; }
    return s;";
    assert_eq!(text(source), "bac");
}

/// An empty object has no keys, and that is an empty array, not a trap.
#[test]
fn an_empty_object_has_no_keys() {
    assert_eq!(text("return Object.keys({}).length;"), "0");
}

/// A key written twice is one key: 13.2.5.5 makes the second write an
/// overwrite, and the record has one entry for it.
#[test]
fn a_repeated_key_is_one_key() {
    assert_eq!(text("return Object.keys({ a: 1, a: 2 }).length;"), "1");
}

/// The keys are usable as keys: reading `o[k]` for each gives the values.
#[test]
fn the_keys_index_back_into_the_object() {
    let source = "const o = { x: 10, y: 20 };
    let total = 0;
    for (const k of Object.keys(o)) { total = total + o[k]; }
    return total;";
    assert_eq!(text(source), "30");
}

/// Keys added after the literal count too.
#[test]
fn keys_added_by_assignment_are_included() {
    let source = "const o = { a: 1 };
    o.b = 2;
    o[\"c\"] = 3;
    return Object.keys(o).length;";
    assert_eq!(text(source), "3");
}

/// It reads the way the corpus writes it.
#[test]
fn it_reads_the_way_the_corpus_uses_it() {
    let source = "const seen = {};
    for (const name of [\"dev\", \"release\", \"dev\"]) { seen[name] = true; }
    let n = 0;
    for (const profile of Object.keys(seen)) { n = n + 1; }
    return n;";
    assert_eq!(text(source), "2");
}

/// A script that declares its own `Object` gets its own.
///
/// The fold is a default and a declaration is a deliberate act -- the
/// precedence `JSON` and `Number` already follow.
#[test]
fn a_script_that_declares_object_gets_its_own() {
    let source = "const Object = { keys: function (o) { return [\"mine\"]; } };
    return Object.keys({ a: 1 })[0];";
    assert_eq!(text(source), "mine");
}

/// Wrong arity is refused at compile time, by name.
#[test]
fn the_wrong_arity_is_refused_by_name() {
    let error = compile_qjs_m1("return Object.keys();").expect_err("no receiver");
    assert!(error.message.contains("Object.keys"), "{}", error.message);
}

/// A program that never writes `Object.keys` carries none of it.
#[test]
fn a_program_that_never_asks_for_keys_pays_nothing() {
    for (source, want) in [
        ("return 1;", 10_198),
        ("let o = {a:1}; o.b = 2; return o.a;", 10_929), /* +23 on 2026-08-29: a program that reads a static property can reach `__obj_get` with a String receiver, and the arm that names the missing property is 23 bytes; see runtime.rs `FAULT_MISSING_STRING_METHOD` */
    ] {
        let n = compile_qjs_m1(source).expect("compiles").len();
        assert_eq!(n, want, "{source:?} is {n} bytes");
    }
}

/// What it costs, written down: one prefab plus the array set it needs.
#[test]
fn what_object_keys_costs_is_written_down() {
    let size = |src: &str| compile_qjs_m1(src).expect("compiles").len();
    let base = size("const o = {a:1}; const a = [1]; return a.length;");
    let with = size("const o = {a:1}; const a = [1]; return Object.keys(o).length;");
    let cost = with - base;
    println!("Object.keys: {cost} bytes over a program that already has arrays and objects");
    assert!(
        cost > 0 && cost < 600,
        "Object.keys costs {cost} bytes, which is a surprise"
    );
}
