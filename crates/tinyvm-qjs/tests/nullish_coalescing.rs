//! Exact `lhs ?? rhs` subset: one left value, a right branch only for Null or
//! Undefined, and an explicit refusal where ECMA-262 forbids loose mixing with
//! the boolean short-circuit grammar.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{CompileError, Value, compile_qjs_m1};

#[derive(Debug, PartialEq)]
enum Out {
    Number(f64),
    Bool(bool),
}

fn run(source: &str) -> Out {
    let wasm =
        compile_qjs_m1(source).unwrap_or_else(|error| panic!("compiling {source:?}: {error}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .unwrap_or_else(|error| panic!("loading {source:?}: {}", error.message()));
    let mut instance = module
        .instantiate()
        .expect("instantiate coalescing program");
    let result = instance
        .invoke_by_name("main", &[])
        .unwrap_or_else(|error| panic!("running {source:?}: {}", error.message()));
    match Value::returned(&result).expect("one JavaScript result") {
        Value::Number(value) => Out::Number(value),
        Value::Bool(value) => Out::Bool(value),
        other => panic!("{source:?}: unexpected result {other:?}"),
    }
}

fn refuse(source: &str) -> CompileError {
    match compile_qjs_m1(source) {
        Ok(bytes) => panic!("{source:?} compiled to {} bytes", bytes.len()),
        Err(error) => error,
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

#[test]
fn null_and_undefined_choose_the_right_operand() {
    assert_eq!(run("return null ?? 7;"), Out::Number(7.0));
    assert_eq!(run("return undefined ?? 8;"), Out::Number(8.0));
}

#[test]
fn false_zero_and_empty_string_are_values_not_absence() {
    assert_eq!(run("return (false ?? true) === false;"), Out::Bool(true));
    assert_eq!(run("return (0 ?? 7) === 0;"), Out::Bool(true));
    assert_eq!(
        run("return (\"\" ?? \"fallback\") === \"\";"),
        Out::Bool(true)
    );
}

#[test]
fn the_left_operand_runs_once_and_the_right_runs_only_when_needed() {
    assert_eq!(
        run("let lefts = 0; let rights = 0; \
             function left() { lefts += 1; return null; } \
             function right() { rights += 1; return 7; } \
             const value = left() ?? right(); return lefts * 100 + rights * 10 + value;"),
        Out::Number(117.0)
    );
    assert_eq!(
        run("let lefts = 0; let rights = 0; \
             function left() { lefts += 1; return 4; } \
             function right() { rights += 1; return 7; } \
             const value = left() ?? right(); return lefts * 100 + rights * 10 + value;"),
        Out::Number(104.0)
    );
}

#[test]
fn precedence_and_left_associativity_are_observable() {
    assert_eq!(run("return null ?? 1 + 2;"), Out::Number(3.0));
    assert_eq!(run("return 0 ?? 1 ? 2 : 3;"), Out::Number(3.0));
    assert_eq!(run("return null ?? undefined ?? 9;"), Out::Number(9.0));
}

#[test]
fn mixing_with_boolean_short_circuit_and_assignment_are_named_refusals() {
    for source in [
        "return true || null ?? 1;",
        "return null ?? false && true;",
        "return null ?? (false || true);",
    ] {
        assert_eq!(
            refuse(source).message,
            "this engine does not support combining `??` with `&&` or `||` in this subset yet"
        );
    }
    assert_eq!(
        refuse("let value = null; value ??= 7; return value;").message,
        "this engine does not support logical assignment yet"
    );
}

#[test]
fn an_ordinary_or_program_keeps_its_pre_milestone_bytes() {
    let wasm = compile_qjs_m1("let value = 0; return value || 7;").expect("compiles");
    // Filled from the clean parent revision before the Nullish token and
    // lowering existed. This is the exact non-participant gate, not a
    // compile-twice determinism check.
    assert_eq!(wasm.len(), 10_247);
    assert_eq!(fnv1a64(&wasm), 0x4cb0_0fb5_98fb_2227);
}
