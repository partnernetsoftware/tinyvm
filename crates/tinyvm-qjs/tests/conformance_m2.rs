//! The conformance corpus for everything M1/M2 claims to support.
//!
//! Every expectation in this file is derived from ECMA-262, not from what the
//! implementation happens to do. A test that says `number("3 > 2 > 1", ...)`
//! got its answer by running 13.10 over 7.2.13 by hand, and if the engine
//! disagrees the engine is wrong. That is the difference between this file and
//! a golden-output suite: a golden suite ratifies whatever came out last time,
//! and this one does not.
//!
//! Where the current subset genuinely diverges from JavaScript, the divergence
//! is asserted *as it is* and marked `DIVERGENCE:` with the milestone that
//! retires it. A divergence that is written down is a debt; one that is only
//! implied by an absent test is a surprise.
//!
//! Everything runs for real: `compile_qjs_m1` -> tinyvm's load gate ->
//! instantiate -> `invoke_by_name("main", ...)`. "It compiled" is not evidence
//! of a value and never appears here on its own. The one exception is the
//! rejection corpus at the bottom, where not compiling *is* the claim.

use std::cell::RefCell;

use tinyvm::{Limits, WasmInstance, WasmModule};
use tinyvm_qjs::{
    Boundary, CompileError, Names, Options, Value, compile_qjs_m1, compile_qjs_m1_with,
};

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

thread_local! {
    /// Every value the host import `js.note` was handed, in call order. A
    /// `#[test]` gets its own thread, so this is per-test state without a lock.
    static LOG: RefCell<Vec<f64>> = const { RefCell::new(Vec::new()) };
}

/// Compile, load, instantiate, call. `note` binds the observable side effect
/// the short-circuit and evaluation-order tests watch.
fn attempt(source: &str, args: &[Value], names: Names, note: bool) -> Result<Out, String> {
    let wasm = compile_qjs_m1_with(source, Options { names })
        .map_err(|e| format!("compiling {source:?}: {e}"))?;
    let mut module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .map_err(|e| format!("load gate rejected {source:?}: {}", e.message()))?;
    if note {
        module
            .bind_import_typed("js", "note", |args, _memory| {
                let seen = Value::returned(args).expect("a V1 pair");
                LOG.with(|log| {
                    log.borrow_mut().push(match seen {
                        Value::Number(x) => x,
                        // Only Numbers are logged; anything else is a test bug
                        // and shows up as a NaN nobody asked for.
                        _ => f64::NAN,
                    })
                });
                // Identity, so `note(x)` can stand anywhere `x` could.
                Ok(args.to_vec())
            })
            .map_err(|e| format!("binding js.note: {}", e.message()))?;
    }
    let mut instance = module
        .instantiate()
        .map_err(|e| format!("instantiating {source:?}: {}", e.message()))?;
    let vals = instance
        .invoke_by_name("main", &Value::args(args))
        .map_err(|e| format!("trap in {source:?}: {}", e.message()))?;
    Ok(match Value::returned(&vals)? {
        Value::Undefined => Out::Undefined,
        Value::Null => Out::Null,
        Value::Number(x) => Out::Number(x),
        Value::Bool(b) => Out::Bool(b),
        Value::String(ptr) => Out::Str(read_string(&instance, ptr)?),
    })
}

/// A string record in guest memory: `[len: i32][utf8 bytes]`.
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
fn run(source: &str) -> Out {
    attempt(source, &[], Names::Unbound, false).unwrap_or_else(|e| panic!("{e}"))
}

/// Run with `js.note` bound, and hand back what it saw.
#[track_caller]
fn logged(source: &str) -> (Out, Vec<f64>) {
    LOG.with(|log| log.borrow_mut().clear());
    let out = attempt(source, &[], Names::HostImport, true).unwrap_or_else(|e| panic!("{e}"));
    (out, LOG.with(|log| log.borrow().clone()))
}

/// A Number, compared *exactly*: by bits, so `-0` is not `0`, and by
/// `is_nan`, because no NaN equals itself.
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

/// The diagnostic a source outside the subset produces.
#[track_caller]
fn refuse(source: &str) -> CompileError {
    refuse_in(source, Names::Unbound)
}

#[track_caller]
fn refuse_in(source: &str, names: Names) -> CompileError {
    match compile_qjs_m1_with(source, Options { names }) {
        Ok(bytes) => panic!(
            "{source:?} compiled to {} bytes; expected a capability diagnostic",
            bytes.len()
        ),
        Err(e) => e,
    }
}

/// A source that compiles and then traps. The message is the harness's, not
/// the engine's: what matters is that no value came back.
#[track_caller]
fn traps(source: &str) {
    match attempt(source, &[], Names::Unbound, false) {
        Err(message) => assert!(
            message.contains("trap in"),
            "{source:?} failed for the wrong reason: {message}"
        ),
        Ok(value) => panic!("{source:?} produced {value:?} instead of trapping"),
    }
}

// =========================================================================
// Numbers: the boundaries of the one numeric type there is
// =========================================================================

/// ECMA-262 6.1.6.1: the Number type *is* IEEE-754 binary64. Not "an integer
/// until it overflows": every one of these is a double from the first token.
#[test]
fn a_number_is_a_double_end_to_end() {
    number("1 / 2", 0.5);
    number("1 / 8", 0.125);
    number("7 / 2", 3.5);
    // The classic decimal-in-binary result, reachable without a fractional
    // literal: 0.1 + 0.2 is not 0.3.
    boolean("1 / 10 + 2 / 10 === 3 / 10", false);
    number("1 / 10 + 2 / 10", 0.1f64 + 0.2f64);
    // An i32 would have wrapped here; a double does not.
    number("2147483647 + 1", 2147483648.0);
    number("2147483647 * 2147483647", 2147483647.0f64 * 2147483647.0f64);
}

/// 6.1.6.1: integers above 2^53 are not all representable, and the engine has
/// to lose the same ones the spec does -- no more and no fewer.
#[test]
fn integer_precision_ends_where_ieee_754_says_it_does() {
    // 2^53, spelled without a literal wider than an i32.
    let two_53 = "(2147483647 + 1) * 4194304";
    number(two_53, 9007199254740992.0);
    boolean(&format!("{two_53} + 1 === {two_53}"), true);
    boolean(&format!("{two_53} + 2 === {two_53}"), false);
    boolean(&format!("{two_53} - 1 === {two_53}"), false);
}

/// 6.1.6.1.5 (Number::divide) and 6.1.6.1.7 (subtract) produce the infinities
/// and the NaN, and 7.2.15 makes NaN unequal to itself.
#[test]
fn the_non_finite_numbers_are_reachable_and_behave() {
    number("1 / 0", f64::INFINITY);
    number("-1 / 0", f64::NEG_INFINITY);
    number("0 / 0", f64::NAN);
    number("1 / 0 - 1 / 0", f64::NAN);
    number("1 / 0 + 1 / 0", f64::INFINITY);
    boolean("0 / 0 === 0 / 0", false);
    boolean("0 / 0 !== 0 / 0", true);
    boolean("1 / 0 > 2147483647", true);
    boolean("-1 / 0 < -2147483648", true);
    // NaN is falsy and fails every relational comparison (7.2.13's undefined
    // result becomes false at 13.10).
    boolean("!(0 / 0)", true);
    boolean("0 / 0 < 1", false);
    boolean("0 / 0 >= 1", false);
}

/// 6.1.6.1: `+0` and `-0` are different Numbers that `===` cannot tell apart.
/// The only observable difference is what dividing by them yields.
#[test]
fn negative_zero_exists_and_is_not_positive_zero() {
    boolean("0 * -1 === 0", true);
    number("0 * -1", -0.0);
    number("1 / (0 * -1)", f64::NEG_INFINITY);
    number("1 / 0", f64::INFINITY);
    // Unary minus is a sign flip (13.5.5), not a subtraction from zero.
    number("-(1 - 1)", -0.0);
    // The literal spelling, which is the one place the sign is easy to lose:
    // the parser folds a leading minus onto the magnitude so that
    // `-2147483648` is representable, and a zero magnitude has no negative
    // `i32` to be folded onto. `-0` is a Number the spec names, and the only
    // way to see it is to divide by it.
    number("-0", -0.0);
    number("1 / -0", f64::NEG_INFINITY);
    boolean("-0 === 0", true);
    boolean("-0 < 0", false);
    // The sign survives storage and a call, not just the expression that made
    // it: a fold that lost it would lose it here too.
    number("let z = -0; return 1 / z;", f64::NEG_INFINITY);
    number(
        "function f() { return -0; } return 1 / f();",
        f64::NEG_INFINITY,
    );
    // And `+0` stays `+0`: the fix must not flip the sign the other way.
    number("0", 0.0);
    number("1 / 0", f64::INFINITY);
    number("-(-0)", 0.0);
}

/// The literal grammar this engine reads is a strict subset of 12.9.3: plain
/// decimal integers that fit an `i32`. Everything else names itself.
///
/// DIVERGENCE: JavaScript has one numeric literal grammar and it covers
/// `1.5`, `1e3`, `0x10` and every integer up to 2^53. Retire the whole of this
/// test's second half when the lexer produces `f64` literals.
#[test]
fn the_integer_literal_range_is_the_signed_32_bit_one() {
    number("2147483647", 2147483647.0);
    number("-2147483648", -2147483648.0);
    number("0", 0.0);
    for (source, phrase) in [
        ("2147483648", "integers outside the signed 32-bit range"),
        ("-2147483649", "integers outside the signed 32-bit range"),
        (
            "99999999999999999999999999",
            "integers outside the signed 32-bit range",
        ),
        ("1.5", "fractional numbers"),
        ("1e3", "numbers with an exponent"),
        ("1n", "BigInt literals"),
        ("0x10", "hexadecimal number literals"),
        ("0o17", "octal number literals"),
        ("0b101", "binary number literals"),
    ] {
        assert_eq!(
            refuse(source).message,
            format!("this engine does not support {phrase} yet"),
            "{source:?}"
        );
    }
}

/// 6.1.6.1.6, Number::remainder. The result takes the sign of the *dividend*,
/// which is what separates it from a modulo: `-5 % 3` is `-2`, not `1`.
#[test]
fn the_remainder_takes_the_sign_of_the_dividend() {
    number("5 % 3", 2.0);
    number("-5 % 3", -2.0);
    number("5 % -3", 2.0);
    number("-5 % -3", -2.0);
    // A zero result keeps the dividend's sign too, which is the only way to
    // see the difference between `6 % 3` and `-6 % 3`.
    number("6 % 3", 0.0);
    number("-6 % 3", -0.0);
    number("1 / (-6 % 3)", f64::NEG_INFINITY);
    // Fractional operands, spelled without a fractional literal.
    number("11 / 2 % 2", 1.5);
    number("-11 / 2 % 2", -1.5);
    number("1 % (3 / 2)", 1.0);
    number("let x = 5; x %= 2; return x;", 1.0);
}

/// The non-finite arms of 6.1.6.1.6, each of which is a named step rather
/// than something the arithmetic falls into.
#[test]
fn the_remainder_has_the_five_special_cases_the_spec_names() {
    // A zero divisor is NaN, not a trap: JavaScript has one numeric type and
    // NaN is in it. (`/` and `%` diverge at M0, where there is no NaN to be.)
    number("5 % 0", f64::NAN);
    number("0 % 0", f64::NAN);
    // An infinite dividend is NaN whatever the divisor is.
    number("1 / 0 % 2", f64::NAN);
    number("-1 / 0 % 2", f64::NAN);
    // An infinite divisor gives back the dividend, sign and all.
    number("5 % (1 / 0)", 5.0);
    number("-5 % (1 / 0)", -5.0);
    number("-5 % (-1 / 0)", -5.0);
    // A zero dividend gives back the dividend, sign and all.
    number("0 % 5", 0.0);
    number("-0 % 5", -0.0);
    number("1 / (-0 % 5)", f64::NEG_INFINITY);
    // NaN on either side.
    number("0 / 0 % 2", f64::NAN);
    number("2 % (0 / 0)", f64::NAN);
    // A dividend smaller in magnitude than the divisor is itself.
    number("2 % 5", 2.0);
    number("-2 % 5", -2.0);
}

/// 6.1.6.1.6's `q` is "an integer ... whose magnitude is as large as possible
/// without exceeding the magnitude of the *true mathematical quotient*". That
/// is an exact statement about real arithmetic, and the obvious transcription
/// `x - trunc(x / y) * y` does not implement it: `x / y` is rounded first, so
/// the subtraction is of the wrong multiple.
///
/// `2147483647 * 2147483647` is 4611686014132420608 as a double. Its true
/// remainder by 1000 is 608; the transcription yields -512, which is not even
/// in range. These three cases are the ones that make `__rem` a real
/// algorithm rather than three instructions.
#[test]
fn the_remainder_is_exact_where_a_rounded_quotient_is_not() {
    let big = 2147483647.0f64 * 2147483647.0f64;
    for divisor in [1000.0f64, 10.0, 5.0, 7.0, 3.0] {
        number(
            &format!("2147483647 * 2147483647 % {divisor}"),
            big % divisor,
        );
    }
    // Spelled out, so a reader can see the numbers the comment claims.
    number("2147483647 * 2147483647 % 1000", 608.0);
    number("2147483647 * 2147483647 % 10", 8.0);
    number("2147483647 * 2147483647 % 5", 3.0);
    // A tiny divisor against a huge dividend: the scaling loop's long path.
    number("2147483647 * 2147483647 % (1 / 8)", big % 0.125);
}

/// `%` sits on the multiplicative rung with `*` and `/`, left associative
/// (13.7), and above `+` (13.8).
#[test]
fn the_remainder_binds_where_the_multiplicative_rung_is() {
    // `(7 % 5) % 3` is 2; `7 % (5 % 3)` is 1.
    number("7 % 5 % 3", 2.0);
    // `(10 % 4) * 3` is 6; `10 % (4 * 3)` is 10.
    number("10 % 4 * 3", 6.0);
    // `1 + (7 % 4)` is 4; `(1 + 7) % 4` is 0.
    number("1 + 7 % 4", 4.0);
    // Unary minus binds tighter than `%`: `(-7) % 4` is -3, `-(7 % 4)` is -3
    // as well, so the readable claim is the divisor side: `7 % -4` is 3.
    number("7 % -4", 3.0);
}

/// 13.5.3, the `typeof` operator: one string per ECMA-262 language type this
/// engine has. `typeof null` is `"object"` because step 3 of the table says
/// so -- it is the spec's answer, not this engine's approximation of one.
#[test]
fn typeof_names_the_language_type() {
    text("typeof 1", "number");
    text("typeof (1 / 0)", "number");
    text("typeof (0 / 0)", "number");
    text("typeof -0", "number");
    text("typeof \"a\"", "string");
    text("typeof \"\"", "string");
    text("typeof true", "boolean");
    text("typeof false", "boolean");
    text("typeof undefined", "undefined");
    text("typeof null", "object");
    // Not only over literals: a binding, an uninitialised binding, a `$N`, and
    // a call all have a type at the moment `typeof` asks.
    text("let x = \"a\"; return typeof x;", "string");
    text("let y; return typeof y;", "undefined");
    text("function f() { return 1; } return typeof f();", "number");
    text("function g() {} return typeof g();", "undefined");
    assert_eq!(
        attempt(
            "return typeof $0;",
            &[Value::Bool(true)],
            Names::Unbound,
            false
        )
        .unwrap(),
        Out::Str("boolean".to_string())
    );
}

/// `typeof` is a UnaryExpression operator (13.5), so it binds tighter than
/// every infix rung -- and the proof is what `typeof 1 + 1` answers. It used
/// to be that the answer was a *trap*, because `"number" + 1` reached the
/// unimplemented ToString; now the answer is `"number1"`, which says the same
/// thing and says it in one piece rather than by the absence of one.
#[test]
fn typeof_binds_where_a_unary_operator_binds() {
    boolean("typeof 1 === \"number\"", true);
    // `(typeof 1) + 1` is `"number" + 1`, which is `"number1"`;
    // `typeof (1 + 1)` would be `"number"`.
    text("typeof 1 + 1", "number1");
    // `(typeof 2) * 3` is `ToNumber("number") * 3`, which is NaN;
    // `typeof (2 * 3)` would be the String `"number"`.
    number("typeof 2 * 3", f64::NAN);
    // It nests, because its own result is a String and Strings have a type.
    text("typeof typeof 1", "string");
    // A non-empty String is truthy, so every `typeof` is.
    boolean("!typeof undefined", false);
    // The operand is an ordinary expression, evaluated first.
    text("let n = 0; return typeof (n = \"s\");", "string");
}

/// DIVERGENCE: in JavaScript `typeof undeclared` is the one place a free name
/// does not throw -- it evaluates to `"undefined"`. This engine has no global
/// object for a name to be absent *from*, so an undeclared name is refused
/// before `typeof` is reached, under every `Names` setting. Retire when the
/// language grows a global scope.
#[test]
fn typeof_does_not_rescue_an_undeclared_name() {
    let e = refuse_in("typeof nope;", Names::Unbound);
    assert!(e.message.starts_with("this engine "), "{}", e.message);
    assert!(e.message.contains("nope"), "{}", e.message);
}

// =========================================================================
// The precedence ladder, rung by rung
// =========================================================================

/// Each pair here is chosen so the two possible parses give *different
/// values*, not merely different trees. A test that both parses satisfy is
/// not a precedence test.
#[test]
fn every_rung_of_the_ladder_binds_tighter_than_the_one_below_it() {
    // update/unary over multiplicative: `!0 * 2` is `(!0) * 2` = 2, where
    // `!(0 * 2)` would be `true`. Different *types*, so this cannot pass by
    // accident.
    number("!0 * 2", 2.0);
    number("- 2 * 3", -6.0);
    number("1 + + 2", 3.0);

    // multiplicative over additive.
    number("2 + 3 * 4", 14.0);
    number("2 * 3 + 4", 10.0);
    number("10 - 6 / 2", 7.0);

    // additive over relational.
    boolean("1 + 2 < 4", true);
    boolean("1 + 2 < 3", false);

    // relational over equality.
    boolean("1 < 2 === true", true);
    boolean("true === 1 < 2", true);

    // equality over `&&`.
    boolean("1 === 1 && 2 === 2", true);
    boolean("1 === 2 && 2 === 2", false);

    // `&&` over `||`: `true || false && false` is `true || (false && false)`.
    boolean("true || false && false", true);
    boolean("(true || false) && false", false);
    boolean("false && false || true", true);

    // `||` over assignment: the whole logical expression is the value stored.
    number("let a = 9; a = 0 || 3; return a;", 3.0);
    number("let a = 9; a = 1 && 0 || 5; return a;", 5.0);

    // Parentheses beat all of it.
    number("(2 + 3) * 4", 20.0);
    boolean("!(0 * 2)", true);
}

/// 13.6-13.15: every binary level is left associative, and only assignment is
/// right associative. Both directions are tested with non-commutative
/// operators, where the wrong grouping is a wrong number.
#[test]
fn associativity_is_left_everywhere_except_assignment() {
    number("1 - 2 - 3", -4.0);
    number("10 - 3 - 4", 3.0);
    number("12 / 6 / 2", 1.0);
    number("100 / 10 / 5", 2.0);
    number("2 - 3 + 4", 3.0);

    // Relational and equality chains do not mean what they look like: each
    // step's Boolean result is the next step's operand (13.10, 13.11).
    boolean("3 > 2 > 1", false); // (3>2) is true; ToNumber(true) is 1; 1 > 1 is false
    boolean("1 < 2 < 3", true); // (1<2) is true; 1 < 3 is true
    boolean("3 > 2 >= 1", true); // 1 >= 1
    boolean("1 == 1 == 1", true); // true == 1 -> 1 == 1
    boolean("2 == 2 == 2", false); // true == 2 -> 1 == 2

    // Assignment is right associative and yields the value assigned (13.15.2).
    number("let a = 0; let b = 0; a = b = 3; return a * 10 + b;", 33.0);
    number(
        "let a = 1; let b = 2; return (a = b = 7) * 100 + a * 10 + b;",
        777.0,
    );
    number("let x = 1; x += 2; x *= 3; return x;", 9.0);
}

/// 13.4: the position of `++` decides the expression's value, never the
/// variable's final value.
#[test]
fn update_operators_bind_tighter_than_every_infix_level() {
    number("let x = 1; let y = -x++; return y * 100 + x;", -98.0);
    number("let x = 2; return x++ * 3;", 6.0);
    number("let x = 2; return ++x * 3;", 9.0);
    number("let x = 5; return x-- - 1;", 4.0);
    number("let x = 1; return x++ + x++;", 3.0);
    // ToNumeric runs first (13.4.4.1 step 3), so the result is a Number even
    // when the operand was not.
    boolean("let b = true; b++; return b === 2;", true);
    number("let n = null; n++; return n;", 1.0);
    number("let u; u++; return u;", f64::NAN);
}

// =========================================================================
// Short circuiting, with a side effect that proves it
// =========================================================================

/// 13.13.1: `&&` and `||` evaluate the right operand *conditionally* and
/// yield an operand, not a Boolean. The value half can be faked by a
/// ToBoolean-and-select lowering; the log cannot.
#[test]
fn a_short_circuit_actually_skips_the_right_operand() {
    let (out, log) = logged("return false && note(1);");
    assert_eq!(out, Out::Bool(false));
    assert!(
        log.is_empty(),
        "the right operand of a false `&&` ran: {log:?}"
    );

    let (out, log) = logged("return true || note(1);");
    assert_eq!(out, Out::Bool(true));
    assert!(
        log.is_empty(),
        "the right operand of a true `||` ran: {log:?}"
    );

    let (out, log) = logged("return true && note(1);");
    assert_eq!(out, Out::Number(1.0));
    assert_eq!(log, vec![1.0]);

    let (out, log) = logged("return false || note(2);");
    assert_eq!(out, Out::Number(2.0));
    assert_eq!(log, vec![2.0]);
}

/// A chain stops at the first operand that settles it, and the whole chain's
/// value is that operand.
#[test]
fn a_chain_stops_at_the_operand_that_settles_it() {
    let (out, log) = logged("return note(1) && note(0) && note(2);");
    assert_eq!(out, Out::Number(0.0), "the falsy operand is the value");
    assert_eq!(log, vec![1.0, 0.0], "the third operand must not run");

    let (out, log) = logged("return note(0) || note(0) || note(3);");
    assert_eq!(out, Out::Number(3.0));
    assert_eq!(log, vec![0.0, 0.0, 3.0]);

    // Falsy values that are not `false`: 7.1.2 ToBoolean over the five types.
    let (out, log) = logged("return note(0) && note(1);");
    assert_eq!(out, Out::Number(0.0));
    assert_eq!(log, vec![0.0]);
    let (_, log) = logged("null && note(1);");
    assert!(log.is_empty());
    let (_, log) = logged("undefined && note(1);");
    assert!(log.is_empty());
    let (_, log) = logged("\"\" && note(1);");
    assert!(log.is_empty());
    // ...and one that looks falsy and is not: a non-empty String, "0" included.
    let (out, log) = logged("return \"0\" && note(1);");
    assert_eq!(out, Out::Number(1.0));
    assert_eq!(log, vec![1.0]);
}

/// Operands are evaluated left to right, before the operator runs (13.15.3
/// and friends). The only way to see the order is a side effect.
#[test]
fn operands_and_arguments_evaluate_left_to_right() {
    let (out, log) = logged("return note(1) + note(2);");
    assert_eq!(out, Out::Number(3.0));
    assert_eq!(log, vec![1.0, 2.0]);

    let (out, log) = logged("return note(1) - note(2) * note(3);");
    assert_eq!(out, Out::Number(-5.0));
    assert_eq!(log, vec![1.0, 2.0, 3.0], "grouping does not reorder");

    let (out, log) = logged("function f(a, b) { return a - b; } return f(note(1), note(2));");
    assert_eq!(out, Out::Number(-1.0));
    assert_eq!(log, vec![1.0, 2.0]);

    // The assignment's right side runs; the target is a name, so there is
    // nothing to evaluate on the left.
    let (out, log) = logged("let x = 0; x = note(4); return x;");
    assert_eq!(out, Out::Number(4.0));
    assert_eq!(log, vec![4.0]);
}

/// A short circuit inside a loop test runs once per iteration, and an `if`
/// evaluates its test exactly once.
#[test]
fn a_condition_is_evaluated_once_per_pass() {
    let (_, log) = logged("let i = 0; while (i < 3 && note(i) >= 0) { i = i + 1; }");
    assert_eq!(log, vec![0.0, 1.0, 2.0]);

    let (_, log) = logged("if (note(1)) { note(2); } else { note(3); }");
    assert_eq!(log, vec![1.0, 2.0]);

    // A `for` header: init once, then test/body/update until the test fails.
    let (_, log) = logged("for (let i = note(0); note(i) < 2; i++) { note(10 + i); }");
    assert_eq!(log, vec![0.0, 0.0, 10.0, 1.0, 11.0, 2.0]);
}

// =========================================================================
// Automatic semicolon insertion (12.10)
// =========================================================================

/// Rule 3, the restricted production after `return`. The trap is that the
/// expression on the next line is *not* returned, and nothing about the source
/// looks wrong.
#[test]
fn a_return_on_its_own_line_returns_undefined() {
    undefined("let x = 1;\nreturn\nx;");
    undefined("return\n1 + 1;");
    // 12.4: a line terminator inside a multi-line comment is a line
    // terminator. This one catches a lexer that only looks at raw newlines.
    undefined("return /*\n*/ 1;");
    // ...and one that is genuinely on the same line still returns its value,
    // multi-line comment or not.
    number("return /* nothing here */ 1;", 1.0);
    number("return 1;", 1.0);
}

/// Rule 3's other half: `LeftHandSideExpression [no LineTerminator here] ++`.
/// Without the inserted `;` this source is `a ++ b`, which parses as nothing
/// at all -- so the value 102 is proof the semicolon went in.
#[test]
fn a_line_break_before_an_update_operator_ends_the_statement() {
    number(
        "let a = 1; let b = 1; let c = a\n++b; return c * 100 + b;",
        102.0,
    );
    // No line break, no insertion: `a++` then `b` is not a statement.
    let e = refuse("let a = 1; let b = 1; let c = a ++b; return c;");
    assert!(e.message.starts_with("this engine "), "{:?}", e.message);
}

/// Rules 1 and 2: a semicolon appears only where the next token *cannot*
/// continue the statement. These are the four places a line-break-eating
/// lexer gets it wrong, and every one of them is legal JavaScript with a
/// specific meaning.
#[test]
fn a_line_break_alone_does_not_end_a_statement() {
    // The expression continues: `1 + 2`, not `1;` and `+2;`.
    number("let x = 1 +\n2; return x;", 3.0);
    number("let x = 1\n+ 2; return x;", 3.0);
    number("let x = 1\n+2; return x;", 3.0);
    // An `=` on the next line still assigns.
    number("let x = 1; x\n= 2; return x;", 2.0);
    // A `<` on the next line is still a comparison.
    boolean("let a = 1;\nlet b = 2;\nreturn a\n< b;", true);
    // Rule 1(b): a `}` ends the statement before it without a written `;`.
    number("function f() { return 1 } return f();", 1.0);
    // Rule 2: so does the end of the source.
    number("let x = 41; x = x + 1; return x", 42.0);
    // Rule 1(a) doing the whole job: a script with no written `;` at all.
    number("let a = 1\nlet b = 2\nlet c = a + b\nreturn c * 10", 30.0);
}

/// The trap ASI is famous for: a `(` on the next line is a *call*, so no
/// semicolon is inserted and `1(2)` is what the engine is asked to compile.
///
/// It used to be provable by the diagnostic. Now that calling a value is a
/// capability, it is provable by the *trap*: the engine compiles `a(2)`,
/// evaluates the argument, tests the callee's tag, and faults -- which is
/// exactly what ECMA-262 says `1(2)` does, and is stronger evidence that no
/// semicolon was inserted than a refusal was.
#[test]
fn a_parenthesis_on_the_next_line_is_a_call_and_not_a_new_statement() {
    let wasm =
        compile_qjs_m1("let a = 1\n(2)\nreturn a;").expect("`a(2)` is a call this engine lowers");
    let module =
        WasmModule::from_bytes_with(&wasm, Limits::default()).expect("clears the load gate");
    let mut instance = module.instantiate().expect("instantiates");
    let outcome = instance.invoke_by_name("main", &Value::args(&[]));
    assert!(
        outcome.is_err(),
        "calling the Number 1 has to fault, not answer: {outcome:?}"
    );
    // And with a semicolon written, the same source is two statements and
    // never calls anything.
    assert_eq!(run("let a = 1;\n(2)\nreturn a;"), Out::Number(1.0));
}

/// 12.10's two overrides: an inserted semicolon may never become an empty
/// statement, and never one of the `for` header's two.
#[test]
fn no_semicolon_is_inserted_where_it_would_change_the_grammar() {
    // A written `;` already ends this; nothing is inserted before it.
    number("let x = 1\n;\nreturn x;", 1.0);
    // The `for` header's semicolons must be written, even across line breaks.
    number(
        "let s = 0;\nfor (let i = 0;\ni < 3;\ni++) {\ns = s + i;\n}\nreturn s;",
        3.0,
    );
}

// =========================================================================
// Scoping
// =========================================================================

/// 14.3.2: a `var` binds in the enclosing *function* scope, so a block does
/// not contain one -- and it exists, holding `undefined`, even on a path that
/// never ran.
#[test]
fn var_hoists_out_of_a_block_and_let_does_not() {
    number("{ var v = 5; } return v;", 5.0);
    number("if (true) { var v = 5; } return v;", 5.0);
    undefined("if (false) { var v = 5; } return v;");
    number("function f() { { var a = 3; } return a; } return f();", 3.0);
    // A `let` in a block is the block's, and the outer name survives it.
    number("let x = 1; { let x = 2; } return x;", 1.0);
    number("let x = 1; { let x = 2; { let x = 3; } } return x;", 1.0);
    number(
        "let x = 1; let r = 0; { let x = 2; r = x; } return r * 10 + x;",
        21.0,
    );
}

/// 14.3.2 again: only a `var` may be redeclared, and only by another `var`.
#[test]
fn a_redeclaration_is_allowed_for_var_and_refused_for_the_rest() {
    number("var v = 1; var v = 2; return v;", 2.0);
    for source in [
        "let a = 1; let a = 2; return a;",
        "let a = 1; const a = 2; return a;",
        "const a = 1; let a = 2; return a;",
        "let a = 1; var a = 2; return a;",
        "function f(p) { let p = 1; return p; } return f(0);",
    ] {
        let e = refuse(source);
        assert!(
            e.message.contains("cannot bind `a` twice")
                || e.message.contains("cannot bind `p` twice"),
            "{source:?} gave {:?}",
            e.message
        );
    }
}

/// 13.15.1: a `const` binding cannot be the target of an assignment, and the
/// engine says which declaration made it one.
#[test]
fn a_const_cannot_be_reassigned() {
    let e = refuse("const c = 1; c = 2; return c;");
    assert_eq!(
        e.message,
        "this engine cannot assign to `c`, which is declared `const` at byte 6"
    );
    for source in [
        "const c = 1; c += 1;",
        "const c = 1; c++;",
        "const c = 1; --c;",
    ] {
        assert!(
            refuse(source).message.contains("declared `const`"),
            "{source:?}"
        );
    }
    // Reading one is fine, and it is the only declaration form that must have
    // an initialiser (14.3.1).
    number("const k = 6; return k * 7;", 42.0);
    assert!(
        refuse("const k; return 1;")
            .message
            .contains("needs a value for the `const` binding `k`")
    );
}

/// A function's own scope: parameters shadow, locals are private, and the
/// script's bindings are visible because they outlive every frame.
#[test]
fn a_function_scope_shadows_the_script_without_disturbing_it() {
    number(
        "let x = 1; function f(x) { return x; } return f(2) * 10 + x;",
        21.0,
    );
    number(
        "let x = 1; function f() { let x = 2; return x; } return f() * 10 + x;",
        21.0,
    );
    number("let g = 10; function f() { return g; } return f();", 10.0);
    number(
        "let g = 10; function f() { g = 20; return 0; } f(); return g;",
        20.0,
    );
    // A parameter is ordinary storage, and writing it does not reach the
    // caller: arguments are passed by value.
    number(
        "function f(x) { x = 9; return x; } let v = 1; return f(v) * 10 + v;",
        91.0,
    );
}

/// A `for` header gets its own scope: the loop variable is not visible after
/// the loop, which the engine reports as an undeclared name.
#[test]
fn a_for_header_binding_does_not_escape_the_loop() {
    let e = refuse("for (let i = 0; i < 2; i++) { } return i;");
    assert!(
        e.message.contains("finds no declaration of `i`"),
        "{:?}",
        e.message
    );
    // A `var` in the header does escape, because it binds in the function
    // scope like every other `var`.
    number("for (var j = 0; j < 3; j++) { } return j;", 3.0);
}

/// 11.2.2: a module is always strict, so a function declared in a block is
/// block scoped and is gone afterwards.
#[test]
fn a_function_declared_in_a_block_is_the_blocks() {
    let e = refuse("if (true) { function g() { return 1; } } return g();");
    assert!(
        e.message.contains("finds no declaration of `g`"),
        "{:?}",
        e.message
    );
    number(
        "if (true) { function g() { return 1; } return g(); } return 0;",
        1.0,
    );
}

/// 8.2.4 / 9.1.1.1 put a `let` or `const` in a temporal dead zone from the top
/// of its scope until its declaration is evaluated, and reading one there is a
/// ReferenceError. This engine has no `throw`, so what it has instead is a
/// refusal -- and a refusal can only speak for the cases the text settles on
/// its own: the read stands in the very function that declares the binding, and
/// it stands before the declarator. Those are refused. A fabricated `undefined`
/// is not offered for any of them.
#[test]
fn a_temporal_dead_zone_read_is_refused() {
    for source in [
        "let y = x; let x = 1; return y;",
        "function f() { let y = x; let x = 1; return y; } return f();",
        // The self-reading initialiser, which is a dead-zone read in every
        // JavaScript there has ever been.
        "let x = x; return x;",
        "const c = c; return c;",
    ] {
        let e = refuse(source);
        assert!(
            e.message.contains("before the declaration"),
            "{source:?}: {}",
            e.message
        );
    }
    // `var` is *not* a dead zone: 14.3.2 really does say `undefined` here.
    undefined("var y = v; var v = 1; return y;");
}

/// DIVERGENCE: the half of the dead zone no compiler can settle from the text.
/// The read is inside another function, so whether it happens before the
/// declarator is a question about what *runs*; here the storage is a zeroed
/// global and `TAG_UNDEFINED` is 0, so the read yields `undefined` where
/// ECMA-262 throws. Retire when exceptions land -- the storage already exists,
/// only the poison value and the throw are missing.
#[test]
fn a_dead_zone_read_from_another_function_yields_undefined() {
    undefined("function f() { return x; } let y = f(); let x = 1; return y;");
}

// =========================================================================
// Control flow
// =========================================================================

/// 14.6: the `else` binds to the nearest `if`. Written without braces, which
/// is the only way the ambiguity exists at all.
#[test]
fn a_dangling_else_binds_to_the_nearest_if() {
    number(
        "if (true) if (false) return 1; else return 2; return 3;",
        2.0,
    );
    number(
        "if (true) if (true) return 1; else return 2; return 3;",
        1.0,
    );
    number(
        "if (false) if (true) return 1; else return 2; return 3;",
        3.0,
    );
    // With braces the outer `else` is reachable again.
    number(
        "if (true) { if (false) { return 1; } } else { return 2; } return 3;",
        3.0,
    );
}

/// A loop body is a Statement, not a Block: an expression statement, an empty
/// statement and an empty block all have to work.
#[test]
fn a_loop_body_may_be_any_statement_including_an_empty_one() {
    number("let i = 0; while (i < 3) i = i + 1; return i;", 3.0);
    number("let i = 0; for (; i < 3; i++) ; return i;", 3.0);
    number("let i = 0; for (; i < 3; i++) { } return i;", 3.0);
    number(
        "let n = 0; for (let i = 0; i < 3; i++) n = n + 1; return n;",
        3.0,
    );
    // An empty body with a `while`: the test has to be what makes progress,
    // because there is no `break` to leave by.
    number("let i = 0; while ((i = i + 1) < 3) { } return i;", 3.0);
    // A `while` whose test is false from the start runs its body zero times.
    number("let n = 0; while (false) { n = 1; } return n;", 0.0);
    number(
        "let n = 0; for (let i = 0; i < 0; i++) { n = 1; } return n;",
        0.0,
    );
}

/// With no `break` in the subset, the only ways out of a `while (true)` are
/// the test and a `return`. Both are load-bearing, so both are tested.
#[test]
fn a_break_less_loop_leaves_through_its_test_or_a_return() {
    number(
        "let i = 0; while (true) { if (i > 2) { return i; } i = i + 1; } return -1;",
        3.0,
    );
    number(
        "let i = 0; for (;;) { i = i + 1; if (i === 4) { return i; } }",
        4.0,
    );
    // The test itself can be the side effect that ends the loop.
    number("let i = 0; while ((i = i + 1) < 4) { } return i;", 4.0);
    number(
        "let n = 0; let i = 0; while ((i = i + 1) < 4) { n = n + i; } return n * 10 + i;",
        64.0,
    );
    number(
        "function f() { while (true) { return 7; } } return f();",
        7.0,
    );
}

/// Nested loops at the same depth: an inner loop's exit must not be read as
/// the outer loop's, which is a wrong *number* and not a validation failure.
#[test]
fn nested_loops_keep_their_own_branch_targets() {
    number(
        "let n = 0; for (let i = 0; i < 3; i++) { for (let j = 0; j < 3; j++) { n = n + 1; } } return n;",
        9.0,
    );
    number(
        "let n = 0; let i = 0; while (i < 3) { let j = 0; while (j < 4) { n = n + 1; j = j + 1; } i = i + 1; } return n;",
        12.0,
    );
    // Three deep, with the answer depending on every level.
    number(
        "let n = 0;
         for (let i = 0; i < 2; i++) {
             for (let j = 0; j < 2; j++) {
                 for (let k = 0; k < 2; k++) { n = n * 2 + i + j + k; }
             }
         }
         return n;",
        // n = n*2 + (i+j+k) over the eight triples in order:
        // 0, 1, 3, 8, 17, 36, 74, 151.
        151.0,
    );
    // A `return` from the innermost loop leaves the function, not the loop.
    number(
        "function find(t) {
             for (let i = 0; i < 5; i++) {
                 for (let j = 0; j < 5; j++) {
                     if (i * 5 + j === t) { return i * 10 + j; }
                 }
             }
             return -1;
         }
         return find(7);",
        12.0,
    );
}

/// 14.1.1 over the `UpdateEmpty` in 14.6.7 and 14.7: a script with no `return`
/// yields its completion value, and the statements that produce *nothing* are
/// the ones a "last expression wins" rule gets wrong.
#[test]
fn the_completion_value_follows_update_empty() {
    number("1 + 1;", 2.0);
    number("1; ;", 1.0);
    number("1; { }", 1.0);
    number("1; let x = 2;", 1.0);
    number("1; { 2; }", 2.0);
    undefined("let x = 1;");
    undefined(";");
    // An `if` or a loop that does not run its body produces `undefined`, not
    // the value before it.
    undefined("1; if (false) { 2; }");
    undefined("1; while (false) { 2; }");
    undefined("1; for (let i = 0; i < 0; i++) { 2; }");
    number("1; if (true) { 2; }", 2.0);
    number("let s = 10; for (let i = 0; i < 3; i++) { s + i; }", 12.0);
}

// =========================================================================
// Functions
// =========================================================================

/// Recursion, mutual recursion, and depth. A per-call frame is the claim; a
/// shared one produces a wrong answer rather than a crash.
#[test]
fn recursion_gets_a_frame_per_call() {
    number(
        "function fib(n) { if (n < 2) { return n; } return fib(n - 1) + fib(n - 2); } return fib(10);",
        55.0,
    );
    number(
        "function fact(n) { if (n <= 1) { return 1; } return n * fact(n - 1); } return fact(10);",
        3628800.0,
    );
    boolean(
        "function even(n) { if (n === 0) { return true; } return odd(n - 1); }
         function odd(n) { if (n === 0) { return false; } return even(n - 1); }
         return even(7);",
        false,
    );
    boolean(
        "function even(n) { if (n === 0) { return true; } return odd(n - 1); }
         function odd(n) { if (n === 0) { return false; } return even(n - 1); }
         return even(8);",
        true,
    );
    // A local in a recursive frame must not be the caller's local.
    number(
        "function f(n) { let acc = 0; if (n > 0) { acc = f(n - 1); } return acc + n; } return f(10);",
        55.0,
    );
    // Deep enough to be a real stack, well inside tinyvm's 512-frame ceiling.
    number(
        "function down(n) { if (n === 0) { return 0; } return down(n - 1) + 1; } return down(200);",
        200.0,
    );
}

/// 10.2.11 and 8.6.3: a JavaScript call is not arity checked. A missing
/// argument is `undefined`; an extra one is evaluated and discarded.
#[test]
fn a_call_is_not_arity_checked() {
    undefined("function f(a, b) { return b; } return f(1);");
    number("function f(a, b) { return a; } return f(1);", 1.0);
    undefined("function f(a) { return a; } return f();");
    number("function f(a) { return a; } return f(1, 2);", 1.0);
    number("function f() { return 5; } return f(1, 2, 3);", 5.0);
    // The dropped argument still runs -- an argument list is not lazy.
    let (out, log) = logged("function f(a) { return a; } return f(note(1), note(2));");
    assert_eq!(out, Out::Number(1.0));
    assert_eq!(
        log,
        vec![1.0, 2.0],
        "the dropped argument must still evaluate"
    );
    // A missing argument is `undefined`, and `undefined` arithmetic is NaN.
    number("function f(a, b) { return a + b; } return f(1);", f64::NAN);
}

/// 14.1.3: a function declaration is hoisted, so a call may precede it -- at
/// script level and inside another function.
#[test]
fn a_function_declaration_is_hoisted() {
    number("return twice(4); function twice(x) { return x * 2; }", 8.0);
    number(
        "function outer() { return inner(); } function inner() { return 3; } return outer();",
        3.0,
    );
    number(
        "function outer() { return inner(1); function inner(x) { return x + 1; } } return outer();",
        2.0,
    );
}

/// Calls nest as arguments, as callees' arguments, and as a whole expression.
#[test]
fn calls_nest_to_any_depth() {
    number(
        "function add(a, b) { return a + b; }
         function mul(a, b) { return a * b; }
         return add(mul(2, 3), add(1, mul(2, 2)));",
        11.0,
    );
    number(
        "function id(x) { return x; } return id(id(id(id(7))));",
        7.0,
    );
    number(
        "function f(a, b, c) { return a * 100 + b * 10 + c; } return f(1, 2, 3);",
        123.0,
    );
    // An immediately invoked function expression, with and without arguments.
    number("return (function (x) { return x + 1; })(41);", 42.0);
    number("return (function () { return 42; })();", 42.0);
}

/// 14.1.1: a function with no `return`, or a bare `return`, yields
/// `undefined`. Falling off the end is not "the last expression".
#[test]
fn a_function_without_a_return_yields_undefined() {
    undefined("function f() { } return f();");
    undefined("function f() { return; } return f();");
    undefined("function f() { 1 + 1; } return f();");
    undefined("function f(n) { if (n > 0) { return 1; } } return f(0);");
    number(
        "function f(n) { if (n > 0) { return 1; } } return f(1);",
        1.0,
    );
}

// =========================================================================
// Strings
// =========================================================================

/// 12.9.4: the escapes the engine reads, and the value each one stands for.
#[test]
fn string_literals_decode_their_escapes() {
    text("\"hi\"", "hi");
    text("''", "");
    text("'single'", "single");
    text("\"a\\nb\"", "a\nb");
    text("\"a\\tb\"", "a\tb");
    text("\"a\\\\b\"", "a\\b");
    text("\"a\\\"b\"", "a\"b");
    text("'a\\'b'", "a'b");
    text("\"\\x41\"", "A");
    text("\"\\u0041\"", "A");
    text("\"\\u{1F600}\"", "\u{1F600}");
    text("\"\\0\"", "\0");
    // A surrogate pair spelled as two `\u` escapes is one character.
    text("\"\\ud83d\\ude00\"", "\u{1F600}");
    // A LineContinuation: the backslash and the terminator both vanish.
    text("\"a\\\nb\"", "ab");
    // Non-ASCII source text passes through unchanged.
    text("\"héllo\"", "héllo");
    text("\"日本\"", "日本");
}

/// 13.15.3: `+` concatenates when either side is a String. The result is a
/// fresh allocation and stays readable.
#[test]
fn strings_concatenate_and_compare_by_content() {
    text("\"a\" + \"b\"", "ab");
    text("\"a\" + \"b\" + \"c\"", "abc");
    text("let s = \"x\"; s += \"y\"; return s;", "xy");
    text(
        "let s = \"\"; for (let i = 0; i < 4; i++) { s = s + \"ab\"; } return s;",
        "abababab",
    );
    // 7.2.15: Strings are equal by content, so a built one equals a literal.
    boolean("let s = \"\"; s = s + \"ab\"; return s === \"ab\";", true);
    boolean("\"ab\" === \"a\" + \"b\"", true);
    boolean("\"q\" === \"q\"", true);
    boolean("\"abc\" === \"abd\"", false);
    boolean("\"abc\" !== \"abd\"", true);
    boolean("\"\" === \"\"", true);
    boolean("\"a\" == \"a\"", true);
    // Different types are never strictly equal, and that needs no conversion.
    boolean("\"1\" === 1", false);
    boolean("\"\" === 0", false);
    boolean("\"a\" === null", false);
    text(
        "function greet(who) { return \"hello, \" + who; } return greet(\"world\");",
        "hello, world",
    );
}

/// 7.1.2: the empty String is the only falsy one. `"0"` and `" "` are truthy,
/// which is where a ToBoolean written as "looks like a number" goes wrong.
#[test]
fn only_the_empty_string_is_falsy() {
    number("if (\"\") { return 1; } return 2;", 2.0);
    number("if (\"0\") { return 1; } return 2;", 1.0);
    number("if (\" \") { return 1; } return 2;", 1.0);
    number("if (\"false\") { return 1; } return 2;", 1.0);
    boolean("!\"\"", true);
    boolean("!\"a\"", false);
    text("\"\" || \"fallback\"", "fallback");
    text("\"a\" && \"b\"", "b");
}

/// The three ECMA-262 conversions that used to be unimplemented, each
/// answering. ToString of a Number (6.1.6.1.20), StringToNumber (7.1.4.1) and
/// String relational comparison (7.2.13) are now `convert.rs`, wired into
/// `__add`, `__to_number` and the four relational operators.
///
/// This test is the previous milestone's
/// `the_unimplemented_string_conversions_trap_rather_than_guess` with every
/// line turned over: each `traps(...)` became the value JavaScript gives.
#[test]
fn the_three_string_conversions_answer() {
    // 13.15.3: either side a String makes `+` concatenation, and both sides
    // then run ToString.
    text("return \"a\" + 1;", "a1");
    text("return 1 + \"a\";", "1a");
    text("return \"a\" + true;", "atrue");
    text("return \"a\" + null;", "anull");
    text("return \"a\" + undefined;", "aundefined");
    // 7.1.4.1, reached through `__to_number`.
    number("return \"1\" - 1;", 0.0);
    number("return \"2\" * 2;", 4.0);
    number("return -\"1\";", -1.0);
    // 7.2.13 step 3, both sides Strings.
    boolean("\"a\" < \"b\"", true);
    boolean("\"a\" > \"b\"", false);
    // 7.2.14 steps 4 and 5.
    boolean("1 == \"1\"", true);
    boolean("\"1\" == 1", true);
    // 7.2.13 step 4: one String and one Number is *not* the code-unit path.
    boolean("\"a\" < 1", false);
    // What still does not convert: strict equality never does, and `==`
    // between a String and null/undefined is settled by 7.2.14 steps 2 and 3
    // before any conversion is reached.
    boolean("\"1\" === 1", false);
    boolean("\"\" == null", false);
    boolean("\"\" == undefined", false);
    // And the conversion that is still missing, which is a fourth one and not
    // one of these three: 7.1.1 ToPrimitive reaches the `valueOf`/`toString` a
    // prototype would carry, and there is no prototype.
    traps("const o = {}; return \"x\" + o;");
}

// =========================================================================
// Type coercion at the operators that do implement it
// =========================================================================

/// 7.1.4 ToNumber over the four types that have an answer here.
#[test]
fn to_number_follows_the_table() {
    number("+true", 1.0);
    number("+false", 0.0);
    number("+null", 0.0);
    number("+undefined", f64::NAN);
    number("1 - true", 0.0);
    number("null + 1", 1.0);
    number("undefined + 1", f64::NAN);
    number("true + true", 2.0);
    number("null * 5", 0.0);
    number("-true", -1.0);
    number("-null", -0.0);
}

/// 7.1.2 ToBoolean, complete over the five types.
#[test]
fn to_boolean_follows_the_table() {
    for (source, want) in [
        ("0", false),
        ("0 * -1", false),
        ("0 / 0", false),
        ("1", true),
        ("-1", true),
        ("\"\"", false),
        ("\"a\"", true),
        ("true", true),
        ("false", false),
        ("null", false),
        ("undefined", false),
    ] {
        boolean(&format!("!!({source})"), want);
        number(
            &format!("if ({source}) {{ return 1; }} return 0;"),
            if want { 1.0 } else { 0.0 },
        );
    }
}

/// 7.2.14 IsLooselyEqual and 7.2.15 IsStrictlyEqual, on the pairs that do not
/// need StringToNumber.
#[test]
fn equality_follows_ecma262() {
    boolean("null == undefined", true);
    boolean("undefined == null", true);
    boolean("null === undefined", false);
    boolean("null == null", true);
    boolean("undefined == undefined", true);
    boolean("null == 0", false);
    boolean("undefined == 0", false);
    boolean("null == false", false);
    boolean("1 == true", true);
    boolean("0 == false", true);
    boolean("2 == true", false);
    boolean("1 === true", false);
    boolean("0 / 0 == 0 / 0", false);
    // 7.2.15 step 1: different Language Types are never strictly equal.
    boolean("1 === \"1\"", false);
    boolean("null === false", false);
    boolean("undefined === false", false);
    // `!=` and `!==` are exactly the negations (13.11.1).
    boolean("null != undefined", false);
    boolean("null !== undefined", true);
    boolean("1 !== 1", false);
}

/// 13.10 over 7.2.13: relational comparison between Numbers, and through
/// ToNumber for everything else that is not a String.
#[test]
fn relational_comparison_follows_ecma262() {
    boolean("1 < 2", true);
    boolean("2 < 1", false);
    boolean("2 <= 2", true);
    boolean("3 > 4", false);
    boolean("4 >= 4", true);
    boolean("true > false", true);
    boolean("null < 1", true);
    boolean("null >= 0", true);
    boolean("undefined < 1", false); // NaN on one side: false either way
    boolean("undefined > 1", false);
    boolean("-1 / 0 < 1 / 0", true);
}

// =========================================================================
// Values crossing the host boundary
// =========================================================================

/// Every type survives the round trip through the two-word ABI, in both
/// directions.
#[test]
fn every_value_type_crosses_the_call_boundary() {
    number("42", 42.0);
    boolean("true", true);
    boolean("false", false);
    assert_eq!(run("null"), Out::Null);
    undefined("undefined");
    text("\"hi\"", "hi");

    for (value, want) in [
        (Value::Number(1.5), Out::Number(1.5)),
        (Value::Number(f64::INFINITY), Out::Number(f64::INFINITY)),
        (Value::Bool(true), Out::Bool(true)),
        (Value::Bool(false), Out::Bool(false)),
        (Value::Null, Out::Null),
        (Value::Undefined, Out::Undefined),
    ] {
        assert_eq!(
            attempt("$0", &[value], Names::Unbound, false).unwrap(),
            want,
            "argument {value:?}"
        );
    }
    // An argument the script never names is still a parameter the host filled.
    assert_eq!(
        attempt(
            "$1",
            &[Value::Number(1.0), Value::Bool(false)],
            Names::Unbound,
            false
        )
        .unwrap(),
        Out::Bool(false)
    );
    assert_eq!(
        attempt(
            "$0 + $1",
            &[Value::Number(40.0), Value::Number(2.0)],
            Names::Unbound,
            false
        )
        .unwrap(),
        Out::Number(42.0)
    );
}

/// The argument-index ceiling is exactly 64, and the last legal index still
/// compiles, loads and runs -- an off-by-one here is a module the load gate
/// rejects rather than a diagnostic.
#[test]
fn the_argument_index_ceiling_holds_on_both_sides() {
    let mut args = vec![Value::Number(0.0); 64];
    args[63] = Value::Number(7.0);
    assert_eq!(
        attempt("$63", &args, Names::Unbound, false).unwrap(),
        Out::Number(7.0),
        "index 63 is the last one inside the ceiling"
    );
    assert_eq!(
        refuse("$64").message,
        "this engine does not support more than 64 call arguments yet"
    );
}

// =========================================================================
// The rejection corpus: what is out of subset, and what it says
// =========================================================================

/// The lock. Every construct here is real JavaScript that this engine does not
/// lower, and every one of them must be refused with a sentence naming the
/// *engine's* boundary. If a milestone lands one of these, its row moves out
/// of this table and into a behaviour test above -- which is the point: the
/// table is the list product copy is allowed to describe as "not yet".
#[test]
fn every_unsupported_construct_names_its_own_capability() {
    for (source, phrase) in UNSUPPORTED {
        let e = refuse(source);
        assert_eq!(
            e.message,
            format!("this engine does not support {phrase} yet"),
            "{source:?}"
        );
    }
}

/// `(source, the noun phrase the diagnostic must use)`.
const UNSUPPORTED: &[(&str, &str)] = &[
    // -- numeric literal forms ----------------------------------------------
    ("1.5", "fractional numbers"),
    ("1e3", "numbers with an exponent"),
    ("1n", "BigInt literals"),
    ("0x10", "hexadecimal number literals"),
    ("0o17", "octal number literals"),
    ("0b101", "binary number literals"),
    ("2147483648", "integers outside the signed 32-bit range"),
    // -- operators ----------------------------------------------------------
    ("2 ** 3", "exponentiation"),
    ("1 & 2", "bitwise operators"),
    ("1 | 2", "bitwise operators"),
    ("1 ^ 2", "bitwise operators"),
    ("~1", "bitwise operators"),
    ("1 << 2", "bitwise operators"),
    ("1 >> 2", "bitwise operators"),
    ("1 >>> 2", "bitwise operators"),
    ("null ?? 1", "the nullish coalescing operator"),
    ("let x = 1; x ||= 2; return x;", "logical assignment"),
    ("let x = 1; x &&= 2; return x;", "logical assignment"),
    ("let x = 1; x **= 2; return x;", "assignment"),
    ("1, 2;", "the comma operator"),
    // -- things that need a third binding -----------------------------------
    //
    // Object literals and property access left this table when M3 landed
    // them; `tests/objects_m3.rs` is where their behaviour is asserted now.
    // `"ab".length` left it too, and did *not* become a diagnostic: the
    // receiver of a property access is a run-time fact, so a String receiver
    // is refused where its type is known, which is at run time. That trap is
    // `objects_m3::a_property_of_a_non_object_traps`.
    ("let a = [1]; return 0;", "array literals"),
    ("let x = 1; x?.y;", "optional chaining"),
    (
        "function f(a) { return a; } return f(...1);",
        "the spread and rest syntax",
    ),
    // `?:`, `try`/`catch`/`finally` and `throw` left this table when the
    // milestone that lowers them landed; `tests/conditional_and_try.rs` is
    // where their behaviour is asserted now, and the diagnostic a `?` or a
    // `try` in a position the parser cannot use still prints is the phrase
    // the lexer keeps for them.
    // -- syntax whole milestones away ---------------------------------------
    ("let f = (x) => x; return 0;", "arrow functions"),
    ("`t`", "template literals"),
    ("eval(\"1\");", "the `eval` function"),
    ("while (true) { break; }", "the `break` keyword"),
    ("while (true) { continue; }", "the `continue` keyword"),
    ("do { } while (false);", "the `do` keyword"),
    ("switch (1) { }", "the `switch` keyword"),
    ("class C { }", "the `class` keyword"),
    ("new C();", "the `new` keyword"),
    ("this;", "the `this` keyword"),
    ("delete x;", "the `delete` keyword"),
    ("void 0;", "the `void` keyword"),
    ("1 instanceof Object;", "the `instanceof` keyword"),
    ("for (let k in o) { }", "the `in` keyword"),
    ("for (let k of o) { }", "the `of` keyword"),
    ("async function f() { }", "the `async` keyword"),
    ("await 1;", "the `await` keyword"),
    ("yield 1;", "the `yield` keyword"),
    ("import x;", "the `import` keyword"),
    ("export let x = 1;", "the `export` keyword"),
    ("super();", "the `super` keyword"),
    // -- the string forms the value representation cannot hold --------------
    ("\"\\ud800\"", "unpaired surrogates in string literals"),
    ("\"\\012\"", "legacy octal escapes in string literals"),
    // -- the front end's own boundaries -------------------------------------
    ("$64", "more than 64 call arguments"),
    (
        "function f() { return $0; } return f();",
        "an argument reference inside a nested function",
    ),
    (
        "function outer() { let a = 1; function inner() { return a; } return inner(); } return outer();",
        "closures that capture a variable",
    ),
    // -- a character the engine has no lexeme for ---------------------------
    ("#x", "the character `#`"),
];

/// The wording is the product promise: the sentence is about the engine, never
/// about the author, and it never uses the vocabulary of a mistake.
#[test]
fn no_diagnostic_blames_the_script() {
    let structural = [
        "1 2;",
        "(1",
        "let = 1;",
        "let x = ;",
        "function () { }",
        "if (1 { }",
        "let a = 1; let a = 2;",
        "const c = 1; c = 2;",
        "return x;",
        "for (let i = 0; i < 2; i++) { } return i;",
    ];
    for source in UNSUPPORTED.iter().map(|(s, _)| *s).chain(structural) {
        let message = refuse(source).message;
        assert!(
            message.starts_with("this engine "),
            "{source:?} does not speak for the engine: {message:?}"
        );
        for blame in [
            "syntax error",
            "invalid",
            "illegal",
            "unexpected",
            "bad ",
            "wrong",
        ] {
            assert!(
                !message.to_lowercase().contains(blame),
                "{source:?} blames the script with {blame:?}: {message:?}"
            );
        }
    }
}

/// A diagnostic points at the construct it names, so an editor can underline
/// it rather than the whole file.
#[test]
fn a_diagnostic_points_at_the_construct_it_names() {
    assert_eq!(refuse("1 + 0x10").offset, 4);
    assert_eq!(refuse("let x = 1; x ** 2;").offset, 13);
    assert_eq!(refuse("let a = 1;\nlet b = 1.5;").offset, 19);
}

/// The three boundaries are a machine-readable category, and the fmt-free
/// core gets the terse form of the same fact.
#[test]
fn the_boundary_is_classified_not_just_worded() {
    assert_eq!(refuse("2 ** 3").boundary, Boundary::Subset);
    assert_eq!(
        refuse("let a = [1]; return 0;").boundary,
        Boundary::ThirdBinding
    );
    assert_eq!(refuse("this;").boundary, Boundary::FullJs);
    assert_eq!(refuse("class C { }").boundary, Boundary::FullJs);
    assert_eq!(
        refuse("function o() { let a = 1; function i() { return a; } return i(); } return o();")
            .boundary,
        Boundary::FullJs
    );
}

/// Under `Names::Unbound` a free name has nothing to resolve against, and the
/// engine says that rather than inventing a global.
///
/// The sentence used to end "this engine has no global bindings yet", and
/// `JSON` made that false -- the engine binds exactly one name now. Saying it
/// anyway would be the engine disclaiming something it has, which is the
/// defect `a_misplaced_token_says_what_was_wanted_and_never_disclaims_what_the
/// _engine_has` above is about, in a second place. One name is not a scope,
/// and the sentence now says which name it is.
#[test]
fn a_free_name_is_refused_when_there_is_nothing_to_bind_it_to() {
    for source in ["return x;", "x = 1;", "return f();", "console;"] {
        let e = refuse_in(source, Names::Unbound);
        assert!(
            e.message.contains("finds no declaration of ")
                && e.message
                    .contains("`JSON` is the only name this engine binds"),
            "{source:?} gave {:?}",
            e.message
        );
    }
    // And the name it says is the one it means: `JSON` resolves.
    assert!(compile_qjs_m1("return typeof JSON;").is_ok());
}

/// Under `Names::HostImport` a free name *is* a binding -- an import -- and
/// the two things an import cannot be are named as such.
#[test]
fn the_host_table_is_a_table_of_imports_and_says_what_that_rules_out() {
    let e = refuse_in("g = 1;", Names::HostImport);
    assert_eq!(
        e.message,
        "this engine does not support assigning to a host name yet"
    );
    assert_eq!(e.boundary, Boundary::ThirdBinding);

    // A wasm import has exactly one signature.
    let e = refuse_in("g(1); return g(1, 2);", Names::HostImport);
    assert_eq!(
        e.message,
        "this engine does not support calling the host name `g` with two different argument counts yet"
    );
    assert_eq!(e.boundary, Boundary::ThirdBinding);
}

/// Source the engine cannot finish reading is a different category from a
/// capability boundary: it says what it was looking for, and still speaks for
/// the engine.
#[test]
fn unreadable_source_says_what_is_missing() {
    for (source, needle) in [
        ("(1", "needs a `)` to close the group opened at byte 0"),
        ("\"abc", "needs a `\"` to close the string opened at byte 0"),
        (
            "/* open",
            "needs a `*/` to close the comment opened at byte 0",
        ),
        ("1 +", "needs an operand here"),
        // The shape every one of these should have: what was wanted, and what
        // stood there instead. See the divergence test at the end of the file
        // for the tokens that still answer with a false capability claim.
        (
            "let x = 1; x < ;",
            "needs an operand here, and found a `;` instead",
        ),
        (
            "function f() { return 1;",
            "needs a `}` to close the function body",
        ),
        ("{ 1;", "needs a `}` to close the block"),
        ("", "needs a statement to compile; this source is empty"),
    ] {
        let message = refuse(source).message;
        assert!(
            message.contains(needle),
            "{source:?} gave {message:?}, wanted it to mention {needle:?}"
        );
        assert!(message.starts_with("this engine "), "{message:?}");
    }
}

/// DIVERGENCE (diagnostic wording): `diag::malformed` already prefixes "this
/// engine ", so a `what` phrase that names the subject again says it twice.
/// Two sites in `parse.rs` do. The sentence is still engine-voiced and still
/// true, which is why this is a wart and not a lie -- retire it by trimming
/// the second clause's subject at those two call sites.
#[test]
fn two_diagnostics_currently_name_the_engine_twice() {
    for (source, message) in [
        (
            "1 = 2;",
            "this engine needs a name or a property on the left of an assignment; this engine has nothing else to assign to yet",
        ),
        (
            "1++;",
            "this engine needs a name or a property to increment or decrement; this engine has nothing else to write back to yet",
        ),
    ] {
        assert_eq!(refuse(source).message, message, "{source:?}");
    }
}

/// **CLOSED.** `TokenKind::capability` in `src/lex.rs` carries phrases for
/// lexemes M1 *does* lower, and a token in the wrong place used to claim the
/// engine lacked a capability it demonstrably has -- a misplaced `else`
/// reported "does not support the `else` keyword yet", next to a suite full
/// of working `else` arms.
///
/// The end of a statement was fixed first, then generalised:
/// `Parser::cannot_use` now trusts the phrase only for the tokens M1 lowers
/// **nowhere** (`parse::unlowered_by_m1`: `[`, `]`, `:`, `,`, and the lexer's
/// own `Unsupported` bucket) and says what it was looking for otherwise. The
/// table is shared with M0's expression compiler, which really does lack every
/// capability in it, so the table stayed and the caller changed.
///
/// Each row is a source that used to disclaim a capability, the sentence it
/// gives now, and a program in that capability's correct spelling -- which is
/// what says the disclaimer would have been false.
#[test]
fn a_misplaced_token_says_what_was_wanted_and_never_disclaims_what_the_engine_has() {
    for (source, message, works) in [
        (
            "else { }",
            "this engine needs an operand here, and found the `else` keyword instead",
            "if (false) { } else { return 1; } return 0;",
        ),
        (
            "return 1; }",
            "this engine needs an operand here, and found a `}` instead",
            "{ return 1; }",
        ),
        (
            "if true) { }",
            "this engine needs a `(` after `if`, and found the `true` literal instead",
            "if (true) { return 1; } return 0;",
        ),
        (
            "let x = 1; x = = 2;",
            "this engine needs an operand here, and found a `=` instead",
            "let x = 1; x = 2; return x;",
        ),
        (
            "let ++x = 1;",
            "this engine needs a name after the `let` keyword, and found a `++` instead",
            "let x = 1; x++; return x;",
        ),
        (
            "let && x = 1;",
            "this engine needs a name after the `let` keyword, and found a `&&` instead",
            "return true && 1;",
        ),
        (
            "let ! x = 1;",
            "this engine needs a name after the `let` keyword, and found a `!` instead",
            "return !false;",
        ),
        (
            "let < x = 1;",
            "this engine needs a name after the `let` keyword, and found a `<` instead",
            "return 1 < 2;",
        ),
        (
            "let return x = 1;",
            "this engine needs a name after the `let` keyword, and found the `return` keyword instead",
            "return 1;",
        ),
        (
            "let while x = 1;",
            "this engine needs a name after the `let` keyword, and found the `while` keyword instead",
            "let i = 0; while (i < 1) { i++; } return i;",
        ),
    ] {
        assert_eq!(refuse(source).message, message, "{source:?}");
        // Nothing here may disclaim a capability, and the capability each row
        // used to disclaim is one the engine has.
        assert!(
            !refuse(source).message.contains("does not support"),
            "{source:?} still disclaims something"
        );
        assert!(
            compile_qjs_m1(works).is_ok(),
            "{works:?} must compile, or the row above proves nothing"
        );
    }
}

/// The position above that has been fixed: the end of a statement.
///
/// The statement dispatch's last arm takes *any* token as the start of an
/// expression statement, so a token standing at the end of one is the next
/// statement's first token and not a token the engine could not lower.
/// `Parser::semicolon` therefore says what it was looking for.
///
/// Two kinds keep their capability phrase, because for them it is true: a `,`
/// or a `:` is a JavaScript operator that would have continued the expression
/// and that this engine does not lower, and the lexer's `Unsupported` bucket
/// is beyond the engine whatever the lexeme is.
#[test]
fn the_end_of_a_statement_names_the_missing_semicolon() {
    for (source, found) in [
        (
            "const o = {}; o.m = function (x) { return x; } function rec() { return 1; } return 0;",
            "the `function` keyword",
        ),
        ("let x = 1; x \"a\";", "a string literal"),
        ("let a = 1 if (1) { } return 1;", "the `if` keyword"),
        ("let a = 1 return 1;", "the `return` keyword"),
        ("let a = 1 let b = 2; return 1;", "the `let` keyword"),
        ("let a = 1 { } return 1;", "a `{`"),
    ] {
        assert_eq!(
            refuse(source).message,
            format!(
                "this engine needs a `;` to end the statement, and found {found} instead; \
                 ECMA-262 12.10 supplies one only across a line break"
            ),
            "{source:?}"
        );
    }
    // Written with the line break ECMA-262 12.10 rule 1 asks for, each one
    // compiles -- which is what makes the diagnostic above a `;` and not a
    // capability.
    for source in [
        "const o = {}; o.m = function (x) { return x; }\nfunction rec() { return 1; }\nreturn 0;",
        "let a = 1\nif (a) { return 2; }\nreturn 1;",
        "let a = 1\nreturn a;",
    ] {
        assert!(compile_qjs_m1(source).is_ok(), "{source:?}");
    }
    // And the two kinds that keep their phrase, because it is the truth.
    // A `:` used to name the conditional expression here; the milestone that
    // landed `?:` took that meaning away, so what a `:` the parser cannot use
    // spells is a label, and that is what it now says.
    assert_eq!(
        refuse("let a = 1 : 2; return a;").message,
        "this engine does not support labelled statements yet"
    );
    assert_eq!(
        refuse("let a = 1 ** 2; return a;").message,
        "this engine does not support exponentiation yet"
    );
    assert_eq!(
        refuse("let a = 1 class X { } return 1;").message,
        "this engine does not support the `class` keyword yet"
    );
}
