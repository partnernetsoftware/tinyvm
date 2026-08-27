//! Criterion ① of the method-binding experiment: the four methods run and the
//! answers are right.
//!
//! **This file is variant-independent on purpose.** It is written before any
//! variant exists so that no variant's convenience can shape what "correct"
//! means, and all three must pass it unchanged. A variant that needs this file
//! edited has failed criterion ①; editing it is the finding, not the fix.
//! See `plan/design-method-binding-experiment.md` §2 and §3.
//!
//! The whole file compiles to nothing unless exactly one variant feature is
//! on, which is also how it stays out of the default build's way.
#![cfg(any(
    feature = "method-this",
    feature = "method-bound",
    feature = "method-callsite"
))]

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
    number("let a = []; a.push(1); a.push(2); a.push(3); return a.length;", 3.0);
    number("let a = []; a.push(1); a.push(2); a.push(3); return a[1];", 2.0);
}

/// `a.map(f)` -- Array receiver whose argument is a **function value the
/// runtime has to call back into**. The most expensive square, and the one
/// that says the most about a mechanism.
///
/// ECMA-262 23.1.3.20: a new array, same length, `f` applied to each element.
#[test]
fn map_calls_back_into_a_function_value() {
    number("let a = [1, 2, 3]; return a.map(function (x) { return x + 1; })[0];", 2.0);
    number("let a = [1, 2, 3]; return a.map(function (x) { return x + 1; })[2];", 4.0);
    number("let a = [1, 2, 3]; return a.map(function (x) { return x + 1; }).length;", 3.0);
    // An arrow, which is the way anyone will actually write it.
    number("let a = [1, 2, 3]; return a.map(x => x * 2)[1];", 4.0);
    // Empty array: the callback is never called and the result is empty.
    number("let a = []; return a.map(x => x + 1).length;", 0.0);
    // A named function value, passed rather than written inline.
    number("function inc(x) { return x + 1; } let a = [5]; return a.map(inc)[0];", 6.0);
    // The callback closes over an outer binding -- closures and methods have
    // to compose, and this is where a mechanism that rebuilds environments
    // would go wrong.
    number("let k = 10; let a = [1, 2]; return a.map(x => x + k)[1];", 12.0);
    // `map` does not mutate the receiver.
    number("let a = [1, 2]; a.map(x => x + 1); return a[0];", 1.0);
    // The result is a real Array: it indexes, it has `length`, and it can be
    // pushed to.
    number("let a = [1]; let b = a.map(x => x); b.push(2); return b.length;", 2.0);
    // Chained, which is the shape that makes methods worth having at all.
    number("let a = [1, 2, 3]; return a.map(x => x + 1).map(x => x * 2)[0];", 4.0);
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
    // An Array: reading is `undefined`, calling is the fault.
    undefined("let a = [1]; return a.filter;");
    undefined("let a = [1]; return a.join;");
    undefined("let a = [1]; return a.forEach;");
    for source in [
        "let a = [1]; return a.filter(x => x);",
        "let a = [1]; return a.join(\",\");",
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
    number("const o = { trim: function () { return 4; } }; return o.trim();", 4.0);
}
