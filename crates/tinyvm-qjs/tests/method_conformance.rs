//! Criterion ① of the method-binding experiment: the four methods run and the
//! answers are right.
//!
//! This file was written **before any implementation existed**, as criterion
//! ① of `plan/design-method-binding-experiment.md`: all three candidate
//! binding mechanisms had to pass it unchanged, and all three did. The
//! mechanism that shipped was chosen on the other criteria, not on this one --
//! which is the reason the file is worth keeping now that the experiment is
//! over. It says what the methods must *do*, with no reference to how.

use tinyvm::{Limits, Val, WasmInstance, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

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
fn undefined(source: &str) {
    assert_eq!(run(source), Out::Undefined, "{source:?}");
}

#[track_caller]
fn number(source: &str, want: f64) {
    assert_eq!(run(source), Out::Number(want), "{source:?}");
}

#[track_caller]
fn boolean(source: &str, want: bool) {
    assert_eq!(run(source), Out::Bool(want), "{source:?}");
}

// =========================================================================
// The four methods, chosen in §2 for what each one forces
// =========================================================================

/// `"s".trim()` -- String receiver, no arguments, returns a new String.
/// The simplest square there is; if this does not work nothing else will.
///
/// ECMA-262 22.1.3.32: `trim` removes leading and trailing **WhiteSpace and
/// LineTerminator** (the `TrimString` abstract op, 22.1.3.32.1), which is not
/// the same set as "ASCII space" -- `\t`, `\n`, `\r`, `\u{b}`, `\u{c}`,
/// `\u{a0}` and `\u{feff}` are all in it. A variant that trims only `' '`
/// passes a lazy test and fails this one.
#[test]
fn trim_removes_whitespace_from_both_ends() {
    string("return \"  ab  \".trim();", "ab");
    string("return \"ab\".trim();", "ab");
    string("return \"\".trim();", "");
    string("return \"   \".trim();", "");
    // Interior whitespace is untouched.
    string("return \"  a b  \".trim();", "a b");
    // The full WhiteSpace set, not just the space character.
    string("return \"\\t\\n\\r ab \\t\\n\\r\".trim();", "ab");
    string("return \"\\u{a0}ab\\u{a0}\".trim();", "ab");
    string("return \"\\u{feff}ab\\u{feff}\".trim();", "ab");
    // The whole of ECMA-262 12.2's WhiteSpace, which is the Unicode `Zs`
    // category plus TAB/VT/FF/ZWNBSP, together with 12.3's LineTerminators.
    // The `Zs` members past the named ones are the ones an implementation
    // quietly skips, and skipping them is a **wrong answer** rather than a
    // missing feature: the space stays and nothing says so.
    for space in [
        "\u{1680}", "\u{2000}", "\u{2003}", "\u{200a}", "\u{2028}", "\u{2029}", "\u{202f}",
        "\u{205f}", "\u{3000}",
    ] {
        string(&format!("return \"{space}ab{space}\".trim();"), "ab");
    }
    // And the characters that *look* like whitespace and are not: U+200B ZERO
    // WIDTH SPACE and U+2060 WORD JOINER are `Cf`, not `Zs`, so `trim` must
    // leave them exactly where they are.
    string(
        "return \"\u{200b}ab\u{200b}\".trim();",
        "\u{200b}ab\u{200b}",
    );
    string(
        "return \"\u{2060}ab\u{2060}\".trim();",
        "\u{2060}ab\u{2060}",
    );

    // Non-ASCII content survives, and the result is a real String: its
    // `.length` is UTF-16 code units, as `string_length_m3.rs` pins.
    string("return \"  caf\u{e9}  \".trim();", "caf\u{e9}");
    number("return \"  caf\u{e9}  \".trim().length;", 4.0);
}

/// `"s".indexOf(x)` -- String receiver **with an argument**. Argument passing
/// is a thing a binding mechanism can get wrong on its own, so it needs a
/// method of its own rather than a second no-argument one.
///
/// ECMA-262 22.1.3.9. The position is in **UTF-16 code units**, for the same
/// reason `length` is; the two must agree or one of them is lying.
#[test]
fn index_of_finds_a_substring_by_code_unit_position() {
    number("return \"abc\".indexOf(\"b\");", 1.0);
    number("return \"abc\".indexOf(\"a\");", 0.0);
    number("return \"abc\".indexOf(\"z\");", -1.0);
    // 22.1.3.9 step 6: the empty string is found at 0.
    number("return \"abc\".indexOf(\"\");", 0.0);
    number("return \"\".indexOf(\"\");", 0.0);
    number("return \"\".indexOf(\"a\");", -1.0);
    // Multi-unit needle, and the first match wins.
    number("return \"abcabc\".indexOf(\"ca\");", 2.0);
    number("return \"abcabc\".indexOf(\"a\");", 0.0);
    // Positions are code units, so they line up with `length`.
    number("return \"caf\u{e9}x\".indexOf(\"x\");", 4.0);
    number("return \"\u{1f600}x\".indexOf(\"x\");", 2.0);
    // The argument is an expression, not just a literal.
    number("let n = \"b\"; return \"abc\".indexOf(n);", 1.0);
}

/// `a.push(x)` -- Array receiver, and it **mutates the receiver**. Whether the
/// binding carries a reference or a copy is invisible until something writes
/// through it, which is why one of the four has to be a mutator.
///
/// ECMA-262 23.1.3.23 returns the new length.
#[test]
fn push_mutates_the_receiver_and_returns_the_new_length() {
    number("let a = [1, 2]; a.push(3); return a.length;", 3.0);
    number("let a = [1, 2]; return a.push(3);", 3.0);
    number("let a = []; return a.push(7);", 1.0);
    number("let a = [1, 2]; a.push(3); return a[2];", 3.0);
    // The mutation is visible through a *second name* for the same array,
    // which is the assertion a copying binding fails.
    number("let a = [1]; let b = a; b.push(2); return a.length;", 2.0);
    number("let a = [1]; let b = a; b.push(2); return a[1];", 2.0);
    // And through a function that received it.
    number(
        "function add(x) { x.push(9); } let a = [1]; add(a); return a[1];",
        9.0,
    );
    // Repeated pushes, so a one-shot implementation fails.
    number(
        "let a = []; a.push(1); a.push(2); a.push(3); return a.length;",
        3.0,
    );
    number(
        "let a = []; a.push(1); a.push(2); a.push(3); return a[1];",
        2.0,
    );
}

/// `a.map(f)` -- Array receiver whose argument is a **function value the
/// runtime has to call back into**. The most expensive square, and the one
/// that says the most about a mechanism.
///
/// ECMA-262 23.1.3.20: a new array, same length, `f` applied to each element.
#[test]
fn map_calls_back_into_a_function_value() {
    number(
        "let a = [1, 2, 3]; return a.map(function (x) { return x + 1; })[0];",
        2.0,
    );
    number(
        "let a = [1, 2, 3]; return a.map(function (x) { return x + 1; })[2];",
        4.0,
    );
    number(
        "let a = [1, 2, 3]; return a.map(function (x) { return x + 1; }).length;",
        3.0,
    );
    // An arrow, which is the way anyone will actually write it.
    number("let a = [1, 2, 3]; return a.map(x => x * 2)[1];", 4.0);
    // Empty array: the callback is never called and the result is empty.
    number("let a = []; return a.map(x => x + 1).length;", 0.0);
    // A named function value, passed rather than written inline.
    number(
        "function inc(x) { return x + 1; } let a = [5]; return a.map(inc)[0];",
        6.0,
    );
    // The callback closes over an outer binding -- closures and methods have
    // to compose, and this is where a mechanism that rebuilds environments
    // would go wrong.
    number(
        "let k = 10; let a = [1, 2]; return a.map(x => x + k)[1];",
        12.0,
    );
    // `map` does not mutate the receiver.
    number("let a = [1, 2]; a.map(x => x + 1); return a[0];", 1.0);
    // The result is a real Array: it indexes, it has `length`, and it can be
    // pushed to.
    number(
        "let a = [1]; let b = a.map(x => x); b.push(2); return b.length;",
        2.0,
    );
    // Chained, which is the shape that makes methods worth having at all.
    number(
        "let a = [1, 2, 3]; return a.map(x => x + 1).map(x => x * 2)[0];",
        4.0,
    );
}

// =========================================================================
// The 2026-08-31 batch: what the migrated scripts spelled by hand
// =========================================================================

/// `Array.isArray(x)` -- ECMA-262 23.1.2.2. `rh_compat.qjs` wrote it as
/// "an object with a numeric `length`", which an object literal with a
/// `length` key satisfies; the tag does not lie.
#[test]
fn is_array_answers_the_tag_and_nothing_else() {
    boolean("return Array.isArray([1, 2]);", true);
    boolean("return Array.isArray([]);", true);
    boolean("let a = [1]; let b = a; return Array.isArray(b);", true);
    boolean("return Array.isArray(JSON.parse(\"[1]\"));", true);
    boolean("return Array.isArray({ length: 1 });", false);
    boolean("return Array.isArray(\"ab\");", false);
    boolean("return Array.isArray(undefined);", false);
    boolean("return Array.isArray(null);", false);
    boolean("return Array.isArray(3);", false);
    boolean("return Array.isArray(function () {});", false);
    // Every value is a receiver here, so a call inside an expression
    // that could not otherwise be typed still answers.
    number(
        "let n = 0; for (const x of [[], 1, [2]]) { if (Array.isArray(x)) { n = n + 1; } } return n;",
        2.0,
    );
    // A script that declares its own `Array` gets its own, as with
    // `Number`, `Object` and `JSON`.
    number(
        "const Array = { isArray: function (x) { return 7; } }; return Array.isArray(1);",
        7.0,
    );
}

/// `a.indexOf(x)` and `a.includes(x)` on an Array -- ECMA-262 23.1.3.17 and
/// 23.1.3.16. The two differ in one value: `indexOf` compares with
/// IsStrictlyEqual and never finds `NaN`; `includes` uses SameValueZero and
/// does. Both share their name with the String method, so the receiver's
/// tag decides which runs -- and both receivers are exercised in one
/// program below, which is the case a text-only dispatch would get wrong.
#[test]
fn array_index_of_and_includes_compare_strictly() {
    number("return [1, 2, 3].indexOf(2);", 1.0);
    number("return [1, 2, 3].indexOf(4);", -1.0);
    number("return [1, 2, 2].indexOf(2);", 1.0);
    number("return [].indexOf(1);", -1.0);
    number("return [\"a\", \"b\"].indexOf(\"b\");", 1.0);
    number(
        "let s = \"b\"; return [\"a\", \"b\"].indexOf(s + \"\");",
        1.0,
    );
    // Strict: no coercion across tags.
    number("return [1, \"1\"].indexOf(\"1\");", 1.0);
    number("return [true].indexOf(1);", -1.0);
    number("return [null].indexOf(undefined);", -1.0);
    number("return [undefined].indexOf(undefined);", 0.0);
    // Identity for records.
    number("let o = {}; return [1, o].indexOf(o);", 1.0);
    number("return [{}].indexOf({});", -1.0);
    number("let a = [1]; return [a, [1]].indexOf(a);", 0.0);
    // NaN: never found by indexOf, found by includes.
    number("let n = 0 / 0; return [n].indexOf(n);", -1.0);
    boolean("let n = 0 / 0; return [1, n].includes(n);", true);
    boolean("let n = 0 / 0; return [1, 2].includes(n);", false);
    boolean("return [1, 2].includes(2);", true);
    boolean("return [1, 2].includes(3);", false);
    boolean("return [].includes(undefined);", false);
    boolean("return [undefined].includes(undefined);", true);
    boolean("return [\"x\"].includes(\"x\");", true);
    boolean("return [0].includes(-0);", true);
    // The String receiver still answers as before, in the same program.
    number(
        "let a = [\"b\"]; let s = \"abc\"; return s.indexOf(\"c\") * 10 + a.indexOf(\"b\");",
        20.0,
    );
    boolean(
        "let a = [\"b\"]; let s = \"abc\"; return s.includes(\"d\") || a.includes(\"b\");",
        true,
    );
    // The receiver is not mutated.
    number(
        "let a = [1, 2]; a.indexOf(2); a.includes(1); return a.length;",
        2.0,
    );
}

/// `a.concat(x)` and `a.concat(x, y)` -- ECMA-262 23.1.3.1 without symbols:
/// an Array argument spreads, anything else is one element.
#[test]
fn concat_spreads_arrays_and_appends_everything_else() {
    number("return [1, 2].concat([3, 4]).length;", 4.0);
    number("return [1, 2].concat([3, 4])[3];", 4.0);
    number("return [1].concat(2).length;", 2.0);
    number("return [1].concat(2)[1];", 2.0);
    string("return [\"a\"].concat(\"b\")[1];", "b");
    number("return [].concat([]).length;", 0.0);
    number("return [].concat([1])[0];", 1.0);
    // Two arguments, each spread or appended on its own.
    number("return [1].concat([2], [3]).length;", 3.0);
    number("return [1].concat([2], [3])[2];", 3.0);
    number("return [1].concat([2, 3], 4).length;", 4.0);
    number("return [1].concat([2, 3], 4)[3];", 4.0);
    // One level only: a nested array is an element.
    number("return [1].concat([[2, 3]]).length;", 2.0);
    number("return [1].concat([[2, 3]])[1].length;", 2.0);
    // `undefined` and `null` are elements too.
    number("return [1].concat(undefined).length;", 2.0);
    number("return [1].concat([null, undefined]).length;", 3.0);
    // A new array: neither operand is touched, and the result is real.
    number(
        "let a = [1]; let b = [2]; a.concat(b); return a.length + b.length;",
        2.0,
    );
    number(
        "let a = [1]; let c = a.concat([2]); c.push(3); return a.length * 10 + c.length;",
        13.0,
    );
    // The demand site: argv from three lists.
    string(
        "function argv(base, sel, act) { return base.concat(sel, act).join(\" \"); } return argv([\"--target\", \"x\"], [\"a\"], [\"b\", \"c\"]);",
        "--target x a b c",
    );
}

/// `a.join(sep)` -- ECMA-262 23.1.3.18. `undefined` and `null` elements are
/// empty (step 7.c); the separator defaults to `","`; every other element
/// is ToString.
#[test]
fn join_writes_every_element_with_the_separator_between() {
    string("return [1, 2, 3].join(\"-\");", "1-2-3");
    string("return [\"a\", \"b\"].join();", "a,b");
    string("return [\"a\", \"b\"].join(undefined);", "a,b");
    string("return [].join(\"-\");", "");
    string("return [].join();", "");
    string("return [1].join(\"-\");", "1");
    string("return [undefined, null, 1].join(\"|\");", "||1");
    string("return [null].join(\",\");", "");
    string("return [1.5, true, false].join(\" \");", "1.5 true false");
    string("return [\"x\", \"y\"].join(\"\");", "xy");
    string("return [\"a\", \"b\"].join(1);", "a1b");
    string("return [\"a\", \"b\"].join(\", \");", "a, b");
    // Bytes, not characters: multi-byte elements and separators copy whole.
    string(
        "return [\"caf\u{e9}\", \"\u{1f600}\"].join(\"\u{2192}\");",
        "caf\u{e9}\u{2192}\u{1f600}",
    );
    // The receiver is untouched, and the answer is an ordinary String.
    number("let a = [1, 2]; a.join(\"-\"); return a.length;", 2.0);
    number("return [1, 2].join(\"-\").length;", 3.0);
    // Built from a loop, which is how a path is spelled.
    string(
        "let p = []; for (const s of [\"File\", \"Do Thing\"]) { p.push(s); } return p.join(\"/\");",
        "File/Do Thing",
    );
    // An Object or an Array element has no string form here: the same
    // named refusal `\"\" + o` raises, never `[object Object]`.
    assert!(attempt("return [{}].join(\",\");").is_err());
    assert!(attempt("return [[1]].join(\",\");").is_err());
}

/// `a.sort()` and `a.sort(f)` -- ECMA-262 23.1.3.30: stable, in place, the
/// receiver returned. The default order is String order of the ToString
/// forms -- `[10, 9, 1]` sorts to `[1, 10, 9]`, which is the spec and every
/// engine, not a defect -- and `undefined` sorts last under both forms.
#[test]
fn sort_is_stable_in_place_and_returns_the_receiver() {
    string("return [3, 1, 2].sort().join();", "1,2,3");
    string("return [10, 9, 1].sort().join();", "1,10,9");
    string("return [1, 100, 2, 20].sort().join();", "1,100,2,20");
    string("return [\"b\", \"a\", \"c\"].sort().join();", "a,b,c");
    string("return [2, \"10\", 1].sort().join();", "1,10,2");
    // Code-unit order: `é` (U+00E9) is after `z`.
    string(
        "return [\"\u{e9}\", \"z\", \"a\"].sort().join(\"\");",
        "az\u{e9}",
    );
    string(
        "return [\"b\", undefined, \"a\"].sort().join(\"-\");",
        "a-b-",
    );
    number("return [\"b\", undefined, \"a\"].sort().length;", 3.0);
    undefined("return [\"b\", undefined, \"a\"].sort()[2];");
    number("return [].sort().length;", 0.0);
    number("return [1].sort()[0];", 1.0);
    // In place, and the answer *is* the receiver.
    number("let a = [2, 1]; a.sort(); return a[0];", 1.0);
    boolean("let a = [2, 1]; return a.sort() === a;", true);
    number("let a = [2, 1]; let b = a; a.sort(); return b[0];", 1.0);
    // A comparator: numeric, descending, named, closing over a binding.
    string("return [10, 9, 1].sort((a, b) => a - b).join();", "1,9,10");
    string("return [10, 9, 1].sort((a, b) => b - a).join();", "10,9,1");
    string(
        "function up(a, b) { return a - b; } return [3, 2, 1].sort(up).join();",
        "1,2,3",
    );
    string(
        "let sign = -1; return [1, 2, 3].sort((a, b) => sign * (a - b)).join();",
        "3,2,1",
    );
    // Stable: equal keys keep their order, under a comparator and by default.
    string(
        "let a = [{k: 1, v: \"a\"}, {k: 0, v: \"b\"}, {k: 1, v: \"c\"}, {k: 0, v: \"d\"}]; return a.sort((x, y) => x.k - y.k).map(o => o.v).join(\"\");",
        "bdac",
    );
    string(
        "let a = [{k: 1, v: \"a\"}, {k: 1, v: \"b\"}, {k: 1, v: \"c\"}]; return a.sort((x, y) => x.k - y.k).map(o => o.v).join(\"\");",
        "abc",
    );
    // A comparator answering NaN is 0: nothing moves (step 6).
    string("return [3, 1, 2].sort((a, b) => 0 / 0).join();", "3,1,2");
    // `undefined` is last under a comparator too, and the comparator never
    // sees it (steps 1-3 come first).
    string(
        "let calls = 0; let a = [undefined, 2, 1].sort((x, y) => { calls = calls + 1; return x - y; }); return a.join(\"-\") + \":\" + calls;",
        "1-2-:1",
    );
    // Eight elements: more than one merge pass, every run shape.
    string(
        "return [8, 3, 5, 1, 7, 2, 6, 4].sort((a, b) => a - b).join();",
        "1,2,3,4,5,6,7,8",
    );
    string(
        "return [\"h\", \"c\", \"e\", \"a\", \"g\", \"b\", \"f\", \"d\", \"i\"].sort().join(\"\");",
        "abcdefghi",
    );
    // A comparator that is not a function is the TypeError of step 1;
    // an Object element has no default string form (the named refusal).
    assert!(attempt("return [2, 1].sort(1);").is_err());
    assert!(attempt("return [{}, {}].sort();").is_err());
}

/// `s.charCodeAt(i)` -- ECMA-262 22.1.3.3 -- and `s.charAt(i)` (22.1.3.2),
/// on UTF-16 positions. A surrogate half is a Number and `charCodeAt`
/// answers it; `charAt` on the same position would have to fabricate a
/// lone surrogate, which UTF-8 cannot hold, so that is the named refusal
/// the mid-pair `slice` boundary already is (`tests/refused_operations.rs`).
#[test]
fn char_code_at_and_char_at_read_utf16_positions() {
    number("return \"abc\".charCodeAt(0);", 97.0);
    number("return \"abc\".charCodeAt(2);", 99.0);
    number("return \"abc\".charCodeAt(1.7);", 98.0);
    number("return \"caf\u{e9}\".charCodeAt(3);", 0xe9 as f64);
    number("return \"\u{4e2d}\u{6587}\".charCodeAt(1);", 0x6587 as f64);
    // A pair: the two halves, and the unit after it is unit 2 not 1.
    number("return \"\u{1f600}\".charCodeAt(0);", 0xd83d as f64);
    number("return \"\u{1f600}\".charCodeAt(1);", 0xde00 as f64);
    number("return \"\u{1f600}x\".charCodeAt(2);", 120.0);
    number("return \"a\u{1f600}\".charCodeAt(2);", 0xde00 as f64);
    // Outside the string: NaN (step 6), spelled as `x !== x`.
    boolean("let n = \"ab\".charCodeAt(2); return n !== n;", true);
    boolean("let n = \"ab\".charCodeAt(-1); return n !== n;", true);
    boolean("let n = \"\".charCodeAt(0); return n !== n;", true);
    // The demand site: a byte from a character, in a loop.
    number(
        "let s = \"AZ\"; let sum = 0; for (let i = 0; i < s.length; i = i + 1) { sum = sum + s.charCodeAt(i); } return sum;",
        155.0,
    );
    string("return \"abc\".charAt(0);", "a");
    string("return \"abc\".charAt(2);", "c");
    string("return \"abc\".charAt(3);", "");
    string("return \"abc\".charAt(-1);", "");
    string("return \"caf\u{e9}\".charAt(3);", "\u{e9}");
    string("return \"a\u{1f600}b\".charAt(3);", "b");
    string("return \"\u{4e2d}\u{6587}\".charAt(1);", "\u{6587}");
    // Half a pair has no UTF-8: refused, not fabricated.
    assert!(attempt("return \"\u{1f600}\".charAt(1);").is_err());
}

/// `s[i]` -- ECMA-262 10.4.3.5: an integer index below the length is the
/// unit there, at or past it is `undefined`, and every other key is the
/// ordinary property read. Read only: a write is the refusal it was.
#[test]
fn a_string_indexes_by_code_unit() {
    string("let s = \"abc\"; return s[0];", "a");
    string("let s = \"abc\"; let i = 2; return s[i];", "c");
    string("let s = \"caf\u{e9}\"; return s[3];", "\u{e9}");
    undefined("let s = \"abc\"; return s[3];");
    undefined("let s = \"abc\"; return s[-1];");
    undefined("let s = \"abc\"; return s[1.5];");
    undefined("let s = \"\"; return s[0];");
    number("let s = \"abc\"; let k = \"length\"; return s[k];", 3.0);
    // A program with arrays takes the array set's road to the same answer,
    // and `a[i]` next to it still answers as an array.
    string(
        "let a = [\"x\"]; let s = \"abc\"; let i = 1; return s[i] + a[0];",
        "bx",
    );
    // And one without arrays takes the emitter's.
    string(
        "let s = \"abc\"; let o = {k: \"v\"}; let i = 1; let n = \"k\"; return s[i] + o[n];",
        "bv",
    );
    // The demand site: every character of a string, by index.
    string(
        "let s = \"abc\"; let out = \"\"; for (let i = 0; i < s.length; i = i + 1) { out = s[i] + out; } return out;",
        "cba",
    );
    // The index is a Number: `s["0"]` is a String key and stays the
    // property read (10.4.3.5 would answer `a`; this engine's `__arr_index`
    // divergence is recorded at array.rs, and this is the same line).
    assert!(attempt("let s = \"abc\"; return s[\"0\"];").is_err());
    // Half a pair: the same named refusal `charAt` and `slice` give.
    assert!(attempt("let s = \"\u{1f600}\"; return s[1];").is_err());
}

/// `s.substring(a[, b])` -- ECMA-262 22.1.3.24: clamp to `[0, length]`,
/// swap when out of order. The two rules `slice` does not have.
#[test]
fn substring_clamps_and_swaps() {
    string("return \"abcdef\".substring(1, 3);", "bc");
    string("return \"abcdef\".substring(3, 1);", "bc");
    string("return \"abcdef\".substring(2);", "cdef");
    string("return \"abcdef\".substring(-2, 2);", "ab");
    string("return \"abcdef\".substring(4, 100);", "ef");
    string("return \"abcdef\".substring(100, 4);", "ef");
    string("return \"abcdef\".substring(2, 2);", "");
    string("return \"abcdef\".substring(0);", "abcdef");
    string("return \"abcdef\".substring(-5);", "abcdef");
    string("return \"abcdef\".substring(0 / 0, 2);", "ab");
    string("return \"abcdef\".substring(1.9, 3.1);", "bc");
    string("return \"caf\u{e9} ok\".substring(3, 5);", "\u{e9} ");
    string("return \"a\u{1f600}b\".substring(1, 3);", "\u{1f600}");
    string("return \"a\u{1f600}b\".substring(3);", "b");
    // The demand site: the last `limit` characters.
    string(
        "function tail(t, limit) { return t.length <= limit ? t : t.substring(t.length - limit); } return tail(\"abcdefgh\", 3);",
        "fgh",
    );
    // Half a pair is the named refusal, as for `slice`.
    assert!(attempt("return \"\u{1f600}\".substring(0, 1);").is_err());
}

// =========================================================================
// What must NOT change, whichever variant wins
// =========================================================================

/// A member this engine has no answer for must not quietly become a usable
/// value. The two receivers answer differently **on purpose**, and the
/// difference is the thing a method mechanism is most likely to flatten, so
/// it is pinned per receiver rather than as one rule.
///
/// * **String**: the *read* traps.
///   `plan/design-string-length-milestone.md` §1 -- `"a".toUpperCase` is a
///   real function in ECMA-262, so `undefined` would be a wrong answer
///   wearing a right answer's clothes.
/// * **Array**: the read is `undefined` and the *call* traps.
///   `plan/design-array-milestone.md` -- an absent index really is absent, and
///   the record has no key space to distinguish "absent" from "a method we
///   have not built".
///
/// This test was wrong on its first writing: it asserted the String rule for
/// both, and `a.filter` correctly answered `undefined`. Recorded rather than
/// quietly fixed, because "the two receivers differ" is exactly the fact the
/// experiment must not lose.
#[test]
fn an_unknown_member_is_still_refused_the_way_its_receiver_refuses_it() {
    // A String: reading is already the fault.
    for source in [
        "return \"abc\".toUpperCase;",
        "return \"abc\".normalize;",
        "return \"abc\".toUpperCase();",
        "return \"abc\".normalize();",
        "return (1).toFixed;",
        "return (1).toFixed();",
    ] {
        assert!(attempt(source).is_err(), "{source:?} must still trap");
    }
    // An Array: reading is `undefined`, calling is the fault. (`join` sat
    // in this list until 2026-08-31, when it became a method; `splice`
    // took its seat.)
    undefined("let a = [1]; return a.filter;");
    undefined("let a = [1]; return a.splice;");
    undefined("let a = [1]; return a.forEach;");
    for source in [
        "let a = [1]; return a.filter(x => x);",
        "let a = [1]; return a.splice(0);",
    ] {
        assert!(attempt(source).is_err(), "{source:?} must still trap");
    }
}

/// `.length` did not move. It is a value, not a method, and whichever
/// mechanism lands must leave it exactly where `string_length_m3.rs` put it.
#[test]
fn length_is_still_a_value_and_not_a_method() {
    number("return \"abc\".length;", 3.0);
    number("return \"caf\u{e9}\".length;", 4.0);
    number("return [1, 2].length;", 2.0);
    number("const o = { length: 5 }; return o.length;", 5.0);
    // And calling it is still a trap, because it is not a function.
    assert!(attempt("return \"abc\".length();").is_err());
}

/// A method name used as an ordinary object property is still that.
#[test]
fn a_plain_object_property_named_like_a_method_is_untouched() {
    number("const o = { trim: 1 }; return o.trim;", 1.0);
    number("const o = { push: 2 }; return o.push;", 2.0);
    number("const o = { map: 3 }; return o.map;", 3.0);
    // Including when it holds a function the script calls.
    number(
        "const o = { trim: function () { return 4; } }; return o.trim();",
        4.0,
    );
}
