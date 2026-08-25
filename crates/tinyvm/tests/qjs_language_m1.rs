//! PRD-mapped acceptance for the M1 language layer, executed through the
//! shipped face: `compile_qjs_m1` -> tinyvm's load gate -> `invoke_by_name`.
//!
//! `eval_qjs_skin.rs` owns the M0 skin, whose world is one `i32` expression and
//! the two `eval_wasm` bindings. This file owns the milestone above it, where a
//! script is statements over the V1 value representation and one JavaScript
//! value is a `(tag: i32, payload: i64)` pair. The two entry points exist side
//! by side on purpose, so their acceptance does too.
//!
//! These tests execute the product sentences; the exhaustive corpus lives in
//! `tinyvm-qjs`'s own suite.

use tinyvm::{Limits, WasmInstance, WasmModule};
use tinyvm_qjs::{CompileError, Value, compile_qjs_m1};

/// Compile, load, instantiate, call `main`. Every stage's refusal comes back as
/// a sentence, so a failing test says *which* stage refused.
fn run(source: &str, args: &[Value]) -> Result<(Value, WasmInstance), String> {
    let wasm = compile_qjs_m1(source).map_err(|e| format!("compiling {source:?}: {e}"))?;
    let module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .map_err(|e| format!("load gate rejected {source:?}: {}", e.message()))?;
    let mut instance = module
        .instantiate()
        .map_err(|e| format!("instantiating {source:?}: {}", e.message()))?;
    let vals = instance
        .invoke_by_name("main", &Value::args(args))
        .map_err(|e| format!("trap in {source:?}: {}", e.message()))?;
    let value = Value::returned(&vals)?;
    Ok((value, instance))
}

#[track_caller]
fn value(source: &str) -> Value {
    run(source, &[]).unwrap_or_else(|e| panic!("{e}")).0
}

#[track_caller]
fn number(source: &str, want: f64) {
    assert_eq!(value(source), Value::Number(want), "{source:?}");
}

#[track_caller]
fn refuse(source: &str) -> CompileError {
    match compile_qjs_m1(source) {
        Ok(bytes) => panic!(
            "{source:?} compiled to {} bytes; expected a refusal",
            bytes.len()
        ),
        Err(e) => e,
    }
}

/// The V1 boundary: one JavaScript value is two wasm values, in both
/// directions, for every type the milestone has.
#[test]
fn qjs_m1_moves_v1_values_across_the_call_boundary() {
    assert_eq!(value("return undefined;"), Value::Undefined);
    assert_eq!(value("return null;"), Value::Null);
    assert_eq!(value("return true;"), Value::Bool(true));
    // Binary64, not `i32`: neither of these is representable as a wrapped
    // 32-bit integer, and neither traps.
    number("return 2147483647 + 1;", 2_147_483_648.0);
    number("return 1 / 0;", f64::INFINITY);
    // A string is a pointer into the instance's own memory, which is why
    // reading it needs the instance the call ran in.
    let (out, instance) = run("return \"tiny\" + \"vm\";", &[]).unwrap_or_else(|e| panic!("{e}"));
    let Value::String(ptr) = out else {
        panic!("want a String, got {out:?}");
    };
    let view = instance.memory().expect("guest memory");
    let bytes: &[u8] = &view;
    let at = ptr as usize;
    let len = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("length header")) as usize;
    assert_eq!(&bytes[at + 4..at + 4 + len], b"tinyvm");
    // `$N` in, one value out.
    let (doubled, _) =
        run("return $0 * 2;", &[Value::Number(21.0)]).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(doubled, Value::Number(42.0));
}

/// Declarations, functions and control flow, in one script whose answer
/// depends on all three.
#[test]
fn qjs_m1_lowers_declarations_functions_and_control_flow() {
    number(
        "function fib(n) { if (n < 2) { return n; } return fib(n - 1) + fib(n - 2); }
         const start = 10;
         let total = 0;
         for (let i = 0; i < 3; i++) { total = total + fib(start + i); }
         var seen = total;
         while (seen > 1000) { seen = seen - 1000; }
         return seen;",
        // fib(10..12) = 55 + 89 + 144 = 288.
        288.0,
    );
    // A named function expression may take parameters, and its own name does
    // not displace them (ECMA-262 15.2.5).
    number(
        "const f = function g(a, b) { return a * 10 + b; }; return f(1, 2);",
        12.0,
    );
}

/// Nesting is bounded by a diagnostic rather than by the native stack.
///
/// Recursive descent runs on the native stack and a stack overflow aborts the
/// process, which for a host compiling untrusted `.qjs` is worse than any wrong
/// answer: there is no caller left to hear about it. So the depth is a number
/// the compiler keeps, and reaching it is a refusal like any other.
#[test]
fn qjs_m1_bounds_nesting_with_a_diagnostic_not_an_abort() {
    for source in [
        format!("return {}1{};", "(".repeat(20_000), ")".repeat(20_000)),
        format!("{}return 1;{}", "{".repeat(20_000), "}".repeat(20_000)),
        format!("return {};", vec!["1"; 20_000].join("+")),
        format!("{}return 1;", "if (1) ".repeat(20_000)),
    ] {
        let error = refuse(&source);
        assert!(
            error.message.contains("nested deeper"),
            "expected a nesting-limit diagnostic, got: {}",
            error.message
        );
    }
    // The shallow end of the same shapes still compiles and still answers.
    number("return ((((1))));", 1.0);
    number(&format!("return {};", vec!["1"; 100].join("+")), 100.0);
}

/// Every refusal speaks for the engine. A subset this small rejects mostly
/// perfectly good scripts, so a sentence blaming the author would be a lie --
/// and a sentence that names no boundary leaves the reader guessing where the
/// engine stops.
#[test]
fn qjs_m1_rejections_name_the_engine_boundary() {
    for source in [
        "return 1.5;",
        "return 0x10;",
        "return 1_000;",
        "return 0777;",
        "return [1, 2];",
        "return obj.field;",
        "class C {}",
        "return x; let x = 1;",
        "if (1) let x = 1; return 2;",
    ] {
        let error = refuse(source);
        assert!(
            error.message.starts_with("this engine "),
            "{source:?}: {}",
            error.message
        );
        assert!(
            error.offset <= source.len(),
            "{source:?}: offset {} is past the source",
            error.offset
        );
    }
}

/// The host door a script reaches with arguments: an embedder declares raw
/// wasm functions, the compiler unwraps JavaScript values onto them, and the
/// module that comes out imports exactly what a hand-written `.wasm` guest
/// would.
///
/// That is the product sentence being executed here: **the compiler unwraps;
/// the door does not learn about JavaScript values.** The host bound below
/// speaks `i32` and a byte slice and nothing else -- no V1 pair crosses it --
/// which is why the same host can stand behind a hand-written guest.
#[test]
fn qjs_m1_reaches_a_declared_host_door_with_arguments() {
    use std::cell::RefCell;
    use std::rc::Rc;

    use tinyvm::{Val, WasmError};
    use tinyvm_qjs::{HostFn, HostParam, HostResult, Names, Options, compile_qjs_m1_with};

    // `sys.echo(ptr, len) -> ()` takes a string; `sys.said_len() -> i32` and
    // `sys.said(dst, cap) -> i32` hand one back in the two passes a wasm
    // function needs to return a slice.
    let table = vec![
        HostFn {
            name: "echo".to_string(),
            module: "sys".to_string(),
            field: "echo".to_string(),
            params: vec![HostParam::StrPtrLen],
            result: HostResult::Void,
        },
        HostFn {
            name: "said".to_string(),
            module: "sys".to_string(),
            field: "said".to_string(),
            params: Vec::new(),
            result: HostResult::Bytes {
                length: "said_len".to_string(),
            },
        },
    ];
    let source = "echo(\"ping\"); return said() + \"!\";";
    let wasm = compile_qjs_m1_with(
        source,
        Options {
            names: Names::Declared(table),
        },
    )
    .unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));

    let mut module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .unwrap_or_else(|e| panic!("load gate: {}", e.message()));

    // Raw signatures, in declaration order. Not `(i32, i64)` anywhere.
    let imports: Vec<(String, usize, usize)> = module
        .imports()
        .iter()
        .map(|i| (format!("{}.{}", i.module, i.field), i.n_params, i.n_results))
        .collect();
    assert_eq!(
        imports,
        vec![
            ("sys.echo".to_string(), 2, 0),
            ("sys.said_len".to_string(), 0, 1),
            ("sys.said".to_string(), 2, 1),
        ]
    );

    let heard: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&heard);
    module
        .bind_import_typed("sys", "echo", move |args, memory| {
            let [Val::I32(ptr), Val::I32(len)] = args else {
                return Err(WasmError::Trap("sys.echo wants (i32, i32)"));
            };
            let at = *ptr as usize;
            let text = String::from_utf8(memory[at..at + *len as usize].to_vec())
                .map_err(|_| WasmError::Trap("sys.echo was handed invalid utf-8"))?;
            sink.borrow_mut().push(text);
            Ok(Vec::new())
        })
        .expect("bind sys.echo");
    let answer = b"pong";
    module
        .bind_import_typed("sys", "said_len", move |_args, _memory| {
            Ok(vec![Val::I32(answer.len() as i32)])
        })
        .expect("bind sys.said_len");
    module
        .bind_import_typed("sys", "said", move |args, memory| {
            let [Val::I32(dst), Val::I32(cap)] = args else {
                return Err(WasmError::Trap("sys.said wants (i32, i32)"));
            };
            if (answer.len() as i32) > *cap {
                return Ok(vec![Val::I32(-1)]);
            }
            let at = *dst as usize;
            memory[at..at + answer.len()].copy_from_slice(answer);
            Ok(vec![Val::I32(answer.len() as i32)])
        })
        .expect("bind sys.said");

    let mut instance = module.instantiate().expect("instantiate");
    let vals = instance
        .invoke_by_name("main", &[])
        .unwrap_or_else(|e| panic!("trap in {source:?}: {}", e.message()));
    let Value::String(ptr) = Value::returned(&vals).expect("a V1 pair") else {
        panic!("the declared door must hand back a JavaScript String");
    };
    let view = instance.memory().expect("guest memory");
    let bytes: &[u8] = &view;
    let at = ptr as usize;
    let len = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("length header")) as usize;
    assert_eq!(&bytes[at + 4..at + 4 + len], b"pong!");
    assert_eq!(*heard.borrow(), vec!["ping".to_string()]);
}
