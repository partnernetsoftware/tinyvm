//! `Names::Declared`: the door a `.qjs` script reaches a host capability
//! through, with arguments.
//!
//! # What this file is testing, and what it deliberately is not
//!
//! The compiler unwraps; the door does not learn about JS values. An embedder
//! declares raw wasm functions -- module, field, signature -- and says how a
//! JavaScript argument maps onto raw parameters. The emitted module then
//! imports exactly those raw functions, so a hand-written `.wasm` guest and a
//! compiled `.qjs` guest reach the same host through the same import table.
//! That is the whole point: making the door speak this engine's two-word value
//! representation would break every hand-written guest and would leak one
//! language's value shape into a boundary meant to serve any guest.
//!
//! So the host bound below is written the way a raw guest's host is written:
//! `&[Val]` of `i32`s and a `&mut [u8]` of guest memory. Nothing in it knows
//! what a JS String is.
//!
//! The declaration table here is deliberately generic -- `sys.invoke`,
//! `sys.reply_len`, `sys.reply`, `sys.log`. This crate has no business
//! vocabulary in it and must not acquire any; an embedder's own names are the
//! embedder's.
//!
//! Everything runs for real: compile -> tinyvm's load gate -> instantiate ->
//! `invoke_by_name("main", ...)`. "It compiled" is a claim only where not
//! compiling is the thing being asserted.

use std::cell::RefCell;
use std::rc::Rc;

use tinyvm::{Limits, Val, ValueType, WasmError, WasmInstance, WasmModule};
use tinyvm_qjs::{
    Boundary, CompileError, HostFn, HostParam, HostResult, Names, Options, Value,
    compile_qjs_m1_with,
};

// =========================================================================
// The declaration table under test
// =========================================================================

/// The shape a variable-length host door has: a call that takes two byte
/// strings and answers with a status code, and a two-pass reader for whatever
/// bytes that call produced. Plus a one-way sink that takes a byte string and
/// answers with nothing.
///
/// ```text
/// sys.invoke(op_ptr, op_len, params_ptr, params_len) -> i32
/// sys.reply_len() -> i32
/// sys.reply(dst_ptr, dst_len) -> i32
/// sys.log(ptr, len) -> ()
/// ```
fn table() -> Vec<HostFn> {
    vec![
        HostFn {
            name: "invoke".to_string(),
            module: "sys".to_string(),
            field: "invoke".to_string(),
            params: vec![HostParam::StrPtrLen, HostParam::StrPtrLen],
            result: HostResult::I32,
        },
        HostFn {
            name: "reply".to_string(),
            module: "sys".to_string(),
            field: "reply".to_string(),
            params: Vec::new(),
            result: HostResult::Bytes {
                length: "reply_len".to_string(),
            },
        },
        HostFn {
            name: "log".to_string(),
            module: "sys".to_string(),
            field: "log".to_string(),
            params: vec![HostParam::StrPtrLen],
            result: HostResult::Void,
        },
    ]
}

fn declared(hosts: Vec<HostFn>) -> Options {
    Options {
        names: Names::Declared(hosts),
    }
}

// =========================================================================
// Harness
// =========================================================================

#[derive(Debug, Clone, PartialEq)]
enum Out {
    Undefined,
    Null,
    Number(f64),
    Bool(bool),
    Str(String),
}

/// What the raw host saw and what it will answer with. Shared by `Rc` because
/// every bound import is a `'static` closure.
#[derive(Debug, Default)]
struct Seen {
    logged: Vec<String>,
    invoked: Vec<(String, String)>,
    /// The bytes `sys.reply` hands back.
    reply: Vec<u8>,
    /// A deliberately wrong length from `sys.reply_len`, to prove the
    /// rewrapping checks rather than trusts.
    lie_by: i32,
}

fn compile(source: &str, options: Options) -> Result<Vec<u8>, CompileError> {
    compile_qjs_m1_with(source, options)
}

#[track_caller]
fn module(source: &str, options: Options) -> WasmModule {
    let wasm = compile(source, options).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    WasmModule::from_bytes_with(&wasm, Limits::default())
        .unwrap_or_else(|e| panic!("load gate rejected {source:?}: {}", e.message()))
}

/// Read a `(ptr, len)` pair out of the guest's linear memory as text.
fn borrowed(memory: &[u8], ptr: i32, len: i32) -> String {
    let at = ptr as usize;
    String::from_utf8(memory[at..at + len as usize].to_vec()).expect("utf-8")
}

/// Compile with the table, bind the raw host, run `main`.
#[track_caller]
fn run_with(source: &str, seen: &Rc<RefCell<Seen>>, hosts: Vec<HostFn>) -> Result<Out, String> {
    let wasm =
        compile(source, declared(hosts)).map_err(|e| format!("compiling {source:?}: {e}"))?;
    let mut module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .map_err(|e| format!("load gate rejected {source:?}: {}", e.message()))?;

    let bound = module.imports().to_vec();
    let has = |field: &str| bound.iter().any(|i| i.module == "sys" && i.field == field);

    if has("log") {
        let seen = Rc::clone(seen);
        module
            .bind_import_typed("sys", "log", move |args, memory| {
                let [Val::I32(ptr), Val::I32(len)] = args else {
                    return Err(WasmError::Trap("sys.log wants (i32, i32)"));
                };
                seen.borrow_mut().logged.push(borrowed(memory, *ptr, *len));
                Ok(Vec::new())
            })
            .map_err(|e| e.message().to_string())?;
    }
    if has("invoke") {
        let seen = Rc::clone(seen);
        module
            .bind_import_typed("sys", "invoke", move |args, memory| {
                let [Val::I32(op), Val::I32(op_len), Val::I32(p), Val::I32(p_len)] = args else {
                    return Err(WasmError::Trap("sys.invoke wants four i32"));
                };
                seen.borrow_mut()
                    .invoked
                    .push((borrowed(memory, *op, *op_len), borrowed(memory, *p, *p_len)));
                Ok(vec![Val::I32(0)])
            })
            .map_err(|e| e.message().to_string())?;
    }
    if has("reply_len") {
        let seen = Rc::clone(seen);
        module
            .bind_import_typed("sys", "reply_len", move |_args, _memory| {
                let seen = seen.borrow();
                Ok(vec![Val::I32(seen.reply.len() as i32 + seen.lie_by)])
            })
            .map_err(|e| e.message().to_string())?;
    }
    if has("reply") {
        let seen = Rc::clone(seen);
        module
            .bind_import_typed("sys", "reply", move |args, memory| {
                let [Val::I32(dst), Val::I32(cap)] = args else {
                    return Err(WasmError::Trap("sys.reply wants (i32, i32)"));
                };
                let bytes = seen.borrow().reply.clone();
                if (bytes.len() as i32) > *cap {
                    // The contract's negative answer: the destination is too
                    // small for what there is to write.
                    return Ok(vec![Val::I32(-1)]);
                }
                let at = *dst as usize;
                memory[at..at + bytes.len()].copy_from_slice(&bytes);
                Ok(vec![Val::I32(bytes.len() as i32)])
            })
            .map_err(|e| e.message().to_string())?;
    }

    let mut instance = module
        .instantiate()
        .map_err(|e| format!("instantiating {source:?}: {}", e.message()))?;
    let vals = instance
        .invoke_by_name("main", &[])
        .map_err(|e| format!("trap in {source:?}: {}", e.message()))?;
    Ok(match Value::returned(&vals)? {
        Value::Undefined => Out::Undefined,
        Value::Null => Out::Null,
        Value::Number(x) => Out::Number(x),
        Value::Bool(b) => Out::Bool(b),
        Value::String(ptr) => Out::Str(read_string(&instance, ptr)?),
    })
}

#[track_caller]
fn run(source: &str, seen: &Rc<RefCell<Seen>>) -> Out {
    run_with(source, seen, table()).unwrap_or_else(|e| panic!("{e}"))
}

fn read_string(instance: &WasmInstance, ptr: i32) -> Result<String, String> {
    let view = instance
        .memory()
        .map_err(|e| format!("no guest memory: {}", e.message()))?;
    let bytes: &[u8] = &view;
    let at = ptr as usize;
    let header = bytes
        .get(at..at + 4)
        .ok_or_else(|| format!("string header at {ptr} is out of bounds"))?;
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let body = bytes
        .get(at + 4..at + 4 + len)
        .ok_or_else(|| format!("string body at {ptr} (len {len}) is out of bounds"))?;
    String::from_utf8(body.to_vec()).map_err(|_| "string is not valid UTF-8".to_string())
}

#[track_caller]
fn refuse(source: &str, hosts: Vec<HostFn>) -> CompileError {
    match compile(source, declared(hosts)) {
        Ok(bytes) => panic!(
            "{source:?} compiled to {} bytes; expected a diagnostic",
            bytes.len()
        ),
        Err(e) => e,
    }
}

/// `ValueType` has no `Debug` -- the core is fmt-free -- so a signature is
/// compared as the text a wasm reader would write.
fn spell(ty: ValueType) -> &'static str {
    match ty {
        ValueType::I32 => "i32",
        ValueType::I64 => "i64",
        ValueType::F32 => "f32",
        ValueType::F64 => "f64",
        _ => "other",
    }
}

/// Every import in order, as `"module.field(params) -> results"`.
fn signature(m: &WasmModule) -> Vec<String> {
    m.imports()
        .iter()
        .enumerate()
        .map(|(position, desc)| {
            let params: Vec<&str> = (0..desc.n_params)
                .map(|k| spell(m.import_parameter_type(position, k).expect("a param type")))
                .collect();
            let results: Vec<&str> = (0..desc.n_results)
                .map(|k| spell(m.import_result_type(position, k).expect("a result type")))
                .collect();
            format!(
                "{}.{}({}) -> {}",
                desc.module,
                desc.field,
                params.join(", "),
                results.join(", ")
            )
        })
        .collect()
}

// =========================================================================
// The import table is exactly what the embedder declared
// =========================================================================

/// The signatures are the raw ones, not V1 pairs. A String argument becomes
/// two `i32` parameters -- pointer then length -- and a `Bytes` result becomes
/// a second import beside the first. Nothing here is `(i32, i64)`.
#[test]
fn a_declared_host_imports_the_raw_signature_the_embedder_wrote() {
    let m = module(
        "log(\"a\"); let s = invoke(\"op\", \"{}\"); return reply();",
        declared(table()),
    );
    assert_eq!(
        signature(&m),
        vec![
            "sys.invoke(i32, i32, i32, i32) -> i32",
            "sys.reply_len() -> i32",
            "sys.reply(i32, i32) -> i32",
            "sys.log(i32, i32) -> ",
        ]
    );
}

/// Declaration order, not use order and not alphabetical: an embedder reading
/// its own table can predict the import list without reading the script.
#[test]
fn the_import_order_is_the_declaration_order() {
    let m = module(
        "let s = reply(); log(\"a\"); return invoke(\"o\", \"p\");",
        declared(table()),
    );
    let fields: Vec<&str> = m.imports().iter().map(|i| i.field.as_str()).collect();
    assert_eq!(fields, vec!["invoke", "reply_len", "reply", "log"]);
}

/// A declaration the script never mentions is not an import, so a host does
/// not have to bind a capability the guest cannot reach.
#[test]
fn only_the_declarations_a_script_uses_become_imports() {
    let m = module("log(\"a\"); return 1;", declared(table()));
    let fields: Vec<&str> = m.imports().iter().map(|i| i.field.as_str()).collect();
    assert_eq!(fields, vec!["log"]);

    let none = module("return 1;", declared(table()));
    assert!(none.imports().is_empty());
}

// =========================================================================
// Unwrapping: a JS value becomes raw parameters
// =========================================================================

/// A JS String argument arrives at the host as the address of its UTF-8 bytes
/// and their length -- the bytes themselves, not the `[len][bytes]` record the
/// engine keeps them in.
#[test]
fn a_string_argument_arrives_as_a_pointer_and_a_length() {
    let seen = Rc::new(RefCell::new(Seen::default()));
    assert_eq!(
        run(
            "log(\"hello\"); log(\"\"); return invoke(\"spawn\", \"{\\\"n\\\":1}\");",
            &seen
        ),
        Out::Number(0.0)
    );
    let seen = seen.borrow();
    assert_eq!(seen.logged, vec!["hello".to_string(), String::new()]);
    assert_eq!(
        seen.invoked,
        vec![("spawn".to_string(), "{\"n\":1}".to_string())]
    );
}

/// A computed String works the same way: concatenation puts the bytes on the
/// bump heap rather than in the data segment, and the host sees no difference.
#[test]
fn a_computed_string_argument_reaches_the_host_too() {
    let seen = Rc::new(RefCell::new(Seen::default()));
    run("let a = \"he\"; log(a + \"llo\"); return 0;", &seen);
    assert_eq!(seen.borrow().logged, vec!["hello".to_string()]);
}

/// Arguments are evaluated left to right, once each, before any of them is
/// unwrapped -- so an argument with a side effect happens in source order.
#[test]
fn arguments_are_evaluated_in_source_order() {
    let seen = Rc::new(RefCell::new(Seen::default()));
    run(
        "let x = \"\"; invoke((x = x + \"a\"), (x = x + \"b\")); log(x); return 0;",
        &seen,
    );
    let seen = seen.borrow();
    assert_eq!(seen.invoked, vec![("a".to_string(), "ab".to_string())]);
    assert_eq!(seen.logged, vec!["ab".to_string()]);
}

/// A Number parameter, in both widths the declaration can ask for.
#[test]
fn a_number_argument_lowers_to_the_width_the_declaration_names() {
    let numbers = vec![
        HostFn {
            name: "as_i32".to_string(),
            module: "sys".to_string(),
            field: "as_i32".to_string(),
            params: vec![HostParam::I32],
            result: HostResult::I32,
        },
        HostFn {
            name: "as_f64".to_string(),
            module: "sys".to_string(),
            field: "as_f64".to_string(),
            params: vec![HostParam::F64],
            result: HostResult::F64,
        },
    ];
    let m = module("as_i32(1); return as_f64(1);", declared(numbers.clone()));
    assert_eq!(
        signature(&m),
        vec!["sys.as_i32(i32) -> i32", "sys.as_f64(f64) -> f64"]
    );

    let doubled = |source: &str| -> Result<Out, String> {
        let wasm = compile(source, declared(numbers.clone()))
            .map_err(|e| format!("compiling {source:?}: {e}"))?;
        let mut m = WasmModule::from_bytes_with(&wasm, Limits::default())
            .map_err(|e| e.message().to_string())?;
        for field in ["as_i32", "as_f64"] {
            if m.imports().iter().any(|i| i.field == field) {
                m.bind_import_typed("sys", field, |args, _| match args {
                    [Val::I32(v)] => Ok(vec![Val::I32(v * 2)]),
                    [Val::F64(v)] => Ok(vec![Val::F64(v * 2.0)]),
                    _ => Err(WasmError::Trap("unexpected arity")),
                })
                .map_err(|e| e.message().to_string())?;
            }
        }
        let mut instance = m.instantiate().map_err(|e| e.message().to_string())?;
        let vals = instance
            .invoke_by_name("main", &[])
            .map_err(|e| format!("trap in {source:?}: {}", e.message()))?;
        Ok(match Value::returned(&vals)? {
            Value::Number(x) => Out::Number(x),
            other => panic!("{source:?} gave {other:?}"),
        })
    };
    assert_eq!(doubled("return as_i32(21);").unwrap(), Out::Number(42.0));
    assert_eq!(doubled("return as_i32(-21);").unwrap(), Out::Number(-42.0));
    // `f64` is the JS Number itself, so nothing is lost on the way over.
    assert_eq!(doubled("return as_f64(1 / 4);").unwrap(), Out::Number(0.5));

    // An `i32` parameter refuses every Number that is not one. Truncating
    // would hand the host a number the script never wrote.
    for source in [
        "return as_i32(1 / 2);",
        "return as_i32(0 / 0);",
        "return as_i32(1 / 0);",
        "return as_i32(2147483647 + 1);",
    ] {
        let e = doubled(source).expect_err("an i32 parameter is not a rounding");
        assert!(e.contains("trap in"), "{source:?} failed with {e}");
    }
}

// =========================================================================
// Rewrapping: a raw result becomes a JS value
// =========================================================================

/// The two-pass byte result: ask the length, bump-allocate, ask for the copy,
/// build the String. On the engine's own heap -- there is one allocator, and
/// this uses it rather than inventing a second.
#[test]
fn a_byte_result_is_rebuilt_as_a_javascript_string() {
    let seen = Rc::new(RefCell::new(Seen::default()));
    seen.borrow_mut().reply = b"{\"ok\":true}".to_vec();
    assert_eq!(
        run("return reply();", &seen),
        Out::Str("{\"ok\":true}".to_string())
    );

    // An empty answer is a String too, not `undefined`.
    seen.borrow_mut().reply.clear();
    assert_eq!(run("return reply();", &seen), Out::Str(String::new()));

    // And it is an ordinary String once it is back: `.length` is not in the
    // subset yet, but concatenation and equality are.
    seen.borrow_mut().reply = b"ab".to_vec();
    assert_eq!(
        run("return reply() + \"c\";", &seen),
        Out::Str("abc".into())
    );
    assert_eq!(run("return reply() === \"ab\";", &seen), Out::Bool(true));
    assert_eq!(
        run("return typeof reply();", &seen),
        Out::Str("string".into())
    );
}

/// The rewrapping checks what the copy pass wrote instead of trusting it. A
/// host that reports one length and writes another -- or answers with the
/// contract's negative "your buffer is too small" -- is a trap, not a String
/// with a fabricated tail.
#[test]
fn a_short_or_negative_copy_traps_rather_than_fabricating_a_string() {
    for lie_by in [1, -1] {
        let seen = Rc::new(RefCell::new(Seen::default()));
        seen.borrow_mut().reply = b"abcd".to_vec();
        seen.borrow_mut().lie_by = lie_by;
        let e = run_with("return reply();", &seen, table())
            .expect_err("a length that does not match the copy must trap");
        assert!(e.contains("trap in"), "lie_by {lie_by} gave {e}");
    }
}

/// A `Void` declaration is a statement, and its value is `undefined` -- the
/// ECMA-262 value a function that returns nothing gives back.
#[test]
fn a_void_declaration_evaluates_to_undefined() {
    let seen = Rc::new(RefCell::new(Seen::default()));
    assert_eq!(run("return log(\"a\");", &seen), Out::Undefined);
    assert_eq!(
        run("return typeof log(\"a\");", &seen),
        Out::Str("undefined".into())
    );
}

/// A declared call is an ordinary expression: it works inside a nested
/// function, inside a loop, and many times in one frame. The scratch locals a
/// call needs are per-frame and recycled, so the tenth call costs what the
/// first did.
#[test]
fn a_declared_call_works_wherever_an_expression_does() {
    let seen = Rc::new(RefCell::new(Seen::default()));
    seen.borrow_mut().reply = b"r".to_vec();
    assert_eq!(
        run(
            "function shout(s) { log(s); return invoke(s, reply()); }
             let n = 0;
             for (let i = 0; i < 3; i++) { n = n + shout(\"x\"); }
             return n;",
            &seen
        ),
        Out::Number(0.0)
    );
    let seen = seen.borrow();
    assert_eq!(seen.logged, vec!["x".to_string(); 3]);
    assert_eq!(seen.invoked, vec![("x".to_string(), "r".to_string()); 3]);
}

// =========================================================================
// A wrong type is a clean typed failure
// =========================================================================

/// Statically known: a literal of the wrong type never reaches the host, and
/// the diagnostic names the parameter and both types.
#[test]
fn a_statically_wrong_argument_type_is_a_compile_diagnostic() {
    for (source, got, name) in [
        ("log(1);", "a Number", "log"),
        ("log(true);", "a Boolean", "log"),
        ("log(null);", "Null", "log"),
        ("log(undefined);", "Undefined", "log"),
        ("log(-1);", "a Number", "log"),
        ("log(+1);", "a Number", "log"),
        ("log(!1);", "a Boolean", "log"),
        ("let n = 0; log(n++);", "a Number", "log"),
        ("invoke(\"op\", 2);", "a Number", "invoke"),
    ] {
        let e = refuse(source, table());
        assert_eq!(e.boundary, Boundary::ThirdBinding, "{source:?}");
        assert!(
            e.message.starts_with("this engine "),
            "{source:?} gave {:?}",
            e.message
        );
        // The diagnostic names what was passed, what was wanted, and which
        // argument of which host function -- all four, because any one of them
        // alone leaves the reader hunting.
        for part in [got, "a String", name] {
            assert!(
                e.message.contains(part),
                "{source:?} gave {:?}, which does not name {part:?}",
                e.message
            );
        }
        assert!(
            e.offset > 0,
            "{source:?} must point at the argument, not at byte 0"
        );
    }
    // The second argument is named as the second, not as the first.
    assert!(
        refuse("invoke(\"op\", 2);", table())
            .message
            .contains("2 of"),
        "the diagnostic must say which argument"
    );
    // The controls: a String-typed expression the compiler *can* settle is
    // not refused, so the check above is about the type and not about the
    // shape of the expression.
    for source in ["log(\"a\");", "log(typeof 1);", "log(\"a\" + \"b\");"] {
        compile(source, declared(table()))
            .unwrap_or_else(|e| panic!("{source:?} should compile: {e}"));
    }
}

/// Not statically known: the type test is emitted and the guest traps. A
/// dynamic language cannot settle this at compile time and must not pretend to.
#[test]
fn a_dynamically_wrong_argument_type_traps() {
    let seen = Rc::new(RefCell::new(Seen::default()));
    for source in [
        "let x = 1; log(x); return 0;",
        "let x = 1; if (x) { x = 2; } else { x = \"s\"; } log(x); return 0;",
        "function f() { return 1; } log(f()); return 0;",
    ] {
        let e = run_with(source, &seen, table()).expect_err("a wrong type must trap");
        assert!(e.contains("trap in"), "{source:?} gave {e}");
    }
    // The same shape with the right type does not trap, so the test above is
    // about the type and not about the shape.
    assert_eq!(
        run("let x = \"s\"; log(x); return 0;", &seen),
        Out::Number(0.0)
    );
    assert_eq!(seen.borrow().logged, vec!["s".to_string()]);
}

// =========================================================================
// The table is the world, and says so when it is asked for more
// =========================================================================

#[test]
fn a_name_no_declaration_covers_is_refused_and_the_table_is_listed() {
    let e = refuse("return nope();", table());
    assert_eq!(e.boundary, Boundary::ThirdBinding);
    assert!(e.message.starts_with("this engine "), "{}", e.message);
    assert!(e.message.contains("nope"), "{}", e.message);
    // The reader is told what there *is*, not only what there is not.
    for declared in ["invoke", "reply", "log"] {
        assert!(
            e.message.contains(declared),
            "{:?} does not list {declared}",
            e.message
        );
    }
}

#[test]
fn a_call_with_the_wrong_number_of_arguments_is_refused() {
    for source in [
        "log();",
        "log(\"a\", \"b\");",
        "log;",
        "return invoke(\"a\");",
    ] {
        let e = refuse(source, table());
        assert_eq!(e.boundary, Boundary::ThirdBinding, "{source:?}");
        assert!(
            e.message.starts_with("this engine "),
            "{source:?} gave {:?}",
            e.message
        );
    }
}

/// Assigning to a host name is refused under `Declared` exactly as it is under
/// `HostImport`: an import is not a place a value can be put.
#[test]
fn a_host_name_is_not_a_place_to_assign_to() {
    let e = refuse("log = 1;", table());
    assert_eq!(e.boundary, Boundary::ThirdBinding);
    assert!(e.message.contains("host"), "{}", e.message);
}

/// An embedder's own mistakes are refused too, and named as the engine's view
/// of the table it was handed.
#[test]
fn a_table_that_cannot_be_an_import_table_is_refused() {
    let twice = vec![
        HostFn {
            name: "log".to_string(),
            module: "sys".to_string(),
            field: "log".to_string(),
            params: vec![HostParam::StrPtrLen],
            result: HostResult::Void,
        },
        HostFn {
            name: "log".to_string(),
            module: "sys".to_string(),
            field: "other".to_string(),
            params: Vec::new(),
            result: HostResult::Void,
        },
    ];
    let e = refuse("log(\"a\");", twice);
    assert!(e.message.contains("log"), "{}", e.message);

    // A `Bytes` result is two imports, so its length import cannot share the
    // field name of the read import: they have different signatures.
    let collide = vec![HostFn {
        name: "reply".to_string(),
        module: "sys".to_string(),
        field: "reply".to_string(),
        params: Vec::new(),
        result: HostResult::Bytes {
            length: "reply".to_string(),
        },
    }];
    let e = refuse("return reply();", collide);
    assert!(e.message.contains("reply"), "{}", e.message);
}

// =========================================================================
// The other two `Names` are untouched
// =========================================================================

/// `Names::HostImport` still means a `js.<name>` import in V1 pairs. The
/// declared table is a third mode beside it, not a replacement for it.
#[test]
fn the_zero_argument_host_import_mode_still_works() {
    let m = module(
        "return g() + h;",
        Options {
            names: Names::HostImport,
        },
    );
    let fields: Vec<&str> = m.imports().iter().map(|i| i.field.as_str()).collect();
    assert_eq!(fields, vec!["g", "h"]);
    // V1 pairs, not raw: two results per JS value.
    assert_eq!(signature(&m)[0], "js.g() -> i32, i64");
}

/// The M0 pipeline is one `i32` expression and has neither JS values nor a
/// linear memory, so a declared table cannot be reached from it. Saying so is
/// better than emitting something that loads and then means nothing.
#[test]
fn the_m0_pipeline_says_it_cannot_reach_a_declared_table() {
    let e = tinyvm_qjs::compile_qjs_with("log", declared(table()))
        .expect_err("M0 has no door for a declared table");
    assert_eq!(e.boundary, Boundary::ThirdBinding);
    assert!(e.message.starts_with("this engine "), "{}", e.message);
    assert!(e.message.contains("compile_qjs_m1_with"), "{}", e.message);
}
