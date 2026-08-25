//! An ECMA-262 conformance corpus for objects: literals, property keys,
//! reading, writing, order, and the places an object goes.
//!
//! Every expectation here was written from the specification *before* it was
//! run -- the section is named at each one -- and every one of them executes:
//! compile -> tinyvm's load gate -> instantiate -> `invoke_by_name("main")`.
//! Where the engine and ECMA-262 genuinely part company the current behaviour
//! is asserted and the arm is marked `DIVERGENCE:` with the answer real
//! JavaScript gives, so the gap is a line of code somebody has to delete
//! rather than a fact nobody wrote down.
//!
//! # How this differs from `objects_m3.rs`
//!
//! `objects_m3.rs` is the implementation's own suite: it was written next to
//! the code and it knows why the code is shaped the way it is. This file was
//! written from the other side, off the standard, and its job is the cases the
//! implementer had no reason to think of -- reserved words as property names,
//! integer-index key ordering, a read that must not create a slot, the
//! prototype-provided properties a real script reaches for by reflex. The
//! overlap between the two files is deliberate: an expectation that only one
//! of them holds is an expectation with one witness.
//!
//! # Why some tests read guest memory
//!
//! Property order (10.1.11.1) has no observable surface in this subset: no
//! `Object.keys`, no `for...in`, no `JSON.stringify`. Those tests walk the
//! object record in linear memory. They restate the layout from outside the
//! crate, which is the only place a contract can be checked from.

use tinyvm::{Limits, Val, WasmInstance, WasmModule};
use tinyvm_qjs::{Boundary, CompileError, Value, compile_qjs_m1};

// =========================================================================
// Harness
// =========================================================================

/// What `main` returned, with a String's text already resolved.
#[derive(Debug, Clone, PartialEq)]
enum Out {
    Undefined,
    Null,
    Number(f64),
    Bool(bool),
    Str(String),
}

/// `repr.rs` numbers the Object tag 5. Restated rather than imported: `repr`
/// is crate-private, and a contract checked only from inside is not checked.
const TAG_OBJECT: i32 = 5;

/// The record: `[len: i32][cap: i32][entries: i32]`, then 16 bytes per entry
/// (`[key: i32][tag: i32][payload: i64]`).
const OBJ_LEN: usize = 0;
const OBJ_CAP: usize = 4;
const OBJ_ENTRIES: usize = 8;
const ENTRY_BYTES: usize = 16;

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
        Value::String(ptr) => {
            let view = instance.memory().expect("guest memory");
            Out::Str(read_string_at(&view, ptr).expect("a string record"))
        }
    }
}

/// A string record in guest memory: `[len: i32][utf8 bytes]`.
fn read_string_at(bytes: &[u8], ptr: i32) -> Result<String, String> {
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

fn word(bytes: &[u8], at: usize) -> i32 {
    i32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// The own property keys of the object `main` returned, in record order.
#[track_caller]
fn returned_keys(source: &str) -> Vec<String> {
    returned_record(source).0
}

/// The keys, plus `(len, cap)` -- the sizing decision, for the tests that are
/// about growth rather than about semantics.
#[track_caller]
fn returned_record(source: &str) -> (Vec<String>, i32, i32) {
    let (instance, vals) = attempt(source).unwrap_or_else(|e| panic!("{e}"));
    let [Val::I32(tag), Val::I64(payload)] = vals.as_slice() else {
        panic!("{source:?}: want one V1 pair back, got {vals:?}");
    };
    assert_eq!(*tag, TAG_OBJECT, "{source:?}: want an Object back");
    let view = instance.memory().expect("guest memory");
    let bytes: &[u8] = &view;
    let object = *payload as u32 as usize;
    let len = word(bytes, object + OBJ_LEN);
    let cap = word(bytes, object + OBJ_CAP);
    let entries = word(bytes, object + OBJ_ENTRIES) as usize;
    let keys = (0..len as usize)
        .map(|i| {
            let key = word(bytes, entries + i * ENTRY_BYTES);
            read_string_at(bytes, key).expect("a key string record")
        })
        .collect();
    (keys, len, cap)
}

#[track_caller]
fn number(source: &str, want: f64) {
    match run(source) {
        Out::Number(got) if want.is_nan() && got.is_nan() => {}
        Out::Number(got) if got.to_bits() == want.to_bits() => {}
        other => panic!("{source:?}: want Number({want}), got {other:?}"),
    }
}

#[track_caller]
fn boolean(source: &str, want: bool) {
    assert_eq!(run(source), Out::Bool(want), "{source:?}");
}

#[track_caller]
fn text(source: &str, want: &str) {
    assert_eq!(run(source), Out::Str(want.to_string()), "{source:?}");
}

#[track_caller]
fn undefined(source: &str) {
    assert_eq!(run(source), Out::Undefined, "{source:?}");
}

#[track_caller]
fn null(source: &str) {
    assert_eq!(run(source), Out::Null, "{source:?}");
}

/// Compiles, clears the load gate, then faults at run time. In this subset a
/// thrown TypeError has nowhere to go -- there is no `try` -- so a trap is how
/// one reaches the host.
#[track_caller]
fn traps(source: &str) {
    match attempt(source) {
        Err(message) => assert!(
            message.contains("trap in"),
            "{source:?} failed for the wrong reason: {message}"
        ),
        Ok((_, vals)) => panic!("{source:?} produced {vals:?} instead of trapping"),
    }
}

#[track_caller]
fn refuse(source: &str) -> CompileError {
    match compile_qjs_m1(source) {
        Ok(bytes) => panic!(
            "{source:?} compiled to {} bytes; expected a capability diagnostic",
            bytes.len()
        ),
        Err(e) => e,
    }
}

/// A refusal in the fixed wording `diag::unsupported` locks.
#[track_caller]
fn refuses_capability(source: &str, construct: &str, boundary: Boundary) {
    let error = refuse(source);
    assert_eq!(
        error.message,
        format!("this engine does not support {construct} yet"),
        "{source:?}"
    );
    assert_eq!(error.boundary, boundary, "{source:?}");
}

/// A refusal whose sentence this corpus deliberately does not pin, because
/// the construct it names is not the construct in the source. Asserts the
/// part that is a product promise: the engine speaks about itself, gives an
/// offset, and never says "syntax error".
#[track_caller]
fn refuses_somehow(source: &str) -> CompileError {
    let error = refuse(source);
    assert!(
        error.message.starts_with("this engine"),
        "{source:?}: a diagnostic must speak for the engine, got {:?}",
        error.message
    );
    assert!(
        !error.message.to_lowercase().contains("syntax error"),
        "{source:?}: never a bare syntax error, got {:?}",
        error.message
    );
    error
}

// =========================================================================
// 13.2.5 Object Initializer -- the literal forms
// =========================================================================

/// The empty object. 13.2.5.4: `{}` evaluates to `OrdinaryObjectCreate` with
/// no properties -- an object, truthy, with no own keys, not a falsy nothing.
#[test]
fn the_empty_literal_is_an_object_with_no_properties() {
    text("return typeof ({});", "object");
    boolean("const o = {}; return !!o;", true);
    boolean("const o = {}; return o === o;", true);
    let (keys, len, _) = returned_record("return {};");
    assert!(keys.is_empty(), "no own property keys, got {keys:?}");
    assert_eq!(len, 0);
    undefined("const o = {}; return o.anything;");
}

/// 12.9.6 and the ObjectLiteral production: a trailing comma is grammar, not
/// an extra property. One is not the same object as none, and neither grows a
/// slot for the comma.
#[test]
fn a_trailing_comma_adds_no_property() {
    assert_eq!(returned_keys("return { a: 1, };"), ["a"]);
    assert_eq!(returned_keys("return { a: 1, b: 2, };"), ["a", "b"]);
    number("const o = { a: 1, }; return o.a;", 1.0);
    number("const o = { a: 1, b: 2, }; return o.a + o.b;", 3.0);
    // `{,}` has no production: PropertyDefinitionList cannot be empty before
    // the comma. Refused, and the engine says so in its own voice.
    refuses_somehow("const o = { , }; return 0;");
    refuses_somehow("const o = { a: 1,, }; return 0;");
}

/// 13.2.5.5 PropertyDefinitionEvaluation runs the definitions **in source
/// order**, each a CreateDataPropertyOrThrow, so a repeated key is written
/// twice to one slot and the last write is what stays. Both sides matter: the
/// value is the last one, and there is exactly one slot.
#[test]
fn a_duplicate_key_keeps_the_last_value_in_one_slot() {
    number("const o = { a: 1, a: 2 }; return o.a;", 2.0);
    number("const o = { a: 1, a: 2, a: 3 }; return o.a;", 3.0);
    assert_eq!(returned_keys("return { a: 1, a: 2, a: 3 };"), ["a"]);
    // The types need not agree; the last definition wins outright.
    boolean("const o = { a: 1, a: \"s\", a: true }; return o.a;", true);
    text("const o = { a: true, a: \"s\" }; return o.a;", "s");
    // A duplicate does not move the key: 10.1.9's Set on an existing property
    // touches the value only.
    assert_eq!(returned_keys("return { a: 1, b: 2, a: 3 };"), ["a", "b"]);
    number("const o = { a: 1, b: 2, a: 3 }; return o.a;", 3.0);
    // And the *earlier* definitions still run -- they are evaluated, not
    // skipped, which a "last wins" shortcut that never evaluated the first
    // would get wrong. Observed through a side effect on another object.
    let source = "
        const log = { n: 0 };
        function bump(v) { log.n = log.n + 1; return v; }
        const o = { a: bump(1), a: bump(2) };
        return log.n * 10 + o.a;
    ";
    number(source, 22.0);
}

/// 13.2.5: a PropertyName may be written as a StringLiteral, and the key is
/// that String value -- so a key may hold anything a String can, including a
/// space, an empty text and a non-ASCII character, none of which a dotted
/// access could ever name.
#[test]
fn a_string_literal_key_may_be_any_text() {
    number("const o = { \"a b\": 1 }; return o[\"a b\"];", 1.0);
    number("const o = { \"\": 1 }; return o[\"\"];", 1.0);
    number("const o = { \"\\u00e9\": 1 }; return o[\"\\u00e9\"];", 1.0);
    number("const o = { \"a-b\": 1 }; return o[\"a-b\"];", 1.0);
    number("const o = { \"0 \": 1 }; return o[\"0 \"];", 1.0);
    assert_eq!(
        returned_keys("return { \"a b\": 1, \"\": 2 };"),
        ["a b", ""]
    );
    // A quoted key that *is* a valid identifier is the same key as the bare
    // one: 13.2.5's PropertyName has one String value either way.
    number("const o = { \"a\": 1 }; return o.a;", 1.0);
    number("const o = { a: 1 }; return o[\"a\"];", 1.0);
    boolean("const o = { \"a\": 1, a: 2 }; return o.a === 2;", true);
    assert_eq!(returned_keys("return { \"a\": 1, a: 2 };"), ["a"]);
}

/// 13.2.5 again: a NumericLiteral PropertyName is ToString'd at *parse* time,
/// so `{ 1: x }` is the property `"1"` and reaching it by number or by string
/// finds the same slot (7.1.19 ToPropertyKey does the same conversion on the
/// way in).
#[test]
fn a_numeric_looking_key_is_the_string_of_that_number() {
    text("const o = { 1: \"n\" }; return o[1];", "n");
    text("const o = { 1: \"n\" }; return o[\"1\"];", "n");
    text("const o = { 0: \"z\" }; return o[0];", "z");
    assert_eq!(returned_keys("return { 0: \"z\", 1: \"o\" };"), ["0", "1"]);
    assert_eq!(returned_keys("return { 10: 1, 9: 2 };"), ["10", "9"]);
    // One slot, reached from both spellings.
    number("const o = { 1: 1 }; o[\"1\"] = 2; return o[1];", 2.0);
    assert_eq!(
        returned_keys("const o = { 1: 1 }; o[\"1\"] = 2; return o;"),
        ["1"]
    );
    // ToString is not a *parse* of the key back: `"01"` and `"1.0"` are their
    // own strings and name their own properties.
    undefined("const o = {}; o[\"01\"] = 1; return o[1];");
    undefined("const o = {}; o[\"1.0\"] = 1; return o[1];");
    number("const o = {}; o[\"01\"] = 1; return o[\"01\"];", 1.0);
}

/// 13.2.5 ObjectLiteral, shorthand: `{ x }` is `{ x: x }` -- the property name
/// is the IdentifierReference and the value is what it resolves to.
#[test]
fn shorthand_names_the_binding_it_reads() {
    number("const x = 9; const o = { x }; return o.x;", 9.0);
    number("let x = 1; const o = { x }; x = 2; return o.x;", 1.0);
    assert_eq!(
        returned_keys("const b = 2; const a = 1; return { b, a };"),
        ["b", "a"]
    );
    number("const x = 1; const o = { x, y: 2 }; return o.x + o.y;", 3.0);
    // The value is copied, not aliased: 13.2.5.5 evaluates it once, now.
    number(
        "const x = { n: 1 }; const o = { x }; o.x.n = 5; return x.n;",
        5.0,
    );
}

// =========================================================================
// 7.1.19 ToPropertyKey -- what a key is
// =========================================================================

/// 13.3.2.1 (dot) takes the *String value* of an IdentifierName, and 13.3.3.1
/// (bracket) runs ToPropertyKey on the evaluated expression. Two productions,
/// one property, and the corpus says so in both directions for each way of
/// creating the property.
#[test]
fn dotted_and_computed_access_agree() {
    boolean("const o = { a: 1 }; return o.a === o[\"a\"];", true);
    number("const o = {}; o.a = 1; return o[\"a\"];", 1.0);
    number("const o = {}; o[\"a\"] = 1; return o.a;", 1.0);
    number("const o = { a: 1 }; o[\"a\"] = 2; return o.a;", 2.0);
    number("const o = { a: 1 }; o.a = 2; return o[\"a\"];", 2.0);
    // The bracket form takes an expression, not a spelling.
    number("const k = \"a\"; const o = { a: 5 }; return o[k];", 5.0);
    number("const o = { ab: 6 }; return o[\"a\" + \"b\"];", 6.0);
    number(
        "const o = { a: 1 }; const k = \"a\"; o[k] = 7; return o.a;",
        7.0,
    );
    // Neither form creates a slot the other cannot see.
    assert_eq!(
        returned_keys("const o = {}; o.a = 1; o[\"b\"] = 2; return o;"),
        ["a", "b"]
    );
}

// =========================================================================
// 10.1.8.1 OrdinaryGet -- reading
// =========================================================================

/// OrdinaryGet on an absent property returns `undefined` after walking a
/// prototype chain that, here, is empty. `undefined`, not a fault: a script
/// asking whether an optional field is there is the commonest read there is.
#[test]
fn a_missing_property_reads_undefined() {
    undefined("const o = {}; return o.a;");
    undefined("const o = { a: 1 }; return o.b;");
    undefined("const o = { a: 1 }; return o[\"b\"];");
    undefined("const o = { a: 1 }; const k = \"b\"; return o[k];");
    undefined("const o = { a: { b: 1 } }; return o.a.c;");
    boolean("const o = {}; return o.a === undefined;", true);
    text("const o = {}; return typeof o.a;", "undefined");
    // A near miss is a miss: keys are compared by content, exactly.
    undefined("const o = { ab: 1 }; return o.a;");
    undefined("const o = { a: 1 }; return o.A;");
    undefined("const o = { \"a \": 1 }; return o[\"a\"];");
}

/// Reading is not creating. OrdinaryGet has no step that installs anything,
/// so a read of an absent property must leave the own-key list untouched --
/// the mistake a "find or insert" helper makes on its first day.
#[test]
fn reading_a_missing_property_creates_nothing() {
    let (keys, len, _) = returned_record("const o = {}; o.a; return o;");
    assert!(keys.is_empty(), "a read installed {keys:?}");
    assert_eq!(len, 0);
    assert_eq!(
        returned_keys("const o = { a: 1 }; o.b; o[\"c\"]; return o;"),
        ["a"]
    );
    let repeated = "
        const o = { a: 1 };
        let i = 0;
        while (i < 20) { o.missing; i = i + 1; }
        return o;
    ";
    assert_eq!(returned_keys(repeated), ["a"]);
    // Nor does reading through `typeof` or a comparison.
    assert_eq!(
        returned_keys("const o = {}; typeof o.a; o.b === undefined; return o;"),
        Vec::<String>::new()
    );
}

/// 13.3.2.1 step 3 / 13.3.3.1 step 5: GetValue on a Reference whose base is
/// `undefined` or `null` throws a TypeError (6.2.5.5 step 3.a). There is no
/// `try` in this subset, so a throw reaches the host as a trap -- the engine
/// agrees with the specification about *what happened*, and the subset
/// decides where it lands.
#[test]
fn reading_a_property_of_null_or_undefined_faults() {
    traps("return undefined.a;");
    traps("return null.a;");
    traps("return undefined[\"a\"];");
    traps("return null[\"a\"];");
    // The chained shape, which is how this actually happens: the first read
    // is a legal `undefined`, the second is the TypeError.
    traps("const o = {}; return o.a.b;");
    traps("const o = { a: null }; return o.a.b;");
}

// =========================================================================
// 10.1.9 OrdinarySet -- writing
// =========================================================================

/// 10.1.9.2 step 3.b: a Set whose property does not exist ends in
/// CreateDataProperty, so assignment creates. The new key goes last, because
/// creation order is the order (10.1.11.1).
#[test]
fn assignment_creates_a_property() {
    number("const o = {}; o.a = 1; return o.a;", 1.0);
    text("const o = {}; o.name = \"fleet\"; return o.name;", "fleet");
    assert_eq!(returned_keys("const o = {}; o.a = 1; return o;"), ["a"]);
    assert_eq!(
        returned_keys("const o = { a: 1 }; o.b = 2; return o;"),
        ["a", "b"]
    );
    assert_eq!(
        returned_keys("const o = { b: 1 }; o.a = 2; return o;"),
        ["b", "a"]
    );
    number("const o = { a: 1 }; o.b = 2; return o.a + o.b;", 3.0);
    // Creating a property whose value is `undefined` still creates it.
    let (keys, len, _) = returned_record("const o = {}; o.a = undefined; return o;");
    assert_eq!(keys, ["a"]);
    assert_eq!(len, 1);
    undefined("const o = {}; o.a = undefined; return o.a;");
}

/// The same through a computed key, including a key that arrives as a value
/// rather than as text. 7.1.19 runs on the evaluated expression, so the slot
/// is the one the *value* names.
#[test]
fn assignment_through_a_computed_key() {
    number("const o = {}; o[\"a\"] = 1; return o.a;", 1.0);
    number("const k = \"a\"; const o = {}; o[k] = 1; return o.a;", 1.0);
    number("const o = {}; o[\"a\" + \"b\"] = 1; return o.ab;", 1.0);
    text("const o = {}; o[1] = \"x\"; return o[\"1\"];", "x");
    number("const o = {}; o[true] = 1; return o[\"true\"];", 1.0);
    number("const o = {}; o[null] = 1; return o[\"null\"];", 1.0);
    number(
        "const o = {}; o[undefined] = 1; return o[\"undefined\"];",
        1.0,
    );
    assert_eq!(
        returned_keys("const o = {}; const k = \"z\"; o[k] = 1; o[\"y\"] = 2; return o;"),
        ["z", "y"]
    );
    // A key built in a loop: each distinct text is its own slot.
    let loop_keys = "
        const o = {};
        let k = \"\";
        let i = 0;
        while (i < 3) { k = k + \"x\"; o[k] = i; i = i + 1; }
        return o;
    ";
    assert_eq!(returned_keys(loop_keys), ["x", "xx", "xxx"]);
    number(
        "const o = {}; let k = \"\"; let i = 0; while (i < 3) { k = k + \"x\"; o[k] = i; i = i + 1; } return o.xxx;",
        2.0,
    );
}

/// 13.15.2 AssignmentExpression: evaluate the LeftHandSideExpression to a
/// Reference (which evaluates the base *and* the computed key), then the
/// right-hand side, then PutValue. Left to right, observed through a side
/// effect on a witness object.
#[test]
fn an_assignment_evaluates_left_to_right() {
    let source = "
        const log = { s: \"\" };
        function k() { log.s = log.s + \"k\"; return \"p\"; }
        function v() { log.s = log.s + \"v\"; return 1; }
        const o = {};
        o[k()] = v();
        return log.s;
    ";
    text(source, "kv");
    let base = "
        const log = { s: \"\" };
        const target = {};
        function b() { log.s = log.s + \"b\"; return target; }
        function v() { log.s = log.s + \"v\"; return 1; }
        b().p = v();
        return log.s + typeof target.p;
    ";
    text(base, "bvnumber");
}

/// 10.1.9.2 step 3.a.i: setting an *existing* own data property is a
/// value-only update. Position is not part of the value, so it does not move.
#[test]
fn assignment_overwrites_in_place() {
    number("const o = { a: 1 }; o.a = 2; return o.a;", 2.0);
    text("const o = { a: 1 }; o.a = \"two\"; return o.a;", "two");
    assert_eq!(
        returned_keys("const o = { a: 1, b: 2, c: 3 }; o.a = 9; return o;"),
        ["a", "b", "c"]
    );
    assert_eq!(
        returned_keys("const o = { a: 1, b: 2, c: 3 }; o.c = 9; o.a = 8; return o;"),
        ["a", "b", "c"]
    );
    let (_, len, _) = returned_record("const o = { a: 1 }; o.a = 2; o.a = 3; o.a = 4; return o;");
    assert_eq!(len, 1, "four writes, one slot");
}

/// 13.15.2 step 1.f: the value of an assignment is the value that was
/// assigned -- not the object, and not a copy read back out.
#[test]
fn an_assignment_evaluates_to_the_assigned_value() {
    number("const o = {}; return (o.a = 5);", 5.0);
    text("const o = {}; return (o[\"k\"] = \"v\");", "v");
    boolean("const o = {}; return (o.a = true);", true);
    null("const o = {}; return (o.a = null);");
    undefined("const o = {}; return (o.a = undefined);");
    number("const o = {}; const x = (o.a = 5); return x + o.a;", 10.0);
    // Chained assignment: one value, two slots (13.15.2 is right-associative).
    number(
        "const a = {}; const b = {}; a.x = b.y = 3; return a.x + b.y;",
        6.0,
    );
}

// =========================================================================
// 10.1.11.1 OrdinaryOwnPropertyKeys -- order
// =========================================================================

/// The String keys of an ordinary object come out in **creation order**, and
/// nothing that is not a creation reorders them.
#[test]
fn insertion_order_survives_reads_and_rewrites() {
    assert_eq!(
        returned_keys("return { a: 1, b: 2, c: 3 };"),
        ["a", "b", "c"]
    );
    assert_eq!(
        returned_keys("return { c: 1, b: 2, a: 3 };"),
        ["c", "b", "a"]
    );
    assert_eq!(
        returned_keys("const o = {}; o.z = 1; o.y = 2; o.x = 3; return o;"),
        ["z", "y", "x"]
    );
    // A literal, then assignment: one sequence, not two groups.
    assert_eq!(
        returned_keys("const o = { b: 1 }; o.a = 2; o.c = 3; return o;"),
        ["b", "a", "c"]
    );
    // Reads do not reorder -- the shape a move-to-front cache would break.
    assert_eq!(
        returned_keys("const o = { a: 1, b: 2, c: 3 }; o.c; o.c; o.b; o.c; return o;"),
        ["a", "b", "c"]
    );
    // Neither does a read in a loop.
    let hot = "
        const o = { a: 1, b: 2, c: 3 };
        let i = 0;
        let n = 0;
        while (i < 50) { n = n + o.c; i = i + 1; }
        return o;
    ";
    assert_eq!(returned_keys(hot), ["a", "b", "c"]);
    // Nor a rewrite of the last key, nor of the first.
    assert_eq!(
        returned_keys("const o = { a: 1, b: 2 }; o.b = 3; o.a = 4; return o;"),
        ["a", "b"]
    );
}

/// Order survives the reallocation that growth needs. Twenty properties is
/// past every doubling from the initial capacity, and past the point the
/// layout note calls the linear scan's comfort zone -- so it is also the test
/// that a hash or a shape table added later must still pass.
#[test]
fn order_and_values_survive_growth() {
    let names = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r",
        "s", "t",
    ];
    let mut source = String::from("const obj = {};");
    for (i, name) in names.iter().enumerate() {
        source.push_str(&format!(" obj.{name} = {};", i + 1));
    }
    let keys_source = format!("{source} return obj;");
    assert_eq!(returned_keys(&keys_source), names);
    // Every value still reads back, first, last and across each doubling.
    for (i, name) in names.iter().enumerate() {
        number(&format!("{source} return obj.{name};"), (i + 1) as f64);
    }
    let (_, len, cap) = returned_record(&keys_source);
    assert_eq!(len, 20);
    assert!(cap >= 20, "capacity {cap} must cover twenty properties");
    // A rewrite after growth still lands in the original slot.
    assert_eq!(
        returned_keys(&format!("{source} obj.a = 99; return obj;")),
        names
    );
    number(&format!("{source} obj.a = 99; return obj.a;"), 99.0);
}

// =========================================================================
// An object is an ordinary value
// =========================================================================

/// 6.1.7: an Object value is a reference. Passing it to a function passes the
/// reference, so a mutation inside is visible outside, and the identity that
/// comes back is the identity that went in.
#[test]
fn an_object_argument_is_the_same_object() {
    number(
        "function m(o) { o.a = 1; return 0; } const x = {}; m(x); return x.a;",
        1.0,
    );
    boolean(
        "function id(o) { return o; } const x = {}; return id(x) === x;",
        true,
    );
    boolean(
        "function id(o) { return o; } return id({}) === id({});",
        false,
    );
    number(
        "function read(o) { return o.a; } return read({ a: 4 });",
        4.0,
    );
    // Through two levels, and back out.
    let nested = "
        function inner(o) { o.n = o.n + 1; return o; }
        function outer(o) { return inner(inner(o)); }
        const x = { n: 0 };
        outer(x);
        return x.n;
    ";
    number(nested, 2.0);
    // A parameter rebound inside does not touch the caller's binding.
    let rebind = "
        function swap(o) { o = { n: 99 }; return o.n; }
        const x = { n: 1 };
        swap(x);
        return x.n;
    ";
    number(rebind, 1.0);
}

/// Returning an object: a literal built in the callee, a parameter object
/// built from arguments, and one returned through a recursive call. This is
/// the `fleet.js` parameter-object shape, which is why it is here.
#[test]
fn an_object_is_a_return_value() {
    number("function make() { return { a: 1 }; } return make().a;", 1.0);
    text(
        "function params(tab, note) { return { tab: tab, note: note }; } return params(1, \"n\").note;",
        "n",
    );
    assert_eq!(
        returned_keys("function params(t, n) { return { tab: t, note: n }; } return params(1, 2);"),
        ["tab", "note"]
    );
    // Mutating what a call returned.
    number(
        "function make() { return {}; } const o = make(); o.a = 2; return o.a;",
        2.0,
    );
    // Two calls, two objects.
    boolean(
        "function make() { return {}; } return make() === make();",
        false,
    );
    // Built down a recursion: each frame wraps the one below.
    let recursive = "
        function chain(n) {
            if (n === 0) { return { depth: 0 }; }
            return { depth: n, next: chain(n - 1) };
        }
        return chain(3).next.next.next.depth;
    ";
    number(recursive, 0.0);
    number(
        "function chain(n) { if (n === 0) { return { depth: 0 }; } return { depth: n, next: chain(n - 1) }; } return chain(3).depth;",
        3.0,
    );
}

/// An object in a `let`: the binding can be rebound, and rebinding it does
/// not touch the object it used to name. `const` binds the reference, so the
/// object it names is still mutable -- 13.3.1 is about the binding.
#[test]
fn a_binding_and_the_object_it_names_are_two_things() {
    number("let o = { a: 1 }; o.a = 2; return o.a;", 2.0);
    number("let o = { a: 1 }; o = { a: 2 }; return o.a;", 2.0);
    number(
        "let o = { a: 1 }; const first = o; o = { a: 2 }; return first.a;",
        1.0,
    );
    boolean(
        "let o = { a: 1 }; const first = o; o = { a: 2 }; return o === first;",
        false,
    );
    number("const o = { a: 1 }; o.a = 2; return o.a;", 2.0);
    number("const a = { n: 1 }; const b = a; b.n = 5; return a.n;", 5.0);
    // Mutated in a loop through the binding.
    number(
        "let o = { n: 0 }; for (let i = 0; i < 5; i = i + 1) { o.n = o.n + i; } return o.n;",
        10.0,
    );
    // Rebound in a loop: five objects, the last one survives.
    number(
        "let o = { n: 0 }; for (let i = 0; i < 5; i = i + 1) { o = { n: i }; } return o.n;",
        4.0,
    );
    // A `const` binding cannot be rebound -- and the diagnostic is about the
    // binding, not about objects.
    let error = refuses_somehow("const o = { a: 1 }; o = { a: 2 }; return 0;");
    assert!(error.message.contains("const"), "got {:?}", error.message);
}

/// Nesting: literals inside literals, and chains built by assignment. Ten
/// deep both ways -- deeper than `fleet.js`'s three (`fleet.ui.composer.op`)
/// with room to spare, and enough that a one-level shortcut would show.
#[test]
fn objects_nest() {
    number("const o = { a: { b: 2 } }; return o.a.b;", 2.0);
    number(
        "const o = { a: { b: { c: { d: { e: 5 } } } } }; return o.a.b.c.d.e;",
        5.0,
    );
    let ten = "const o = { a: { b: { c: { d: { e: { f: { g: { h: { i: { j: 10 } } } } } } } } } }; return o.a.b.c.d.e.f.g.h.i.j;";
    number(ten, 10.0);
    text(
        "const o = { a: { b: { c: 1 } } }; return typeof o.a.b;",
        "object",
    );
    // Built by assignment instead, the `fleet.js` namespace-table shape.
    let built = "
        const fleet = {};
        fleet.ui = {};
        fleet.ui.composer = {};
        fleet.ui.composer.op = \"ui.composer.send\";
        return fleet.ui.composer.op;
    ";
    text(built, "ui.composer.send");
    // A deep write reaches the deep object, and the intermediate objects are
    // shared rather than copied.
    let deep_write = "
        const o = { a: { b: { c: 1 } } };
        const inner = o.a.b;
        o.a.b.c = 2;
        return inner.c;
    ";
    number(deep_write, 2.0);
    // Mixed: computed and dotted steps in one chain.
    number(
        "const o = { a: { b: { c: 7 } } }; return o[\"a\"].b[\"c\"];",
        7.0,
    );
    // A cycle is representable, because a property holds a reference.
    number(
        "const a = { n: 1 }; const b = { n: 2 }; a.other = b; b.other = a; return a.other.other.n;",
        1.0,
    );
}

/// A property holds any value this engine has: Number, String, Boolean, Null,
/// Undefined, Object. Each one reads back as itself, answers `typeof` with
/// its 13.5.3 name, and occupies exactly one slot.
#[test]
fn a_property_holds_every_value_type() {
    let all = "const o = { n: 1, s: \"s\", b: true, z: null, u: undefined, o: { i: 9 } };";
    number(&format!("{all} return o.n;"), 1.0);
    text(&format!("{all} return o.s;"), "s");
    boolean(&format!("{all} return o.b;"), true);
    null(&format!("{all} return o.z;"));
    undefined(&format!("{all} return o.u;"));
    number(&format!("{all} return o.o.i;"), 9.0);
    text(&format!("{all} return typeof o.n;"), "number");
    text(&format!("{all} return typeof o.s;"), "string");
    text(&format!("{all} return typeof o.b;"), "boolean");
    text(&format!("{all} return typeof o.z;"), "object");
    text(&format!("{all} return typeof o.u;"), "undefined");
    text(&format!("{all} return typeof o.o;"), "object");
    let (keys, len, _) = returned_record(&format!("{all} return o;"));
    assert_eq!(keys, ["n", "s", "b", "z", "u", "o"]);
    assert_eq!(len, 6);
    // The distinctions that a tag-losing round trip would flatten.
    boolean(&format!("{all} return o.z === null;"), true);
    boolean(&format!("{all} return o.u === undefined;"), true);
    boolean(&format!("{all} return o.z === o.u;"), false);
    boolean(&format!("{all} return o.b === true;"), true);
    boolean(&format!("{all} return o.n === 1;"), true);
    boolean(&format!("{all} return o.s === \"s\";"), true);
    // Assignment carries the same six.
    let assigned = "
        const o = {};
        o.n = 1; o.s = \"s\"; o.b = false; o.z = null; o.u = undefined; o.o = {};
        return typeof o.z + \"|\" + typeof o.u + \"|\" + typeof o.o;
    ";
    text(assigned, "object|undefined|object");
    // A value survives being written over a value of a different type.
    text(
        "const o = { a: 1 }; o.a = \"s\"; return typeof o.a;",
        "string",
    );
    text(
        "const o = { a: \"s\" }; o.a = null; return typeof o.a;",
        "object",
    );
    text("const o = { a: {} }; o.a = 1; return typeof o.a;", "number");
}

// =========================================================================
// Where this engine and ECMA-262 part company
//
// Each test below asserts what the engine does **today** and names, in a
// `DIVERGENCE:` line, the answer real JavaScript gives. Every one of them was
// first written the other way round -- asserting the specification -- and run,
// which is how the list was found rather than guessed. A divergence that is
// written down is a decision; one that is not is a bug waiting to be
// discovered by a script.
// =========================================================================

/// 10.1.11.1 steps 2-3 put the array-index keys first, in ascending numeric
/// order, ahead of the String keys in creation order. This engine's record is
/// a single insertion-ordered vector with no index/string split.
///
/// DIVERGENCE: `{ b: 1, 2: 2, a: 3, 1: 4 }` enumerates `["1","2","b","a"]` in
/// JavaScript and `["b","2","a","1"]` here.
///
/// It is unobservable in this subset -- nothing enumerates -- so it is a
/// choice with no cost *yet*. It stops being free at the first of
/// `Object.keys`, `for...in` and `JSON.stringify`, all of which read this
/// order out loud, and `JSON.stringify` is on this compiler's own roadmap.
#[test]
fn integer_index_keys_are_not_hoisted() {
    assert_eq!(
        returned_keys("return { b: 1, 2: 2, a: 3, 1: 4 };"),
        ["b", "2", "a", "1"]
    );
    assert_eq!(
        returned_keys("const o = {}; o.b = 1; o[2] = 2; o.a = 3; o[1] = 4; return o;"),
        ["b", "2", "a", "1"]
    );
    // Numeric order is not applied among the index keys either.
    assert_eq!(returned_keys("return { 10: 1, 9: 2 };"), ["10", "9"]);
    assert_eq!(
        returned_keys("return { 2: 1, 1: 2, 0: 3 };"),
        ["2", "1", "0"]
    );
    // Lookup is unaffected: every key still finds its own slot.
    number(
        "const o = { b: 1, 2: 2, a: 3, 1: 4 }; return o[1] + o[2] + o.a + o.b;",
        10.0,
    );
}

/// 13.2.5's PropertyName and 13.3.2.1's dotted access both take an
/// **IdentifierName**, which by 12.7 includes every ReservedWord. So
/// `{ class: 1 }` and `o.class` are ordinary JavaScript.
///
/// This engine accepts a property name only where its lexer already had a
/// token to hand back, which splits the keyword list in a place the grammar
/// does not.
///
/// DIVERGENCE: the second list below is legal JavaScript and is refused. Two
/// of its members -- `of` and `static`, plus `async`/`await` outside an async
/// function -- are not ReservedWords at all (12.7.2), so the diagnostic's own
/// noun is wrong for them: they are ordinary identifiers.
///
/// The refusal is honest and the workaround is exact -- a quoted key and a
/// computed access reach every one of them -- so this is a boundary, not a
/// trap. `fleet.js` uses none of these as a property name today, so it costs
/// that file nothing; the list is written out because the split is invisible
/// from the outside (`{ if: 1 }` works, `{ do: 1 }` does not) and the next
/// person should read it rather than rediscover it one word at a time.
#[test]
fn only_some_reserved_words_may_name_a_property() {
    // Accepted, and reachable by dotted access.
    for word in [
        "if",
        "else",
        "for",
        "while",
        "return",
        "function",
        "var",
        "let",
        "const",
        "typeof",
        "null",
        "true",
        "false",
        "undefined",
        "get",
        "set",
        // The milestone that landed unwinding made these four tokens the
        // lexer spells, and 13.2.5.1 admits every IdentifierName.
        "try",
        "catch",
        "finally",
        "throw",
    ] {
        number(&format!("const o = {{ {word}: 1 }}; return o.{word};"), 1.0);
        number(
            &format!("const o = {{ {word}: 1 }}; return o[\"{word}\"];"),
            1.0,
        );
    }
    // Refused, in the literal and at the access, with one fixed sentence.
    for word in [
        "do",
        "class",
        "new",
        "delete",
        "in",
        "of",
        "instanceof",
        "this",
        "void",
        "with",
        "switch",
        "case",
        "default",
        "break",
        "continue",
        "yield",
        "await",
        "async",
        "static",
        "import",
        "export",
        "extends",
        "super",
        "enum",
    ] {
        refuses_capability(
            &format!("const o = {{ {word}: 1 }}; return 0;"),
            "a property named with a reserved word",
            Boundary::FullJs,
        );
        refuses_capability(
            &format!("const o = {{ \"{word}\": 1 }}; return o.{word};"),
            "a property named with a reserved word",
            Boundary::FullJs,
        );
        // The workaround, which is why this is a boundary and not a wall: the
        // property itself exists, in the right slot, under the right name.
        number(
            &format!("const o = {{ \"{word}\": 1 }}; return o[\"{word}\"];"),
            1.0,
        );
        number(
            &format!("const o = {{}}; o[\"{word}\"] = 1; return o[\"{word}\"];"),
            1.0,
        );
        assert_eq!(
            returned_keys(&format!("const o = {{ \"{word}\": 1 }}; return o;")),
            [word]
        );
    }
}

/// 7.1.19 ToPropertyKey runs 7.1.17 ToString, and 6.1.6.1.20 gives every
/// Number a String. `__to_string` now calls the whole of 6.1.6.1.20, so there
/// is no integer boundary left: the divergence this test used to record --
/// `o[0.5]`, `o[NaN]` and `o[1/0]` trapping -- is gone, and each names the
/// property JavaScript names.
///
/// The integer-only `__num_to_str` that made it a boundary is gone with it.
#[test]
fn every_number_key_is_the_property_its_tostring_names() {
    number(
        "const o = {}; const k = 1 / 2; o[k] = 7; return o[\"0.5\"];",
        7.0,
    );
    number(
        "const o = {}; const k = 0 / 0; o[k] = 7; return o[\"NaN\"];",
        7.0,
    );
    number(
        "const o = {}; const k = 1 / 0; o[k] = 7; return o[\"Infinity\"];",
        7.0,
    );
    number(
        "const o = {}; const k = 0 - 1 / 0; o[k] = 7; return o[\"-Infinity\"];",
        7.0,
    );
    number(
        "const o = {}; const k = 2147483647 + 1; o[k] = 7; return o[\"2147483648\"];",
        7.0,
    );
    // Reading is the same conversion, so it finds the same slot.
    number(
        "const o = {}; o[\"0.5\"] = 7; const k = 1 / 2; return o[k];",
        7.0,
    );
    // 6.1.6.1.20 step 5: the shortest decimal that reads back, which is why
    // this key is `"0.1"` and not the exact expansion of the double.
    number(
        "const o = {}; const k = 1 / 10; o[k] = 7; return o[\"0.1\"];",
        7.0,
    );
    // Steps 6 to 9 pick the layout, and a key past the threshold is
    // exponential -- which is a property name like any other.
    text(
        "const o = {}; o[2147483647] = \"hi\"; return o[\"2147483647\"];",
        "hi",
    );
    text(
        "const o = {}; const k = 0 - 2147483647; o[k] = \"lo\"; return o[\"-2147483647\"];",
        "lo",
    );
}

/// 7.1.19 step 1 runs ToPrimitive on a non-Symbol key, which for a plain
/// object reaches `Object.prototype.toString`.
///
/// DIVERGENCE: in JavaScript `o[{}]` is the property `"[object Object]"` --
/// famously, every object key collapses onto that one slot. Here it traps,
/// because there is no prototype to reach and no `toString` to call.
#[test]
fn an_object_used_as_a_key_faults() {
    traps("const o = {}; const k = {}; o[k] = 1; return 0;");
    traps("const o = {}; const k = { a: 1 }; return o[k];");
}

/// 10.1.8.1 OrdinaryGet step 3 walks `[[Prototype]]`, and an object literal's
/// prototype is `Object.prototype`. This engine has no prototype chain: the
/// scan ends at the record.
///
/// DIVERGENCE: `typeof ({}).toString` is `"function"` in JavaScript and
/// `"undefined"` here; the same for `hasOwnProperty`, `valueOf`,
/// `constructor`, `isPrototypeOf` and `propertyIsEnumerable`.
///
/// `undefined` rather than a fault is the *dangerous* half: a script that
/// tests `if (o.hasOwnProperty)` gets a quiet `false` here and a `true`
/// everywhere else. It is asserted so that stays a known cost.
#[test]
fn there_is_no_prototype_to_inherit_from() {
    for member in [
        "toString",
        "valueOf",
        "hasOwnProperty",
        "constructor",
        "isPrototypeOf",
        "propertyIsEnumerable",
        "toLocaleString",
    ] {
        text(
            &format!("const o = {{}}; return typeof o.{member};"),
            "undefined",
        );
        boolean(
            &format!("const o = {{}}; return o.{member} === undefined;"),
            true,
        );
    }
    // And an inherited member is not an own key, so nothing is hidden in the
    // record either -- the object really is empty.
    assert_eq!(returned_keys("return {};"), Vec::<String>::new());
    // Calling one is a *trap*, which is the answer that used to be a compile
    // diagnostic. The call itself is a capability the engine has now; what is
    // absent is the prototype the method would have come from, so `o.toString`
    // reads `undefined` and calling `undefined` faults -- which is what
    // ECMA-262 makes it, a TypeError, and the closest thing to one this engine
    // has.
    traps("const o = {}; return o.toString();");
    traps("const o = {}; return o.hasOwnProperty(\"a\");");
}

/// B.2.2.1: `__proto__` is an accessor property on `Object.prototype`.
/// Reading it returns the prototype; assigning a non-Object is a no-op that
/// installs no own property.
///
/// DIVERGENCE: here `o.__proto__` is an ordinary absent property, and
/// `o.__proto__ = 1` creates an ordinary own slot named `"__proto__"` that
/// enumerates with the rest. In JavaScript the object would still have no own
/// keys.
///
/// The literal form is refused rather than silently misread -- see
/// `objects_m3.rs` -- so the surface that is silently different is exactly
/// this one: assignment and computed access.
#[test]
fn proto_is_an_ordinary_key_here() {
    undefined("const o = {}; return o.__proto__;");
    text("const o = {}; return typeof o.__proto__;", "undefined");
    assert_eq!(
        returned_keys("const o = {}; o.__proto__ = 1; return o;"),
        ["__proto__"]
    );
    assert_eq!(
        returned_keys("const o = {}; o[\"__proto__\"] = 1; return o;"),
        ["__proto__"]
    );
    number("const o = {}; o.__proto__ = 1; return o.__proto__;", 1.0);
    // It is a slot like any other, so it sits in insertion order with them.
    assert_eq!(
        returned_keys("const o = { a: 1 }; o.__proto__ = 2; o.b = 3; return o;"),
        ["a", "__proto__", "b"]
    );
    // Assigning an object to it does not make it a prototype: the object it
    // names is not consulted for a missing property.
    undefined("const p = { inherited: 1 }; const o = {}; o.__proto__ = p; return o.inherited;");
}

/// 13.3.2.1 on a primitive base runs ToObject (6.1.4 for a String), so a
/// property read on a Number, a String or a Boolean is `undefined` -- or, for
/// `"abc".length`, a real answer.
///
/// DIVERGENCE: each of these is `undefined` (or `3`) in JavaScript and traps
/// here. The trap is deliberate and argued in `objects_m3.rs`: answering
/// `undefined` would be a right answer reached by a wrong route, and would be
/// silently wrong for exactly the members a script reaches for -- `.length`,
/// `.trim`, `.toFixed`.
#[test]
fn a_property_of_a_primitive_faults() {
    traps("return (1).a;");
    traps("return true.a;");
    traps("return \"abc\".length;");
    traps("const s = \"abc\"; return s.length;");
    traps("const n = 1; return n.toFixed;");
    traps("const o = { s: \"abc\" }; return o.s.length;");
    // Assigning to one faults too, rather than being the silent no-op
    // ECMA-262 makes it in sloppy mode.
    traps("const s = \"abc\"; s.x = 1; return 0;");
}

/// 13.15.3 ApplyStringOrNumericBinaryOperator: when either operand is a
/// String, `+` is concatenation after ToString on both sides.
///
/// Every primitive operand converts. The one that does not is an Object:
/// 7.1.1 ToPrimitive reaches the `valueOf`/`toString` a prototype would carry,
/// and there is no prototype here, so `"x" + o` traps where JavaScript answers
/// `"x[object Object]"`. That is the remaining DIVERGENCE, and it is one
/// algorithm rather than three.
#[test]
fn concatenating_a_primitive_converts_and_an_object_faults() {
    text("return \"x\" + 2;", "x2");
    text("return 2 + \"x\";", "2x");
    text("const o = { n: 1 }; return \"n=\" + o.n;", "n=1");
    text("return \"x\" + true;", "xtrue");
    text("return \"x\" + null;", "xnull");
    text("return \"x\" + undefined;", "xundefined");
    text("const o = { s: \"b\" }; return \"a\" + o.s + \"c\";", "abc");
    // The one operand with no ToPrimitive.
    traps("const o = {}; return \"x\" + o;");
    traps("const o = {}; return o + \"\";");
}

/// 7.1.4 ToNumber of an Object is ToPrimitive then ToNumber, and for a plain
/// object that is `NaN`; 7.2.14 step 12 makes `{} == 1` a `false`.
///
/// DIVERGENCE: every arm below is an *answer* in JavaScript -- `NaN`, or
/// `false` -- and a trap here, because ToPrimitive needs the `valueOf` and
/// `toString` a prototype would carry.
///
/// `===` is the exception and stays one: 7.2.15 never coerces, so it answers
/// without a prototype and does so correctly.
#[test]
fn coercing_an_object_to_a_primitive_faults() {
    traps("const o = {}; return +o;");
    traps("const o = {}; return -o;");
    traps("const o = {}; return o - 1;");
    traps("const o = {}; return o * 2;");
    traps("const o = {}; return o / 2;");
    traps("const o = {}; return o % 2;");
    traps("const o = {}; return o < 1;");
    traps("const o = {}; return o >= 1;");
    traps("const o = {}; return o == 1;");
    traps("const o = {}; return o != 1;");
    traps("const o = { a: 1 }; return o.a + o;");
    // ToBoolean (7.1.2) needs no prototype, so the truthiness ladder answers.
    boolean("const o = {}; return !o;", false);
    boolean("const o = {}; return !!o;", true);
    number("const o = {}; if (o) { return 1; } return 0;", 1.0);
    // And `===`/`!==` answer by identity, for every type on the other side.
    boolean("const o = {}; return o === 1;", false);
    boolean("const o = {}; return o === \"\";", false);
    boolean("const o = {}; return o === null;", false);
    boolean("const o = {}; return o === undefined;", false);
    boolean("const o = {}; return o !== 1;", true);
    boolean("const a = {}; const b = a; return a === b;", true);
}

// =========================================================================
// The refusal side
//
// A subset this small is honest only if the things it does not do say so.
// Each of these is a facility a reader of `fleet.js` would reach for; each
// must be refused, in the engine's own voice, naming a capability. This is
// the lock that keeps the product's own description of the boundary true.
// =========================================================================

/// Prototypes, in every spelling. There is no `[[Prototype]]` here, so none of
/// these may quietly appear to work.
#[test]
fn prototypes_are_refused() {
    refuses_capability(
        "const o = { __proto__: {} }; return 0;",
        "the `__proto__` property",
        Boundary::FullJs,
    );
    refuses_capability(
        "const o = {}; return o instanceof Object;",
        "the `instanceof` keyword",
        Boundary::FullJs,
    );
    refuses_capability(
        "const o = {}; return new Object();",
        "the `new` keyword",
        Boundary::FullJs,
    );
    refuses_capability(
        "class C {} return 0;",
        "the `class` keyword",
        Boundary::FullJs,
    );
    // `Object.create` / `Object.getPrototypeOf` / `Object.setPrototypeOf` stop
    // at the *name*. The call is a capability the engine has; `Object` is a
    // binding it does not have, and the sentence is about that rather than
    // about the author.
    for source in [
        "const o = Object.create(null); return 0;",
        "const o = {}; return Object.getPrototypeOf(o);",
        "const o = {}; Object.setPrototypeOf(o, null); return 0;",
    ] {
        let error = refuse(source);
        assert!(
            error.message.contains("finds no declaration of `Object`"),
            "{source:?}: got {:?}",
            error.message
        );
    }
    // And without the call, the same sentence.
    let error = refuse("return Object;");
    assert!(
        error.message.contains("finds no declaration of `Object`"),
        "got {:?}",
        error.message
    );
    assert_eq!(error.boundary, Boundary::Subset);
}

/// Accessors. A getter would make a property read run code, which is the one
/// assumption every line of this milestone's lowering is built on.
#[test]
fn getters_and_setters_are_refused() {
    refuses_capability(
        "const o = { get a() { return 1; } }; return 0;",
        "getters and setters in object literals",
        Boundary::FullJs,
    );
    refuses_capability(
        "const o = { set a(v) { } }; return 0;",
        "getters and setters in object literals",
        Boundary::FullJs,
    );
    refuses_capability(
        "const o = { get a() { return 1; }, b: 2 }; return 0;",
        "getters and setters in object literals",
        Boundary::FullJs,
    );
    // `Object.defineProperty` stops at the missing `Object` binding now that
    // the call is a capability the engine has.
    assert!(
        refuse("const o = {}; Object.defineProperty(o, \"a\", { get: 1 }); return 0;")
            .message
            .contains("finds no declaration of `Object`")
    );
    // A method is the same problem wearing shorthand.
    refuses_capability(
        "const o = { f() { return 1; } }; return 0;",
        "methods in object literals",
        Boundary::FullJs,
    );
    // `get` and `set` are contextual, so they stay usable as plain keys.
    number("const o = { get: 1, set: 2 }; return o.get + o.set;", 3.0);
}

/// `delete`. The record has no tombstone and the heap has no free, so a
/// removal has nowhere to go; refusing is the honest answer.
#[test]
fn delete_is_refused() {
    for source in [
        "const o = { a: 1 }; delete o.a; return 0;",
        "const o = { a: 1 }; delete o[\"a\"]; return 0;",
        "const o = { a: 1 }; const gone = delete o.a; return 0;",
        "const o = { a: { b: 1 } }; delete o.a.b; return 0;",
    ] {
        refuses_capability(source, "the `delete` keyword", Boundary::FullJs);
    }
}

/// Enumeration, in every spelling. Nothing here may read the key order out
/// loud -- which is also why the divergence above is currently free.
#[test]
fn enumeration_is_refused() {
    // Every one of these stops at a name this engine has no binding for. The
    // refusal moved from the call to the name when calling a value became a
    // capability, and it is the better sentence: the engine can make the
    // call, it just has nothing to make it on.
    for (source, missing) in [
        ("const o = {}; return Object.keys(o);", "Object"),
        ("const o = {}; return Object.values(o);", "Object"),
        ("const o = {}; return Object.entries(o);", "Object"),
        ("const o = {}; return Object.assign({}, o);", "Object"),
        ("const o = {}; return Object.freeze(o);", "Object"),
    ] {
        let error = refuse(source);
        assert!(
            error
                .message
                .contains(&format!("finds no declaration of `{missing}`")),
            "{source:?}: got {:?}",
            error.message
        );
    }
    // `JSON` used to be on that list and is not: it is the one name this
    // engine binds itself. It reads a key order out loud, which is what this
    // test is about, so the order it reads is the one 10.1.11.1 requires --
    // insertion, not sorted. `tests/json.rs` and `control_conformance.rs`
    // own the rest of 25.5.
    text(
        "const o = { b: 1, a: 2 }; return JSON.stringify(o);",
        "{\"b\":1,\"a\":2}",
    );
    // `o.hasOwnProperty("a")` names nothing that is missing -- `o` is right
    // there -- so it compiles and faults on the absent property instead.
    traps("const o = {}; return o.hasOwnProperty(\"a\");");
    refuses_capability(
        "const o = {}; return \"a\" in o;",
        "the `in` keyword",
        Boundary::FullJs,
    );
    // `for...in` and `for...of` are refused, but by the *`for` header's*
    // reading of a `const` with no initialiser rather than by a sentence
    // naming the loop form. MISLEADING: the construct ahead of the engine is
    // `for...in`/`for...of`, and the diagnostic names neither. Asserted
    // loosely on purpose -- the refusal is the promise, this wording is not.
    for source in [
        "const o = {}; for (const k in o) { } return 0;",
        "const o = {}; for (const k of o) { } return 0;",
    ] {
        let error = refuses_somehow(source);
        assert!(error.offset < source.len(), "an offset inside the source");
    }
}

/// Spread and rest, and destructuring -- the three ways a script asks the
/// engine to walk an object's properties for it.
#[test]
fn spread_and_destructuring_are_refused() {
    for source in [
        "const a = {}; const o = { ...a }; return 0;",
        "const a = {}; const o = { x: 1, ...a }; return 0;",
        "const a = {}; const o = { ...a, x: 1 }; return 0;",
    ] {
        refuses_capability(source, "the spread and rest syntax", Boundary::ThirdBinding);
    }
    // MISLEADING: a binding pattern is read as a block statement, so the
    // sentence names a capability the source does not use. The refusal itself
    // is right and is what this test locks.
    for source in [
        "const o = { a: 1 }; const { a } = o; return a;",
        "const o = { a: 1 }; let { a, b } = o; return 0;",
        "function f({ a }) { return a; } return 0;",
    ] {
        refuses_somehow(source);
    }
}

/// The expression forms a real binding library reaches for around a property
/// access. None of them is an object feature; each is refused by name, which
/// is what makes the boundary readable rather than a cliff.
#[test]
fn the_neighbouring_syntax_is_refused_by_name() {
    refuses_capability(
        "const o = {}; return o?.a;",
        "optional chaining",
        Boundary::Subset,
    );
    refuses_capability(
        "const o = {}; return o?.[\"a\"];",
        "optional chaining",
        Boundary::Subset,
    );
    refuses_capability(
        "const o = {}; return o.a ?? 1;",
        "the nullish coalescing operator",
        Boundary::Subset,
    );
    refuses_capability(
        "const o = { a: 1 }; return `${o.a}`;",
        "template literals",
        Boundary::Subset,
    );
    refuses_capability("return [1, 2];", "array literals", Boundary::ThirdBinding);
    refuses_capability(
        "const o = { a: 1 }; return { k: [o.a] };",
        "array literals",
        Boundary::ThirdBinding,
    );
    refuses_capability(
        "const k = \"a\"; const o = { [k]: 1 }; return 0;",
        "computed property keys",
        Boundary::FullJs,
    );
    // A function as a property value was the next milestone and is now this
    // one, wherever written. What was a refusal corpus is a behaviour claim:
    // the property holds the function, and calling through it runs it.
    for source in [
        "const o = { f: function () { return 1; } }; return o.f();",
        "const o = {}; o.f = function () { return 1; }; return o.f();",
        "function g() { return 1; } const o = { f: g }; return o.f();",
    ] {
        number(source, 1.0);
    }
}

/// The product promise every diagnostic in this file has to keep, checked in
/// one place over one list: the engine speaks about itself, it never says
/// "syntax error", it never blames the script, and it points at a byte inside
/// the source.
#[test]
fn every_refusal_names_a_boundary_and_a_place() {
    for source in [
        "const o = { get a() { return 1; } }; return 0;",
        "const o = { f() { return 1; } }; return 0;",
        "const o = { __proto__: {} }; return 0;",
        "const a = {}; const o = { ...a }; return 0;",
        "const k = \"a\"; const o = { [k]: 1 }; return 0;",
        "const o = { class: 1 }; return 0;",
        "const o = {}; delete o.a; return 0;",
        "const o = {}; return \"a\" in o;",
        "const o = {}; return o instanceof Object;",
        "const o = {}; return new Object();",
        "const o = {}; return o?.a;",
        "const o = {}; const { a } = o; return 0;",
        "const o = {}; for (const k in o) { } return 0;",
        "return [1, 2];",
        "const o = {}; return o.;",
        "const o = {}; return o[;",
        "const o = { a: };",
        "const o = { a 1 };",
        "const o = { a: 1",
    ] {
        let error = refuse(source);
        assert!(
            error.message.starts_with("this engine"),
            "{source:?}: must speak for the engine, got {:?}",
            error.message
        );
        for blamed in ["syntax error", "invalid", "illegal", "bad ", "you "] {
            assert!(
                !error.message.to_lowercase().contains(blamed),
                "{source:?}: must not say {blamed:?}, got {:?}",
                error.message
            );
        }
        assert!(
            error.offset <= source.len(),
            "{source:?}: offset {} is past the end",
            error.offset
        );
    }
}
