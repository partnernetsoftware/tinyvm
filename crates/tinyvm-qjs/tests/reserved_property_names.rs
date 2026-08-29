//! A reserved word is an IdentifierName after `.` and before `:`.
//!
//! `operation["class"]` is what cli-smoke had to write, because `.class`
//! was refused as "a property named with a reserved word". ECMA-262 13.3.2
//! and 13.2.5 admit any IdentifierName there; only statements care.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

fn number(source: &str) -> f64 {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let vals = instance.invoke_by_name("main", &Value::args(&[])).expect("runs");
    match Value::returned(&vals).expect("a value") {
        Value::Number(n) => n,
        other => panic!("{source:?}: expected a Number, got {other:?}"),
    }
}

#[test]
fn reserved_words_name_properties_after_a_dot_and_in_a_literal() {
    assert_eq!(number("let o = { class: 3, await: 4 }; return o.class + o.await;"), 7.0);
    assert_eq!(number("let o = {}; o.class = 5; o.switch = 6; return o.class + o.switch;"), 11.0);
    assert_eq!(number("let o = { class: 1 }; return o[\"class\"] + o.class;"), 2.0);
}

#[test]
fn the_same_words_are_still_refused_as_statements() {
    let err = compile_qjs_m1("class A {}").expect_err("a class declaration");
    assert!(err.to_string().contains("class"), "{err}");
    let err = compile_qjs_m1("let x = await 1;").expect_err("await");
    assert!(err.to_string().contains("await"), "{err}");
}
