//! The `Math` functions (ECMA-262 21.3), as this engine carries them: no
//! `Math` object exists -- each member folds at the parse to a reserved
//! method the prefab layer gates per name (the road `Object.keys` took),
//! the two constants fold to literals, and a member this engine's `Math`
//! does not have is refused at compile time, by name.
//!
//! Second batch of the 2026-08-31 count: `rh_compat.qjs`'s `int_div` (68
//! call lines in 8 files) and `floor_div` exist because the engine had no
//! `Math.floor` and no integer division.

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

/// `floor` / `ceil` / `trunc` / `abs` (21.3.2.16 / .10 / .35 / .1):
/// negatives, the zero whose sign survives, NaN and the infinities.
#[test]
fn the_rounding_functions_answer_the_spec() {
    for (source, want) in [
        ("return Math.floor(1.5);", 1.0),
        ("return Math.floor(-1.5);", -2.0),
        ("return Math.floor(7);", 7.0),
        ("return Math.floor(-0.5);", -1.0),
        ("return Math.ceil(1.5);", 2.0),
        ("return Math.ceil(-1.5);", -1.0),
        ("return 1 / Math.ceil(-0.5);", f64::NEG_INFINITY),
        ("return Math.trunc(3.7);", 3.0),
        ("return Math.trunc(-3.7);", -3.0),
        ("return Math.abs(-5);", 5.0),
        ("return Math.abs(5);", 5.0),
        ("return 1 / Math.abs(-0);", f64::INFINITY),
        ("return Math.floor(0/0);", f64::NAN),
        ("return Math.ceil(1/0);", f64::INFINITY),
        ("return Math.trunc(-1/0);", f64::NEG_INFINITY),
        ("return Math.abs(-1/0);", f64::INFINITY),
        // The argument is a value, not a Number: ToNumber first.
        ("return Math.floor(\"3.9\");", 3.0),
        ("return Math.abs(true);", 1.0),
    ] {
        number(source, want);
    }
}

/// `round` (21.3.2.28): ties toward +∞, `-0` where the answer is a
/// negative zero, and the double just under one half staying down --
/// `floor(x + 0.5)` gets that one wrong and this must not.
#[test]
fn round_ties_toward_positive_infinity() {
    for (source, want) in [
        ("return Math.round(2.5);", 3.0),
        ("return Math.round(-2.5);", -2.0),
        ("return Math.round(2.4);", 2.0),
        ("return Math.round(2.6);", 3.0),
        ("return Math.round(-2.6);", -3.0),
        ("return Math.round(7);", 7.0),
        ("return Math.round(0.49999999999999994);", 0.0),
        ("return 1 / Math.round(-0.4);", f64::NEG_INFINITY),
        ("return 1 / Math.round(-0.5);", f64::NEG_INFINITY),
        ("return Math.round(-0.6);", -1.0),
        ("return Math.round(0/0);", f64::NAN),
        ("return Math.round(1/0);", f64::INFINITY),
        ("return Math.round(4503599627370495.5);", 4503599627370496.0),
    ] {
        number(source, want);
    }
}

/// `sqrt` and `sign` (21.3.2.32 / .30).
#[test]
fn sqrt_and_sign() {
    for (source, want) in [
        ("return Math.sqrt(4);", 2.0),
        ("return Math.sqrt(2);", std::f64::consts::SQRT_2),
        ("return Math.sqrt(0);", 0.0),
        ("return Math.sqrt(-1);", f64::NAN),
        ("return 1 / Math.sqrt(-0);", f64::NEG_INFINITY),
        ("return Math.sqrt(1/0);", f64::INFINITY),
        ("return Math.sign(-3);", -1.0),
        ("return Math.sign(3);", 1.0),
        ("return Math.sign(0);", 0.0),
        ("return 1 / Math.sign(-0);", f64::NEG_INFINITY),
        ("return Math.sign(0/0);", f64::NAN),
        ("return Math.sign(-1/0);", -1.0),
    ] {
        number(source, want);
    }
}

/// `min` / `max` (21.3.2.25 / .24) at every arity the fold covers: none
/// (the identity), one (ToNumber), two (the prefab), more (pairwise).
/// NaN wins over anything, and `-0` sorts below `+0`.
#[test]
fn min_and_max_at_zero_one_two_and_more_arguments() {
    for (source, want) in [
        ("return Math.min();", f64::INFINITY),
        ("return Math.max();", f64::NEG_INFINITY),
        ("return Math.min(5);", 5.0),
        ("return Math.max(\"5\");", 5.0),
        ("return Math.min(3, 1);", 1.0),
        ("return Math.max(3, 1);", 3.0),
        ("return Math.min(-3, 1);", -3.0),
        ("return Math.min(3, 1, 2);", 1.0),
        ("return Math.max(3, 1, 7, 2);", 7.0),
        ("return Math.min(1, 0/0);", f64::NAN),
        ("return Math.min(0/0, 1);", f64::NAN),
        ("return Math.max(1, 2, 0/0);", f64::NAN),
        ("return 1 / Math.min(0, -0);", f64::NEG_INFINITY),
        ("return 1 / Math.max(-0, 0);", f64::INFINITY),
        ("return Math.max(\"5\", 2);", 5.0),
    ] {
        number(source, want);
    }
}

/// `pow` (21.3.2.26 over 6.1.6.1.3): the spec's special-case table, and
/// exponentiation by squaring for the integer exponents every downstream
/// use has.
#[test]
fn pow_answers_the_specs_table_for_integer_exponents() {
    for (source, want) in [
        ("return Math.pow(2, 10);", 1024.0),
        ("return Math.pow(3, 4);", 81.0),
        ("return Math.pow(2, -2);", 0.25),
        ("return Math.pow(-2, 3);", -8.0),
        ("return Math.pow(-2, 2);", 4.0),
        ("return Math.pow(10, 15);", 1e15),
        ("return Math.pow(7, 0);", 1.0),
        ("return Math.pow(0/0, 0);", 1.0),
        ("return Math.pow(0/0, 2);", f64::NAN),
        ("return Math.pow(2, 0/0);", f64::NAN),
        ("return Math.pow(0, -1);", f64::INFINITY),
        ("return 1 / Math.pow(-0, 1);", f64::NEG_INFINITY),
        ("return Math.pow(-0, -1);", f64::NEG_INFINITY),
        ("return Math.pow(-0, -2);", f64::INFINITY),
        ("return Math.pow(-1/0, 3);", f64::NEG_INFINITY),
        ("return Math.pow(-1/0, 2);", f64::INFINITY),
        ("return Math.pow(1/0, -1);", 0.0),
        ("return Math.pow(1, 1/0);", f64::NAN),
        ("return Math.pow(-1, -1/0);", f64::NAN),
        ("return Math.pow(2, 1/0);", f64::INFINITY),
        ("return Math.pow(0.5, 1/0);", 0.0),
        ("return Math.pow(2, -1/0);", 0.0),
        ("return Math.pow(2, 1024);", f64::INFINITY),
        // A non-integer exponent over a negative or zero base is still the
        // spec's own row; only the positive finite base is refused.
        ("return Math.pow(-2, 0.5);", f64::NAN),
        ("return Math.pow(0, 0.5);", 0.0),
        ("return Math.pow(1/0, 0.5);", f64::INFINITY),
    ] {
        number(source, want);
    }
}

/// The one `pow` arm this engine refuses rather than approximates, and
/// the refusal's name, host-readable.
#[test]
fn a_fractional_exponent_over_a_positive_base_is_refused_by_name() {
    let wasm = compile_qjs_m1("return Math.pow(2, 0.5);").expect("compiles");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    assert!(
        instance.invoke_by_name("main", &Value::args(&[])).is_err(),
        "a fractional exponent must stop"
    );
    let memory = instance.memory().expect("guest memory");
    assert_eq!(
        tinyvm_qjs::guest_fault(&memory),
        Some(tinyvm_qjs::GuestFault::CapabilityBoundary)
    );
    assert_eq!(
        tinyvm_qjs::guest_capability_name(&memory).as_deref(),
        Some("a fractional Math.pow exponent")
    );
}

/// The two constants (21.3.1.6 / .1), folded to literals.
#[test]
fn pi_and_e_are_their_literals() {
    number("return Math.PI;", std::f64::consts::PI);
    number("return Math.E;", std::f64::consts::E);
    number("return Math.floor(Math.PI);", 3.0);
    number("return Math.PI * 0 + 1;", 1.0);
}

/// A member this engine's `Math` does not have is refused at compile
/// time, by name -- there is no `Math` record for a read to miss, so the
/// compile-time sentence is the missing-property refusal. Wrong arity is
/// its own sentence.
#[test]
fn an_absent_math_member_is_refused_by_name() {
    for (source, needle) in [
        ("return Math.cos(1);", "`Math.cos`"),
        ("return Math.random();", "`Math.random`"),
        ("return Math.hypot(3, 4);", "`Math.hypot`"),
        (
            "return Math.floor(1, 2);",
            "`Math.floor` called with 2 arguments; it takes one",
        ),
        (
            "return Math.floor();",
            "`Math.floor` called with 0 arguments; it takes one",
        ),
        (
            "return Math.pow(2);",
            "`Math.pow` called with 1 arguments; it takes two",
        ),
    ] {
        let error = compile_qjs_m1(source).expect_err(source);
        assert!(
            error.message.contains(needle),
            "{source:?} gave {:?}, which does not name {needle:?}",
            error.message
        );
    }
}

/// A script that declares its own `Math` means its own, and gets it --
/// the same escape hatch `Number`, `JSON` and `Object` keep.
#[test]
fn a_declared_math_shadows_the_engines() {
    number("let Math = { floor: 7 }; return Math.floor;", 7.0);
    number("let Math = { PI: 3 }; return Math.PI;", 3.0);
    // And the engine's own is untouched elsewhere: a function scope that
    // declares it does not leak.
    number(
        "function f() { let Math = { PI: 3 }; return Math.PI; } return f() + Math.floor(1.5);",
        4.0,
    );
}

/// The downstream spellings this batch retires: `int_div(a, b)` is
/// `Math.trunc(a / b)`, and `floor_div(a, b)` is `Math.floor(a / b)`.
#[test]
fn the_downstream_spellings() {
    number("return Math.trunc(7 / 2);", 3.0);
    number("return Math.trunc(-7 / 2);", -3.0);
    number("return Math.floor(-7 / 2);", -4.0);
    number("return Math.floor(7 / 2);", 3.0);
    // rh_compat's calendar arithmetic, one line of it.
    number("return Math.floor(-1 / 86400);", -1.0);
}
