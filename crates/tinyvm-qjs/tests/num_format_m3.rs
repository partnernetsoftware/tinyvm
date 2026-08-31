//! `Number.prototype.toString(radix)` and `toFixed` (ECMA-262 21.1.3.6 /
//! 21.1.3.3), landed 2026-08-31 in the fourth A13 batch: rh_compat.qjs's
//! `hex4` peels nibbles with `HEX[x % 16]` because the engine had no
//! radix, and `toFixed` is the exact decimal -- the digits of
//! `round_half_up(|x| * 10^f)` on the bignum kit -- not a `%f`.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1, guest_capability_name, guest_fault};

fn run(source: &str) -> Result<String, String> {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let outcome = instance.invoke_by_name("main", &Value::args(&[]));
    let memory = instance.memory().expect("guest memory");
    let bytes: &[u8] = &memory;
    match outcome {
        Err(_) => Err(format!(
            "{:?} {:?}",
            guest_fault(&memory),
            guest_capability_name(&memory)
        )),
        Ok(vals) => {
            let Ok(Value::String(p)) = Value::returned(&vals) else {
                panic!("{source:?} did not answer a String");
            };
            let at = p as usize;
            let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            Ok(std::str::from_utf8(&bytes[at + 4..at + 4 + len])
                .expect("UTF-8")
                .to_string())
        }
    }
}

#[track_caller]
fn string(source: &str, want: &str) {
    assert_eq!(run(source).unwrap().as_str(), want, "{source:?}");
}

/// 21.1.3.6 step 4: digits of the integer value in the radix, the sign in
/// front, the special Numbers spelling as they do in base ten.
#[test]
fn to_string_answers_the_radix_digits() {
    string("let n = 255; return n.toString(16);", "ff");
    string("let n = 255; return n.toString(2);", "11111111");
    string("let n = -255; return n.toString(16);", "-ff");
    string("let n = 48879; return n.toString(16);", "beef");
    string("let n = 35; return n.toString(36);", "z");
    string("let n = 7; return n.toString(8);", "7");
    string("let n = 0; return n.toString(2);", "0");
    string("let n = -0; return n.toString(2);", "0");
    string("let n = 0/0; return n.toString(16);", "NaN");
    string("let n = 1/0; return n.toString(16);", "Infinity");
    string("let n = -1/0; return n.toString(16);", "-Infinity");
    string("let n = 255; return n.toString(10);", "255");
    string("let n = 255; return n.toString(16.9);", "ff");
    // Past 2^53 the digits agree with V8's own loop.
    string(
        "let n = 9007199254740993; return n.toString(16);",
        "20000000000000",
    );
    string("let n = 1e21; return n.toString(16);", "3635c9adc5dea00000");
    // The zero-argument spelling is ToString whole.
    string("let n = 255; return n.toString();", "255");
    string("let n = 1.5; return n.toString();", "1.5");
}

/// The two refusals: the spec's RangeError outside 2..=36, and the one
/// arm this engine refuses rather than approximates -- a fractional
/// value under a non-decimal radix (V8 prints those through a
/// delta-terminated loop).
#[test]
fn to_string_refuses_by_name() {
    for (source, needle) in [
        (
            "let n = 255; return n.toString(1);",
            "a toString radix outside 2..36",
        ),
        (
            "let n = 255; return n.toString(37);",
            "a toString radix outside 2..36",
        ),
        (
            "let n = 1.5; return n.toString(2);",
            "a fractional Number under a non-decimal radix",
        ),
    ] {
        let refusal = run(source).unwrap_err();
        assert!(refusal.contains(needle), "{source:?}: {refusal}");
    }
}

/// 21.1.3.3: the exact decimal. The two famous rows are the point:
/// `(1.005).toFixed(2)` is `"1.00"` because 1.005 *is* 1.00499…, and
/// `(8.005).toFixed(2)` is `"8.01"` because that one sits above its
/// half. Ties round to the larger n of the non-negative x (step 6 runs
/// before step 9), which is away from zero once the sign returns.
#[test]
fn to_fixed_is_the_exact_decimal() {
    string("let n = 1.005; return n.toFixed(2);", "1.00");
    string("let n = 8.005; return n.toFixed(2);", "8.01");
    string("let n = 4.35; return n.toFixed(1);", "4.3");
    string("let n = 1.5; return n.toFixed(0);", "2");
    string("let n = -1.5; return n.toFixed(0);", "-2");
    string("let n = 2.5; return n.toFixed(0);", "3");
    string("let n = 0.5; return n.toFixed(0);", "1");
    string("let n = 123.456; return n.toFixed(2);", "123.46");
    string("let n = 123.456; return n.toFixed(0);", "123");
    string("let n = 123.456; return n.toFixed(6);", "123.456000");
    string("let n = 0; return n.toFixed(2);", "0.00");
    string("let n = -0; return n.toFixed(2);", "0.00");
    string("let n = 0.001; return n.toFixed(2);", "0.00");
    string("let n = 0.000001; return n.toFixed(7);", "0.0000010");
    string(
        "let n = 1234567890123.456; return n.toFixed(3);",
        "1234567890123.456",
    );
    string("let n = 7; return n.toFixed(0);", "7");
    string("let n = 7; return n.toFixed(3);", "7.000");
    string("let n = 0/0; return n.toFixed(2);", "NaN");
    // At and past 1e21, ToString's answer (step 10).
    string(
        "let n = 1e20; return n.toFixed(2);",
        "100000000000000000000.00",
    );
    string("let n = 1e21; return n.toFixed(2);", "1e+21");
}

#[test]
fn to_fixed_refuses_digits_outside_the_range() {
    for source in [
        "let n = 5; return n.toFixed(101);",
        "let n = 5; return n.toFixed(-1);",
    ] {
        let refusal = run(source).unwrap_err();
        assert!(
            refusal.contains("toFixed digits outside 0..100"),
            "{source:?}: {refusal}"
        );
    }
}

fn bytes(source: &str) -> usize {
    compile_qjs_m1(source)
        .unwrap_or_else(|e| panic!("compiling {source:?}: {e}"))
        .len()
}

#[test]
fn each_addition_has_a_published_price() {
    let base = bytes("return 6 + 3;");
    let rows = [
        ("let n = 255; return n.toString(16);", 938),
        ("let n = 255; return n.toString();", 363),
        ("let n = 1.5; return n.toFixed(2);", 1_243),
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
    assert_eq!(bytes("return 1;"), 10_198, "the base program moved");
}
