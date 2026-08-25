//! Adversarial pressure on the object heap: the flat key/value record, the
//! bump allocator it shares with strings, and the boundaries that are supposed
//! to be traps rather than corruption.
//!
//! Nothing here proposes a design. Every test is an attempt to make the engine
//! answer wrongly, and every expectation is derived from ECMA-262 or from a
//! guarantee `runtime.rs` states in prose. A panic in the compiler is always a
//! bug; a typed refusal or a trap is a correct answer.
//!
//! # What is red, and why
//!
//! Four tests fail, all four on one defect, all four in section A18. A
//! `HostResult::Bytes` length is taken from the host and never checked to be a
//! length: `emit::two_pass_string` stores it as a string record's header and
//! passes it to `__alloc`, and the only guard it has compares the host's copy
//! answer to the host's length answer, so two matching lies pass. A negative
//! length then makes `__alloc`'s `(size + 3) & -4` *negative*, which walks the
//! bump pointer backwards, below `DATA_ORIGIN`, over the fault word --
//! breaking, in one step, both the README's "a short or negative write traps
//! instead of producing a String with a fabricated tail" and `FAULT_WORD`'s
//! "a word no allocation can ever hand out".
//!
//! Everything else -- 68 tests over width, depth, growth, identity, key
//! spelling, exhaustion, two heaps in one allocator, and the equality ladder
//! from both sides -- is green.

use tinyvm::{Limits, Val, WasmInstance, WasmModule};
use tinyvm_qjs::{GuestFault, Value, compile_qjs_m1, guest_fault};

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
    Object(i32),
}

const TAG_OBJECT: i32 = 5;
const OBJ_LEN: usize = 0;
const OBJ_CAP: usize = 4;
const OBJ_ENTRIES: usize = 8;
const ENTRY_BYTES: usize = 16;

fn limits() -> Limits {
    Limits::default()
}

fn attempt_with(source: &str, limits: Limits) -> Result<(WasmInstance, Vec<Val>), String> {
    let wasm = compile_qjs_m1(source).map_err(|e| format!("compile: {e}"))?;
    let module = WasmModule::from_bytes_with(&wasm, limits)
        .map_err(|e| format!("load gate: {}", e.message()))?;
    let mut instance = module
        .instantiate()
        .map_err(|e| format!("instantiate: {}", e.message()))?;
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .map_err(|e| format!("trap: {}", e.message()))?;
    Ok((instance, vals))
}

fn attempt(source: &str) -> Result<(WasmInstance, Vec<Val>), String> {
    attempt_with(source, limits())
}

fn decode(instance: &WasmInstance, vals: &[Val]) -> Out {
    let [Val::I32(tag), Val::I64(payload)] = vals else {
        panic!("want one V1 pair back, got {vals:?}");
    };
    match *tag {
        0 => Out::Undefined,
        1 => Out::Number(f64::from_bits(*payload as u64)),
        2 => Out::Bool(*payload != 0),
        3 => Out::Str(read_string(instance, *payload as u32 as i32).expect("string record")),
        4 => Out::Null,
        5 => Out::Object(*payload as u32 as i32),
        other => panic!("unknown tag {other}"),
    }
}

#[track_caller]
fn run(source: &str) -> Out {
    let (instance, vals) = attempt(source).unwrap_or_else(|e| panic!("{e}"));
    decode(&instance, &vals)
}

fn read_string(instance: &WasmInstance, ptr: i32) -> Result<String, String> {
    let view = instance.memory().map_err(|e| e.message().to_string())?;
    read_string_at(&view, ptr)
}

fn read_string_at(bytes: &[u8], ptr: i32) -> Result<String, String> {
    let at = ptr as usize;
    let header = bytes
        .get(at..at + 4)
        .ok_or_else(|| format!("string header at {ptr} out of bounds"))?;
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let body = bytes
        .get(at + 4..at + 4 + len)
        .ok_or_else(|| format!("string body at {ptr} (len {len}) out of bounds"))?;
    String::from_utf8(body.to_vec()).map_err(|_| "not utf-8".to_string())
}

fn word(bytes: &[u8], at: usize) -> i32 {
    i32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// (len, cap, keys) of the object `main` returned.
#[track_caller]
fn returned_record(source: &str) -> (i32, i32, Vec<String>) {
    let (instance, vals) = attempt(source).unwrap_or_else(|e| panic!("{e}"));
    let [Val::I32(tag), Val::I64(payload)] = vals.as_slice() else {
        panic!("want a pair, got {vals:?}");
    };
    assert_eq!(*tag, TAG_OBJECT, "want an Object back");
    let view = instance.memory().expect("memory");
    let bytes: &[u8] = &view;
    let object = *payload as u32 as usize;
    let len = word(bytes, object + OBJ_LEN);
    let cap = word(bytes, object + OBJ_CAP);
    let entries = word(bytes, object + OBJ_ENTRIES) as usize;
    let keys = (0..len as usize)
        .map(|i| read_string_at(bytes, word(bytes, entries + i * ENTRY_BYTES)).expect("key"))
        .collect();
    (len, cap, keys)
}

#[track_caller]
fn number(source: &str, want: f64) {
    match run(source) {
        Out::Number(got) if got.to_bits() == want.to_bits() => {}
        other => panic!("want Number({want}), got {other:?}"),
    }
}

#[track_caller]
fn text(source: &str, want: &str) {
    assert_eq!(run(source), Out::Str(want.to_string()));
}

#[track_caller]
fn boolean(source: &str, want: bool) {
    assert_eq!(run(source), Out::Bool(want));
}

#[track_caller]
fn undefined(source: &str) {
    assert_eq!(run(source), Out::Undefined);
}

/// Compiles, clears the load gate, and then traps -- the correct answer for a
/// boundary only the run can see.
#[track_caller]
fn traps(source: &str) {
    match attempt(source) {
        Err(message) => assert!(
            message.starts_with("trap:"),
            "failed for the wrong reason: {message}"
        ),
        Ok((instance, vals)) => panic!(
            "produced {:?} instead of trapping",
            decode(&instance, &vals)
        ),
    }
}

// =========================================================================
// A1. Width: an object with very many keys
// =========================================================================

/// Growth doubles from `FIRST_CAP`; two hundred appends is six reallocations
/// and six abandoned vectors. Every key must still be findable, and the record
/// must still say it holds exactly two hundred of them.
#[test]
fn two_hundred_keys_by_assignment() {
    let mut src = String::from("var o = {};");
    for i in 0..200 {
        src.push_str(&format!("o.k{i} = {i};"));
    }
    src.push_str("return o.k0 + o.k137 + o.k199;");
    number(&src, 0.0 + 137.0 + 199.0);

    let mut src = String::from("var o = {};");
    for i in 0..200 {
        src.push_str(&format!("o.k{i} = {i};"));
    }
    src.push_str("return o;");
    let (len, cap, keys) = returned_record(&src);
    assert_eq!(len, 200, "len");
    assert!(cap >= 200, "cap {cap} must cover len");
    assert_eq!(keys.len(), 200);
    for (i, key) in keys.iter().enumerate() {
        assert_eq!(key, &format!("k{i}"), "entry {i} out of order");
    }
}

/// A literal is allocated at its exact property count and never reallocates.
#[test]
fn two_hundred_keys_by_literal() {
    let body: Vec<String> = (0..200).map(|i| format!("k{i}: {i}")).collect();
    let src = format!("var o = {{{}}}; return o.k0 + o.k199;", body.join(","));
    number(&src, 199.0);
    let src = format!("return {{{}}};", body.join(","));
    let (len, cap, keys) = returned_record(&src);
    assert_eq!((len, cap), (200, 200), "a literal is sized exactly");
    assert_eq!(keys[199], "k199");
}

/// Every key readable, one at a time, after all the growth.
#[test]
fn every_one_of_a_hundred_keys_reads_back() {
    let mut src = String::from("var o = {}; var sum = 0;");
    for i in 0..100 {
        src.push_str(&format!("o.k{i} = {i};"));
    }
    for i in 0..100 {
        src.push_str(&format!("sum = sum + o.k{i};"));
    }
    src.push_str("return sum;");
    number(&src, (0..100).sum::<i32>() as f64);
}

// =========================================================================
// A2. A very long key
// =========================================================================

#[test]
fn a_four_kilobyte_key() {
    let key = "z".repeat(4096);
    number(
        &format!("var o = {{}}; o[\"{key}\"] = 7; return o[\"{key}\"];"),
        7.0,
    );
    // One byte different at the very end must be a different property.
    let mut other = key.clone();
    other.pop();
    other.push('y');
    undefined(&format!(
        "var o = {{}}; o[\"{key}\"] = 7; return o[\"{other}\"];"
    ));
}

/// A key that is a prefix of another key is a different key. `__str_eq`
/// compares the length first, so this is the test that the length compare is
/// there at all.
#[test]
fn a_prefix_key_is_a_different_key() {
    number(
        "var o = {}; o.a = 1; o.aa = 2; o.aaa = 3; return o.a * 100 + o.aa * 10 + o.aaa;",
        123.0,
    );
    let (len, _, keys) = returned_record("var o = {}; o.a = 1; o.aa = 2; return o;");
    assert_eq!(len, 2);
    assert_eq!(keys, vec!["a".to_string(), "aa".to_string()]);
}

/// The empty string is a property key like any other (ECMA-262 6.1.7: a
/// property key is any String, including `""`).
#[test]
fn the_empty_string_is_a_key() {
    number("var o = {}; o[\"\"] = 5; return o[\"\"];", 5.0);
    undefined("var o = {}; o.a = 1; return o[\"\"];");
    let (len, _, keys) = returned_record("var o = {}; o[\"\"] = 1; o.a = 2; return o;");
    assert_eq!(len, 2);
    assert_eq!(keys, vec![String::new(), "a".to_string()]);
}

// =========================================================================
// A3. Depth
// =========================================================================

#[test]
fn a_literal_nested_sixty_deep() {
    let depth = 60;
    let src = format!(
        "var o = {}{}{}; return o{}.leaf;",
        "{ a: ".repeat(depth),
        "{ leaf: 42 }",
        " }".repeat(depth),
        ".a".repeat(depth)
    );
    number(&src, 42.0);
}

/// Past the compiler's frame budget the answer must be a diagnostic, never a
/// stack overflow -- which is a process abort and takes the host with it.
#[test]
fn a_literal_nested_past_the_frame_budget_is_a_diagnostic() {
    for depth in [200usize, 2_000, 20_000] {
        let src = format!(
            "return {}{}{};",
            "{ a: ".repeat(depth),
            "1",
            " }".repeat(depth)
        );
        match compile_qjs_m1(&src) {
            Ok(_) => { /* within budget: fine, it must then also run */ }
            Err(e) => assert!(
                e.message.contains("does not support") || e.message.contains("nest"),
                "depth {depth}: {}",
                e.message
            ),
        }
    }
}

#[test]
fn a_member_chain_past_the_frame_budget_is_a_diagnostic() {
    for depth in [200usize, 2_000, 20_000] {
        let src = format!("var o = {{}}; return o{};", ".a".repeat(depth));
        let _ = compile_qjs_m1(&src);
    }
}

// =========================================================================
// A4. The heap running out
// =========================================================================

/// The guest must be able to *say* the heap ran out. A loop that concatenates
/// forever reaches the ceiling; the fault word has to name it.
#[test]
fn an_exhausted_heap_says_so_from_a_string_loop() {
    let src = "var s = \"xxxxxxxxxxxxxxxx\"; var i = 0; while (i < 1000000) { s = s + \"y\"; i = i + 1; } return s;";
    let wasm = compile_qjs_m1(src).expect("compiles");
    let tight = Limits {
        max_memory_pages: 4,
        max_steps: 200_000_000,
        ..Limits::default()
    };
    let module = WasmModule::from_bytes_with(&wasm, tight).expect("load gate");
    let mut instance = module.instantiate().expect("instantiate");
    let err = instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect_err("the heap cannot hold a million concatenations of 4 pages");
    let memory = instance.memory().expect("memory zero");
    assert_eq!(
        guest_fault(&memory),
        Some(GuestFault::HeapExhausted),
        "the guest must name the budget, not leave `{}` to be guessed at",
        err.message()
    );
}

/// The same ceiling, reached through the *object* heap instead: one object per
/// turn, each one growing its own entry vector.
#[test]
fn an_exhausted_heap_says_so_from_an_object_loop() {
    let src = "var i = 0; var last = 0; while (i < 1000000) { var o = {}; o.a = i; o.b = i; o.c = i; o.d = i; o.e = i; last = o.e; i = i + 1; } return last;";
    let wasm = compile_qjs_m1(src).expect("compiles");
    let tight = Limits {
        max_memory_pages: 4,
        max_steps: 200_000_000,
        ..Limits::default()
    };
    let module = WasmModule::from_bytes_with(&wasm, tight).expect("load gate");
    let mut instance = module.instantiate().expect("instantiate");
    let err = instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect_err("a million five-property objects do not fit in 4 pages");
    let memory = instance.memory().expect("memory zero");
    assert_eq!(
        guest_fault(&memory),
        Some(GuestFault::HeapExhausted),
        "the object heap must name the budget too, not leave `{}` to be guessed at",
        err.message()
    );
}

/// A trap that is *not* the heap must not claim it is.
#[test]
fn an_ordinary_fault_does_not_claim_the_heap_ran_out() {
    let wasm = compile_qjs_m1("var x = undefined; return x.a;").expect("compiles");
    let module = WasmModule::from_bytes_with(&wasm, limits()).expect("load gate");
    let mut instance = module.instantiate().expect("instantiate");
    instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect_err("undefined.a is a fault");
    let memory = instance.memory().expect("memory zero");
    assert_eq!(guest_fault(&memory), None);
}

// =========================================================================
// A5. A receiver that is not an Object
// =========================================================================

#[test]
fn property_access_on_a_primitive_traps() {
    traps("var x = 1; return x.a;");
    traps("var x = \"abc\"; return x.a;");
    traps("var x = null; return x.a;");
    traps("var x = undefined; return x.a;");
    traps("var x = true; return x.a;");
    traps("var x = 1; return x[\"a\"];");
    traps("var x = null; x.a = 1; return 0;");
    traps("var x = undefined; x.a = 1; return 0;");
}

/// `"abc".length` is 3 in ECMA-262 only because `String.prototype` exists.
/// There is no prototype here, so a trap is the honest answer -- but the
/// engine knows the receiver is a String at compile time, so record what it
/// actually does.
#[test]
fn string_length_is_a_trap_not_an_answer() {
    traps("var s = \"abc\"; return s.length;");
    traps("return \"abc\".length;");
}

// =========================================================================
// A6. Identity, aliasing, and self-reference
// =========================================================================

/// The record's address is the object's identity, and growth reallocates the
/// *entry vector* and not the record. An alias taken before the growth must
/// still be the same object afterwards, and must see the new properties.
#[test]
fn growth_does_not_move_the_object() {
    let mut src = String::from("var o = {}; var alias = o;");
    for i in 0..40 {
        src.push_str(&format!("o.k{i} = {i};"));
    }
    src.push_str("return (alias === o) && (alias.k39 === 39) && (alias.k0 === 0);");
    boolean(&src, true);
}

/// An object is expressible as its own property value; reading the cycle back
/// must reach the same record.
#[test]
fn an_object_may_hold_itself() {
    boolean("var o = {}; o.self = o; return o.self === o;", true);
    boolean(
        "var o = {}; o.self = o; return o.self.self.self.self === o;",
        true,
    );
    number(
        "var o = {}; o.self = o; o.n = 7; return o.self.self.n;",
        7.0,
    );
    let (len, _, keys) = returned_record("var o = {}; o.self = o; return o;");
    assert_eq!(len, 1);
    assert_eq!(keys, vec!["self".to_string()]);
}

/// A cycle of two.
#[test]
fn two_objects_may_point_at_each_other() {
    boolean(
        "var a = {}; var b = {}; a.other = b; b.other = a; return a.other.other === a;",
        true,
    );
}

/// Mutating through one name is visible through the other -- an object is a
/// reference, not a copy.
#[test]
fn an_object_is_a_reference() {
    number("var a = {}; var b = a; b.x = 5; return a.x;", 5.0);
    number(
        "var a = {}; var b = {}; b.inner = a; a.x = 9; return b.inner.x;",
        9.0,
    );
}

/// Two literals with the same shape and the same contents are two objects.
#[test]
fn two_equal_literals_are_two_objects() {
    boolean("return {a:1} === {a:1};", false);
    boolean("return {} === {};", false);
    boolean("var o = {a:1}; return o === o;", true);
}

// =========================================================================
// A7. What a slot holds: the pair goes in and comes back bit for bit
// =========================================================================

/// The entry stores the V1 pair whole. Nothing may be re-boxed on the way out,
/// which is exactly what `-0` and a NaN would expose.
#[test]
fn a_property_round_trips_every_type_bit_for_bit() {
    number("var o = {}; o.a = -0; return o.a;", -0.0);
    number("var o = {a: -0}; return o.a;", -0.0);
    number("var o = {}; o.a = 0/0; return o.a;", f64::NAN);
    number("var o = {}; o.a = 1/0; return o.a;", f64::INFINITY);
    number("var o = {}; o.a = -1/0; return o.a;", f64::NEG_INFINITY);
    number(
        "var o = {}; o.a = 1/10 + 2/10; return o.a;",
        0.1f64 + 0.2f64,
    );
    text("var o = {}; o.a = \"hi\"; return o.a;", "hi");
    boolean("var o = {}; o.a = true; return o.a;", true);
    boolean("var o = {}; o.a = false; return o.a;", false);
    assert_eq!(run("var o = {}; o.a = null; return o.a;"), Out::Null);
    undefined("var o = {}; o.a = undefined; return o.a;");
    // `-0` stored, and it is `-0` and not `0`.
    boolean("var o = {}; o.a = -0; return 1 / o.a === -1/0;", true);
}

/// `undefined` explicitly stored and a property that was never there are the
/// same *value* and a different *record* -- 10.1.8.1 answers `undefined` for
/// both, but only one of them is an own property.
#[test]
fn an_explicit_undefined_is_still_an_own_property() {
    undefined("var o = {}; o.a = undefined; return o.a;");
    undefined("var o = {}; return o.a;");
    let (len, _, keys) = returned_record("var o = {}; o.a = undefined; return o;");
    assert_eq!(len, 1, "an explicitly stored undefined is an own property");
    assert_eq!(keys, vec!["a".to_string()]);
    let (len, _, _) = returned_record("var o = {}; return o;");
    assert_eq!(len, 0);
}

/// Overwriting changes the type in place and leaves the position alone.
#[test]
fn overwriting_changes_type_without_moving() {
    text(
        "var o = {a: 1, b: 2}; o.a = \"s\"; return typeof o.a;",
        "string",
    );
    let (len, _, keys) =
        returned_record("var o = {a:1, b:2, c:3}; o.a = \"s\"; o.b = {}; return o;");
    assert_eq!(len, 3);
    assert_eq!(
        keys,
        vec!["a".to_string(), "b".to_string(), "c".to_string()]
    );
    number("var o = {a: 1, b: 2, c: 3}; o.b = 99; return o.c;", 3.0);
}

// =========================================================================
// A8. Number keys, over the whole of Number::toString
// =========================================================================

#[test]
fn a_number_key_and_its_string_are_one_property() {
    number("var o = {}; o[7] = 1; o[\"7\"] = 2; return o[7];", 2.0);
    let (len, _, keys) = returned_record("var o = {}; o[7] = 1; o[\"7\"] = 2; return o;");
    assert_eq!(len, 1, "7 and \"7\" are one property (ECMA-262 7.1.19)");
    assert_eq!(keys, vec!["7".to_string()]);
    // 6.1.6.1.20 step 2: -0 stringifies to "0".
    number("var o = {}; o[-0] = 3; return o[\"0\"];", 3.0);
    number("var o = {}; o[0] = 3; return o[-0];", 3.0);
    text("var o = {}; o[-5] = 1; o[-5] = 2; return \"ok\";", "ok");
    number("var o = {}; o[-5] = 8; return o[\"-5\"];", 8.0);
    number(
        "var o = {}; o[2147483647] = 1; return o[\"2147483647\"];",
        1.0,
    );
    number("var o = {}; o[100] = 1; return o[\"100\"];", 1.0);
    number("var o = {}; o[10 * 10] = 1; return o[\"100\"];", 1.0);
}

/// There is no integer domain any more: `__to_string` runs the whole of
/// 6.1.6.1.20, so a fractional, infinite or NaN key names a property.
#[test]
fn a_number_key_outside_the_integers_names_a_property() {
    number("var o = {}; o[1/2] = 3; return o[\"0.5\"];", 3.0);
    number("var o = {}; o[0/0] = 3; return o[\"NaN\"];", 3.0);
    number("var o = {}; o[1/0] = 3; return o[\"Infinity\"];", 3.0);
    number(
        "var o = {}; o[(0 - 2147483647) - 1] = 3; return o[\"-2147483648\"];",
        3.0,
    );
    // Reading an absent one is still `undefined` rather than a fault.
    let (len, _, keys) = returned_record("var o = {}; o[1/2] = 1; return o;");
    assert_eq!(len, 1);
    assert_eq!(keys, vec!["0.5".to_string()]);
}

/// An Object as a key needs `ToPrimitive`, which needs a prototype.
#[test]
fn an_object_key_traps() {
    traps("var o = {}; var k = {}; return o[k];");
    traps("var o = {}; var k = {}; o[k] = 1; return 0;");
}

// =========================================================================
// A9. Three hundred computed keys in a loop
// =========================================================================

/// Every key is a fresh `__num_to_string` record, so this is the object heap and
/// the string heap interleaved three hundred times, plus seven reallocations.
#[test]
fn three_hundred_computed_keys_in_a_loop() {
    let src = "var o = {}; var i = 0; while (i < 300) { o[i] = i * 2; i = i + 1; } \
               return o[0] + o[150] + o[299];";
    number(src, 0.0 + 300.0 + 598.0);
    let src = "var o = {}; var i = 0; while (i < 300) { o[i] = i * 2; i = i + 1; } \
               return o[\"150\"];";
    number(src, 300.0);
    let src = "var o = {}; var i = 0; while (i < 300) { o[i] = i * 2; i = i + 1; } return o;";
    let (len, cap, keys) = returned_record(src);
    assert_eq!(len, 300);
    assert!(cap >= 300);
    assert_eq!(keys[0], "0");
    assert_eq!(keys[299], "299");
    for (i, key) in keys.iter().enumerate() {
        assert_eq!(key, &i.to_string(), "entry {i}");
    }
}

/// The same loop writing the *same* key each turn must leave one property.
#[test]
fn a_key_rewritten_in_a_loop_stays_one_property() {
    let src = "var o = {}; var i = 0; while (i < 500) { o[\"k\"] = i; i = i + 1; } return o;";
    let (len, cap, keys) = returned_record(src);
    assert_eq!((len, cap), (1, 4), "one property, one first-cap vector");
    assert_eq!(keys, vec!["k".to_string()]);
    number(
        "var o = {}; var i = 0; while (i < 500) { o[\"k\"] = i; i = i + 1; } return o.k;",
        499.0,
    );
}

// =========================================================================
// A10. Strings and objects on one bump heap
// =========================================================================

/// Alternate the two allocators and check both populations afterwards. A
/// record whose entry vector was reallocated over a string, or a string
/// written over an entry vector, shows up here as a wrong answer.
#[test]
fn interleaved_string_and_object_allocation() {
    let mut src = String::from("var o = {}; var s = \"\";");
    for i in 0..40 {
        src.push_str(&format!("o.k{i} = \"v{i}\"; s = s + \"{}\";", i % 10));
    }
    src.push_str("return s;");
    let want: String = (0..40).map(|i| char::from(b'0' + (i % 10) as u8)).collect();
    text(&src, &want);

    let mut src = String::from("var o = {}; var s = \"\";");
    for i in 0..40 {
        src.push_str(&format!("o.k{i} = \"v{i}\"; s = s + \"{}\";", i % 10));
    }
    src.push_str("return o.k0 + o.k17 + o.k39;");
    text(&src, "v0v17v39");
}

/// A key computed by concatenation is a fresh record every time, so this is
/// the case `__str_eq` exists for: byte equality, not pointer equality.
#[test]
fn a_concatenated_key_finds_an_interned_one() {
    number("var o = {ab: 1}; return o[\"a\" + \"b\"];", 1.0);
    number("var o = {}; o[\"a\" + \"b\"] = 2; return o.ab;", 2.0);
    let src =
        "var o = {}; var i = 0; while (i < 60) { o[\"k\" + \"x\"] = i; i = i + 1; } return o;";
    let (len, _, keys) = returned_record(src);
    assert_eq!(len, 1, "sixty fresh key records naming one property");
    assert_eq!(keys, vec!["kx".to_string()]);
}

/// The string a property holds must survive the object's own reallocation.
#[test]
fn a_stored_string_survives_the_vector_moving() {
    let mut src = String::from("var o = {}; o.first = \"held\";");
    for i in 0..40 {
        src.push_str(&format!("o.k{i} = {i};"));
    }
    src.push_str("return o.first;");
    text(&src, "held");
}

// =========================================================================
// A11. A persistent instance: several calls, two instances
// =========================================================================

/// The bump pointer is a global initialised at instantiation and never reset,
/// so an object built by one call is still there for the next -- nothing is
/// reused underneath it.
#[test]
fn an_object_survives_across_calls_on_one_instance() {
    let wasm =
        compile_qjs_m1("var o = {}; o.n = $0; o.tag = \"kept\"; return o;").expect("compiles");
    let module = WasmModule::from_bytes_with(&wasm, limits()).expect("load gate");
    let mut instance = module.instantiate().expect("instantiate");

    let first = instance
        .invoke_by_name("main", &Value::args(&[Value::Number(11.0)]))
        .expect("call one");
    let [Val::I32(t1), Val::I64(p1)] = first.as_slice() else {
        panic!("{first:?}")
    };
    assert_eq!(*t1, TAG_OBJECT);
    let a = *p1 as u32 as usize;

    let second = instance
        .invoke_by_name("main", &Value::args(&[Value::Number(22.0)]))
        .expect("call two");
    let [Val::I32(t2), Val::I64(p2)] = second.as_slice() else {
        panic!("{second:?}")
    };
    assert_eq!(*t2, TAG_OBJECT);
    let b = *p2 as u32 as usize;

    assert_ne!(
        a, b,
        "the second call must not be handed the first object's address"
    );

    let view = instance.memory().expect("memory");
    let bytes: &[u8] = &view;
    for (at, want) in [(a, 11.0f64), (b, 22.0f64)] {
        assert_eq!(word(bytes, at + OBJ_LEN), 2, "record at {at}");
        let entries = word(bytes, at + OBJ_ENTRIES) as usize;
        assert_eq!(read_string_at(bytes, word(bytes, entries)).unwrap(), "n");
        let payload = i64::from_le_bytes(bytes[entries + 8..entries + 16].try_into().unwrap());
        assert_eq!(
            f64::from_bits(payload as u64),
            want,
            "record at {at} still holds its own n"
        );
        assert_eq!(
            read_string_at(bytes, word(bytes, entries + ENTRY_BYTES)).unwrap(),
            "tag"
        );
    }
}

/// Two instances of one module are two heaps. Neither may see the other's.
#[test]
fn two_instances_do_not_share_a_heap() {
    let wasm = compile_qjs_m1("var o = {}; o.n = $0; return o;").expect("compiles");
    let mut one = WasmModule::from_bytes_with(&wasm, limits())
        .expect("load gate")
        .instantiate()
        .expect("instance one");
    let mut two = WasmModule::from_bytes_with(&wasm, limits())
        .expect("load gate")
        .instantiate()
        .expect("instance two");

    let a = one
        .invoke_by_name("main", &Value::args(&[Value::Number(1.0)]))
        .expect("one");
    let b = two
        .invoke_by_name("main", &Value::args(&[Value::Number(2.0)]))
        .expect("two");
    let [Val::I32(_), Val::I64(pa)] = a.as_slice() else {
        panic!()
    };
    let [Val::I32(_), Val::I64(pb)] = b.as_slice() else {
        panic!()
    };
    assert_eq!(*pa, *pb, "two fresh heaps hand out the same first address");

    let read = |instance: &WasmInstance, at: usize| -> f64 {
        let view = instance.memory().expect("memory");
        let bytes: &[u8] = &view;
        let entries = word(bytes, at + OBJ_ENTRIES) as usize;
        let payload = i64::from_le_bytes(bytes[entries + 8..entries + 16].try_into().unwrap());
        f64::from_bits(payload as u64)
    };
    assert_eq!(read(&one, *pa as u32 as usize), 1.0);
    assert_eq!(read(&two, *pb as u32 as usize), 2.0);
}

/// The fault word describes *this* call. A call that exhausted the heap must
/// not make the next call's ordinary type error look like a budget problem.
#[test]
fn the_fault_word_is_about_the_call_that_just_failed() {
    let src = "if ($0 === 0) { var s = \"xxxxxxxxxxxxxxxx\"; var i = 0; \
               while (i < 1000000) { s = s + \"yyyyyyyyyyyyyyyy\"; i = i + 1; } } \
               var u = undefined; return u.a;";
    let wasm = compile_qjs_m1(src).expect("compiles");
    let tight = Limits {
        max_memory_pages: 4,
        max_steps: 200_000_000,
        ..Limits::default()
    };
    let module = WasmModule::from_bytes_with(&wasm, tight).expect("load gate");
    let mut instance = module.instantiate().expect("instantiate");

    instance
        .invoke_by_name("main", &Value::args(&[Value::Number(0.0)]))
        .expect_err("call one exhausts the heap");
    assert_eq!(
        guest_fault(&instance.memory().expect("memory")),
        Some(GuestFault::HeapExhausted),
        "call one"
    );

    instance
        .invoke_by_name("main", &Value::args(&[Value::Number(1.0)]))
        .expect_err("call two is a type error");
    assert_eq!(
        guest_fault(&instance.memory().expect("memory")),
        None,
        "call two is `undefined.a`, not a budget problem -- the word must have been cleared"
    );
}

// =========================================================================
// A12. Keys the grammar has opinions about
// =========================================================================

/// ECMA-262 13.3.2: after `.` the grammar is IdentifierName, which *includes*
/// every reserved word. `o.if`, `o.class`, `o.new`, `o.default` are all legal
/// JavaScript, and a binding library is exactly the kind of code that writes
/// them.
#[test]
fn a_reserved_word_after_a_dot_is_a_property_name() {
    // The words this lexer spells are IdentifierNames and work.
    let spelled = [
        "if",
        "else",
        "return",
        "var",
        "let",
        "const",
        "function",
        "while",
        "for",
        "typeof",
        "null",
        "true",
        "false",
        "undefined",
    ];
    for word in spelled {
        let src = format!("var o = {{}}; o.{word} = 1; return o.{word};");
        number(&src, 1.0);
        let src = format!("var o = {{ {word}: 1 }}; return o.{word};");
        number(&src, 1.0);
    }
    // The words it does not spell are refused. That refusal is a divergence
    // from 13.2.5 and not a boundary of the language -- `objects_conformance.rs`
    // asserts it as one -- but the *diagnostic* is right: it names the property
    // and not the keyword, so a reader is told which construct is unsupported
    // rather than being told their `o.new` is a syntax error. Recorded here so
    // the list is a fact and not a guess.
    let refused = [
        "new",
        "class",
        "in",
        "of",
        "delete",
        "this",
        "catch",
        "try",
        "throw",
        "default",
        "switch",
        "case",
        "break",
        "continue",
        "do",
        "instanceof",
        "void",
        "with",
        "extends",
        "super",
        "yield",
        "await",
        "static",
        "import",
        "export",
        "enum",
    ];
    for word in refused {
        let src = format!("var o = {{}}; return o.{word};");
        let error = compile_qjs_m1(&src).expect_err(&format!("`o.{word}` is refused today"));
        assert_eq!(
            error.message, "this engine does not support a property named with a reserved word yet",
            "`o.{word}` must name the property boundary, not the keyword"
        );
    }
}

/// ECMA-262 13.2.5 PropertyName is IdentifierName, StringLiteral or
/// NumericLiteral -- three spellings, one key each.
#[test]
fn every_property_name_spelling_a_literal_allows() {
    number("var o = { if: 1 }; return o.if;", 1.0);
    number("var o = { \"a-b\": 1 }; return o[\"a-b\"];", 1.0);
    number("var o = { 0: 1 }; return o[0];", 1.0);
    number("var o = { \"\": 1 }; return o[\"\"];", 1.0);
}

/// `__proto__` in a literal is ECMA-262 13.2.5.5's one special case: it sets
/// the prototype and creates *no own property*. Rather than create the wrong
/// property, the parser refuses it by name.
#[test]
fn a_proto_key_in_a_literal_is_refused_by_name() {
    let error = compile_qjs_m1("return { __proto__: 1, a: 2 };").expect_err("refused");
    assert_eq!(
        error.message,
        "this engine does not support the `__proto__` property yet"
    );
    // The three spellings that are *not* the 13.2.5.5 special case are
    // ordinary own properties, and are allowed. `o.__proto__ = v` is not one
    // of them: it is 10.1.2's setter on `Object.prototype`, which does not
    // exist here, so what this engine does with it is recorded rather than
    // assumed.
    number(
        "var o = {}; o[\"__proto__\"] = 6; return o[\"__proto__\"];",
        6.0,
    );
    let (len, _, keys) = returned_record("var o = {}; o[\"__proto__\"] = 6; return o;");
    assert_eq!(
        (len, keys.as_slice()),
        (1, ["__proto__".to_string()].as_slice())
    );
    match compile_qjs_m1("var o = {}; o.__proto__ = 1; return o.__proto__;") {
        Ok(_) => number("var o = {}; o.__proto__ = 1; return o.__proto__;", 1.0),
        Err(e) => assert!(e.message.contains("does not support"), "{}", e.message),
    }
}

/// A property whose name is one of the engine's own runtime symbols is an
/// ordinary property: the symbols are wasm function names, not heap keys.
#[test]
fn a_property_named_after_a_runtime_symbol() {
    number(
        "var o = {}; o.__add = 1; o.__obj_get = 2; return o.__add + o.__obj_get;",
        3.0,
    );
    number("var o = {}; o.constructor = 4; return o.constructor;", 4.0);
    number("var o = {}; o.length = 5; return o.length;", 5.0);
    number(
        "var o = {}; o[\"__proto__\"] = 6; return o[\"__proto__\"];",
        6.0,
    );
}

// =========================================================================
// A13. Order of evaluation, and evaluating the receiver once
// =========================================================================

/// 13.15.2: the LeftHandSide is evaluated before the right. `o[k()] = v()`
/// calls `k` first, and calls each exactly once.
#[test]
fn an_assignment_evaluates_its_reference_once_and_first() {
    number(
        "var log = \"\"; \
         function k() { log = log + \"k\"; return \"p\"; } \
         function v() { log = log + \"v\"; return 1; } \
         var o = {}; o[k()] = v(); return o.p;",
        1.0,
    );
    text(
        "var log = \"\"; \
         function k() { log = log + \"k\"; return \"p\"; } \
         function v() { log = log + \"v\"; return 1; } \
         var o = {}; o[k()] = v(); return log;",
        "kv",
    );
    // A compound assignment reads and writes one reference: `k` runs once.
    text(
        "var log = \"\"; \
         function k() { log = log + \"k\"; return \"p\"; } \
         var o = { p: 1 }; o[k()] += 10; return log;",
        "k",
    );
    number(
        "var o = { p: 1 }; var n = 0; function k() { n = n + 1; return \"p\"; } \
         o[k()] += 10; return o.p * 10 + n;",
        111.0,
    );
    // ++ on a member: one evaluation of the reference too.
    text(
        "var log = \"\"; \
         function k() { log = log + \"k\"; return \"p\"; } \
         var o = { p: 1 }; o[k()]++; return log;",
        "k",
    );
}

/// The receiver is evaluated once even when it is itself an expression with a
/// side effect.
#[test]
fn the_receiver_is_evaluated_once() {
    number(
        "var n = 0; var o = { inner: {} }; \
         function r() { n = n + 1; return o.inner; } \
         r().a = 5; return o.inner.a * 10 + n;",
        51.0,
    );
}

/// A value expression that grows the same object mid-assignment. The record
/// address is stable, so the write must land in the *new* vector.
#[test]
fn a_value_expression_that_grows_the_receiver() {
    let mut src = String::from("var o = {}; function fill() {");
    for i in 0..20 {
        src.push_str(&format!("o.f{i} = {i};"));
    }
    src.push_str("return 99; } o.target = fill(); return o.target;");
    number(&src, 99.0);
    let mut src = String::from("var o = {}; function fill() {");
    for i in 0..20 {
        src.push_str(&format!("o.f{i} = {i};"));
    }
    src.push_str("return 99; } o.target = fill(); return o;");
    let (len, _, keys) = returned_record(&src);
    assert_eq!(len, 21);
    assert_eq!(
        keys[20], "target",
        "the write lands after everything fill() added"
    );
    assert_eq!(keys[0], "f0");
}

// =========================================================================
// A14. Population: many objects, none of them freed
// =========================================================================

/// Two thousand discarded objects must not touch the one that is kept. This
/// is `OBJ_HEADER`'s "many objects of one shape" case arriving early -- the
/// answer has to stay right even though the layout says the cost will not.
#[test]
fn two_thousand_discarded_objects_leave_the_kept_one_alone() {
    let src = "var keep = {}; keep.n = 1; keep.s = \"kept\"; var i = 0; \
               while (i < 2000) { var t = {}; t.a = i; t.b = i; i = i + 1; } \
               return keep.n;";
    number(src, 1.0);
    let src = "var keep = {}; keep.n = 1; keep.s = \"kept\"; var i = 0; \
               while (i < 2000) { var t = {}; t.a = i; t.b = i; i = i + 1; } \
               return keep.s;";
    text(src, "kept");
}

/// What the bump heap costs per object, measured rather than guessed -- the
/// number the "no free" decision is spending. Recorded, not judged.
#[test]
fn the_heap_cost_per_object_is_measured() {
    let build = |n: usize| -> usize {
        let src =
            format!("var i = 0; while (i < {n}) {{ var t = {{}}; t.a = i; i = i + 1; }} return 0;");
        let wasm = compile_qjs_m1(&src).expect("compiles");
        let module = WasmModule::from_bytes_with(
            &wasm,
            Limits {
                max_steps: 200_000_000,
                ..Limits::default()
            },
        )
        .expect("load gate");
        let mut instance = module.instantiate().expect("instantiate");
        instance
            .invoke_by_name("main", &Value::args(&[]))
            .expect("runs");
        instance.memory_pages()
    };
    let one = build(1_000);
    let ten = build(10_000);
    // 12 bytes of record + 64 bytes of FIRST_CAP entry vector = 76 per object,
    // none of it reclaimed. Nine thousand more objects is at least ten more
    // pages; the assertion is only that the growth is real and linear.
    assert!(
        ten >= one + 9,
        "1000 objects -> {one} pages, 10000 -> {ten} pages: the leak must be linear and visible"
    );
    println!("bump heap: 1000 objects = {one} pages, 10000 objects = {ten} pages");
}

// =========================================================================
// A15. The namespace-table pattern, three levels deep
// =========================================================================

/// `fleet.ui.composer.send = ...` is the shape the downstream binding library
/// is made of: a chain of dotted assignments through objects created one level
/// at a time.
#[test]
fn a_three_level_namespace_chain() {
    let src = "var fleet = {}; fleet.ui = {}; fleet.ui.composer = {}; \
               fleet.ui.composer.send = 1; fleet.ui.composer.clear = 2; \
               fleet.tabs = {}; fleet.tabs.list = 3; \
               return fleet.ui.composer.send * 100 + fleet.ui.composer.clear * 10 + fleet.tabs.list;";
    number(src, 123.0);
    let src = "var fleet = {}; fleet.ui = {}; fleet.ui.composer = {}; \
               fleet.ui.composer.send = 1; return fleet;";
    let (len, _, keys) = returned_record(src);
    assert_eq!(len, 1);
    assert_eq!(keys, vec!["ui".to_string()]);
    // A compound assignment three levels down.
    number(
        "var f = {}; f.a = {}; f.a.b = {}; f.a.b.n = 1; f.a.b.n += 41; return f.a.b.n;",
        42.0,
    );
    // A ten-property table, the largest `fleet.js` builds.
    let mut src = String::from("var t = {};");
    for i in 0..10 {
        src.push_str(&format!("t.m{i} = {i};"));
    }
    src.push_str("return t;");
    let (len, cap, keys) = returned_record(&src);
    assert_eq!(len, 10);
    assert_eq!(cap, 16, "four then eight then sixteen: two reallocations");
    assert_eq!(keys[9], "m9");
}

// =========================================================================
// A16. Objects at the operators, and at the host door
// =========================================================================

#[test]
fn an_object_at_the_operators() {
    boolean("var o = {}; if (o) { return true; } return false;", true);
    boolean("var o = {}; return !o;", false);
    boolean("var o = {}; return !!o;", true);
    // 7.2.14: an Object is never loosely equal to null or undefined.
    boolean("var o = {}; return o == null;", false);
    boolean("var o = {}; return o == undefined;", false);
    boolean("var o = {}; return o === undefined;", false);
    boolean("var o = {}; return o != null;", true);
    boolean("var o = {}; var p = o; return o == p;", true);
    boolean("return {} == {};", false);
    // ToPrimitive needs a prototype: arithmetic on an Object is a trap.
    traps("var o = {}; return o + 1;");
    traps("var o = {}; return o - 1;");
    traps("var o = {}; return o + \"s\";");
    traps("var o = {}; return o < 1;");
    traps("var o = {}; return +o;");
    traps("var o = {}; return o == 1;");
}

/// An Object cannot cross the host door: `Value` has no variant for a guest
/// heap reference, and the refusal has to name that rather than say "unknown".
#[test]
fn an_object_cannot_be_read_back_by_the_host() {
    let (_, vals) = attempt("return { a: 1 };").expect("runs");
    let error = Value::returned(&vals).expect_err("no `Value::Object`");
    assert!(
        error.contains("Object") && error.contains("heap"),
        "the refusal must name what it is refusing: {error}"
    );
}

// =========================================================================
// A17. Keys that are not plain ASCII
// =========================================================================

#[test]
fn a_multibyte_key() {
    number(
        "var o = {}; o[\"\\u00e9\"] = 1; return o[\"\\u00e9\"];",
        1.0,
    );
    number(
        "var o = { \"\\u4e2d\\u6587\": 2 }; return o[\"\\u4e2d\\u6587\"];",
        2.0,
    );
    // A surrogate pair is one code point in the record's UTF-8.
    number(
        "var o = {}; o[\"\\ud83d\\ude00\"] = 3; return o[\"\\ud83d\\ude00\"];",
        3.0,
    );
    // Two keys whose UTF-8 differs only in the last byte.
    number(
        "var o = {}; o[\"\\u00e9\"] = 1; o[\"\\u00e8\"] = 2; return o[\"\\u00e9\"] * 10 + o[\"\\u00e8\"];",
        12.0,
    );
    let (len, _, keys) =
        returned_record("var o = {}; o[\"\\u00e9\"] = 1; o[\"\\u00e8\"] = 2; return o;");
    assert_eq!(len, 2);
    assert_eq!(keys, vec!["\u{e9}".to_string(), "\u{e8}".to_string()]);
}

/// A key with an embedded NUL. The record is length-prefixed, so a NUL is a
/// byte like any other -- and a C-style comparison would get this wrong.
#[test]
fn a_key_with_an_embedded_nul() {
    number(
        "var o = {}; o[\"a\\u0000b\"] = 1; return o[\"a\\u0000b\"];",
        1.0,
    );
    undefined("var o = {}; o[\"a\\u0000b\"] = 1; return o[\"a\"];");
    undefined("var o = {}; o[\"a\\u0000b\"] = 1; return o[\"a\\u0000c\"];");
    let (len, _, keys) =
        returned_record("var o = {}; o[\"a\\u0000b\"] = 1; o[\"a\"] = 2; return o;");
    assert_eq!(len, 2, "\"a\\0b\" and \"a\" are two keys");
    assert_eq!(keys[0].len(), 3);
}

/// A key big enough that the module's data segment spills past one page.
#[test]
fn a_sixty_four_kilobyte_key() {
    let key = "q".repeat(70_000);
    let src =
        format!("var o = {{}}; o[\"{key}\"] = 1; o.small = 2; return o[\"{key}\"] + o.small;");
    number(&src, 3.0);
}

// =========================================================================
// A18. The other producer on this heap: a `Bytes` host result
// =========================================================================
//
// Objects and strings share one bump allocator, and `HostResult::Bytes` is the
// third thing that allocates on it: it asks a length import, bump-allocates a
// `[len][bytes]` record of that size, asks for the copy and checks it. The
// crate README states the guarantee this section attacks:
//
//   "A short or negative write traps instead of producing a String with a
//    fabricated tail."

use std::cell::RefCell;
use std::rc::Rc;
use tinyvm::WasmError;
use tinyvm_qjs::{HostFn, HostResult, Names, Options, compile_qjs_m1_with};

/// `sys.reply_len() -> i32` and `sys.reply(dst, cap) -> i32`, with both
/// answers under the test's control.
fn reply_table() -> Vec<HostFn> {
    vec![HostFn {
        name: "reply".to_string(),
        module: "sys".to_string(),
        field: "reply".to_string(),
        params: Vec::new(),
        result: HostResult::Bytes {
            length: "reply_len".to_string(),
        },
    }]
}

/// Run `source` against a host that answers `reply_len()` with `len` and
/// `reply(dst, cap)` with `wrote`, writing nothing.
fn with_lying_reply(
    source: &str,
    len: i32,
    wrote: i32,
) -> Result<(WasmInstance, Vec<Val>), String> {
    let wasm = compile_qjs_m1_with(
        source,
        Options {
            names: Names::Declared(reply_table()),
        },
    )
    .map_err(|e| format!("compile: {e}"))?;
    let mut module = WasmModule::from_bytes_with(&wasm, limits())
        .map_err(|e| format!("load gate: {}", e.message()))?;
    module
        .bind_import_typed("sys", "reply_len", move |_a, _m| Ok(vec![Val::I32(len)]))
        .map_err(|e| e.message().to_string())?;
    module
        .bind_import_typed("sys", "reply", move |args, _m| {
            let [Val::I32(_dst), Val::I32(_cap)] = args else {
                return Err(WasmError::Trap("sys.reply wants (i32, i32)"));
            };
            Ok(vec![Val::I32(wrote)])
        })
        .map_err(|e| e.message().to_string())?;
    let mut instance = module
        .instantiate()
        .map_err(|e| format!("instantiate: {}", e.message()))?;
    let vals = instance
        .invoke_by_name("main", &[])
        .map_err(|e| format!("trap: {}", e.message()))?;
    Ok((instance, vals))
}

/// A host that reports a **negative** length and then answers the copy call
/// with the same negative number -- which is exactly the raw contract's
/// "your buffer is too small" -- passes the `wrote != n` check, because the
/// check compares the two lies to each other and never asks whether either is
/// a length at all.
#[test]
fn a_negative_reply_length_must_not_produce_a_string() {
    match with_lying_reply("return reply();", -1, -1) {
        Err(message) => assert!(message.starts_with("trap:"), "{message}"),
        Ok((_instance, vals)) => {
            let [Val::I32(tag), Val::I64(payload)] = vals.as_slice() else {
                panic!("{vals:?}")
            };
            panic!(
                "a length of -1 produced tag {tag} pointing at {}, whose record says its length \
                 is {} bytes -- the README promises a negative write traps",
                *payload as u32,
                u32::MAX
            );
        }
    }
}

/// The same lie, one step further: the fabricated String is then used as a
/// property key, so the bogus length is walked by `__str_eq` inside the object
/// heap's scan.
#[test]
fn a_fabricated_string_must_not_reach_the_object_heap() {
    match with_lying_reply("var o = {}; o[reply()] = 1; return o;", -1, -1) {
        Err(message) => assert!(message.starts_with("trap:"), "{message}"),
        Ok((instance, vals)) => {
            let [Val::I32(_), Val::I64(payload)] = vals.as_slice() else {
                panic!("{vals:?}")
            };
            let view = instance.memory().expect("memory");
            let bytes: &[u8] = &view;
            let object = *payload as u32 as usize;
            let entries = word(bytes, object + OBJ_ENTRIES) as usize;
            let key = word(bytes, entries) as usize;
            panic!(
                "the object heap now holds a key at {key} whose length header reads {}",
                word(bytes, key)
            );
        }
    }
}

/// The honest cases still behave: a length of zero is the empty String, and a
/// host that writes a different number of bytes than it promised traps.
#[test]
fn an_honest_reply_still_behaves() {
    let (instance, vals) = with_lying_reply("return reply();", 0, 0).expect("zero-length reply");
    assert_eq!(decode(&instance, &vals), Out::Str(String::new()));
    assert!(
        with_lying_reply("return reply();", 4, 3).is_err(),
        "a short write must trap"
    );
    assert!(
        with_lying_reply("return reply();", 4, 5).is_err(),
        "a long write must trap"
    );
}

/// Run `source` against a host whose `reply_len()` answers walk a scripted
/// list, and whose `reply(dst, cap)` echoes `cap` back (which is what a raw
/// contract's "wrote exactly what you asked for" looks like).
fn with_scripted_reply(
    source: &str,
    lengths: Vec<i32>,
) -> Result<(WasmInstance, Vec<Val>), (WasmInstance, String)> {
    let wasm = compile_qjs_m1_with(
        source,
        Options {
            names: Names::Declared(reply_table()),
        },
    )
    .expect("compiles");
    let mut module = WasmModule::from_bytes_with(&wasm, limits()).expect("load gate");
    let queue = Rc::new(RefCell::new(lengths));
    let feed = Rc::clone(&queue);
    module
        .bind_import_typed("sys", "reply_len", move |_a, _m| {
            let mut q = feed.borrow_mut();
            Ok(vec![Val::I32(if q.is_empty() { 0 } else { q.remove(0) })])
        })
        .expect("bind reply_len");
    module
        .bind_import_typed("sys", "reply", move |args, memory| {
            let [Val::I32(dst), Val::I32(cap)] = args else {
                return Err(WasmError::Trap("sys.reply wants (i32, i32)"));
            };
            // An honest host writes exactly `cap` bytes when it can.
            if *cap > 0 {
                let at = *dst as usize;
                for byte in memory[at..at + *cap as usize].iter_mut() {
                    *byte = b'z';
                }
            }
            Ok(vec![Val::I32(*cap)])
        })
        .expect("bind reply");
    let mut instance = module.instantiate().expect("instantiate");
    match instance.invoke_by_name("main", &[]) {
        Ok(vals) => Ok((instance, vals)),
        Err(e) => {
            let message = e.message().to_string();
            Err((instance, message))
        }
    }
}

/// `FAULT_WORD`'s doc states the invariant this test attacks:
///
///   "the guest writes down what it knows on the way down, at a word no
///    allocation can ever hand out: the bump pointer starts at
///    `StringPool::heap_start`, which is never below `DATA_ORIGIN`, and the
///    only instruction in the emitted module that stores here is the one
///    below."
///
/// A negative length makes `__alloc` move the bump pointer *backwards*
/// (`(n + 3) & -4` is negative for `n <= -1`), so after two of them the
/// allocator hands out address 0 -- and the string record's own length header
/// is then written straight into the fault word. Choosing the length so that
/// the header reads `FAULT_HEAP_EXHAUSTED` makes the guest report a budget
/// problem for a script that merely has a type error.
#[test]
fn a_negative_reply_length_cannot_be_allowed_to_reach_the_fault_word() {
    // No string literal anywhere, so the pool is empty and the heap starts at
    // DATA_ORIGIN = 8: two backward steps of four bytes reach address 0.
    let source = "var a = reply(); var b = reply(); var c = reply(); var o = {}; return o + 1;";
    let (instance, message) = with_scripted_reply(source, vec![-8, -8, 1])
        .map(|(i, v)| panic!("expected the `{{}} + 1` trap, got {:?}", decode(&i, &v)))
        .unwrap_err();
    let memory = instance.memory().expect("memory zero");
    assert_eq!(
        guest_fault(&memory),
        None,
        "the trap was `{{}} + 1`, a type error, not a budget problem -- but the fault word \
         now reads {} because a string record was allocated at address 0 (trap was: {message})",
        word(&memory, 0)
    );
}

/// The bump pointer must never be handed out below `DATA_ORIGIN`, whatever a
/// host answers. Two negative lengths walk it from 8 down to 0; the third
/// allocation is then handed address 0 and the String it builds lives on top
/// of the fault word.
#[test]
fn the_allocator_never_hands_out_an_address_below_the_data_origin() {
    let source = "var a = reply(); var b = reply(); var c = reply(); return c;";
    match with_scripted_reply(source, vec![-8, -8, 4]) {
        Err(_) => { /* refusing is a correct answer */ }
        Ok((instance, vals)) => {
            let [Val::I32(tag), Val::I64(payload)] = vals.as_slice() else {
                panic!("{vals:?}")
            };
            let at = *payload as u32 as i32;
            assert!(
                *tag != 3 || at >= 8,
                "a String record was placed at {at}, below DATA_ORIGIN = 8 -- the bump \
                 pointer walked backwards over the fault word, which now reads {}",
                word(&instance.memory().expect("memory"), 0)
            );
        }
    }
}

// =========================================================================
// A19. Scratch-local discipline under nested member targets
// =========================================================================
//
// A member target holds two scratch locals -- a JS value pair for the receiver
// and a raw `i32` for the key -- across the evaluation of the value. Nesting
// those is where an off-by-one in the take/give free lists would appear as a
// receiver or a key silently becoming another expression's.

#[test]
fn chained_assignment_through_two_member_targets() {
    number(
        "var o = {a: {}, c: {}}; o.a.b = o.c.d = 5; return o.a.b * 10 + o.c.d;",
        55.0,
    );
    number(
        "var o = {}; var p = {}; var q = {}; o.x = p.y = q.z = 3; return o.x + p.y + q.z;",
        9.0,
    );
}

#[test]
fn a_computed_key_that_is_itself_a_property_read() {
    number(
        "var n = {k: \"target\"}; var o = {}; o[n.k] = 7; return o.target;",
        7.0,
    );
    number(
        "var i = {a: \"b\"}; var m = {b: \"c\"}; var o = {}; o[m[i.a]] = 4; return o.c;",
        4.0,
    );
    // Three levels of computed indirection on the left of an assignment.
    number(
        "var l1 = {k: \"k2\"}; var l2 = {k2: \"k3\"}; var o = {}; o[l2[l1.k]] = 8; return o.k3;",
        8.0,
    );
}

#[test]
fn a_member_as_a_for_loop_variable() {
    number(
        "var o = {i: 0, sum: 0}; for (o.i = 0; o.i < 10; o.i++) { o.sum = o.sum + o.i; } \
         return o.sum * 100 + o.i;",
        4510.0,
    );
    let (len, _, keys) = returned_record(
        "var o = {i: 0, sum: 0}; for (o.i = 0; o.i < 10; o.i++) { o.sum = o.sum + o.i; } return o;",
    );
    assert_eq!(len, 2, "the loop must not append a third property");
    assert_eq!(keys, vec!["i".to_string(), "sum".to_string()]);
}

#[test]
fn the_missing_property_guard_pattern() {
    undefined("var o = {}; return o.a && o.a.b;");
    undefined("var o = {a: {}}; return o.a && o.a.b;");
    number("var o = {a: {b: 3}}; return o.a && o.a.b;", 3.0);
    number("var o = {}; return o.a || 9;", 9.0);
    boolean("var o = {}; return typeof o.a === \"undefined\";", true);
    // `typeof` does not shield the receiver: `o.a` is `undefined`, so `o.a.b`
    // is 13.3.2.1's TypeError before `typeof` is reached.
    traps("var o = {}; return typeof o.a.b;");
}

/// A literal whose property values are calls that allocate on the same heap
/// while the literal's own record is half filled.
#[test]
fn a_literal_whose_values_allocate() {
    number(
        "function mk(n) { var t = {}; t.v = n; return t.v; } \
         var o = { a: mk(1), b: mk(2), c: mk(3) }; return o.a * 100 + o.b * 10 + o.c;",
        123.0,
    );
    let (len, cap, keys) = returned_record(
        "function mk(n) { var t = {}; t.v = n; return t.v; } return { a: mk(1), b: mk(2) };",
    );
    assert_eq!((len, cap), (2, 2));
    assert_eq!(keys, vec!["a".to_string(), "b".to_string()]);
}

/// Duplicate keys at scale: 13.2.5.5 evaluates every definition against the
/// same object, so fifty `a:` definitions are one property written fifty
/// times -- and the record was sized for fifty.
#[test]
fn fifty_duplicate_keys_in_one_literal() {
    let body: Vec<String> = (0..50).map(|i| format!("a: {i}")).collect();
    let src = format!("return {{{}}};", body.join(","));
    let (len, cap, keys) = returned_record(&src);
    assert_eq!(
        (len, cap),
        (1, 50),
        "one property in a vector sized for fifty"
    );
    assert_eq!(keys, vec!["a".to_string()]);
    let src = format!("var o = {{{}}}; return o.a;", body.join(","));
    number(&src, 49.0);
    // The forty-nine unused slots are then filled by ordinary appends.
    let mut src = format!("var o = {{{}}};", body.join(","));
    for i in 0..49 {
        src.push_str(&format!("o.b{i} = {i};"));
    }
    src.push_str("return o;");
    let (len, cap, keys) = returned_record(&src);
    assert_eq!((len, cap), (50, 50), "no reallocation was needed");
    assert_eq!(keys[0], "a");
    assert_eq!(keys[49], "b48");
}

// =========================================================================
// A20. Objects through functions and recursion
// =========================================================================

/// An object crosses a call boundary as its two words, so the callee mutates
/// the caller's object.
#[test]
fn an_object_passed_to_a_function_is_the_same_object() {
    number(
        "function f(x) { x.a = 1; return 0; } var o = {}; f(o); return o.a;",
        1.0,
    );
    boolean(
        "function id(x) { return x; } var o = {}; return id(o) === o;",
        true,
    );
    number(
        "function fill(x, n) { x.v = n; return x; } var o = {}; fill(fill(o, 1), 2); return o.v;",
        2.0,
    );
}

/// A top-level `var` is a wasm global pair; a function reaching it must reach
/// the same record.
#[test]
fn a_global_object_is_one_object() {
    number(
        "var o = {}; function f() { o.a = 1; return o.a; } return f();",
        1.0,
    );
    number(
        "var o = {}; function f() { o.a = 1; return 0; } f(); return o.a;",
        1.0,
    );
}

/// A hundred-deep linked list built by recursion, then walked iteratively.
#[test]
fn a_recursive_linked_list_of_objects() {
    let src = "function build(n) { var o = {}; o.n = n; if (n > 0) { o.next = build(n - 1); } \
               return o; } \
               var head = build(100); var cur = head; var count = 0; \
               while (cur.n > 0) { cur = cur.next; count = count + 1; } \
               return count * 1000 + cur.n + head.n;";
    number(src, 100.0 * 1000.0 + 0.0 + 100.0);
    // Every node keeps its own `n`.
    let src = "function build(n) { var o = {}; o.n = n; if (n > 0) { o.next = build(n - 1); } \
               return o; } \
               var head = build(50); var cur = head; var sum = 0; \
               while (cur.n > 0) { sum = sum + cur.n; cur = cur.next; } return sum;";
    number(src, (1..=50).sum::<i32>() as f64);
}

// =========================================================================
// A21. One large allocation
// =========================================================================

/// A quarter-megabyte string built by doubling, then used as a property key.
/// `__alloc` grows linear memory one page at a time, so this is four pages of
/// grow loop inside one allocation, and `__str_eq` then walks the whole key.
#[test]
fn a_quarter_megabyte_key() {
    let src = "var s = \"0123456789abcdef\"; var i = 0; while (i < 14) { s = s + s; i = i + 1; } \
               var o = {}; o[s] = 1; o.small = 2; return o[s] + o.small;";
    let tight = Limits {
        max_steps: 2_000_000_000,
        ..Limits::default()
    };
    let wasm = compile_qjs_m1(src).expect("compiles");
    let module = WasmModule::from_bytes_with(&wasm, tight).expect("load gate");
    let mut instance = module.instantiate().expect("instantiate");
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect("a 256 KiB key is inside the default budget");
    assert_eq!(decode(&instance, &vals), Out::Number(3.0));
}

// =========================================================================
// A22. One key, several spellings
// =========================================================================
//
// ECMA-262 6.1.7: a property key is a String. Two PropertyNames that denote
// the same String are one property, whatever the source spelled.

#[test]
fn a_numeric_property_name_is_its_string_value() {
    let (len, _, keys) = returned_record("return {0: 1};");
    assert_eq!((len, keys.as_slice()), (1, ["0".to_string()].as_slice()));
    let (len, _, keys) = returned_record("return {1: 1, \"1\": 2};");
    assert_eq!(
        (len, keys.as_slice()),
        (1, ["1".to_string()].as_slice()),
        "`1` and `\"1\"` are one PropertyName"
    );
    number("var o = {1: 1, \"1\": 2}; return o[1];", 2.0);
}

#[test]
fn one_key_spelled_three_ways_is_one_property() {
    let (len, _, keys) = returned_record("var o = {a: 1}; o[\"a\"] = 2; o.a = 3; return o;");
    assert_eq!((len, keys.as_slice()), (1, ["a".to_string()].as_slice()));
    number("var o = {a: 1}; o[\"a\"] = 2; o.a = 3; return o.a;", 3.0);
    // A `\u` escape in a string key is the same String as the plain spelling.
    number("var o = {A: 1}; return o[\"\\u0041\"];", 1.0);
    let (len, _, _) = returned_record("var o = {A: 1}; o[\"\\u0041\"] = 2; return o;");
    assert_eq!(len, 1);
    // The literal itself, twice.
    let (len, _, _) = returned_record("return {a: 1, \"a\": 2};");
    assert_eq!(len, 1);
}

#[test]
fn a_trailing_comma_and_a_property_called_get() {
    number("var o = {a: 1,}; return o.a;", 1.0);
    number("var o = {get: 1, set: 2}; return o.get * 10 + o.set;", 12.0);
    let (len, _, keys) = returned_record("return {get: 1, set: 2};");
    assert_eq!(keys, vec!["get".to_string(), "set".to_string()]);
    assert_eq!(len, 2);
}

// =========================================================================
// A23. Fifty member targets alive at once
// =========================================================================

/// `=` is right associative, so this holds fifty member references -- fifty
/// receiver pairs and fifty raw key locals -- open at the same time. The free
/// lists have to be LIFO for every one of them to come back to the right slot.
#[test]
fn fifty_nested_member_assignments() {
    let n = 50;
    let mut src = String::from("var o = {};");
    let chain: Vec<String> = (0..n).map(|i| format!("o.k{i}")).collect();
    src.push_str(&format!("{} = 7;", chain.join(" = ")));
    src.push_str("return o.k0 + o.k49;");
    number(&src, 14.0);

    let mut src = String::from("var o = {};");
    src.push_str(&format!("{} = 7;", chain.join(" = ")));
    src.push_str("return o;");
    let (len, _, keys) = returned_record(&src);
    assert_eq!(len, n);
    // Right associative: the innermost (last written) assignment happens first,
    // so `k49` is created before `k0`.
    assert_eq!(
        keys[0], "k49",
        "right associativitysettles the  creation order"
    );
    assert_eq!(keys[49], "k0");
}

/// The same shape with computed keys, so every one of the fifty also holds a
/// heap-allocated key string across the others' evaluation.
#[test]
fn fifty_nested_computed_member_assignments() {
    let n = 50;
    let chain: Vec<String> = (0..n).map(|i| format!("o[\"k\" + \"{i}\"]")).collect();
    let src = format!(
        "var o = {{}}; {} = 7; return o.k0 + o.k49;",
        chain.join(" = ")
    );
    number(&src, 14.0);
    let src = format!("var o = {{}}; {} = 7; return o;", chain.join(" = "));
    let (len, _, keys) = returned_record(&src);
    assert_eq!(len, n);
    assert_eq!(keys[0], "k49");
}

// =========================================================================
// A24. The other two budgets
// =========================================================================

/// A step-budget trap in the middle of the object heap's growth must leave a
/// coherent record and must not be reported as a heap problem.
#[test]
fn a_step_budget_trap_is_not_a_heap_problem() {
    let src = "var o = {}; var i = 0; while (i < 3000) { o[\"k\" + i] = i; i = i + 1; } return o;";
    let wasm = compile_qjs_m1(src).expect("compiles");
    let module = WasmModule::from_bytes_with(
        &wasm,
        Limits {
            max_steps: 500_000,
            ..Limits::default()
        },
    )
    .expect("load gate");
    let mut instance = module.instantiate().expect("instantiate");
    instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect_err("500k steps is not enough for three thousand appends");
    assert_eq!(
        guest_fault(&instance.memory().expect("memory")),
        None,
        "a step-budget trap is not `HeapExhausted`"
    );
    // The same instance, invoked again: a fresh step budget, the same globals,
    // and no panic anywhere.
    instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect_err("and again");
    assert_eq!(guest_fault(&instance.memory().expect("memory")), None);
}

/// A module whose literals need more pages than the embedder allows is refused
/// by the load gate, not by a trap and not by a panic.
#[test]
fn a_data_segment_past_the_page_budget_is_refused_at_the_gate() {
    let key = "q".repeat(200_000);
    let src = format!("var o = {{}}; o[\"{key}\"] = 1; return o[\"{key}\"];");
    let wasm = compile_qjs_m1(&src).expect("the compiler does not bound this");
    let refused = WasmModule::from_bytes_with(
        &wasm,
        Limits {
            max_memory_pages: 2,
            ..Limits::default()
        },
    );
    match refused {
        Err(error) => assert!(!error.message().is_empty()),
        Ok(_) => panic!("four pages of literals must not load under a two-page budget"),
    }
    // The same bytes load when the budget allows them.
    WasmModule::from_bytes_with(&wasm, limits()).expect("the default budget is enough");
}

// =========================================================================
// A25. The equality ladder, from both sides
// =========================================================================
//
// `__strict_eq` collapses "same tag, and neither a Number nor a String" into
// one `i64.eq` on the payloads, and Object rides that arm as reference
// identity. Every asymmetry in the dispatch order shows up as one of these
// answering differently from its mirror image.

#[test]
fn object_equality_answers_the_same_from_either_side() {
    for (left, right) in [
        ("{}", "null"),
        ("{}", "undefined"),
        ("{}", "true"),
        ("{}", "\"\""),
        ("{}", "\"[object Object]\""),
    ] {
        let a = format!("var l = {left}; var r = {right}; return l === r;");
        let b = format!("var l = {left}; var r = {right}; return r === l;");
        assert_eq!(run(&a), Out::Bool(false), "{a}");
        assert_eq!(run(&b), Out::Bool(false), "{b}");
    }
    // 7.2.14: `==` between an Object and null/undefined is false, both ways,
    // and reaches no ToPrimitive.
    boolean("var o = {}; return null == o;", false);
    boolean("var o = {}; return undefined == o;", false);
    boolean("var o = {}; return o != undefined;", true);
    // Everything else needs ToPrimitive and traps -- from both sides.
    traps("var o = {}; return 1 == o;");
    traps("var o = {}; return o == 1;");
    traps("var o = {}; return \"s\" == o;");
    traps("var o = {}; return o == \"s\";");
    traps("var o = {}; return true == o;");
    // A String and an Object never compare equal even if their payloads are
    // adjacent addresses on the one shared heap.
    boolean("var s = \"x\" + \"y\"; var o = {}; return s === o;", false);
    boolean("var s = \"x\" + \"y\"; var o = {}; return o === s;", false);
    boolean("var o = {}; var s = \"x\" + \"y\"; return o === s;", false);
}

/// Two objects stored as property values compare by identity, not by shape.
#[test]
fn identity_survives_a_round_trip_through_a_property() {
    boolean("var o = {}; var t = {}; o.a = t; return o.a === t;", true);
    boolean("var o = {}; o.a = {}; o.b = {}; return o.a === o.b;", false);
    boolean("var o = {}; o.a = {}; o.b = o.a; return o.a === o.b;", true);
    // Through forty appends, so the pair is copied by the growth loop.
    let mut src = String::from("var o = {}; var t = {}; o.a = t;");
    for i in 0..40 {
        src.push_str(&format!("o.k{i} = {i};"));
    }
    src.push_str("return o.a === t;");
    boolean(&src, true);
}
