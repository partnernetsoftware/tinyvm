//! The allocation waterline is observable only through an explicit diagnostic
//! compile. Ordinary modules keep the allocator global private.

use tinyvm::{Limits, Val, WasmModule};
use tinyvm_qjs::{
    HostFn, HostParam, HostResult, Names, Options, Value, compile_qjs_m1,
    compile_qjs_m1_with_allocation_probe, compile_qjs_m1_with_modules_and_allocation_probe,
};

const PROBE: &str = "__tinyvm_qjs_heap_ptr";
const PARSE_BYTES: &str = "__tinyvm_qjs_json_parse_bytes";
const STRINGIFY_BYTES: &str = "__tinyvm_qjs_json_stringify_bytes";
const IMMEDIATE_HOST_ARGUMENT_BYTES: &str = "__tinyvm_qjs_immediate_stringify_host_argument_bytes";

fn i32_result(values: Vec<Val>) -> i32 {
    match values.as_slice() {
        [Val::I32(value)] => *value,
        other => panic!("unexpected allocation probe result: {other:?}"),
    }
}

fn read(instance: &mut tinyvm::WasmInstance, name: &str) -> i32 {
    i32_result(instance.invoke_by_name(name, &[]).expect("probe runs"))
}

#[test]
fn ordinary_compilation_does_not_publish_the_probe() {
    let wasm = compile_qjs_m1("return JSON.parse(\"[1,2,3]\").length;").expect("compiles");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    assert!(instance.invoke_by_name(PROBE, &[]).is_err());
    assert!(instance.invoke_by_name(PARSE_BYTES, &[]).is_err());
    assert!(instance.invoke_by_name(STRINGIFY_BYTES, &[]).is_err());
    assert!(
        instance
            .invoke_by_name(IMMEDIATE_HOST_ARGUMENT_BYTES, &[])
            .is_err()
    );
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

    let initial = read(&mut instance, PROBE);
    assert_eq!(
        read(&mut instance, PROBE),
        initial,
        "the probe is read-only"
    );
    let out = instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect("main runs");
    assert_eq!(Value::returned(&out), Ok(Value::Number(3.0)));
    let after = read(&mut instance, PROBE);
    assert!(after > initial, "JSON.parse must move the bump waterline");
    assert_eq!(
        read(&mut instance, PROBE),
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
    let mut waterlines = vec![read(&mut instance, PROBE)];
    for _ in 0..3 {
        let out = instance
            .invoke_by_name("main", &Value::args(&[]))
            .expect("main runs");
        assert_eq!(Value::returned(&out), Ok(Value::Number(2.0)));
        waterlines.push(read(&mut instance, PROBE));
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
    let initial = read(&mut instance, PROBE);
    instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect("main runs");
    assert!(read(&mut instance, PROBE) > initial);
}

#[test]
fn json_operation_counters_partition_their_own_allocations() {
    let wasm = compile_qjs_m1_with_allocation_probe(
        "return JSON.stringify(JSON.parse(\"[{\\\"name\\\":\\\"alpha\\\"}]\")).length;",
        Options::default(),
    )
    .expect("compiles");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let start = read(&mut instance, PROBE);
    assert_eq!(read(&mut instance, PARSE_BYTES), 0);
    assert_eq!(read(&mut instance, STRINGIFY_BYTES), 0);
    instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect("main runs");
    let parse = read(&mut instance, PARSE_BYTES);
    let stringify = read(&mut instance, STRINGIFY_BYTES);
    let allocated = read(&mut instance, PROBE) - start;
    assert!(parse > 0, "parse allocation must be attributed");
    assert!(stringify > 0, "stringify allocation must be attributed");
    assert!(
        parse + stringify <= allocated,
        "operation counters cannot exceed whole-call allocation"
    );
}

fn declared_sink(params: Vec<HostParam>, result: HostResult) -> Options {
    Options {
        names: Names::Declared(vec![HostFn {
            name: "sink".to_owned(),
            module: "probe".to_owned(),
            field: "sink".to_owned(),
            params,
            result,
        }]),
    }
}

#[track_caller]
fn immediate_host_argument_bytes(source: &str, options: Options) -> i32 {
    let result = match &options.names {
        Names::Declared(hosts) => hosts[0].result.clone(),
        _ => panic!("probe helper requires one declared host"),
    };
    let wasm = compile_qjs_m1_with_allocation_probe(source, options).expect("compiles");
    let mut module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    for import in module.imports().to_vec() {
        let answer = if import.field == "sink" {
            result.clone()
        } else {
            HostResult::I32
        };
        module
            .bind_import_typed(&import.module, &import.field, move |_args, _memory| {
                Ok(match answer {
                    HostResult::Void => Vec::new(),
                    HostResult::I32 | HostResult::Bytes { .. } => vec![Val::I32(0)],
                    HostResult::F64 => vec![Val::F64(0.0)],
                })
            })
            .expect("binds probe host");
    }
    let mut instance = module.instantiate().expect("instantiates");
    instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect("main runs");
    read(&mut instance, IMMEDIATE_HOST_ARGUMENT_BYTES)
}

#[test]
fn immediate_stringify_host_argument_attributes_gross_allocated_bytes() {
    for (name, source) in [
        (
            "script local",
            r#"let payload={text:"visible"}; sink(JSON.stringify(payload)); return 0;"#,
        ),
        (
            "function parameter",
            r#"function f(payload){sink(JSON.stringify(payload));} f({text:"visible"}); return 0;"#,
        ),
        (
            "script global read",
            r#"let payload={text:"visible"}; function f(){sink(JSON.stringify(payload));} f(); return 0;"#,
        ),
        (
            "captured read",
            r#"function outer(payload){function inner(){sink(JSON.stringify(payload));} inner();} outer({text:"visible"}); return 0;"#,
        ),
    ] {
        let bytes = immediate_host_argument_bytes(
            source,
            declared_sink(vec![HostParam::StrPtrLen], HostResult::Void),
        );
        assert!(bytes > 0, "{name} producer-consumer region must allocate");
    }

    let i32_bytes = immediate_host_argument_bytes(
        r#"let payload={text:"visible"}; return sink(JSON.stringify(payload));"#,
        declared_sink(vec![HostParam::StrPtrLen], HostResult::I32),
    );
    assert!(i32_bytes > 0, "an I32 host result remains eligible");
    let f64_bytes = immediate_host_argument_bytes(
        r#"let payload={text:"visible"}; return sink(JSON.stringify(payload));"#,
        declared_sink(vec![HostParam::StrPtrLen], HostResult::F64),
    );
    assert!(f64_bytes > 0, "an F64 host result remains eligible");
}

#[test]
fn diagnostic_compile_cannot_perturb_ordinary_module_bytes() {
    let source = "let o={a:1}; sink(JSON.stringify(o)); return 0;";
    let options = declared_sink(vec![HostParam::StrPtrLen], HostResult::Void);
    let before = tinyvm_qjs::compile_qjs_m1_with(source, options.clone()).expect("ordinary A");
    let diagnostic =
        compile_qjs_m1_with_allocation_probe(source, options.clone()).expect("diagnostic compile");
    let after = tinyvm_qjs::compile_qjs_m1_with(source, options).expect("ordinary B");
    assert_eq!(
        before, after,
        "diagnostic lowering has no shared compiler state"
    );
    assert_ne!(
        before, diagnostic,
        "the opt-in diagnostic module must carry its exports"
    );
}

#[test]
fn immediate_stringify_host_argument_probe_is_fail_closed_on_negative_shapes() {
    let one_string = || declared_sink(vec![HostParam::StrPtrLen], HostResult::Void);
    let cases = [
        (
            "stored alias",
            "let o={a:1}; let s=JSON.stringify(o); sink(s); return 0;",
            one_string(),
        ),
        (
            "stringify alias",
            "let o={a:1}; const f=JSON.stringify; sink(f(o)); return 0;",
            one_string(),
        ),
        (
            "multiple host arguments",
            "let o={a:1}; sink(JSON.stringify(o), \"tail\"); return 0;",
            declared_sink(
                vec![HostParam::StrPtrLen, HostParam::StrPtrLen],
                HostResult::Void,
            ),
        ),
        (
            "bytes host result",
            "let o={a:1}; return sink(JSON.stringify(o)).length;",
            declared_sink(
                vec![HostParam::StrPtrLen],
                HostResult::Bytes {
                    length: "sink_len".to_owned(),
                },
            ),
        ),
        (
            "lexical try",
            "let o={a:1}; try { sink(JSON.stringify(o)); } catch (e) {} return 0;",
            one_string(),
        ),
        (
            "lexical catch",
            "let o={a:1}; try { throw 1; } catch (e) { sink(JSON.stringify(o)); } return 0;",
            one_string(),
        ),
        (
            "lexical finally",
            "let o={a:1}; try {} finally { sink(JSON.stringify(o)); } return 0;",
            one_string(),
        ),
        (
            "stringify spacing argument",
            "let o={a:1}; sink(JSON.stringify(o, null, 2)); return 0;",
            one_string(),
        ),
        (
            "non-binding stringify input",
            "sink(JSON.stringify({a:1})); return 0;",
            one_string(),
        ),
    ];
    for (name, source, options) in cases {
        assert_eq!(
            immediate_host_argument_bytes(source, options),
            0,
            "negative shape {name:?} must not be attributed"
        );
    }
}
