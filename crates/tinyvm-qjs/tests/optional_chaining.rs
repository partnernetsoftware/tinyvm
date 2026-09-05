//! First optional-chain slice: one property access, with the receiver held
//! once and the computed key wholly inside the non-nullish branch.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

#[derive(Debug, PartialEq)]
enum Out {
    Undefined,
    Number(f64),
    Bool(bool),
}

fn run(source: &str) -> Out {
    let wasm =
        compile_qjs_m1(source).unwrap_or_else(|error| panic!("compiling {source:?}: {error}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .unwrap_or_else(|error| panic!("loading {source:?}: {}", error.message()));
    let mut instance = module.instantiate().expect("instantiate optional chain");
    let result = instance
        .invoke_by_name("main", &[])
        .unwrap_or_else(|error| panic!("running {source:?}: {}", error.message()));
    match Value::returned(&result).expect("one JavaScript result") {
        Value::Undefined => Out::Undefined,
        Value::Number(value) => Out::Number(value),
        Value::Bool(value) => Out::Bool(value),
        other => panic!("{source:?}: unexpected result {other:?}"),
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

#[test]
fn an_ordinary_member_program_keeps_its_pre_milestone_bytes() {
    let wasm = compile_qjs_m1("const o = { answer: 7 }; return o.answer;").expect("compiles");
    assert_eq!(wasm.len(), 10_618);
    assert_eq!(fnv1a64(&wasm), 0xa5c6_3dc1_778a_cdc9);
}

#[test]
fn null_and_undefined_short_circuit_to_undefined() {
    assert_eq!(run("return null?.answer;"), Out::Undefined);
    assert_eq!(run("return undefined?.[\"answer\"];"), Out::Undefined);
}

#[test]
fn the_receiver_is_evaluated_once() {
    assert_eq!(
        run(
            "let calls = 0; function base() { calls += 1; return { answer: 7 }; } \
             const value = base()?.answer; return calls * 10 + value;"
        ),
        Out::Number(17.0)
    );
}

#[test]
fn a_computed_key_is_skipped_for_a_nullish_receiver() {
    assert_eq!(
        run(
            "let calls = 0; function key() { calls += 1; return \"answer\"; } \
             const value = null?.[key()]; return calls === 0 && value === undefined;"
        ),
        Out::Bool(true)
    );
}

#[test]
fn a_computed_key_runs_once_after_a_non_nullish_receiver() {
    assert_eq!(
        run("let bases = 0; let keys = 0; \
             function base() { bases += 1; return { answer: 7 }; } \
             function key() { keys += 1; return \"answer\"; } \
             const value = base()?.[key()]; return bases * 100 + keys * 10 + value;"),
        Out::Number(117.0)
    );
}

#[test]
fn non_nullish_access_reuses_the_ordinary_missing_property_truth() {
    assert_eq!(run("return ({})?.missing;"), Out::Undefined);
    assert_eq!(run("return \"abc\"?.length;"), Out::Number(3.0));
}
