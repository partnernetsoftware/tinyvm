//! Decisive experiment for a per-function active expression-chain ceiling.

use tinyvm::{Limits, Val, WasmInstance, WasmModule};
use tinyvm_qjs::{
    EXPRESSION_DEPTH_LIMIT_IMPORT, GuestFault, Options, RUNTIME_LIMIT_MODULE, RuntimeLimits, Value,
    compile_qjs_m1_with_runtime_limits, guest_fault,
};

fn compile(source: &str) -> Vec<u8> {
    compile_qjs_m1_with_runtime_limits(
        source,
        Options::default(),
        RuntimeLimits {
            expression_depth: true,
            ..RuntimeLimits::default()
        },
    )
    .expect("source compiles")
}

fn instantiate(bytes: &[u8], limit: i32) -> WasmInstance {
    let mut module = WasmModule::from_bytes_with(bytes, Limits::default()).expect("loads");
    assert!(module.imports().iter().any(|import| {
        import.module == RUNTIME_LIMIT_MODULE && import.field == EXPRESSION_DEPTH_LIMIT_IMPORT
    }));
    module
        .bind_import_typed(
            RUNTIME_LIMIT_MODULE,
            EXPRESSION_DEPTH_LIMIT_IMPORT,
            move |args, _memory| {
                assert!(args.is_empty());
                Ok(vec![Val::I32(limit)])
            },
        )
        .expect("runtime limit binds");
    module.instantiate().expect("instantiates")
}

fn run(bytes: &[u8], limit: i32) -> Result<Value, GuestFault> {
    let mut instance = instantiate(bytes, limit);
    match instance.invoke_by_name("main", &Value::args(&[])) {
        Ok(values) => Ok(Value::returned(&values).expect("returns one JS value")),
        Err(_) => Err(guest_fault(&instance.memory().expect("memory zero"))
            .expect("a limit refusal records its reason")),
    }
}

fn number(value: f64) -> Result<Value, GuestFault> {
    Ok(Value::Number(value))
}

#[test]
fn exact_depth_passes_and_plus_one_refuses_from_the_same_bytes() {
    let bytes = compile("return 1 + (2 + (3 + 4));");
    assert_eq!(run(&bytes, 4), number(10.0));
    assert_eq!(run(&bytes, 3), Err(GuestFault::ExpressionDepthExhausted));
}

#[test]
fn calls_start_fresh_expression_chains_and_dead_branches_are_free() {
    let recursive = compile("function f(n) { return n ? f(n - 1) : 0; } return f(32);");
    assert_eq!(run(&recursive, 4), number(0.0));

    let callback = compile(
        "let f = function (x) { return 1 + (2 + (3 + x)); }; let a = [1]; let b = a.map(f); return b.length;",
    );
    assert_eq!(run(&callback, 4), number(1.0));
    assert_eq!(run(&callback, 3), Err(GuestFault::ExpressionDepthExhausted));

    for source in [
        "function f(x) { return 1 + (2 + (3 + x)); } return f(4);",
        "let f = function (x) { return 1 + (2 + (3 + x)); }; let g = f; return g(4);",
    ] {
        let bytes = compile(source);
        assert_eq!(
            run(&bytes, 4),
            number(10.0),
            "call path did not reset: {source}"
        );
        assert_eq!(
            run(&bytes, 3),
            Err(GuestFault::ExpressionDepthExhausted),
            "call path stopped charging its callee: {source}"
        );
    }

    for source in [
        "return false && (1 + (2 + (3 + 4)));",
        "return true ? 1 : (1 + (2 + (3 + 4)));",
    ] {
        assert!(
            run(&compile(source), 2).is_ok(),
            "dead branch ran: {source}"
        );
    }
}

#[test]
fn caught_throws_finally_and_return_leave_no_depth_debt() {
    for (source, expected) in [
        (
            "try { return 1 + null.x; } catch (error) { return 1 + 2; }",
            number(3.0),
        ),
        (
            "function fail() { throw 1; } try { return 1 + fail(); } catch (error) { return 1 + 2; }",
            number(3.0),
        ),
        (
            "let x = 0; try { x = 1 + 2; } finally { x = 3 + 4; } return x;",
            number(7.0),
        ),
        (
            "let x = 0; try { throw 1; } finally { x = 3 + 4; }",
            Err(GuestFault::UncaughtThrow),
        ),
        (
            "function f() { try { return 1; } finally { let x = 3 + 4; } } return f();",
            number(1.0),
        ),
        (
            "function f() { try { return 1; } finally { return 3 + 4; } } return f();",
            number(7.0),
        ),
    ] {
        let result = run(&compile(source), 3);
        assert_eq!(result, expected, "abrupt edge changed its result: {source}");
    }
}

#[test]
fn runtime_import_is_opt_in() {
    let ordinary_bytes = tinyvm_qjs::compile_qjs_m1("return 1 + 2;").expect("compiles");
    let explicit_default = compile_qjs_m1_with_runtime_limits(
        "return 1 + 2;",
        Options::default(),
        RuntimeLimits::default(),
    )
    .expect("explicit default compiles");
    assert_eq!(ordinary_bytes, explicit_default, "opt-out bytes changed");
    let ordinary = WasmModule::from_bytes_with(&ordinary_bytes, Limits::default()).expect("loads");
    assert!(!ordinary.imports().iter().any(|import| {
        import.module == RUNTIME_LIMIT_MODULE && import.field == EXPRESSION_DEPTH_LIMIT_IMPORT
    }));
}

#[test]
fn emitted_growth_is_recorded_at_eight_and_sixty_four_levels() {
    fn nested(levels: usize) -> String {
        let mut expression = "1".to_owned();
        for _ in 0..levels {
            expression = format!("1 + ({expression})");
        }
        format!("return {expression};")
    }

    let eight = compile(&nested(8));
    let sixty_four = compile(&nested(64));
    println!(
        "expression-depth G4: levels=8 bytes={} dynamic_checks={} levels=64 bytes={} dynamic_checks={}",
        eight.len(),
        17,
        sixty_four.len(),
        129
    );
    assert!(sixty_four.len() > eight.len());
}
