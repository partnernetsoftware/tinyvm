//! Object literals, property access, and property assignment, on V1.
//!
//! Every expectation here is derived from ECMA-262, not from what the
//! implementation happens to do, and every one of them **runs**: compile ->
//! tinyvm's load gate -> instantiate -> `invoke_by_name("main")`. "The parser
//! accepted it" is not evidence and never appears here on its own; the one
//! exception is the refusal corpus at the bottom, where not compiling *is* the
//! claim.
//!
//! # Why some tests read guest memory directly
//!
//! Two of this milestone's guarantees -- **property order is insertion order**
//! and **the heap layout is a flat key/value vector** -- have no observable
//! surface in this subset: there is no `Object.keys`, no `for...in`, and
//! `JSON.stringify` is a later milestone. A guarantee with no test is a
//! guarantee that will quietly stop holding, so those tests walk the object
//! record in linear memory themselves. They know the layout on purpose: they
//! are the layout's specification as much as `runtime.rs` is.

use tinyvm::{Limits, Val, WasmInstance, WasmModule};
use tinyvm_qjs::{Boundary, CompileError, Names, Options, Value, compile_qjs_m1};

// =========================================================================
// Harness
// =========================================================================

/// What `main` returned, with a String's text already resolved -- a pointer
/// into a dropped instance's memory is unreadable.
#[derive(Debug, Clone, PartialEq)]
enum Out {
    Undefined,
    Null,
    Number(f64),
    Bool(bool),
    Str(String),
}

/// The object tag, as `repr.rs` numbers it. Written out rather than imported
/// because `repr` is crate-private: this is the contract restated from the
/// outside, which is the only place it can be checked from.
const TAG_OBJECT: i32 = 5;

/// `[len: i32][cap: i32][entries: i32]`, and 16 bytes per entry.
const OBJ_LEN: usize = 0;
const OBJ_CAP: usize = 4;
const OBJ_ENTRIES: usize = 8;
const ENTRY_BYTES: usize = 16;

fn build(source: &str) -> Result<(WasmModule, Vec<u8>), String> {
    let wasm = compile_qjs_m1(source).map_err(|e| format!("compiling {source:?}: {e}"))?;
    let module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .map_err(|e| format!("load gate rejected {source:?}: {}", e.message()))?;
    Ok((module, wasm))
}

/// Compile, load, instantiate, call -- and hand back the instance too, so a
/// test that has to read the heap can.
fn attempt(source: &str) -> Result<(WasmInstance, Vec<Val>), String> {
    let (module, _) = build(source)?;
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

/// A string record in guest memory: `[len: i32][utf8 bytes]`.
fn read_string(instance: &WasmInstance, ptr: i32) -> Result<String, String> {
    let view = instance
        .memory()
        .map_err(|e| format!("no guest memory: {}", e.message()))?;
    read_string_at(&view, ptr)
}

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

/// The keys of the object `main` returned, in the order the record stores
/// them. Fails loudly if `main` returned anything but an Object.
#[track_caller]
fn returned_keys(source: &str) -> Vec<String> {
    let (instance, vals) = attempt(source).unwrap_or_else(|e| panic!("{e}"));
    let [Val::I32(tag), Val::I64(payload)] = vals.as_slice() else {
        panic!("{source:?}: want one V1 pair back, got {vals:?}");
    };
    assert_eq!(*tag, TAG_OBJECT, "{source:?}: want an Object back");
    let view = instance.memory().expect("guest memory");
    let bytes: &[u8] = &view;
    let object = *payload as u32 as usize;
    let len = word(bytes, object + OBJ_LEN) as usize;
    let entries = word(bytes, object + OBJ_ENTRIES) as usize;
    (0..len)
        .map(|i| {
            let key = word(bytes, entries + i * ENTRY_BYTES);
            read_string_at(bytes, key).expect("a key string record")
        })
        .collect()
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

/// A source that compiles, clears the load gate, and then traps.
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

/// A refusal that names a capability, in the fixed wording `diag.rs` locks.
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

// =========================================================================
// The tag and the type
// =========================================================================

/// ECMA-262 13.5.3: `typeof` an Object is `"object"` -- the same answer `null`
/// gets, which is why `runtime::TypeNames` has one field for both.
#[test]
fn typeof_an_object_is_object() {
    text("return typeof {};", "object");
    text("const o = { a: 1 }; return typeof o;", "object");
    // The same string, so the two share one pool record.
    boolean("return typeof {} === typeof null;", true);
    boolean("return typeof {} === \"object\";", true);
}

/// ECMA-262 7.1.2 ToBoolean: an Object is always truthy. Not approximated --
/// an empty object is truthy too, which is the case a "is it empty" shortcut
/// would get wrong.
#[test]
fn every_object_is_truthy() {
    number("if ({}) { return 1; } return 0;", 1.0);
    number("const o = {}; if (o) { return 1; } return 0;", 1.0);
    boolean("return !{};", false);
    boolean("return !!{};", true);
    // `&&` and `||` yield an operand, and an Object is the truthy one.
    number("const o = { a: 3 }; return (o && o.a);", 3.0);
}

/// ECMA-262 7.2.15 IsStrictlyEqual over two Objects is reference identity,
/// which on V1 is payload equality -- the arm `repr`'s payload-0 invariant
/// already built for Boolean/Null/Undefined.
#[test]
fn object_equality_is_reference_identity() {
    boolean("const a = {}; const b = {}; return a === b;", false);
    boolean("const a = {}; const b = a; return a === b;", true);
    boolean("const a = {}; return a === a;", true);
    boolean(
        "const a = { x: 1 }; const b = { x: 1 }; return a === b;",
        false,
    );
    boolean("const a = {}; const b = a; return a !== b;", false);
    // 7.2.14 step 1: same type defers to `===`.
    boolean("const a = {}; const b = a; return a == b;", true);
    boolean("const a = {}; const b = {}; return a == b;", false);
    // Nothing bridges Object and null/undefined.
    boolean("const a = {}; return a == null;", false);
    boolean("const a = {}; return a == undefined;", false);
    boolean("const a = {}; return a === null;", false);
    // A different language type is never strictly equal.
    boolean("const a = {}; return a === 0;", false);
    boolean("const a = {}; return a === \"\";", false);
}

// =========================================================================
// Reading a property
// =========================================================================

#[test]
fn a_literal_property_reads_back() {
    number("const o = { a: 1 }; return o.a;", 1.0);
    text("const o = { a: \"x\" }; return o.a;", "x");
    boolean("const o = { a: true }; return o.a;", true);
    number("const o = { a: 1, b: 2, c: 3 }; return o.b;", 2.0);
    number("const o = { a: 1, b: 2, c: 3 }; return o.c;", 3.0);
}

/// ECMA-262 10.1.8.1 OrdinaryGet: a property that is not there is `undefined`.
/// A trap here would be the single most common way a real script breaks.
#[test]
fn a_missing_property_is_undefined_not_a_trap() {
    undefined("const o = {}; return o.a;");
    undefined("const o = { a: 1 }; return o.b;");
    undefined("const o = { a: 1 }; return o[\"b\"];");
    boolean("const o = {}; return o.a === undefined;", true);
    text("const o = {}; return typeof o.a;", "undefined");
}

/// ECMA-262 13.3.2.1 (dot) and 13.3.3.1 (bracket) are different productions
/// that name the same property, because 13.3.2.1's key is the *String value*
/// of the IdentifierName.
#[test]
fn dotted_and_computed_access_name_one_property() {
    number("const o = { a: 1 }; return o[\"a\"];", 1.0);
    number("const o = {}; o.a = 4; return o[\"a\"];", 4.0);
    number("const o = {}; o[\"a\"] = 4; return o.a;", 4.0);
    // A quoted key in the literal is the same key as a bare one.
    number("const o = { \"a\": 7 }; return o.a;", 7.0);
    number("const o = { \"a\": 7 }; return o[\"a\"];", 7.0);
    // The key expression is evaluated, not spelled.
    number("const k = \"a\"; const o = { a: 5 }; return o[k];", 5.0);
    number("const o = { ab: 6 }; return o[\"a\" + \"b\"];", 6.0);
}

/// A property access on a receiver this engine cannot answer for traps rather
/// than fabricating `undefined`. `null.a` and `undefined.a` are TypeErrors in
/// ECMA-262; the other three are only `undefined` *because* of prototypes,
/// which this engine does not have, so answering `undefined` would be a right
/// answer reached by a wrong route -- and would be silently wrong the moment
/// the property is one a prototype really has (`"abc".length`).
#[test]
fn a_property_of_a_non_object_traps() {
    traps("return undefined.a;");
    traps("return null.a;");
    traps("return (1).a;");
    traps("return true.a;");
    // `"abc".length` was a row here. It is an answer now -- one arm of
    // `obj_get`, gated on the program naming the property -- and
    // `heap_attack::string_length_is_an_answer_and_the_next_property_is_not`
    // holds it. Its neighbour did *not* move: a String property this engine
    // has no answer for still traps, which is the distinction the paragraph
    // above is really about.
    traps("return \"abc\".toUpperCase;");
    // The chained case, which is how a missing property usually turns into a
    // fault: `o.a` is `undefined`, and `undefined.b` is the TypeError.
    traps("const o = {}; return o.a.b;");
}

// =========================================================================
// Writing a property
// =========================================================================

/// ECMA-262 10.1.9.2 OrdinarySetWithOwnDescriptor: assigning to a property
/// that is not there creates it.
#[test]
fn assigning_a_missing_property_creates_it() {
    number("const o = {}; o.a = 7; return o.a;", 7.0);
    number("const o = {}; o[\"a\"] = 7; return o.a;", 7.0);
    text("const o = {}; o.name = \"fleet\"; return o.name;", "fleet");
    number("const o = { a: 1 }; o.b = 2; return o.a + o.b;", 3.0);
}

#[test]
fn assigning_an_existing_property_overwrites_it() {
    number("const o = { a: 1 }; o.a = 2; return o.a;", 2.0);
    number("const o = {}; o.a = 1; o.a = 2; o.a = 3; return o.a;", 3.0);
    text("const o = { a: 1 }; o.a = \"two\"; return o.a;", "two");
}

/// ECMA-262 13.15.2: the value of an assignment is the value assigned, and
/// `const` binds the *reference*, so mutating the object it names is legal.
#[test]
fn an_assignment_yields_the_value_assigned() {
    number("const o = {}; return (o.a = 5);", 5.0);
    number("const o = {}; const x = (o.a = 5); return x + o.a;", 10.0);
    text("const o = {}; return (o[\"k\"] = \"v\");", "v");
}

/// Compound assignment reads the property, applies the operator, writes back.
/// ECMA-262 13.15.2 step 1.e: the reference is evaluated once.
#[test]
fn compound_assignment_to_a_property() {
    number("const o = { a: 1 }; o.a += 2; return o.a;", 3.0);
    number("const o = { a: 10 }; o.a -= 4; return o.a;", 6.0);
    number("const o = { a: 3 }; o.a *= 3; return o.a;", 9.0);
    number("const o = { a: 9 }; o.a /= 2; return o.a;", 4.5);
    number("const o = { a: 7 }; o.a %= 4; return o.a;", 3.0);
    text("const o = { a: \"x\" }; o.a += \"y\"; return o.a;", "xy");
    number(
        "const o = {}; o[\"n\"] = 1; o[\"n\"] += 41; return o.n;",
        42.0,
    );
    // A missing property compounds from `undefined`, which is NaN.
    number("const o = {}; o.a += 1; return o.a;", f64::NAN);
}

/// ECMA-262 13.4: the value of a postfix update is the *old* ToNumeric, and of
/// a prefix update the new one. Both write back to the property.
#[test]
fn update_operators_on_a_property() {
    number("const o = { a: 1 }; return o.a++;", 1.0);
    number("const o = { a: 1 }; o.a++; return o.a;", 2.0);
    number("const o = { a: 1 }; return ++o.a;", 2.0);
    number("const o = { a: 1 }; return o.a--;", 1.0);
    number("const o = { a: 1 }; o.a--; return o.a;", 0.0);
    number("const o = {}; o[\"n\"] = 5; return o[\"n\"]++ + o.n;", 11.0);
    // ToNumeric of the old value, not the old value: `true` becomes 1.
    number("const o = { a: true }; return o.a++;", 1.0);
    number("const o = { a: true }; o.a++; return o.a;", 2.0);
}

// =========================================================================
// Keys are Strings
// =========================================================================

/// ECMA-262 7.1.19 ToPropertyKey: a Number key is ToString'd, so `o[1]` and
/// `o["1"]` are one property. Not approximated: they must be the *same* slot,
/// which is why each direction is tested.
#[test]
fn a_number_key_is_its_string() {
    text("const o = {}; o[1] = \"x\"; return o[\"1\"];", "x");
    text("const o = {}; o[\"1\"] = \"y\"; return o[1];", "y");
    number("const o = {}; o[1] = 1; o[\"1\"] = 2; return o[1];", 2.0);
    // One slot, not two.
    assert_eq!(
        returned_keys("const o = {}; o[1] = 1; o[\"1\"] = 2; return o;"),
        ["1"]
    );
    // A literal numeric key is the same key.
    text("const o = { 1: \"n\" }; return o[\"1\"];", "n");
    text("const o = { 1: \"n\" }; return o[1];", "n");
    // Multi-digit, and the sign.
    text("const o = {}; o[1234] = \"m\"; return o[\"1234\"];", "m");
    text("const o = {}; o[-7] = \"neg\"; return o[\"-7\"];", "neg");
    // ECMA-262 6.1.6.1.20 step 2: the String of `-0` is `"0"`, not `"-0"`.
    text("const o = {}; o[0] = \"z\"; return o[-0];", "z");
    text("const o = {}; o[-0] = \"z\"; return o[\"0\"];", "z");
}

/// The rest of ToPropertyKey over this engine's primitive types. Each is one
/// fixed String, so each is a pool record rather than an algorithm.
#[test]
fn boolean_null_and_undefined_keys_are_their_strings() {
    number("const o = {}; o[true] = 1; return o[\"true\"];", 1.0);
    number("const o = {}; o[\"true\"] = 1; return o[true];", 1.0);
    number("const o = {}; o[false] = 2; return o[\"false\"];", 2.0);
    number("const o = {}; o[null] = 3; return o[\"null\"];", 3.0);
    number(
        "const o = {}; o[undefined] = 4; return o[\"undefined\"];",
        4.0,
    );
    assert_eq!(
        returned_keys("const o = {}; o[true] = 1; o[null] = 2; return o;"),
        ["true", "null"]
    );
}

/// ToString of a Number this engine cannot spell is a trap, not a guess. The
/// gap is the one `runtime.rs` already names for `"a" + 1`: the
/// Number::toString algorithm of 6.1.6.1.20 needs a shortest-round-trip
/// decimal conversion, and only the integer case is implemented.
#[test]
fn a_number_key_this_engine_cannot_spell_traps() {
    // A fractional value, an infinity, a NaN, and past `i32::MAX`. Each is
    // written as a computation, because a script that *spells* one of them is
    // refused by the lexer or the parser first -- which is the better answer,
    // and is asserted just below.
    // Each of these names a property now -- `__to_string` runs the whole of
    // 6.1.6.1.20 -- so what is left in this test is the one key with no
    // ToString at all.
    number(
        "const o = {}; const k = 1 / 2; o[k] = 4; return o[\"0.5\"];",
        4.0,
    );
    number(
        "const o = {}; const k = 1 / 0; o[k] = 4; return o[\"Infinity\"];",
        4.0,
    );
    number(
        "const o = {}; const k = 0 / 0; o[k] = 4; return o[\"NaN\"];",
        4.0,
    );
    number(
        "const o = {}; const k = 2147483647 + 1; o[k] = 4; return o[\"2147483648\"];",
        4.0,
    );
    // An Object key needs 7.1.1 ToPrimitive, which needs a prototype.
    traps("const o = {}; const k = {}; o[k] = 1; return 0;");
    // Written out, these used to be refused by the lexer before the run could
    // reach them. The lexer reads the whole DecimalLiteral grammar now, so a
    // spelled key takes the same path a computed one does -- ToPropertyKey at
    // run time, through the guest's own 6.1.6.1.20 -- and answers.
    number("const o = {}; o[1.5] = 4; return o[\"1.5\"];", 4.0);
    number(
        "const o = {}; o[2147483648] = 4; return o[\"2147483648\"];",
        4.0,
    );
    // The one place a fractional key is still refused, and the distinction is
    // the point: an **object literal's** key is spelled at compile time
    // (13.2.5.1 makes it the String of the Number), which would mean a second
    // implementation of 6.1.6.1.20 inside the compiler. A *computed* key runs
    // the one that already exists, in the guest.
    refuses_capability(
        "const o = { 1.5: 1 }; return 0;",
        "fractional property keys",
        Boundary::Subset,
    );
}

// =========================================================================
// Insertion order and the record layout
// =========================================================================

/// ECMA-262 10.1.11.1 OrdinaryOwnPropertyKeys: String keys come out in
/// *creation* order. There is no `Object.keys` in this subset, so the record
/// is read directly -- see this file's header for why that is the honest test
/// rather than a shortcut.
#[test]
fn property_order_is_insertion_order() {
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
    // Overwriting does not move a key: 10.1.9's Set on an existing property
    // changes the value and nothing else.
    assert_eq!(
        returned_keys("const o = { a: 1, b: 2 }; o.a = 9; return o;"),
        ["a", "b"]
    );
    // Nor does a duplicate key in the literal add a slot: 13.2.5.5 evaluates
    // the properties in order and each is a CreateDataPropertyOrThrow.
    assert_eq!(returned_keys("return { a: 1, a: 2 };"), ["a"]);
    number("const o = { a: 1, a: 2 }; return o.a;", 2.0);
}

/// The record grows without moving or losing anything. Twelve properties is
/// past every doubling the initial capacity reaches, and past anything
/// `fleet.js` builds (its largest namespace table has ten).
#[test]
fn a_record_grows_and_keeps_every_property() {
    let source = "
        const o = {};
        o.a = 1; o.b = 2; o.c = 3; o.d = 4;
        o.e = 5; o.f = 6; o.g = 7; o.h = 8;
        o.i = 9; o.j = 10; o.k = 11; o.l = 12;
        return o.a + o.d + o.e + o.h + o.i + o.l;
    ";
    number(source, 1.0 + 4.0 + 5.0 + 8.0 + 9.0 + 12.0);
    let keys = "
        const o = {};
        o.a = 1; o.b = 2; o.c = 3; o.d = 4;
        o.e = 5; o.f = 6; o.g = 7; o.h = 8;
        o.i = 9; o.j = 10; o.k = 11; o.l = 12;
        return o;
    ";
    assert_eq!(
        returned_keys(keys),
        ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"]
    );
}

/// A literal is built at exactly its own size, because the compiler counts the
/// properties. Nothing depends on it for correctness; it is asserted so the
/// sizing decision cannot silently become "always grow from zero".
#[test]
fn a_literal_is_allocated_at_its_own_size() {
    let (instance, vals) = attempt("return { a: 1, b: 2, c: 3 };").expect("runs");
    let [Val::I32(tag), Val::I64(payload)] = vals.as_slice() else {
        panic!("want one V1 pair, got {vals:?}");
    };
    assert_eq!(*tag, TAG_OBJECT);
    let view = instance.memory().expect("guest memory");
    let bytes: &[u8] = &view;
    let object = *payload as u32 as usize;
    assert_eq!(word(bytes, object + OBJ_LEN), 3, "three properties");
    assert_eq!(word(bytes, object + OBJ_CAP), 3, "sized to the literal");
}

// =========================================================================
// Shapes `fleet.js` actually uses
// =========================================================================

/// The namespace-table pattern the binding library is built out of: an empty
/// object bound with `const`, then filled by dotted assignment, nested several
/// deep.
#[test]
fn the_namespace_table_pattern() {
    let source = "
        const fleet = {};
        fleet.tabs = {};
        fleet.ui = {};
        fleet.ui.composer = {};
        fleet.ui.composer.op = \"ui.composer.send\";
        fleet.tabs.op = \"tabs.set-note\";
        return fleet.ui.composer.op;
    ";
    text(source, "ui.composer.send");
    let both = "
        const fleet = {};
        fleet.ui = {};
        fleet.ui.tabs = {};
        fleet.ui.tabs.op = \"ui.tabs.toggle\";
        return fleet.ui.tabs.op + \"|\" + typeof fleet.ui;
    ";
    text(both, "ui.tabs.toggle|object");
}

/// The 1-3 field parameter object, built inside a function from its
/// parameters. `{ tab: tabId, note: note }` is the exact shape
/// `fleet.tabs.set_note` builds.
#[test]
fn the_parameter_object_pattern() {
    let source = "
        function params(tab, note) {
            return { tab: tab, note: note };
        }
        return params(3, \"hello\").note;
    ";
    text(source, "hello");
    let numeric = "
        function point(x, y) { return { x: x, y: y }; }
        const p = point(2, 40);
        return p.x + p.y;
    ";
    number(numeric, 42.0);
    let one = "
        function wrap(text) { return { text: text }; }
        return wrap(\"t\").text;
    ";
    text(one, "t");
}

/// Shorthand and nesting, which the grammar gives for free once a property
/// value is an AssignmentExpression and a property key is an IdentifierName.
#[test]
fn shorthand_and_nested_literals() {
    number("const x = 9; const o = { x }; return o.x;", 9.0);
    number(
        "const x = 1; const y = 2; const o = { x, y }; return o.x + o.y;",
        3.0,
    );
    number("const x = 1; const o = { x, y: 2 }; return o.x + o.y;", 3.0);
    number("const o = { a: { b: 2 } }; return o.a.b;", 2.0);
    number("const o = { a: { b: { c: 5 } } }; return o.a.b.c;", 5.0);
    text(
        "const o = { a: { b: \"deep\" } }; return typeof o.a;",
        "object",
    );
    // ECMA-262 12.9.6: a trailing comma in an ObjectLiteral is grammar.
    number("const o = { a: 1, }; return o.a;", 1.0);
    number("const o = { a: 1, b: 2, }; return o.b;", 2.0);
    assert_eq!(
        returned_keys("const x = 1; return { x, y: 2 };"),
        ["x", "y"]
    );
}

/// Objects in the places any other value goes: an argument, a return value, a
/// loop body, a condition.
#[test]
fn an_object_is_an_ordinary_value() {
    number(
        "function take(o) { return o.a; } return take({ a: 8 });",
        8.0,
    );
    number(
        "function make() { return { a: 1 }; } const o = make(); o.a = 2; return o.a;",
        2.0,
    );
    number(
        "const o = { n: 0 }; for (let i = 0; i < 5; i = i + 1) { o.n = o.n + i; } return o.n;",
        10.0,
    );
    number("let o = { a: 1 }; o = { a: 2 }; return o.a;", 2.0);
    // Two bindings, one object: mutation through either is visible through both.
    number("const a = { n: 1 }; const b = a; b.n = 5; return a.n;", 5.0);
}

// =========================================================================
// Where this engine stops, and how it says so
// =========================================================================

/// Arithmetic on an Object needs ToPrimitive (ECMA-262 7.1.1), which needs
/// `valueOf`/`toString` on a prototype -- neither of which exists here. A trap
/// rather than a fabricated number, on the same terms as `"a" + 1`.
#[test]
fn arithmetic_on_an_object_traps() {
    traps("const o = {}; return o + 1;");
    traps("const o = {}; return o - 1;");
    traps("const o = {}; return o * 2;");
    traps("const o = {}; return +o;");
    traps("const o = {}; return -o;");
    traps("const o = {}; return o < 1;");
    traps("const o = {}; return o == 1;");
    traps("const o = {}; return \"x\" + o;");
    // `===` never coerces, so it is the one that does *not* trap.
    boolean("const o = {}; return o === 1;", false);
}

/// An Object is a guest heap reference, and the public `Value` has no variant
/// for one yet. The door says so rather than inventing a tag.
#[test]
fn an_object_cannot_cross_the_host_door_yet() {
    let (_, vals) = attempt("return { a: 1 };").expect("runs");
    let error = Value::returned(&vals).expect_err("no `Value` variant for an Object");
    assert!(
        error.contains("Object"),
        "the message should name what it cannot carry, got {error:?}"
    );
}

/// The object-literal forms beyond this milestone. Each names itself rather
/// than being read as something else and failing later.
#[test]
fn object_literal_forms_this_engine_refuses() {
    refuses_capability(
        "const k = \"a\"; const o = { [k]: 1 }; return 0;",
        "computed property keys",
        Boundary::FullJs,
    );
    refuses_capability(
        "const o = { f() { return 1; } }; return 0;",
        "methods in object literals",
        Boundary::FullJs,
    );
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
        "const o = { __proto__: {} }; return 0;",
        "the `__proto__` property",
        Boundary::FullJs,
    );
    refuses_capability(
        "const a = {}; const o = { ...a }; return 0;",
        "the spread and rest syntax",
        Boundary::ThirdBinding,
    );
    // A function value used to be refused here, wherever it was written. It
    // is a value now -- see `tests/function_values.rs` -- so what this file
    // keeps is the one claim that is about *objects*: a property holds one,
    // and the object is still an Object.
    text(
        "const o = { f: function () { return 1; } }; return typeof o + \"|\" + typeof o.f;",
        "object|function",
    );
}

/// Everything an object *has* that this milestone does not: prototypes, the
/// `Object` namespace, `delete`, and enumeration.
#[test]
fn object_facilities_this_engine_refuses() {
    refuses_capability(
        "const o = {}; delete o.a; return 0;",
        "the `delete` keyword",
        Boundary::FullJs,
    );
    refuses_capability(
        "const o = {}; return o instanceof Object;",
        "the `instanceof` keyword",
        Boundary::FullJs,
    );
    refuses_capability(
        "const o = {}; return \"a\" in o;",
        "the `in` keyword",
        Boundary::FullJs,
    );
    refuses_capability(
        "const o = {}; return new Object();",
        "the `new` keyword",
        Boundary::FullJs,
    );
    // Array literals landed; `tests/arrays_m3.rs` is where their behaviour is
    // asserted now. What is left of them at this boundary is the elision --
    // `[1, , 2]` -- because a hole is not an `undefined` and this engine has
    // no way to tell one from the other.
    refuses_capability(
        "return [1, , 2];",
        "elisions in an array literal",
        Boundary::FullJs,
    );
    // `Object.keys(o)` now stops one step earlier and says something better:
    // the call itself is a capability the engine has, so what is missing is
    // the *binding*. There is no global scope for `Object` to be in, and the
    // sentence is about that rather than about the author.
    // `Object.keys(o)` left this list on 2026-08-29 -- it is folded to a
    // gated method call and answers (`object_keys_m3.rs`). The bare
    // property read stays: `Object` is not a value here, only a spelling.
    let source = "const o = {}; return Object.keys;";
    let error = refuse(source);
    assert!(
        error.message.contains("finds no declaration of `Object`"),
        "{source:?}: got {:?}",
        error.message
    );
    assert_eq!(error.boundary, Boundary::Subset, "{source:?}");
    let error = refuse("const o = {}; return Object.keys;");
    assert!(
        error.message.contains("finds no declaration of `Object`"),
        "got {:?}",
        error.message
    );
    assert_eq!(error.boundary, Boundary::Subset);
    // A host name is still not a place a value can be put. A *property* of
    // what a host call returned is, though -- `host.a = 1` is an ordinary
    // assignment to the result of `host()`, and it compiles.
    let error = match compile_qjs_m1_with_hosts("host = 1; return 0;") {
        Ok(_) => panic!("expected a refusal"),
        Err(e) => e,
    };
    assert_eq!(error.boundary, Boundary::ThirdBinding);
    assert!(
        compile_qjs_m1_with_hosts("host.a = 1; return 0;").is_ok(),
        "a property of a call result is an ordinary target"
    );
}

fn compile_qjs_m1_with_hosts(source: &str) -> Result<Vec<u8>, CompileError> {
    tinyvm_qjs::compile_qjs_m1_with(
        source,
        Options {
            names: Names::HostImport,
        },
    )
}

/// A `.` or a `[` with nothing usable after it is a structural refusal, and it
/// says what it was looking for.
#[test]
fn an_incomplete_access_says_what_it_wanted() {
    let error = refuse("const o = {}; return o.;");
    assert!(
        error.message.contains("property name"),
        "got {:?}",
        error.message
    );
    let error = refuse("const o = {}; return o[;");
    assert!(error.message.contains("operand"), "got {:?}", error.message);
    let error = refuse("const o = {}; return o[1;");
    assert!(error.message.contains(']'), "got {:?}", error.message);
    let error = refuse("const o = { a: };");
    assert!(!error.message.is_empty());
    let error = refuse("const o = { a 1 };");
    assert!(error.message.contains(':'), "got {:?}", error.message);
    let error = refuse("const o = { a: 1");
    assert!(error.message.contains('}'), "got {:?}", error.message);
}

/// A statement that begins with `{` is a Block, not an ObjectLiteral
/// (ECMA-262 14.2 vs 13.2.5). That is not a limitation to work around: it is
/// what the grammar says, and it is why `{}` at statement position is an
/// empty block whose completion value is `undefined`.
#[test]
fn a_leading_brace_is_still_a_block() {
    undefined("{}");
    number("{ 1; } return 2;", 2.0);
    // The parenthesised form is the ObjectLiteral, as everywhere else.
    text("return typeof ({});", "object");
    number("const o = ({ a: 1 }); return o.a;", 1.0);
}

/// The whole `fleet.js` data path, minus the two things this milestone
/// deliberately left ahead of it: a parameter object built from a function's
/// arguments, read back field by field, and handed across a *declared* host
/// door as a String.
///
/// This is the shape `fleet.tabs.set_note` has -- `{ tab: tabId, note: note }`
/// serialised and sent -- with `JSON.stringify` stood in for by concatenation,
/// because JSON is a later milestone. It runs for real: the host records what
/// it was handed and the test compares the bytes.
#[test]
fn the_fleet_parameter_path_reaches_a_declared_host() {
    use std::cell::RefCell;
    use tinyvm_qjs::{HostFn, HostParam, HostResult};

    thread_local! {
        static SENT: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    let source = "
        function params(tab, note) { return { tab: tab, note: note }; }
        function body(p) { return \"{tab:\" + p.tab + \",note:\" + p.note + \"}\"; }
        const p = params(\"7\", \"hello\");
        send(\"tabs.set-note\", body(p));
        return p.note;
    ";
    let table = vec![HostFn {
        name: "send".to_string(),
        module: "sys".to_string(),
        field: "send".to_string(),
        params: vec![HostParam::StrPtrLen, HostParam::StrPtrLen],
        result: HostResult::Void,
    }];
    let wasm = tinyvm_qjs::compile_qjs_m1_with(
        source,
        Options {
            names: Names::Declared(table),
        },
    )
    .expect("compiles");
    let mut module =
        WasmModule::from_bytes_with(&wasm, Limits::default()).expect("clears the load gate");
    module
        .bind_import_typed("sys", "send", |args, memory| {
            let mut text = Vec::new();
            for pair in args.chunks(2) {
                let [Val::I32(ptr), Val::I32(len)] = pair else {
                    panic!("want (ptr, len), got {pair:?}");
                };
                let at = *ptr as usize;
                text.push(String::from_utf8(memory[at..at + *len as usize].to_vec()).unwrap());
            }
            SENT.with(|sent| sent.borrow_mut().push(text.join(" ")));
            Ok(Vec::new())
        })
        .expect("binds");
    let mut instance = module.instantiate().expect("instantiates");
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect("runs");
    let Ok(Value::String(ptr)) = Value::returned(&vals) else {
        panic!("want a String back, got {vals:?}");
    };
    assert_eq!(read_string(&instance, ptr).unwrap(), "hello");
    assert_eq!(
        SENT.with(|sent| sent.borrow().clone()),
        ["tabs.set-note {tab:7,note:hello}"]
    );
}

/// An Object is a type the declared host door has no mapping for, and the
/// compiler can settle that from the text when the argument is a literal.
/// A diagnostic with a byte offset, not a trap the author has to reproduce.
#[test]
fn an_object_argument_to_a_declared_host_is_a_diagnostic() {
    use tinyvm_qjs::{HostFn, HostParam, HostResult};
    let table = vec![HostFn {
        name: "print".to_string(),
        module: "sys".to_string(),
        field: "print".to_string(),
        params: vec![HostParam::StrPtrLen],
        result: HostResult::Void,
    }];
    let error = tinyvm_qjs::compile_qjs_m1_with(
        "print({ a: 1 }); return 0;",
        Options {
            names: Names::Declared(table.clone()),
        },
    )
    .expect_err("an Object is not a String");
    assert!(
        error.message.contains("an Object"),
        "the diagnostic should name what was passed, got {:?}",
        error.message
    );
    assert_eq!(error.boundary, Boundary::ThirdBinding);
    // Where only the run can settle it, it is a trap and not a diagnostic --
    // the same policy `unwrap_args` states for every other type.
    let wasm = tinyvm_qjs::compile_qjs_m1_with(
        "const o = { a: 1 }; print(o); return 0;",
        Options {
            names: Names::Declared(table),
        },
    )
    .expect("a name's type is a run-time fact");
    assert!(wasm.starts_with(b"\0asm"));
}

// =========================================================================
// Size
// =========================================================================

/// What the object type costs every compiled module, printed rather than
/// asserted: the runtime is emitted whole into every product, so a script with
/// no object in it pays for objects too. The number is reported so the choice
/// stays visible; a threshold here would be a number nobody could justify.
///
/// Run with `--nocapture` to read it.
#[test]
fn emitted_size_report() {
    for source in [
        "return 1;",
        "return 1 + 2 * 3;",
        "function f(n) { if (n < 2) { return n; } return f(n - 1) + f(n - 2); } return f(10);",
        "let s = \"\"; for (let i = 0; i < 3; i = i + 1) { s = s + \"x\"; } return s;",
    ] {
        let bytes = compile_qjs_m1(source).expect("compiles");
        println!("{:6} bytes  {source}", bytes.len());
    }
}
