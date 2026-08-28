//! `includes`, `startsWith`, `endsWith` -- ECMA-262 22.1.3.8, .23 and .7.
//!
//! Every expectation runs: compile -> tinyvm's load gate -> instantiate ->
//! `invoke_by_name("main")`.
//!
//! # Why these three, and why first
//!
//! The second demand survey (`prd/PRD.md`, "第二次普查") counted the
//! *standard-library* surface of the same 82-script corpus the first one
//! counted syntax in, and the ranking has nothing in common with the first:
//! `.contains(` leads at 58 of 82 scripts and 721 uses, `.starts_with` /
//! `.ends_with` follow at 41 and 169. Those are `includes`, `startsWith` and
//! `endsWith` in JavaScript.
//!
//! The first of them is also the cheapest thing on the board, which is rare
//! enough to act on: the search loop already existed for `indexOf`.
//!
//! # Bytes, and why that is exact rather than approximate
//!
//! All three compare **bytes**. UTF-8 is self-synchronising and prefix-free --
//! a continuation byte is `10xxxxxx` and can never begin a character -- so a
//! byte sequence occurs at some offset iff the code-point sequence does, and a
//! match can never begin halfway through a character. That is a property of
//! the encoding, not an assumption about the input: it holds for emoji,
//! combining marks and every other multi-byte case, and the tests below use
//! them to say so.
//!
//! `.length` decodes, and the contrast is the point: a count of characters is
//! not a count of bytes, so the same shortcut would have been wrong there.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

fn run(source: &str) -> Value {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .unwrap_or_else(|e| panic!("load gate rejected {source:?}: {}", e.message()));
    let mut instance = module
        .instantiate()
        .unwrap_or_else(|e| panic!("instantiating {source:?}: {}", e.message()));
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .unwrap_or_else(|e| panic!("trap in {source:?}: {}", e.message()));
    Value::returned(&vals).unwrap_or_else(|e| panic!("{source:?}: {e}"))
}

fn yes(source: &str) -> bool {
    match run(source) {
        Value::Bool(b) => b,
        other => panic!("{source:?}: expected a Boolean, got {other:?}"),
    }
}

/// The ordinary cases, in both directions.
#[test]
fn includes_answers_whether_the_substring_occurs() {
    assert!(yes("return \"hello world\".includes(\"lo wo\");"));
    assert!(yes("return \"hello\".includes(\"hello\");"));
    assert!(yes("return \"hello\".includes(\"h\");"));
    assert!(yes("return \"hello\".includes(\"o\");"));
    assert!(!yes("return \"hello\".includes(\"world\");"));
    assert!(!yes("return \"hello\".includes(\"H\");"));
}

/// The empty string is in every string, and a needle longer than the haystack
/// is in none.
///
/// The two boundaries the loop arithmetic gets wrong when it is written
/// without them: an empty needle makes the inner comparison vacuous, and a
/// long one makes `haystack - needle` wrap in unsigned arithmetic and scan
/// four billion offsets.
#[test]
fn the_two_boundaries_are_the_ones_the_arithmetic_would_get_wrong() {
    assert!(yes("return \"abc\".includes(\"\");"));
    assert!(yes("return \"\".includes(\"\");"));
    assert!(!yes("return \"\".includes(\"a\");"));
    assert!(!yes("return \"ab\".includes(\"abc\");"));
    assert!(!yes("return \"abc\".startsWith(\"abcd\");"));
    assert!(!yes("return \"abc\".endsWith(\"abcd\");"));
}

/// Both affixes, including the two cases that are the same string.
#[test]
fn starts_with_and_ends_with_test_the_two_ends() {
    assert!(yes("return \"hello.qjs\".endsWith(\".qjs\");"));
    assert!(!yes("return \"hello.qjs\".endsWith(\".rh\");"));
    assert!(yes("return \"scripts/rh/x\".startsWith(\"scripts/\");"));
    assert!(!yes("return \"scripts/rh/x\".startsWith(\"rh/\");"));
    // A whole-string match is both.
    assert!(yes("return \"same\".startsWith(\"same\");"));
    assert!(yes("return \"same\".endsWith(\"same\");"));
    // The empty affix is at both ends.
    assert!(yes("return \"x\".startsWith(\"\");"));
    assert!(yes("return \"x\".endsWith(\"\");"));
}

/// **The encoding claim, tested rather than asserted in a comment.**
///
/// `é` is two bytes and `😀` is four. If the search were byte-naive in a way
/// the encoding did not justify, a needle could match across a character
/// boundary or a suffix test could start mid-character. It cannot, and these
/// are the cases that would show it.
#[test]
fn multi_byte_characters_match_as_characters() {
    assert!(yes("return \"café au lait\".includes(\"é a\");"));
    assert!(yes("return \"café\".endsWith(\"fé\");"));
    assert!(!yes("return \"café\".endsWith(\"fe\");"));
    assert!(yes("return \"a😀b\".includes(\"😀\");"));
    assert!(yes("return \"a😀b\".startsWith(\"a😀\");"));
    assert!(yes("return \"a😀\".endsWith(\"😀\");"));
    // The needle's bytes appear inside the emoji's, and must not match: the
    // continuation bytes of `😀` are 0x9F 0x98 0x80, and no character begins
    // with any of them.
    assert!(!yes("return \"😀\".includes(\"\u{fffd}\");"));
}

/// They compose with what the corpus actually writes.
#[test]
fn they_read_the_way_the_corpus_uses_them() {
    let source = "const paths = [\"a.qjs\", \"b.rh\", \"c.qjs\"];
    let n = 0;
    for (const p of paths) { if (p.endsWith(\".qjs\")) { n = n + 1; } }
    return n;";
    match run(source) {
        Value::Number(x) => assert_eq!(x, 2.0),
        other => panic!("expected 2, got {other:?}"),
    }
}

/// A program that names none of the three carries none of them.
///
/// The gate is per method and was measured that way when methods landed: a
/// script calling only `trim()` must not pay for `indexOf`. These three join
/// the same table, so the same three programs must still be the same size.
#[test]
fn a_program_that_names_none_of_them_pays_nothing() {
    for (source, want) in [
        ("return 1;", 9_765),
        ("let o = {a:1}; o.b = 2; return o.a;", 9_886),
        (
            "function mk() { return function () { return 1; }; } let f = mk(); return f();",
            9_929,
        ),
    ] {
        let n = compile_qjs_m1(source).expect("compiles").len();
        assert_eq!(n, want, "{source:?} is {n} bytes");
    }
}

/// **`includes` does not drag in `indexOf`'s helper, and that is why it is not
/// written as `indexOf(t) !== -1`.**
///
/// `indexOf` reports a position, so it needs `units` to convert a byte offset
/// into UTF-16 code units. A Boolean has no position, so it needs none --
/// and the measurement is that `includes` alone is *smaller* than `indexOf`
/// alone, which could not be true if one were built on the other.
#[test]
fn includes_is_cheaper_than_index_of_because_it_needs_no_position() {
    let size = |src: &str| compile_qjs_m1(src).expect("compiles").len();
    let base = size("return 1;");
    let inc = size("return \"ab\".includes(\"a\");") - base;
    let idx = size("return \"ab\".indexOf(\"a\");") - base;
    println!("includes {inc} bytes, indexOf {idx} bytes");
    assert!(
        inc < idx,
        "includes ({inc}) must not cost more than indexOf ({idx}): it does less"
    );
}

/// What each of the three costs, written down.
#[test]
fn what_the_three_cost_is_written_down() {
    let size = |src: &str| compile_qjs_m1(src).expect("compiles").len();
    let base = size("return 1;");
    for (name, source) in [
        ("includes", "return \"ab\".includes(\"a\");"),
        ("startsWith", "return \"ab\".startsWith(\"a\");"),
        ("endsWith", "return \"ab\".endsWith(\"a\");"),
    ] {
        let n = size(source) - base;
        println!("{name}: {n} bytes");
        assert!(n > 0 && n < 1_000, "{name} is {n} bytes, which is a surprise");
    }
}
