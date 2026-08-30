//! `parseInt(s[, radix])` (ECMA-262 19.2.5) and the two `Number` type
//! tests (21.1.2.3 / .4), landed 2026-08-31 as the third batch of the
//! A13 count. `parseInt` stayed refused-by-name for a year because the
//! corpus's `parse_int` was strict; the demand for real prefix parsing
//! arrived with rh_compat.qjs (6 call lines in 4 downstream files), whose
//! hand-written `parse_int` deletes digit characters with `replaceAll`
//! to validate and then calls `Number` -- a shape only an engine without
//! character access could love.

use tinyvm::{Limits, Val, WasmInstance, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

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
fn number(source: &str, want: f64) {
    let (_, vals) = attempt(source).unwrap_or_else(|e| panic!("{e}"));
    let got = Value::returned(&vals).unwrap_or_else(|e| panic!("{source:?}: {e}"));
    match got {
        Value::Number(x)
            if (x == want && x.is_sign_negative() == want.is_sign_negative())
                || (x.is_nan() && want.is_nan()) => {}
        other => panic!("{source:?}: wanted {want}, got {other:?}"),
    }
}

#[track_caller]
fn boolean(source: &str, want: bool) {
    let (_, vals) = attempt(source).unwrap_or_else(|e| panic!("{e}"));
    let got = Value::returned(&vals).unwrap_or_else(|e| panic!("{source:?}: {e}"));
    assert_eq!(got, Value::Bool(want), "{source:?}");
}

/// The whole of 19.2.5: whitespace, one sign, the `0x` prefix with radix
/// 16 or undefined, digits until the first that is not one, NaN when
/// there were none, and a radix outside 2..=36 as NaN.
#[test]
fn parse_int_reads_a_prefix_in_the_given_radix() {
    for (source, want) in [
        (r#"return parseInt("42");"#, 42.0),
        (r#"return parseInt("42abc");"#, 42.0),
        (r#"return parseInt("  42");"#, 42.0),
        (r#"return parseInt("\t\n 7q");"#, 7.0),
        (r#"return parseInt("-42");"#, -42.0),
        (r#"return parseInt("+42");"#, 42.0),
        (r#"return parseInt("");"#, f64::NAN),
        (r#"return parseInt("abc");"#, f64::NAN),
        (r#"return parseInt("   ");"#, f64::NAN),
        (r#"return parseInt("-");"#, f64::NAN),
        // 19.2.5 step 10: `0x` / `0X` when the radix is 16 or unwritten.
        (r#"return parseInt("0x1f");"#, 31.0),
        (r#"return parseInt("0X1F");"#, 31.0),
        (r#"return parseInt("-0x10");"#, -16.0),
        (r#"return parseInt("0x");"#, f64::NAN),
        (r#"return parseInt("0x10", 16);"#, 16.0),
        (r#"return parseInt("1f", 16);"#, 31.0),
        // Not octal: step 9 fixed radix 10 in 1997 and `"08"` is eight.
        (r#"return parseInt("08");"#, 8.0),
        // The radix goes through ToInt32: a fraction truncates, a string
        // converts, and outside 2..=36 the answer is NaN (step 8).
        (r#"return parseInt("10", 2);"#, 2.0),
        (r#"return parseInt("z", 36);"#, 35.0),
        (r#"return parseInt("10", 16.5);"#, 16.0),
        (r#"return parseInt("10", "2");"#, 2.0),
        (r#"return parseInt("10", 37);"#, f64::NAN),
        (r#"return parseInt("10", 1);"#, f64::NAN),
        (r#"return parseInt("10", -1);"#, f64::NAN),
        // A fraction stops at the point; a Number receiver is ToString'd.
        (r#"return parseInt("3.9");"#, 3.0),
        ("return parseInt(42.7);", 42.0),
        ("return parseInt(true);", f64::NAN),
        // `-0` keeps its sign (step 12).
        (r#"return 1 / parseInt("-0");"#, f64::NEG_INFINITY),
        // Past 2^53 the accumulation rounds per digit; this value answers
        // exactly what V8 answers.
        (
            r#"return parseInt("99999999999999999999");"#,
            1.0000000000000002e20,
        ),
    ] {
        number(source, want);
    }
}

/// 21.1.2.3 and 21.1.2.4 are type tests, not conversions: a String is
/// false however numeric it looks, which is the whole reason they are
/// not folds to `+x`.
#[test]
fn the_number_type_tests_do_not_convert() {
    boolean("return Number.isInteger(5);", true);
    boolean("return Number.isInteger(5.5);", false);
    boolean("return Number.isInteger(\"5\");", false);
    boolean("return Number.isInteger(true);", false);
    boolean("return Number.isInteger(0/0);", false);
    boolean("return Number.isInteger(1/0);", false);
    boolean("return Number.isInteger(-0);", true);
    boolean("return Number.isInteger(9007199254740993);", true);
    boolean("return Number.isInteger(null);", false);
    boolean("return Number.isNaN(0/0);", true);
    boolean("return Number.isNaN(5);", false);
    boolean("return Number.isNaN(\"abc\");", false);
    boolean("return Number.isNaN(undefined);", false);
}

/// The escape hatches and the refusals: a declared `parseInt` wins, and
/// the wrong arity names itself.
#[test]
fn a_declared_parse_int_wins_and_wrong_arity_names_itself() {
    number("let parseInt = 5; return parseInt;", 5.0);
    number(
        "function parseInt(s) { return 7; } return parseInt(\"42\");",
        7.0,
    );
    let error = compile_qjs_m1("return parseInt();").expect_err("zero arguments");
    assert!(
        error
            .message
            .contains("`parseInt` called with 0 arguments; it takes one or two"),
        "{}",
        error.message
    );
    let error = compile_qjs_m1("return parseInt(\"1\", 2, 3);").expect_err("three arguments");
    assert!(
        error.message.contains("called with 3 arguments"),
        "{}",
        error.message
    );
}

// ---- the gate -----------------------------------------------------------

fn bytes(source: &str) -> usize {
    compile_qjs_m1(source)
        .unwrap_or_else(|e| panic!("compiling {source:?}: {e}"))
        .len()
}

#[test]
fn a_program_that_never_parses_pays_nothing() {
    let rows = [
        ("return 1;", 10_198),
        ("return 6 + 3;", 10_214),
        // A declared `parseInt` is the script's own binding, not the gate.
        ("let parseInt = 5; return parseInt;", 10_217),
    ];
    let got: Vec<usize> = rows.iter().map(|(source, _)| bytes(source)).collect();
    for ((source, _), n) in rows.iter().zip(&got) {
        println!("{source:?} is {n} bytes");
    }
    for ((source, want), n) in rows.iter().zip(&got) {
        assert_eq!(n, want, "{source:?} is {n} bytes");
    }
}

#[test]
fn each_addition_has_a_published_price() {
    let base = bytes("return 6 + 3;");
    let rows = [
        (r#"return parseInt("42");"#, 1_073),
        (r#"return parseInt("42", 16);"#, 1_082),
        ("return Number.isInteger(6);", 287),
        ("return Number.isNaN(6);", 265),
    ];
    let got: Vec<usize> = rows
        .iter()
        .map(|(source, _)| bytes(source) - base)
        .collect();
    for ((source, _), n) in rows.iter().zip(&got) {
        println!("{source:?} costs {n} bytes over `return 6 + 3;`");
    }
    for ((source, want), n) in rows.iter().zip(&got) {
        assert_eq!(n, want, "{source:?} costs {n} bytes");
    }
}

/// The demand's comparison: rh_compat.qjs's `parse_int` -- validate by
/// deleting every digit with `replaceAll`, then `Number` -- against the
/// engine's prefix parse.
#[test]
fn the_engine_beats_the_hand_written_parse_int() {
    let by_hand = steps(
        r#"
        function parse_int(text) {
          const DIGITS = ["0","1","2","3","4","5","6","7","8","9"];
          const trimmed = ("" + text).trim();
          let rest = trimmed;
          if (rest.startsWith("-")) { rest = rest.replace("-", ""); }
          const body = rest;
          for (const digit of DIGITS) { rest = rest.replaceAll(digit, ""); }
          if (body === "" || rest !== "") { throw "not an int"; }
          return Number(trimmed);
        }
        let n = 0;
        for (let i = 0; i < 200; i = i + 1) { n = n + parse_int("123456"); }
        return n;"#,
    );
    let by_engine = steps(
        r#"let n = 0;
        for (let i = 0; i < 200; i = i + 1) { n = n + parseInt("123456"); }
        return n;"#,
    );
    println!("200 hand-written parse_int: {by_hand} steps; parseInt: {by_engine} steps");
    assert!(
        by_engine * 3 < by_hand,
        "parseInt should be at least 3x cheaper: {by_hand} vs {by_engine}"
    );
}

fn steps(source: &str) -> u64 {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(
        &wasm,
        Limits {
            max_steps: 4_000_000_000,
            ..Limits::default()
        },
    )
    .expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    instance
        .invoke_by_name("main", &Value::args(&[]))
        .unwrap_or_else(|e| panic!("trap in {source:?}: {}", e.message()));
    instance.last_steps()
}
