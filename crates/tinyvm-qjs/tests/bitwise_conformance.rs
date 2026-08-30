//! The bitwise and shift operators (ECMA-262 13.5.6, 13.10, 13.12) over the
//! V1 Number, which is a binary64: every operand goes through ToInt32
//! (7.1.6) first, `>>>` reads its result through ToUint32 (7.1.7), and the
//! answer is a Number again. Every expectation is the spec's, checked
//! against V8 where the spec leaves arithmetic to the reader.
//!
//! Written for the 2026-08-31 count: `rh_compat.qjs`'s `xor16` was a
//! sixteen-turn loop over `% 2` because the engine had no `^`, and
//! `fnv1a64_hex` did a 64-bit multiply in 16-bit limbs on top of it.

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
        Value::Number(x) if x == want && x.is_sign_negative() == want.is_sign_negative() => {}
        other => panic!("{source:?}: wanted {want}, got {other:?}"),
    }
}

#[track_caller]
fn boolean(source: &str, want: bool) {
    let (_, vals) = attempt(source).unwrap_or_else(|e| panic!("{e}"));
    let got = Value::returned(&vals).unwrap_or_else(|e| panic!("{source:?}: {e}"));
    assert_eq!(got, Value::Bool(want), "{source:?}");
}

/// The six binary operators on small non-negative integers, where ToInt32
/// is the identity and the operator is the whole question.
#[test]
fn the_six_operators_on_small_integers() {
    for (source, want) in [
        ("return 6 & 3;", 2.0),
        ("return 6 | 3;", 7.0),
        ("return 6 ^ 3;", 5.0),
        ("return 1 << 4;", 16.0),
        ("return 64 >> 3;", 8.0),
        ("return 64 >>> 3;", 8.0),
        ("return 0 & 0;", 0.0),
        ("return 0 | 0;", 0.0),
        ("return 5 ^ 5;", 0.0),
        ("return 0xff & 0x0f;", 15.0),
        ("return 0xf0 | 0x0f;", 255.0),
        ("return 0xff ^ 0x0f;", 240.0),
    ] {
        number(source, want);
    }
}

/// `~` (13.5.6): ToInt32, then every bit flipped -- so `~x` is `-x - 1`
/// on an integer, and `~~x` is the truncation idiom.
#[test]
fn bitwise_not_flips_every_bit() {
    for (source, want) in [
        ("return ~0;", -1.0),
        ("return ~5;", -6.0),
        ("return ~-1;", 0.0),
        ("return ~~3.7;", 3.0),
        ("return ~~-3.7;", -3.0),
        ("return ~2147483647;", -2147483648.0),
        ("return ~-2147483648;", 2147483647.0),
        ("return ~0xffffffff;", 0.0),
    ] {
        number(source, want);
    }
}

/// 7.1.6 step 5: the result is the two's-complement reading, so a negative
/// operand is its 32-bit pattern and the sign propagates through `>>`.
#[test]
fn negative_operands_are_their_two_complement_pattern() {
    for (source, want) in [
        ("return -1 & 0xff;", 255.0),
        ("return -8 >> 1;", -4.0),
        ("return -8 >> 31;", -1.0),
        ("return -1 | 0;", -1.0),
        ("return -6 ^ 3;", -7.0),
        ("return -2 << 1;", -4.0),
        ("return -2147483648 >> 31;", -1.0),
        ("return -2147483648 & -2147483648;", -2147483648.0),
        ("return -1 >> 0;", -1.0),
    ] {
        number(source, want);
    }
}

/// `>>>` (13.12.3) is the one operator whose result is ToUint32, so it is
/// never negative and reaches 2^32 - 1.
#[test]
fn unsigned_shift_answers_up_to_two_to_the_thirty_two_minus_one() {
    for (source, want) in [
        ("return -1 >>> 0;", 4294967295.0),
        ("return -1 >>> 1;", 2147483647.0),
        ("return -8 >>> 1;", 2147483644.0),
        ("return -2147483648 >>> 0;", 2147483648.0),
        ("return -2147483648 >>> 31;", 1.0),
        ("return 3000000000 >>> 0;", 3000000000.0),
        ("return -3000000000 >>> 0;", 1294967296.0),
        ("return 1 >>> 0;", 1.0),
        ("return 0 >>> 0;", 0.0),
        ("return 4294967295 >>> 0;", 4294967295.0),
        ("return 4294967296 >>> 0;", 0.0),
    ] {
        number(source, want);
    }
}

/// 7.1.6 step 4: modulo 2^32, so an operand past the signed range wraps
/// and one past 2^53 is whatever its low 32 bits are.
#[test]
fn operands_past_two_to_the_thirty_one_wrap_modulo_two_to_the_thirty_two() {
    for (source, want) in [
        ("return 2147483648 | 0;", -2147483648.0),
        ("return 2147483647 | 0;", 2147483647.0),
        ("return -2147483649 | 0;", 2147483647.0),
        ("return 4294967295 | 0;", -1.0),
        ("return 4294967296 | 0;", 0.0),
        ("return 4294967297 | 0;", 1.0),
        ("return 4294967296 & 1;", 0.0),
        ("return 8589934593 & 3;", 1.0),
        ("return 0x7fffffff + 1 | 0;", -2147483648.0),
        // Exactly what V8 answers: the low 32 bits of the integer 1e21.
        ("return 1e21 | 0;", -559939584.0),
        ("return 9007199254740992 | 0;", 0.0),
        ("return -9007199254740992 | 0;", 0.0),
        ("return 9007199254740991 | 0;", -1.0),
        ("return 1e300 | 0;", 0.0),
        ("return -1e300 | 0;", 0.0),
    ] {
        number(source, want);
    }
}

/// 7.1.6 step 3: truncation toward zero, before the modulo.
#[test]
fn a_fraction_is_truncated_toward_zero() {
    for (source, want) in [
        ("return 1.9 | 0;", 1.0),
        ("return -1.9 | 0;", -1.0),
        ("return 0.5 | 0;", 0.0),
        ("return -0.5 | 0;", 0.0),
        ("return 2147483647.9 | 0;", 2147483647.0),
        ("return 2147483648.5 | 0;", -2147483648.0),
        ("return 3.7 & 3.2;", 3.0),
        ("return 1.5 << 1.5;", 2.0),
        ("return 7.9 >> 1.9;", 3.0),
        ("return 7.9 >>> 1.9;", 3.0),
        ("return ~1.5;", -2.0),
    ] {
        number(source, want);
    }
}

/// 7.1.6 step 2: NaN and the two infinities are +0 -- spelled `0/0` and
/// `1/0`, because this engine binds neither name.
#[test]
fn nan_and_infinity_are_zero() {
    for (source, want) in [
        ("return 0/0 | 0;", 0.0),
        ("return 1/0 | 0;", 0.0),
        ("return -1/0 | 0;", 0.0),
        ("return 0/0 & 0xff;", 0.0),
        ("return 5 & 0/0;", 0.0),
        ("return 1/0 >>> 0;", 0.0),
        ("return -1/0 >> 0;", 0.0),
        ("return ~(0/0);", -1.0),
        ("return ~(1/0);", -1.0),
        ("return 1 << 1/0;", 1.0),
        ("return 5 | 0/0;", 5.0),
        // Not a zero the sign survives: `-0 | 0` is +0 (steps 3-5 yield
        // the integer 0, and 13.15.3's Number::bitwiseOR of two 0s is +0).
        ("return -0 | 0;", 0.0),
    ] {
        number(source, want);
    }
}

/// 13.12.1 step 6: the shift count is ToUint32 of the right operand,
/// masked to its low five bits.
#[test]
fn a_shift_count_is_taken_modulo_thirty_two() {
    for (source, want) in [
        ("return 1 << 32;", 1.0),
        ("return 1 << 33;", 2.0),
        ("return 1 << -1;", -2147483648.0),
        ("return 8 >> 35;", 1.0),
        ("return 8 >>> 35;", 1.0),
        ("return 1 << 31;", -2147483648.0),
        ("return 1 << 30;", 1073741824.0),
        ("return -1 >> 32;", -1.0),
        ("return -1 >>> 32;", 4294967295.0),
    ] {
        number(source, want);
    }
}

/// The operands are values, not Numbers: a String and a Boolean go through
/// ToNumber first (13.15.3 step 1 is ToNumeric).
#[test]
fn other_types_convert_through_to_number() {
    for (source, want) in [
        ("return \"12\" & 10;", 8.0),
        ("return \"0x10\" | 1;", 17.0),
        ("return true | 0;", 1.0),
        ("return false | 8;", 8.0),
        ("return null | 4;", 4.0),
        ("return undefined | 4;", 4.0),
        ("return \"abc\" | 0;", 0.0),
        ("return \"\" | 0;", 0.0),
        ("return \" 7 \" ^ 1;", 6.0),
        ("return ~\"3\";", -4.0),
        ("return \"1\" << \"3\";", 8.0),
    ] {
        number(source, want);
    }
}

/// 13.15.2: `x op= y` is `x = x op y`, reading `x` once, through a name
/// and through a member.
#[test]
fn the_six_compound_assignments() {
    for (source, want) in [
        ("let x = 12; x &= 10; return x;", 8.0),
        ("let x = 12; x |= 3; return x;", 15.0),
        ("let x = 12; x ^= 10; return x;", 6.0),
        ("let x = 3; x <<= 2; return x;", 12.0),
        ("let x = -12; x >>= 1; return x;", -6.0),
        ("let x = -12; x >>>= 1; return x;", 2147483642.0),
        (
            "let x = 1; x <<= 4; x |= 1; x ^= 3; x >>= 1; x >>>= 0; return x;",
            9.0,
        ),
        ("let x = 5; let y = (x &= 4); return y;", 4.0),
        ("let o = { n: 6 }; o.n &= 3; return o.n;", 2.0),
        ("let a = [6]; a[0] |= 1; return a[0];", 7.0),
        ("let a = [1]; let i = 0; a[i] <<= 3; return a[0];", 8.0),
        (
            "let x = 0; for (let i = 0; i < 8; i = i + 1) { x |= 1 << i; } return x;",
            255.0,
        ),
    ] {
        number(source, want);
    }
}

/// 13.10 and 13.12 sit between `&&` and `==`, and between the relational
/// and additive rungs: the spec's order, which is also the famous trap.
#[test]
fn precedence_is_the_specs() {
    for (source, want) in [
        // `|` below `^` below `&`.
        ("return 1 | 2 & 3;", 3.0),
        ("return 1 ^ 3 | 4;", 6.0),
        ("return 1 | 2 ^ 3;", 1.0),
        ("return 5 & 4 ^ 1;", 5.0),
        // `&` below `==`: `1 & (3 == 3)`.
        ("return 1 & 3 == 3;", 1.0),
        // Shifts below `+`: `1 << (2 + 3)`.
        ("return 1 << 2 + 3;", 32.0),
        ("return 16 >> 1 + 1;", 4.0),
        // Left associative.
        ("return 1 << 2 << 3;", 32.0),
        ("return 256 >> 2 >> 2;", 16.0),
        ("return 7 & 6 & 5;", 4.0),
        // `~` is a prefix rung: `(~1) * 2`.
        ("return ~1 * 2;", -4.0),
        ("return ~(1 * 2);", -3.0),
    ] {
        number(source, want);
    }
    // Shifts above the relational rung: `5 < (1 << 3)`; `&` below `<`.
    boolean("return 5 < 1 << 3;", true);
    boolean("return (1 & 3) < 2;", true);
    // `1 & (3 < 2)` is `1 & false`, a Number.
    number("return 1 & 3 < 2;", 0.0);
    // `&&` still below `|`: `0 && (1 | 2)` is `0`.
    number("return 0 && 1 | 2;", 0.0);
    number("return 1 && 1 | 2;", 3.0);
}

/// The demand, spelled the way the scripts will spell it: `xor16` becomes
/// `^`, and a 16-bit FNV-1a step is `(h ^ b) * 16777619` masked back --
/// the multiply is a Number multiply, so a 32-bit FNV would overflow 2^53
/// and needs the split the script already does; 16 bits is exact.
#[test]
fn the_downstream_spellings() {
    number("return 8997 ^ 65;", 9060.0);
    number("return (8997 ^ 65) & 0xffff;", 9060.0);
    number(
        "let h = 0; for (let i = 1; i <= 16; i = i + 1) { h ^= i; } return h;",
        16.0,
    );
    // `int_div(a, b)` was `(a - a % b) / b`; for non-negative operands below
    // 2^31 it is `(a / b) | 0`, and `floor_div` of a negative is `>> 0` of
    // nothing in particular -- `Math.floor` is the honest spelling and lands
    // next. This row pins only that `| 0` truncates.
    number("return (7 / 2) | 0;", 3.0);
    number("return (-7 / 2) | 0;", -3.0);
    // `hex4`: the four nibbles of a 16-bit value, each `>> 4*k & 15`.
    number("return (0xbeef >> 12) & 15;", 11.0);
    number("return (0xbeef >> 8) & 15;", 14.0);
    number("return 0xbeef & 15;", 15.0);
}
