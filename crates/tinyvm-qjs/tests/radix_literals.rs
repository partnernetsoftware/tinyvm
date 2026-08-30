//! `0x`, `0o` and `0b` literals -- ECMA-262 12.9.3.
//!
//! Wave 2 ported Win32 message and key constants as decimal with the hex in
//! comments, because the lexer refused the form. It answers now, and a
//! literal that does not fit 64 bits or has no digits is refused by name.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

fn number(source: &str) -> f64 {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect("runs");
    match Value::returned(&vals).expect("a value") {
        Value::Number(n) => n,
        other => panic!("{source:?}: expected a Number, got {other:?}"),
    }
}

#[test]
fn the_three_radices_answer() {
    assert_eq!(number("return 0xff;"), 255.0);
    assert_eq!(number("return 0XFF;"), 255.0);
    assert_eq!(number("return 0x1F + 1;"), 32.0);
    assert_eq!(number("return 0b101;"), 5.0);
    assert_eq!(number("return 0o17;"), 15.0);
    assert_eq!(number("return 0xFFFFFFFF;"), 4_294_967_295.0);
    assert_eq!(number("return 0x0;"), 0.0);
}

#[test]
fn a_literal_with_no_digits_or_a_dangling_letter_is_refused_by_name() {
    let err = compile_qjs_m1("return 0x;").expect_err("no digits");
    assert!(err.to_string().contains("hexadecimal digits"), "{err}");
    let err = compile_qjs_m1("return 0xfg;").expect_err("g is not hex");
    assert!(err.to_string().contains("hexadecimal digits"), "{err}");
    let err = compile_qjs_m1("return 0b102;").expect_err("2 is not binary");
    assert!(err.to_string().contains("binary digits"), "{err}");
}

#[test]
fn wider_than_64_bits_is_refused_rather_than_rounded() {
    let err = compile_qjs_m1("return 0x10000000000000000;").expect_err("65 bits");
    assert!(err.to_string().contains("wider than 64 bits"), "{err}");
}

#[test]
fn a_leading_zero_decimal_is_still_refused() {
    let err = compile_qjs_m1("return 017;").expect_err("legacy octal");
    assert!(err.to_string().contains("leading zero"), "{err}");
}
