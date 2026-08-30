//! Array literals, indexing, `length`, and the property dispatch that reaches
//! them.
//!
//! Same discipline as `objects_m3.rs`, which this file is the sibling of:
//! every expectation is derived from ECMA-262 rather than from what the
//! implementation happens to do, and every one of them **runs** -- compile ->
//! tinyvm's load gate -> instantiate -> `invoke_by_name("main")`. "It
//! compiled" is not evidence except in the refusal corpus, where not
//! compiling is the claim.
//!
//! # What this milestone deliberately does not have
//!
//! No methods (`push`, `map`, `join`), no `Array.isArray`, no `Array`
//! constructor, no `for…of`, no destructuring, and no array crossing the host
//! boundary. Each is refused or absent for a reason recorded in
//! `plan/design-array-milestone.md` §4, and the tests at the bottom pin the
//! ones that produce a diagnostic so the refusal cannot quietly become a
//! wrong answer.

use tinyvm::{Limits, Val, WasmInstance, WasmModule};
use tinyvm_qjs::{Boundary, Names, Options, Value, compile_qjs_m1, compile_qjs_m1_with};

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

/// The array tag, as `repr.rs` numbers it, and the record `array.rs` lays
/// out. Written out rather than imported because both are crate-private: this
/// is the contract restated from the outside, which is the only place it can
/// be checked from -- and the only place a silent renumbering would be caught.
const TAG_ARRAY: i32 = 7;
const ARR_LEN: usize = 0;
const ARR_CAP: usize = 4;
const ARR_ELEMS: usize = 8;
const ELEM_BYTES: usize = 12;

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

fn word(bytes: &[u8], at: usize) -> i32 {
    i32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

#[track_caller]
fn number(source: &str, want: f64) {
    assert_eq!(run(source), Out::Number(want), "{source:?}");
}

#[track_caller]
fn string(source: &str, want: &str) {
    assert_eq!(run(source), Out::Str(want.to_string()), "{source:?}");
}

#[track_caller]
fn boolean(source: &str, want: bool) {
    assert_eq!(run(source), Out::Bool(want), "{source:?}");
}

#[track_caller]
fn undefined(source: &str) {
    assert_eq!(run(source), Out::Undefined, "{source:?}");
}

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
fn refuses_capability(source: &str, needle: &str, boundary: Boundary) {
    match compile_qjs_m1(source) {
        Ok(bytes) => panic!(
            "{source:?} compiled to {} bytes; expected a capability diagnostic",
            bytes.len()
        ),
        Err(e) => {
            assert!(
                e.message.contains(needle),
                "{source:?}: want a message naming {needle:?}, got {}",
                e.message
            );
            assert_eq!(e.boundary, boundary, "{source:?}: wrong boundary");
        }
    }
}

// =========================================================================
// The literal
// =========================================================================

#[test]
fn an_array_literal_holds_its_elements_in_source_order() {
    number("return [10, 20, 30][0];", 10.0);
    number("return [10, 20, 30][1];", 20.0);
    number("return [10, 20, 30][2];", 30.0);
}

#[test]
fn an_array_holds_every_kind_of_value() {
    number("return [1][0];", 1.0);
    string("return [\"a\"][0];", "a");
    boolean("return [true][0];", true);
    undefined("return [undefined][0];");
    assert_eq!(run("return [null][0];"), Out::Null);
    // An element is a whole V1 pair, so a nested array is a pointer like any
    // other and nothing is flattened.
    number("return [[1, 2], [3, 4]][1][0];", 3.0);
    number("return [{ a: 7 }][0].a;", 7.0);
}

#[test]
fn elements_are_expressions_and_are_evaluated_in_order() {
    number("let n = 1; return [n, n + 1, n + 2][2];", 3.0);
    // 13.2.4.1 evaluates each ElementList entry in turn, so a side effect in
    // an earlier element is visible to a later one.
    number(
        "let n = 0; let a = [n = n + 1, n = n + 1]; return a[0] + a[1];",
        3.0,
    );
}

#[test]
fn an_empty_array_is_a_real_array() {
    number("return [].length;", 0.0);
    // 7.1.2 step 8: an Object is always true, and it never looks inside.
    // `[]` being truthy is the case people expect to be false.
    boolean("if ([]) { return true; } return false;", true);
    string("return typeof [];", "object");
}

#[test]
fn a_trailing_comma_is_not_an_element() {
    // 12.9.6: the trailing comma is grammar, not an elision.
    number("return [1, 2, 3, ].length;", 3.0);
    number("return [1, ].length;", 1.0);
}

// =========================================================================
// Indexing
// =========================================================================

#[test]
fn an_index_past_the_end_is_undefined_and_not_a_fault() {
    // 10.1.8.1 with no prototype: an absent property reads `undefined`. A trap
    // would make `if (a[i])` unusable, which is the shape scripts actually
    // write.
    undefined("return [1, 2][5];");
    undefined("return [][0];");
    // The bounds test is unsigned, so a negative index is the same comparison
    // and not a second one.
    undefined("return [1, 2][0 - 1];");
}

#[test]
fn an_index_that_is_not_an_integer_is_not_an_index() {
    // 10.4.2.1 makes an array index a canonical numeric string, which `1.5`
    // and `NaN` are not. Neither is a property this array has, so both read
    // `undefined` rather than truncating to 1 -- truncation would be a
    // fabricated answer, which is the failure this engine refuses everywhere.
    undefined("return [10, 20][3 / 2];");
    undefined("return [10, 20][0 / 0];");
}

#[test]
fn a_string_key_on_an_array_is_not_an_index_here() {
    // **A recorded divergence.** ECMA-262 10.4.2.1 works on the String form,
    // so `a["0"]` is element 0 in a conforming engine. It is `undefined` here:
    // closing the gap means running 7.1.21 CanonicalNumericIndexString on
    // every string key of every array access, and the population this
    // milestone exists for -- `JSON.parse` of a broker answer, then `a[i]` in
    // a loop -- never writes one.
    //
    // Pinned rather than left unsaid: a divergence nobody wrote down is a
    // divergence somebody rediscovers as a bug.
    undefined("return [10, 20][\"0\"];");
}

#[test]
fn assignment_writes_the_element() {
    number("let a = [1, 2]; a[0] = 9; return a[0];", 9.0);
    number("let a = [1, 2]; a[1] = 9; return a[0] + a[1];", 10.0);
    number("let a = []; a[0] = 7; return a[0];", 7.0);
}

#[test]
fn a_write_past_the_end_extends_the_array_with_undefined() {
    // ECMA-262 makes the gap *holes*, which differ from `undefined` only
    // through `in`, `hasOwnProperty`, `Object.keys` and the iteration methods
    // that skip them -- none of which this engine has, so the two are
    // indistinguishable from every script it can run. `array.rs`'s `arr_set`
    // records this in full, including what stops being true when one of those
    // arrives.
    number("let a = [1]; a[3] = 9; return a.length;", 4.0);
    undefined("let a = [1]; a[3] = 9; return a[2];");
    number("let a = [1]; a[3] = 9; return a[3];", 9.0);
    // Appending exactly at the end is the ordinary growth path.
    number(
        "let a = []; a[0] = 1; a[1] = 2; a[2] = 3; return a.length;",
        3.0,
    );
}

#[test]
fn a_compound_assignment_reads_and_writes_the_same_element_once() {
    number("let a = [1, 2]; a[0] += 10; return a[0];", 11.0);
    number("let a = [5]; a[0] -= 2; return a[0];", 3.0);
    // The receiver and the key are evaluated once and held, which is what
    // makes a side-effecting index safe.
    number(
        "let n = 0; let a = [1, 1]; a[n = n + 1] += 10; return a[1] + n;",
        12.0,
    );
}

#[test]
fn an_update_operator_works_through_an_index() {
    number("let a = [1]; a[0]++; return a[0];", 2.0);
    number("let a = [1]; return a[0]++;", 1.0);
    number("let a = [1]; return ++a[0];", 2.0);
}

// =========================================================================
// `length`
// =========================================================================

#[test]
fn length_is_the_element_count() {
    number("return [].length;", 0.0);
    number("return [1].length;", 1.0);
    number("return [1, 2, 3].length;", 3.0);
    number("let a = [1]; a[0] = 2; return a.length;", 1.0);
}

#[test]
fn length_is_reachable_under_both_spellings() {
    // `.length` is a *Static* key on an array, which is the case that broke
    // the first design of this milestone: the rule was "a Static key never
    // needs the array path, because an IdentifierName is never an index", and
    // it is true and does not follow. See `emit::m1::Lower::accessor`.
    number("return [1, 2, 3].length;", 3.0);
    number("return [1, 2, 3][\"length\"];", 3.0);
    number("let k = \"length\"; return [1, 2, 3][k];", 3.0);
}

#[test]
fn a_loop_over_length_reads_every_element() {
    number(
        "let a = [1, 2, 3, 4]; let s = 0; \
         for (let i = 0; i < a.length; i = i + 1) { s = s + a[i]; } return s;",
        10.0,
    );
}

#[test]
fn any_other_property_of_an_array_is_absent_and_not_a_fault() {
    undefined("return [1].foo;");
    undefined("return [1][\"foo\"];");
}

// =========================================================================
// Identity and the type
// =========================================================================

#[test]
fn two_arrays_are_equal_only_when_they_are_the_same_array() {
    // 7.2.15 step 4, SameValueNonNumber: reference identity. The allocator
    // hands out one address per array, so the existing payload comparison
    // already is one -- no arm was added for this, the same way none was
    // added for Object.
    boolean("let a = [1]; return a === a;", true);
    boolean("let a = [1]; let b = [1]; return a === b;", false);
    boolean("let a = [1]; let b = a; return a === b;", true);
    boolean("let a = []; return a !== [];", true);
}

#[test]
fn typeof_an_array_is_object() {
    // 13.5.3 step 8. There is no `Array.isArray` here, so `typeof` cannot be
    // read as "not an array" -- worth stating, because that is exactly how a
    // script ported from a real engine will read it.
    string("return typeof [];", "object");
    string("return typeof [1, 2];", "object");
}

#[test]
fn an_array_has_no_primitive_form() {
    // 7.1.1 ToPrimitive needs the `valueOf`/`toString` a prototype would
    // carry, and this engine has no prototypes -- the same reason `"" + {}`
    // traps. It is a trap and not a fabricated `"1,2"`.
    traps("return [1] + 1;");
    traps("return \"\" + [1];");
    traps("return [1] * 2;");
}

#[test]
fn an_array_is_not_callable() {
    traps("let a = [1]; return a();");
}

// =========================================================================
// Objects are untouched
// =========================================================================

#[test]
fn object_access_still_works_in_a_program_that_has_arrays() {
    // The dispatcher tests Object first and returns, so an object access in an
    // array-using program takes the same path it always did. This is the test
    // that says the two types did not get entangled.
    number("let a = [1]; let o = { x: 2 }; return o.x + a[0];", 3.0);
    number("let a = [1]; let o = { x: 2 }; return o[\"x\"];", 2.0);
    number("let a = [1]; let o = {}; o.y = 5; return o.y;", 5.0);
    number(
        "let a = [1]; let o = {}; o[\"y\"] = 5; return o[\"y\"];",
        5.0,
    );
    // A Number key on an *object* is still ToPropertyKey'd to its digits,
    // which is 7.1.19 and unchanged by arrays existing.
    number("let a = [1]; let o = {}; o[1] = 5; return o[\"1\"];", 5.0);
}

#[test]
fn a_property_of_a_non_object_still_traps() {
    traps("return undefined[0];");
    traps("return null[0];");
    traps("let x = 1; return x[0];");
}

// =========================================================================
// Refusals
// =========================================================================

#[test]
fn an_elision_is_refused_by_name() {
    // A hole is not an `undefined`, and this engine has nothing that could
    // tell them apart -- so reading one as the other would be unobservably
    // wrong today and silently wrong the day `in` or `forEach` arrives.
    // Refusing costs a script nothing: nobody writes an elision on purpose.
    for source in ["[1, , 3];", "[, 1];", "let a = [1, , ]; return 0;"] {
        refuses_capability(source, "elisions in an array literal", Boundary::FullJs);
    }
}

#[test]
fn the_array_facilities_this_engine_does_not_have_are_refused_by_name() {
    // Each of these is refused by something that existed before arrays did,
    // and the point of asserting it here is that landing the type did not
    // quietly turn any of them into a wrong answer.
    refuses_capability(
        "return new Array(3);",
        "the `new` keyword",
        Boundary::FullJs,
    );
    refuses_capability(
        "return [...[1]];",
        "the spread and rest syntax",
        Boundary::ThirdBinding,
    );
    refuses_capability(
        "let a = [1]; return [...a];",
        "the spread and rest syntax",
        Boundary::ThirdBinding,
    );
}

#[test]
fn two_neighbours_answer_with_a_syntax_error_rather_than_a_capability() {
    // Neither is a regression from this milestone and neither is ideal, so
    // both are pinned as they are rather than as they should be -- a
    // diagnostic nobody asserted is a diagnostic that drifts.
    //
    // **Array destructuring** is read as a `const` with no name, which is the
    // same answer object destructuring has always given: the declarator parser
    // wants an identifier and reports the token it found. Consistent, and it
    // does not say the word "destructuring".
    for source in ["const [a] = [1]; return a;", "const {a} = {a:1}; return a;"] {
        let e = compile_qjs_m1(source).expect_err("refused");
        assert!(
            e.message.contains("needs a name after the `const` keyword"),
            "{source:?}: got {}",
            e.message
        );
    }

    // **`for (const x of a)`** reports the `const` binding rather than the
    // `of`, which is a known defect: written as `let` or `var` the same loop
    // correctly names `of`. Recorded downstream in `agenterm-qjswasm`'s README
    // as the one diagnostic still pointing at the wrong token; this is the
    // upstream test that will notice when it is fixed.
    // **The defect this comment used to record is fixed**, and the test that
    // said it would notice is the reason anyone knows.
    //
    // `for (const x of a)` reported "needs a value for the `const` binding
    // `x`" -- the parser read the header as a plain `const` declaration and
    // complained about a missing initialiser, pointing at `const` when the gap
    // was `for … of`. Written as `let` or `var`, the same loop correctly named
    // `of`. It is gone because the construct is supported: the header is
    // recognised by three tokens before `declaration` ever sees it.
    assert!(
        compile_qjs_m1("for (const x of [1]) { } return 0;").is_ok(),
        "`for (const x of …)` compiles since 2026-08-29; the misplaced diagnostic \
         it used to produce is what this assertion was written to outlive"
    );
}

#[test]
fn an_array_method_this_engine_lacks_is_absent_rather_than_refused() {
    // `[1, 2].filter` is not a diagnostic, and that is correct rather than a
    // gap: the receiver of a property access is a run-time fact, so an absent
    // property is `undefined` (10.1.8.1 with no prototype) and calling it is
    // the trap.
    //
    // Worth a test of its own because the diagnostic a reader *expects* here
    // is "arrays have no methods yet", and they will not get one -- so the
    // trap has to be the documented answer rather than a surprise.
    //
    // `map` and `push` were this test's two examples until `research/
    // method-binding/` landed them. They are answers now
    // (`method_conformance.rs`), so the examples moved to two the engine still
    // does not have -- which keeps the row about the *shape* of the refusal
    // rather than about which methods happen to exist today.
    undefined("return [1, 2].filter;");
    undefined("return [1, 2].join;");
    traps("let f = function (x) { return x; }; return [1, 2].filter(f);");
    // And the ones that landed really did: absent then, answers now.
    number("return [1, 2].push(3);", 3.0);
    number(
        "let f = function (x) { return x + 1; }; return [1, 2].map(f)[0];",
        2.0,
    );
}

#[test]
fn a_non_index_property_write_on_an_array_traps() {
    // There is nowhere in a dense vector to put it. A trap and not a dropped
    // write: a dropped write is a value the script believes it stored and
    // reads back as `undefined` later, somewhere else, with nothing pointing
    // at the assignment that lost it.
    //
    // `plan/design-array-milestone.md` names giving the record a second,
    // general property store as the disease this milestone must detect rather
    // than satisfy. This test is the detector.
    traps("let a = [1]; a.foo = 2; return 0;");
    traps("let a = [1]; a[\"foo\"] = 2; return 0;");
    traps("let a = [1]; a.length = 0; return 0;");
    traps("let a = [1]; a[3 / 2] = 2; return 0;");
}

// =========================================================================
// JSON
// =========================================================================

#[test]
fn json_stringify_writes_an_array() {
    string("return JSON.stringify([]);", "[]");
    string("return JSON.stringify([1, 2, 3]);", "[1,2,3]");
    string(
        "return JSON.stringify([\"a\", true, null]);",
        "[\"a\",true,null]",
    );
    string("return JSON.stringify([[1], [2, 3]]);", "[[1],[2,3]]");
    string("return JSON.stringify({ a: [1, 2] });", "{\"a\":[1,2]}");
}

#[test]
fn an_absent_element_is_null_where_an_absent_property_is_omitted() {
    // 25.5.2.5 step 8 against 25.5.2.4 step 5, and the two disagree on
    // purpose: an array's indices are positional, so dropping an element would
    // renumber every one after it. An object's properties are named, so
    // dropping one changes nothing else.
    string("return JSON.stringify([undefined, 1]);", "[null,1]");
    string(
        "return JSON.stringify({ a: undefined, b: 1 });",
        "{\"b\":1}",
    );
    string(
        "let f = function () { return 1; }; return JSON.stringify([f, 1]);",
        "[null,1]",
    );
}

#[test]
fn json_parse_builds_an_array() {
    number("return JSON.parse(\"[]\").length;", 0.0);
    number("return JSON.parse(\"[1,2,3]\").length;", 3.0);
    number("return JSON.parse(\"[1,2,3]\")[1];", 2.0);
    number("return JSON.parse(\"{\\\"a\\\":[1,2]}\").a[1];", 2.0);
    string(
        "return JSON.parse(\"[{\\\"id\\\":\\\"tab1\\\"}]\")[0].id;",
        "tab1",
    );
    string("return typeof JSON.parse(\"[1]\");", "object");
}

#[test]
fn a_parsed_array_round_trips() {
    string(
        "return JSON.stringify(JSON.parse(\"[1,[2,{\\\"c\\\":3}]]\"));",
        "[1,[2,{\"c\":3}]]",
    );
}

#[test]
fn a_malformed_array_is_a_syntax_error_and_is_catchable() {
    // The refusals here are about the *text*, not about this engine -- which
    // is the distinction the deleted "this engine does not support JSON arrays
    // yet" message used to sit on the wrong side of.
    for text in ["[1,", "[1 2]", "[,]", "[1,]"] {
        let source = format!(
            "let n = 0; try {{ JSON.parse(\"{text}\"); }} catch (e) {{ n = 1; }} return n;"
        );
        assert_eq!(run(&source), Out::Number(1.0), "{text:?}");
    }
}

#[test]
fn an_array_that_contains_itself_is_the_same_type_error_an_object_gets() {
    // 25.5.2.5 step 1, the same cycle chain `__json_ser_obj` walks, and
    // catchable for the same reason.
    number(
        "let a = [1]; a[1] = a; let n = 0;          try { JSON.stringify(a); } catch (e) { n = 1; } return n;",
        1.0,
    );
    // A DAG is not a cycle: the chain walked is the ancestors', so a value
    // reached twice by two paths serializes twice, which is what 25.5.2.2
    // requires.
    string(
        "let inner = [1]; return JSON.stringify([inner, inner]);",
        "[[1],[1]]",
    );
}

// =========================================================================
// The gate
// =========================================================================

#[test]
fn a_program_with_no_array_and_no_json_is_byte_identical_to_what_it_was() {
    // The promise `plan/design-array-milestone.md` §1.1 makes, as a test
    // rather than as a sentence. These are the exact byte counts from before
    // the milestone landed; the first measurement of it broke all three by 11
    // bytes, because `__typeof` and `__truthy` are in the *unconditional*
    // runtime and their Array arms had been appended there unguarded.
    //
    // The three numbers moved **down** by 19 on 2026-08-28, and once. `__len`
    // sits in the unconditional runtime, so it was emitted in every module and
    // called from none of them; the string-`.length` milestone gave it a real
    // body and gated that body on the program naming the property. With the
    // gate off it is now an `unreachable` stub, which is 19 bytes less dead
    // weight than the byte-count body it replaced. Nothing here started paying
    // for anything -- the opposite.
    //
    // All three moved **up** by 175 on 2026-08-29, and once: `__num_to_string`
    // (in the unconditional runtime, so every program carries it) gained an
    // integer fast path -- `"" + n` went from ~5 200 steps to ~540. A
    // price every program pays for a speed every program uses; see
    // plan/design-num-to-string-fast.md.
    //
    // The JSON set moved up by 329 on 2026-08-29 (15 584 -> 15 913; the fleet
    // library 22 650 -> 22 981): `__jb_bytes` and the plain-ASCII run scan in
    // `__json_pstr`, paid only by programs that name JSON. `"return 1;"` did
    // not move. And by 244 more the same night: the scan and both copies
    // went four bytes at a time (29 steps a byte on a long string, from 119).
    // And 88 more for the fraction fast path (`1.5`: 1 336 -> 539 steps).
    // Programs that read a static property moved by +24 the same night:
    // `__obj_get`'s arm that names the key read off undefined/null/a
    // primitive (`FAULT_PROPERTY_OF_NON_OBJECT`), behind the same gate as
    // the String arm. `"return 1;"` did not move.
    // The fleet library moved by +301 on 2026-08-29 (late): it has a
    // `try` and property reads, so it carries the TypeError text and the throw
    // arm in `__obj_get`. A JSON-only program did not move.
    for (source, want) in [
        // +85 on 2026-08-30: `__str_concat` copies eight bytes a step and
        // then the tail, two loops where there was one. Every program carries
        // `__str_concat`, so every row here moved by the same 85; one append
        // to a 1 000-character string fell 17 178 -> 2 569 steps
        // (tests/concat_cost.rs).
        // -18 on 2026-08-30, for every program: `__to_number` lost its two
        // bare Object and function arms. They trapped with no name written
        // and, sitting ahead of the named arms below, shadowed them
        // (`f + 1` was the row that found it).
        // +17 on 2026-08-30 (late) for every program that declares a
        // function, none of which are here: the `qjs.lines` custom section
        // (14 bytes of header, then 2-3 per function) says which source line
        // each function was written on, so tinyvm's refusal of a function
        // body can name the line (tests/lower_m2.rs, tinyvm's
        // tests/explained_load.rs). The script itself is not listed, which
        // is why "return 1;" did not move. The fleet library moved by +89.
        ("return 1;", 10_007),
        // +190 on 2026-08-30: a program that can hold an Object, an Array or
        // a function carries the three kind names and the arms that refuse
        // their string and number forms by name (fault 9,
        // tests/to_string_of_objects.rs).
        // +215 on 2026-08-30, later: a program that writes through a member
        // carries the four refusal reasons and the arms that name a refused
        // write or a boundary of the representation (fault 10 and a named
        // fault 3, tests/refused_operations.rs). "return 1;" did not move.
        ("let o = {a:1}; o.b = 2; return o.a;", 10_580), /* +23 on 2026-08-29: a program that reads a static property can reach `__obj_get` with a String receiver, and the arm that names the missing property is 23 bytes; see runtime.rs `FAULT_MISSING_STRING_METHOD` */
        // A computed key whose *value* the text does not settle. This one
        // moved **up** by 117, and not for arrays: a computed key could
        // evaluate to `"length"`, so it turns on the string-`.length` arm of
        // `obj_get`. The row below is the same program with the key written
        // out, where the text settles it and nothing turns on.
        //
        // It moved up another **7** on 2026-08-29, and the number is the point
        // rather than the change: that arm's trap now writes a fault code
        // first, so a host can tell "this engine has no such String property"
        // from "this engine is broken". Seven bytes buys the difference
        // between a sentence and a bare `unreachable`, and only programs that
        // reach the arm pay it -- the row below still does not.
        // +58 on 2026-08-30: `__len` counts eight plain-ASCII bytes a step
        // (180 000 -> 19 900 steps on 6 000 characters, tests/length_cost.rs).
        // Only programs that can reach the string-`.length` arm carry it.
        (
            "let o = {a:1}; let k = \"a\"; return o[k];",
            10_486, /* +7 on 2026-08-30: the nameless capability arm now clears the detail word before it stops, so a named refusal read later cannot inherit an older name (tests/refused_operations.rs) */
        ),
        ("let o = {a:1}; return o[\"a\"];", 10_260),
    ] {
        let n = compile_qjs_m1(source).expect("compiles").len();
        assert_eq!(
            n, want,
            "{source:?} is {n} bytes; an array-free program must pay nothing for arrays"
        );
    }
}

#[test]
fn naming_json_brings_the_array_set_because_parse_can_return_one() {
    // The other half of the gate's predicate, and the reason it is exact: no
    // `[` appears in this source and an array can still come out of it, so
    // gating on the literal alone would be a gate with a hole in it.
    let n = compile_qjs_m1("return JSON.stringify({a:1});")
        .expect("compiles")
        .len();
    assert_eq!(
        n, 16_743,
        "the array set costs 1 130 bytes on top of the JSON set's 14 284 -- 753 for the \
         type and 377 more for `JSON.parse`/`JSON.stringify` of one. That arithmetic is \
         unchanged; the total went down by 28 on 2026-08-28 because `__len`'s dead body \
         got gated, which is 19 bytes here plus 9 of shifted LEB128 widths. If this moved \
         again, say so in \
         `function_values::the_whole_fleet_library_compiles_and_its_methods_are_reachable`, \
         which quotes the same arithmetic"
    );
}

/// **The measurement `plan/design-array-milestone.md` §2.1 owed**, and the one
/// that either justifies the eighth tag or refutes it.
///
/// §2.1 rejected spelling an array as an object with integer-named keys on
/// cost, and the argument was *reasoned* rather than measured: the object
/// record finds a key by walking its entries with `__str_eq`, and the key for
/// `a[i]` is a Number, so every index access would run `__num_to_string` --
/// Dragon4 -- to build a fresh record and then scan. This is that claim as a
/// number.
///
/// # It measures the slope, not the total
///
/// A total would mix in the setup loop, the allocator, and the module prelude,
/// none of which is what §2.1 is about. So each spelling is run at two sizes
/// and the difference divided by the gap: **steps per one more element read**.
/// That is the marginal cost, which is the quantity a growth-law argument is
/// made of -- a design that is cheap today and linear where the other is flat
/// loses, and only the slope can say so.
///
/// The two loops are otherwise identical, down to the accumulator and the
/// bound, and both are built by the same `for` loop so neither gets a literal
/// the other does not.
#[test]
fn an_indexed_read_costs_what_the_eighth_tag_was_chosen_for() {
    /// Steps for one top-level call of `source`.
    #[track_caller]
    fn steps(source: &str) -> u64 {
        let wasm = compile_qjs_m1(source).expect("compiles");
        // A generous ceiling: the object spelling at 210 keys is quadratic in
        // the scan and the point is to let it finish, not to bound it.
        let limits = Limits {
            max_steps: 1 << 32,
            ..Limits::default()
        };
        let module = WasmModule::from_bytes_with(&wasm, limits).expect("loads");
        let mut instance = module.instantiate().expect("instantiates");
        instance
            .invoke_by_name("main", &Value::args(&[]))
            .expect("runs");
        instance.last_steps()
    }

    // Build with `c[i] = i`, then read with `c[i]` -- the same two loops for
    // both spellings, so the only difference is what `c` is.
    let program = |open: &str, n: u32| {
        format!(
            "let c = {open};              for (let i = 0; i < {n}; i = i + 1) {{ c[i] = i; }}              let s = 0;              for (let j = 0; j < {n}; j = j + 1) {{ s = s + c[j]; }}              return s;"
        )
    };

    const SMALL: u32 = 10;
    const LARGE: u32 = 110;
    let gap = u64::from(LARGE - SMALL);

    let array_slope = (steps(&program("[]", LARGE)) - steps(&program("[]", SMALL))) / gap;
    let object_slope = (steps(&program("{}", LARGE)) - steps(&program("{}", SMALL))) / gap;

    println!("per element: array {array_slope} steps, object {object_slope} steps");

    // The verdict, stated as a floor rather than as the exact numbers, because
    // the exact numbers move with every unrelated change to `__add` or the
    // loop lowering and a test that pins them would fail for reasons that are
    // not about this decision. What is being asserted is the *shape* of the
    // difference §2.1 predicted.
    assert!(
        array_slope * 4 < object_slope,
        "the dense vector should be several times cheaper per element than the \
         object spelling; measured array {array_slope}, object {object_slope}. If \
         this ever stops holding, `plan/design-array-milestone.md` §2.1 is the \
         section to reopen -- its argument, not this test, is what would be wrong."
    );
    // And the array's own slope should be flat in the size, which is the other
    // half of the claim: the index is the address, so element 100 costs what
    // element 1 costs.
    let array_far = (steps(&program("[]", 410)) - steps(&program("[]", 310))) / 100;
    assert!(
        array_far <= array_slope + 1,
        "an indexed read must not get more expensive as the array grows: \
         {array_slope} steps per element at 10..110, {array_far} at 310..410"
    );
}

#[test]
fn the_record_is_the_dense_vector_the_design_says_it_is() {
    // The layout has no observable surface in this subset -- there is no
    // `Object.keys`, no `for…in`, and `JSON.stringify` of an array is a later
    // milestone -- and a guarantee with no test is a guarantee that quietly
    // stops holding. So this walks the record itself, the way
    // `objects_m3.rs` walks the object record, and it is the layout's
    // specification as much as `array.rs` is.
    let (instance, vals) = attempt("return [10, 20, 30];").expect("runs");
    let [Val::I32(tag), Val::I64(payload)] = vals.as_slice() else {
        panic!("want one V1 pair back, got {vals:?}");
    };
    assert_eq!(*tag, TAG_ARRAY, "the array tag is 7");

    let view = instance.memory().expect("guest memory");
    let bytes: &[u8] = &view;
    let a = *payload as u32 as usize;
    assert_eq!(word(bytes, a + ARR_LEN), 3, "three elements");
    assert_eq!(
        word(bytes, a + ARR_CAP),
        3,
        "a literal allocates at its exact length and never reallocates"
    );

    let elems = word(bytes, a + ARR_ELEMS) as usize;
    for (i, want) in [10.0f64, 20.0, 30.0].into_iter().enumerate() {
        let at = elems + i * ELEM_BYTES;
        assert_eq!(word(bytes, at), 1, "element {i} is tagged Number");
        let payload = u64::from_le_bytes(bytes[at + 4..at + 12].try_into().expect("8 bytes"));
        assert_eq!(f64::from_bits(payload), want, "element {i}");
    }
}

#[test]
fn an_array_cannot_leave_through_the_host_face() {
    // Deliberate, and the same answer an Object gets: the payload is a guest
    // heap reference the host has no layout for and no way to keep alive.
    // `Value` gains no Array variant here. A script that wants the host to see
    // an array's contents returns a property of it.
    let vals = attempt("return [1];").expect("runs").1;
    let err = Value::returned(&vals).expect_err("an Array has no host-side variant");
    // Named, not "unknown tag 7". The tag landed with this milestone and the
    // arm did not, so for one commit a host that returned an array was told
    // something that reads as a defect in the engine. Found by the downstream
    // crate's own README-claim lock, which is what that lock is for.
    assert_eq!(
        err,
        "V1: an Array is a guest heap reference; `Value` has no variant for one yet"
    );
}

#[test]
fn the_declared_names_mode_reaches_arrays_too() {
    // The gate is a property of the program, not of the naming mode, and the
    // product uses `Names::Declared`. A milestone that only worked under the
    // default would not be reachable from `agenterm-qjswasm` at all.
    let wasm = compile_qjs_m1_with(
        "return [1, 2].length;",
        Options {
            names: Names::Declared(Vec::new()),
        },
    )
    .expect("compiles under Declared");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect("runs");
    assert_eq!(Value::returned(&vals), Ok(Value::Number(2.0)));
}
