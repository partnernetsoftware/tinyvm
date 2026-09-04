//! The allocation waterline is observable only through an explicit diagnostic
//! compile. Ordinary modules keep the allocator global private.

use tinyvm::{Limits, Val, WasmModule};
use tinyvm_qjs::{
    Options, Value, compile_qjs_m1, compile_qjs_m1_with_allocation_probe,
    compile_qjs_m1_with_modules_and_allocation_probe,
};

const PROBE: &str = "__tinyvm_qjs_heap_ptr";

fn heap_ptr(values: Vec<Val>) -> i32 {
    match values.as_slice() {
        [Val::I32(value)] => *value,
        other => panic!("unexpected allocation probe result: {other:?}"),
    }
}

#[test]
fn ordinary_compilation_does_not_publish_the_probe() {
    let wasm = compile_qjs_m1("return JSON.parse(\"[1,2,3]\").length;").expect("compiles");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    assert!(instance.invoke_by_name(PROBE, &[]).is_err());
}

#[test]
fn probe_reads_the_waterline_without_moving_it() {
    let wasm = compile_qjs_m1_with_allocation_probe(
        "return JSON.parse(\"[1,2,3]\").length;",
        Options::default(),
    )
    .expect("compiles");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");

    let initial = heap_ptr(instance.invoke_by_name(PROBE, &[]).expect("probe runs"));
    assert_eq!(
        heap_ptr(instance.invoke_by_name(PROBE, &[]).expect("probe runs")),
        initial,
        "the probe is read-only"
    );
    let out = instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect("main runs");
    assert_eq!(Value::returned(&out), Ok(Value::Number(3.0)));
    let after = heap_ptr(instance.invoke_by_name(PROBE, &[]).expect("probe runs"));
    assert!(after > initial, "JSON.parse must move the bump waterline");
    assert_eq!(
        heap_ptr(instance.invoke_by_name(PROBE, &[]).expect("probe runs")),
        after,
        "reading still allocates nothing"
    );
}

#[test]
fn primitive_json_result_exposes_a_repeatable_dead_allocation_slope() {
    // There is no script binding and the returned value is a Number. Once
    // `main` returns, no guest root can name any object/string allocated by
    // JSON.parse in this call, so the whole positive delta is dead rather
    // than merely "memory that grew".
    let wasm = compile_qjs_m1_with_allocation_probe(
        "return JSON.parse(\"[{\\\"name\\\":\\\"alpha\\\"},{\\\"name\\\":\\\"beta\\\"}]\").length;",
        Options::default(),
    )
    .expect("compiles");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let mut waterlines = vec![heap_ptr(
        instance.invoke_by_name(PROBE, &[]).expect("probe runs"),
    )];
    for _ in 0..3 {
        let out = instance
            .invoke_by_name("main", &Value::args(&[]))
            .expect("main runs");
        assert_eq!(Value::returned(&out), Ok(Value::Number(2.0)));
        waterlines.push(heap_ptr(
            instance.invoke_by_name(PROBE, &[]).expect("probe runs"),
        ));
    }
    let deltas: Vec<i32> = waterlines
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .collect();
    assert!(deltas[0] > 0, "the operation must allocate: {waterlines:?}");
    // The first entry also publishes the lazily-created JSON namespace into
    // its engine global. That object is live and is exactly why "all growth is
    // dead" may only be claimed after warm-up. Calls two and three have the
    // same roots and therefore isolate the operation's dead suffix.
    assert_eq!(deltas[1], deltas[2], "warm calls have one stable slope");
    assert!(
        deltas[0] >= deltas[1],
        "warm-up cannot allocate less state than a warm call"
    );
    println!(
        "JSON.parse warm-up bytes={}, primitive-result dead bytes/warm-call={}",
        deltas[0], deltas[1]
    );
}

#[test]
fn module_resolving_diagnostic_compile_has_the_same_probe() {
    let wasm = compile_qjs_m1_with_modules_and_allocation_probe(
        "import * as m from \"fixture\"; return JSON.stringify(m.value()).length;",
        Options::default(),
        &|name| (name == "fixture").then(|| "export function value() { return [1, 2]; }".into()),
    )
    .expect("compiles");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let initial = heap_ptr(instance.invoke_by_name(PROBE, &[]).expect("probe runs"));
    instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect("main runs");
    assert!(heap_ptr(instance.invoke_by_name(PROBE, &[]).expect("probe runs")) > initial);
}
