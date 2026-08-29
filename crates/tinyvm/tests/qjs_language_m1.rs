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

use tinyvm::{Limits, WasmFaultClass, WasmInstance, WasmModule};
use tinyvm_qjs::{CompileError, GuestFault, Value, compile_qjs_m1, guest_fault};

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

/// Run `source`, expect a String back, and read its bytes out of the instance
/// the call ran in -- a `Value::String` is a pointer into that memory, not text.
#[track_caller]
fn text_of(source: &str) -> String {
    let (out, instance) = run(source, &[]).unwrap_or_else(|e| panic!("{e}"));
    let Value::String(ptr) = out else {
        panic!("{source:?} answered {out:?}, want a String");
    };
    let view = instance.memory().expect("guest memory");
    let bytes: &[u8] = &view;
    let at = ptr as usize;
    let len = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("length header")) as usize;
    String::from_utf8(bytes[at + 4..at + 4 + len].to_vec()).expect("utf-8")
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
    // Two rows left this list on 2026-08-29, and **not** because anything was
    // decided today: `return 1.5;` has compiled since the DecimalLiteral
    // grammar landed at rev `ab29522`, and `return [1, 2];` since arrays
    // landed at `048bcf2`. The test has been red since then, and nobody saw
    // it, because the verification people actually typed was
    // `cargo test -p tinyvm-qjs` and this file lives in the *other* package.
    //
    // `prd/PRD.md`'s acceptance section now writes down a command set with
    // this package in it. That is the fix; this row edit is only the backlog
    // it uncovered.
    //
    // `return obj.field;` stays, and for a reason worth keeping straight:
    // property access landed too, so what is refused now is the undeclared
    // `obj` rather than the `.field`. Same sentence, different boundary --
    // which is exactly why this test asserts the *shape* of a refusal rather
    // than its text.
    for source in [
        "return 0x10;",
        "return 1_000;",
        "return 0777;",
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
/// An exhausted guest heap must not look like a broken script.
///
/// The two are genuinely indistinguishable to the VM. A refused `memory.grow`
/// is not a trap -- standard wasm returns `-1`
/// (`crates/tinyvm/src/wasm.rs`, `Op::MemoryGrow`) -- so the allocator has no
/// reason to carry and falls into an ordinary `unreachable`, byte for byte the
/// same instruction a conversion this milestone lacks executes. The host sees
/// one `WasmError`, one message, one `FaultClass` for both, and a host-side
/// heuristic ("memory is at its ceiling, so call it a budget problem") would
/// mislabel a script that is simply broken.
///
/// So the guest says which it was, in a word of its own memory, before it
/// goes. This test holds both halves: that the VM really cannot tell them
/// apart, and that `guest_fault` can.
#[test]
fn qjs_m1_tells_an_exhausted_heap_from_a_broken_script() {
    // One page and no more. The script allocates a 36-byte record per
    // iteration and discards it -- a bump heap has no free -- so it needs
    // about 288 KiB and is refused at 64 KiB. The step budget stays at its
    // default so that what stops the loop is the heap and not the fuel.
    let one_page = Limits {
        max_memory_pages: 1,
        ..Limits::default()
    };
    let exhausted = trap_in(
        "let s = \"abcdefghijklmnop\"; let i = 0; while (i < 8000) { s + s; i = i + 1; } return i;",
        one_page,
    );
    // A trap the heap had nothing to do with: `"x" + o` needs ToPrimitive
    // (ECMA-262 7.1.1), which reaches the `valueOf`/`toString` a prototype
    // would carry, and there is no prototype -- so the runtime traps rather
    // than fabricate a string. Same instruction, same budget, entirely
    // different cause. (It used to be `"a" + 1`; that one has an answer now.)
    let semantic = trap_in("const o = {}; return \"x\" + o;", one_page);

    // The VM cannot tell them apart, and does not pretend to.
    assert_eq!(exhausted.message, semantic.message);
    assert_eq!(exhausted.class, WasmFaultClass::Guest);
    assert_eq!(semantic.class, WasmFaultClass::Guest);
    assert_eq!(exhausted.ceiling, None);
    assert_eq!(semantic.ceiling, None);

    // The guest can, because it wrote it down on the way out.
    assert_eq!(exhausted.fault, Some(GuestFault::HeapExhausted));
    assert_eq!(semantic.fault, None);

    // And no host has to be watching: neither module imports anything.
    assert_eq!(exhausted.imports, 0);
    assert_eq!(semantic.imports, 0);

    // A call that simply succeeds leaves no fault behind, and a later call
    // does not inherit an earlier one: the entry point clears the word.
    let (_, instance) = run("return 1 + 1;", &[]).unwrap_or_else(|e| panic!("{e}"));
    let memory = instance.memory().expect("memory zero");
    assert_eq!(guest_fault(&memory), None);
}

/// What a host learns from one trapping call: the fault the VM reported, and
/// the guest's own account of it.
struct Trapped {
    message: &'static str,
    class: WasmFaultClass,
    ceiling: Option<tinyvm::WasmCeiling>,
    fault: Option<GuestFault>,
    imports: usize,
}

#[track_caller]
fn trap_in(source: &str, limits: Limits) -> Trapped {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, limits)
        .unwrap_or_else(|e| panic!("load gate rejected {source:?}: {}", e.message()));
    let imports = module.imports().len();
    let mut instance = module
        .instantiate()
        .unwrap_or_else(|e| panic!("instantiating {source:?}: {}", e.message()));
    let error = match instance.invoke_by_name("main", &Value::args(&[])) {
        Err(error) => error,
        Ok(values) => panic!("{source:?} was expected to trap, returned {values:?}"),
    };
    let memory = instance
        .memory()
        .unwrap_or_else(|e| panic!("reading memory after {source:?}: {}", e.message()));
    Trapped {
        message: error.message(),
        class: error.class(),
        ceiling: error.ceiling(),
        fault: guest_fault(&memory),
        imports,
    }
}

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

/// Objects: a literal, a dotted read, a computed read, an assignment that
/// creates a property and one that overwrites it — through the shipped face,
/// so the record is built on the guest's own heap and read back by the guest.
///
/// The shape is `fleet.js`'s, because that is what this milestone is for: a
/// namespace table assembled by assignment, a nested one reached by a chain of
/// dots, and a parameter object built from a function's arguments and read
/// back field by field.
#[test]
fn qjs_m1_builds_objects_and_reads_their_properties() {
    // A namespace table, assembled the way a binding library assembles one.
    number(
        "const fleet = {};
         fleet.ui = {};
         fleet.ui.tabs = {};
         fleet.ui.tabs.width = 40;
         fleet.ui.tabs.width = fleet.ui.tabs.width + 2;
         return fleet.ui.tabs.width;",
        42.0,
    );
    // A parameter object: built from arguments, read back by name, and by a
    // computed key that is the same property.
    number(
        "function params(tab, note) { return { tab: tab, note: note }; }
         const p = params(7, 8);
         return p.tab * 10 + p[\"note\"];",
        78.0,
    );
    // 10.1.8.1: a property that is not there reads as `undefined`, not a
    // fault, and `typeof` says so.
    assert_eq!(value("const o = { a: 1 }; return o.b;"), Value::Undefined);
    assert_eq!(
        text_of("const o = {}; return typeof o.missing;"),
        "undefined"
    );
    assert_eq!(text_of("return typeof {};"), "object");
    // 13.5.3: an Object's `typeof` is `"object"`, and 7.1.2 makes every one
    // of them truthy, `{}` included.
    number("const o = {}; if (o) { return 1; } return 0;", 1.0);
    // 7.2.15 step 4: strict equality on two Objects is reference identity,
    // which the V1 pair already answers without a new type test.
    assert_eq!(
        value("const a = {}; const b = a; return a === b;"),
        Value::Bool(true)
    );
    assert_eq!(value("return {} === {};"), Value::Bool(false));
}

/// Functions are values: stored in a binding and a property, passed, returned,
/// and called from wherever they ended up — through the shipped face, so the
/// call goes through the module's own funcref table and its adapters.
///
/// The shape is `fleet.js`'s again: a namespace table whose entries are
/// functions, reached by `o.a.b()`.
#[test]
fn qjs_m1_stores_passes_and_calls_a_function_value() {
    // A namespace table of methods, called through the property that holds
    // each one.
    number(
        "const fleet = {};
         fleet.tabs = {};
         fleet.tabs.list = function () { return 1; };
         fleet.tabs.count = function (n) { return n * 10; };
         return fleet.tabs.list() + fleet.tabs.count(4);",
        41.0,
    );
    // Passed as an argument and returned as a result.
    number(
        "function apply(f, x) { return f(x); }
         function twice(x) { return x + x; }
         return apply(twice, 21);",
        42.0,
    );
    number(
        "function pick() { return function () { return 7; }; }
         return pick()();",
        7.0,
    );
    // ECMA-262 8.6.1 and 13.3.8.1: too few arguments are `undefined`, too many
    // are evaluated and dropped.
    assert_eq!(
        value("const o = {}; o.m = function (a) { return a; }; return o.m();"),
        Value::Undefined
    );
    number(
        "const o = {}; o.m = function (a) { return a; }; return o.m(1, 2, 3);",
        1.0,
    );
    // 13.5.3 step 6 and 7.1.2: a function is `"function"` and is truthy.
    assert_eq!(
        text_of("const f = function () {}; return typeof f;"),
        "function"
    );
    number(
        "const f = function () {}; if (f) { return 1; } return 0;",
        1.0,
    );
    // 7.2.15 step 4, and 15.2.5: reading one function twice is one object,
    // and *evaluating* one function expression twice is two.
    assert_eq!(
        value("const f = function () {}; const g = f; return f === g;"),
        Value::Bool(true)
    );
    assert_eq!(
        value(
            "function mk() { return function () { return 1; }; }
             return mk() === mk();"
        ),
        Value::Bool(false)
    );
    // Calling something that is not a function is a TypeError in ECMA-262 and
    // there is no `throw` here, so it traps — at the tag test, before any
    // table is reached.
    assert!(run("const o = {}; return o.absent();", &[]).is_err());
    assert!(run("return (1)();", &[]).is_err());
}

/// The three ECMA-262 conversions between Numbers and Strings, through the
/// shipped face: Number::toString (6.1.6.1.20), StringToNumber (7.1.4.1) and
/// String relational comparison by code unit (7.2.13).
#[test]
fn qjs_m1_converts_between_numbers_and_strings() {
    // 13.15.3: either operand a String makes `+` concatenation, and both sides
    // run ToString.
    assert_eq!(text_of("return \"a\" + 1;"), "a1");
    assert_eq!(text_of("return 1 + \"a\";"), "1a");
    assert_eq!(
        text_of("return \"\" + true + null + undefined;"),
        "truenullundefined"
    );
    // 6.1.6.1.20 step 5: the *shortest* decimal that reads back as the same
    // binary64, which is the whole reason this is Dragon4 and not a printf.
    assert_eq!(
        text_of("return \"\" + (1 / 10 + 2 / 10);"),
        "0.30000000000000004"
    );
    assert_eq!(text_of("return \"\" + (1 / 3);"), "0.3333333333333333");
    // Steps 1 to 4, before step 5 is reached.
    assert_eq!(text_of("return \"\" + (0 / 0);"), "NaN");
    assert_eq!(text_of("return \"\" + (1 / 0);"), "Infinity");
    assert_eq!(text_of("return \"\" + (0 - 1 / 0);"), "-Infinity");
    // 7.1.4.1, the whole `StringNumericLiteral` grammar: whitespace, a sign, a
    // hex literal, the empty string, and a string the grammar does not accept.
    number("return \"1\" - 1;", 0.0);
    number("return \"3\" * \"4\";", 12.0);
    number("return -\"  42  \";", -42.0);
    number("return +\"0x1f\";", 31.0);
    number("return +\"\";", 0.0);
    // No NaN equals itself, so this is asserted with `is_nan` and not `==`.
    let Value::Number(x) = value("return +\"nope\";") else {
        panic!("a Number back");
    };
    assert!(x.is_nan(), "a string the grammar does not accept is NaN");
    // 7.2.14 steps 4 and 5: `==` between a Number and a String converts.
    assert_eq!(value("return 1 == \"1\";"), Value::Bool(true));
    assert_eq!(value("return 1 === \"1\";"), Value::Bool(false));
    // 7.2.13 step 3: both operands Strings is the code-unit comparison, and a
    // mixed pair is not — so `"10" < "9"` and `"10" < 9` disagree.
    assert_eq!(value("return \"a\" < \"b\";"), Value::Bool(true));
    assert_eq!(value("return \"10\" < \"9\";"), Value::Bool(true));
    assert_eq!(value("return \"10\" < 9;"), Value::Bool(false));
    // 7.1.19 over 7.1.17: every Number names a property, not just the
    // integers.
    number(
        "const o = {}; const k = 1 / 2; o[k] = 7; return o[\"0.5\"];",
        7.0,
    );
    // The one conversion still missing: 7.1.1 ToPrimitive needs the
    // `valueOf`/`toString` a prototype would carry, and there is no prototype.
    assert!(run("const o = {}; return \"x\" + o;", &[]).is_err());
}

/// A `Bytes` host result whose announced length is not a length is refused.
///
/// The two-pass read asks the host how many bytes it has, allocates that much
/// and asks for the copy. Checking the copy against the announcement is not
/// enough on its own: it compares one host answer to another, so a host that
/// answers `-1` twice — which is exactly what a raw contract returns for "your
/// buffer is too small" — used to pass it, and produced a String whose length
/// header read `0xFFFFFFFF`. Worse, `__alloc` rounds sizes with `(n + 3) & -4`,
/// which is negative for a negative `n`, so repeating it walked the bump
/// pointer backwards below `DATA_ORIGIN` and over the fault word — making the
/// guest report a budget problem for a script that merely had a type error.
///
/// So the announcement is checked for being a length before it becomes a size.
#[test]
fn qjs_m1_refuses_a_host_length_that_is_not_a_length() {
    use std::cell::Cell;
    use std::rc::Rc;

    use tinyvm::{Val, WasmError};
    use tinyvm_qjs::{HostFn, HostResult, Names, Options, compile_qjs_m1_with};

    fn module_for(source: &str, len: Rc<Cell<i32>>) -> tinyvm::WasmModule {
        let table = vec![HostFn {
            name: "reply".to_string(),
            module: "sys".to_string(),
            field: "reply".to_string(),
            params: Vec::new(),
            result: HostResult::Bytes {
                length: "reply_len".to_string(),
            },
        }];
        let wasm = compile_qjs_m1_with(
            source,
            Options {
                names: Names::Declared(table),
            },
        )
        .unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
        let mut module = WasmModule::from_bytes_with(&wasm, Limits::default())
            .unwrap_or_else(|e| panic!("load gate: {}", e.message()));
        let announce = Rc::clone(&len);
        module
            .bind_import_typed("sys", "reply_len", move |_a, _m| {
                Ok(vec![Val::I32(announce.get())])
            })
            .expect("bind sys.reply_len");
        let copy = Rc::clone(&len);
        module
            .bind_import_typed("sys", "reply", move |args, memory| {
                let [Val::I32(dst), Val::I32(cap)] = args else {
                    return Err(WasmError::Trap("sys.reply wants (i32, i32)"));
                };
                // An honest host writes exactly what it was asked for.
                if *cap > 0 {
                    let at = *dst as usize;
                    memory[at..at + *cap as usize].fill(b'z');
                }
                let _ = copy.get();
                Ok(vec![Val::I32(*cap)])
            })
            .expect("bind sys.reply");
        module
    }

    // The lie: a negative length, answered consistently by both passes.
    let len = Rc::new(Cell::new(-1));
    let module = module_for("return reply();", Rc::clone(&len));
    let mut instance = module.instantiate().expect("instantiate");
    let error = instance
        .invoke_by_name("main", &[])
        .expect_err("a negative announced length must trap, not build a String");
    // Not a budget problem: nothing the host can raise makes -1 a length.
    let memory = instance.memory().expect("guest memory");
    assert_eq!(
        guest_fault(&memory),
        None,
        "a bogus length is not `HeapExhausted` (trap was: {})",
        error.message()
    );
    assert_eq!(error.class(), WasmFaultClass::Guest);

    // Repeating it cannot walk the bump pointer below `DATA_ORIGIN`: the third
    // allocation must never be handed the fault word's own address.
    let len = Rc::new(Cell::new(-8));
    let module = module_for(
        "var a = reply(); var b = reply(); var c = reply(); return c;",
        Rc::clone(&len),
    );
    let mut instance = module.instantiate().expect("instantiate");
    match instance.invoke_by_name("main", &[]) {
        Err(_) => {}
        Ok(vals) => {
            let Value::String(ptr) = Value::returned(&vals).expect("a V1 pair") else {
                panic!("want a String, got {vals:?}");
            };
            panic!("a String record was placed at {ptr}, below DATA_ORIGIN = 8");
        }
    }

    // The honest cases are untouched: zero is the empty String, and a real
    // length still round-trips.
    let len = Rc::new(Cell::new(0));
    let module = module_for("return reply();", Rc::clone(&len));
    let mut instance = module.instantiate().expect("instantiate");
    let vals = instance.invoke_by_name("main", &[]).expect("zero-length");
    let Value::String(ptr) = Value::returned(&vals).expect("a V1 pair") else {
        panic!("want a String back");
    };
    let view = instance.memory().expect("guest memory");
    let bytes: &[u8] = &view;
    let at = ptr as usize;
    assert_eq!(
        u32::from_le_bytes(bytes[at..at + 4].try_into().expect("length header")),
        0
    );

    let len = Rc::new(Cell::new(3));
    let module = module_for("return reply();", Rc::clone(&len));
    let mut instance = module.instantiate().expect("instantiate");
    let vals = instance.invoke_by_name("main", &[]).expect("three bytes");
    let Value::String(ptr) = Value::returned(&vals).expect("a V1 pair") else {
        panic!("want a String back");
    };
    let view = instance.memory().expect("guest memory");
    let bytes: &[u8] = &view;
    let at = ptr as usize;
    let n = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("length header")) as usize;
    assert_eq!(&bytes[at + 4..at + 4 + n], b"zzz");
}

/// A conditional expression, and a `throw` that finds the handler ECMA-262
/// names -- the two constructs `fleet.js` stopped at.
///
/// tinyvm has no wasm exception handling, so this is not a `try` instruction:
/// it is a flag plus a check after every call that could raise one. What the
/// product sentence promises is the *language* behaviour, and that is what is
/// executed here.
#[test]
fn qjs_m1_lowers_a_conditional_and_a_try() {
    // 13.14: only the taken branch evaluates, and the value is the branch's.
    number("return 1 ? 2 : 3;", 2.0);
    number("return 0 ? 2 : 3;", 3.0);
    number("let n = 0; const _ = false ? n = 1 : n = 2; return n;", 2.0);
    // Right-associative, and the test runs exactly once.
    number("return 0 ? 1 : 1 ? 2 : 3;", 2.0);
    // The idiom `fleet.js` opens with: a default argument.
    assert_eq!(
        text_of("function f(p) { return p === undefined ? \"{}\" : p; } return f();"),
        "{}"
    );

    // 14.14/14.15: a throw crosses frames to the nearest handler, the value it
    // carries is any JavaScript value, and the handler's own completion is the
    // statement's.
    number(
        "function g() { throw 7; } function f() { try { g(); } catch (e) { return e; } return 0; } return f();",
        7.0,
    );
    assert_eq!(
        text_of("try { throw \"boom\"; } catch (e) { return \"caught \" + e; }"),
        "caught boom"
    );
    // 14.15.3: a finalizer runs on all three paths, an abrupt one replaces
    // what was pending, and a normal one contributes no value at all.
    number(
        "let n = 0; try { throw 1; } catch (e) { n = e; } finally { n = n + 10; } return n;",
        11.0,
    );
    number(
        "function f() { try { return 1; } finally { return 2; } } return f();",
        2.0,
    );
    number("try { 1; } finally { 2; }", 1.0);
}

/// `JSON.parse` and `JSON.stringify`, ECMA-262 25.5, reached from source text.
///
/// `JSON` is an ordinary object holding two ordinary function values, and the
/// name is the one binding this engine supplies -- a script that declares its
/// own shadows it, because the scope walk runs first.
#[test]
fn qjs_m1_parses_and_prints_json() {
    assert_eq!(text_of("return typeof JSON;"), "object");
    assert_eq!(text_of("return typeof JSON.parse;"), "function");
    assert_eq!(
        text_of("return JSON.stringify({ a: 1, b: \"x\" });"),
        "{\"a\":1,\"b\":\"x\"}"
    );
    // 25.5.2.2: `undefined` and a function are not JSON, and neither is a
    // non-finite Number.
    assert_eq!(
        text_of("return JSON.stringify({ a: undefined, b: 1 });"),
        "{\"b\":1}"
    );
    assert_eq!(text_of("return JSON.stringify(1 / 0);"), "null");
    // 25.5.1, and the round trip.
    number("return JSON.parse(\"1\");", 1.0);
    number(
        "return JSON.parse(JSON.stringify({ a: { b: 2 } })).a.b;",
        2.0,
    );
    // A text that is not JSON raises a catchable throw, which is the shape
    // `fleet.js` wraps every broker answer in.
    number("try { JSON.parse(\"nope\"); } catch (e) { return 1; }", 1.0);
    // A script's own binding of the name wins outright.
    assert_eq!(
        text_of(
            "const JSON = { stringify: function (v) { return \"mine\"; } }; return JSON.stringify(1);"
        ),
        "mine"
    );
}

/// An uncaught `throw` is a third thing at the fault word, distinct from a
/// budget failure and from a broken script -- so a host can report it rather
/// than raise a memory ceiling or blame the author.
///
/// The channel is also **per call**: it is a module global, so an uncaught
/// throw that left it raised would poison every later call on that instance,
/// and a persistent instance called repeatedly is what the downstream
/// embedder does.
#[test]
fn qjs_m1_tells_an_uncaught_throw_from_a_broken_script() {
    let wasm = compile_qjs_m1("if ($0 === 1) { throw \"boom\"; } return 42;").expect("compiles");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("clears the gate");
    let mut instance = module.instantiate().expect("instantiates");

    let error = instance
        .invoke_by_name("main", &Value::args(&[Value::Number(1.0)]))
        .expect_err("an uncaught throw traps");
    assert_eq!(error.class(), WasmFaultClass::Guest);
    assert_eq!(
        guest_fault(&instance.memory().expect("guest memory")),
        Some(GuestFault::UncaughtThrow),
        "the host must be able to tell a throw from a broken script"
    );

    // The very next call on the same instance is unaffected, and the word
    // describes it and not the call before it.
    let vals = instance
        .invoke_by_name("main", &Value::args(&[Value::Number(0.0)]))
        .expect("the channel is cleared on the way in");
    assert_eq!(Value::returned(&vals), Ok(Value::Number(42.0)));
    assert_eq!(guest_fault(&instance.memory().expect("guest memory")), None);

    // A genuinely broken script is a different answer, which is the whole
    // point of the code.
    let (_, broken) = (
        (),
        WasmModule::from_bytes_with(
            &compile_qjs_m1("const u = undefined; return u.a;").expect("compiles"),
            Limits::default(),
        )
        .expect("clears the gate"),
    );
    let mut broken = broken.instantiate().expect("instantiates");
    assert!(broken.invoke_by_name("main", &Value::args(&[])).is_err());
    assert_eq!(guest_fault(&broken.memory().expect("guest memory")), None);
}

/// The acceptance library, driven end to end: a `fleet.js` wrapper calls out
/// through a declared raw host door, a broker answers with JSON text,
/// `JSON.parse` turns it into an Object, and the caller reads a property off
/// it.
///
/// This is the product sentence for the whole series -- every capability it
/// added has to hold at once for the last line to read `true`: the object
/// literal, the function value in a property, the call through it, the
/// conditional that supplies the default argument, `JSON.stringify` on the way
/// out, the raw two-pass `Bytes` result, and `JSON.parse` on the way back.
/// `tinyvm-qjs`'s `tests/fleet_acceptance.rs` owns the exhaustive version;
/// this one is the shipped-face edge.
///
/// One reduction, named rather than glossed: `fleet.js` reaches its door as
/// `__host.fleet_call(...)`, a property call on a **free** name, and no host
/// can answer a V1 pair with an Object -- `Value` has no Object variant. So
/// the embedder supplies `__host` in a short prelude of its own, which is what
/// the first two lines below are.
#[test]
fn qjs_m1_runs_a_fleet_wrapper_through_a_declared_host_door() {
    use std::cell::RefCell;
    use std::rc::Rc;

    use tinyvm::{Val, WasmError};
    use tinyvm_qjs::{HostFn, HostParam, HostResult, Names, Options, compile_qjs_m1_with};

    let table = vec![
        HostFn {
            name: "fleet_call".to_string(),
            module: "fleet".to_string(),
            field: "call".to_string(),
            params: vec![HostParam::StrPtrLen, HostParam::StrPtrLen],
            result: HostResult::I32,
        },
        HostFn {
            name: "fleet_result".to_string(),
            module: "fleet".to_string(),
            field: "result".to_string(),
            params: Vec::new(),
            result: HostResult::Bytes {
                length: "result_len".to_string(),
            },
        },
    ];
    // The prelude, then `fleet.js`'s own `call()`, then one of its
    // twenty-nine wrappers, spelled exactly as the library spells it.
    let source = "
        const __host = { fleet_call: function (op, p) { return door(op, p); } };
        function door(op, p) { fleet_call(op, p); return fleet_result(); }

        function call(opId, params) {
          const resultJson = __host.fleet_call(opId, params === undefined ? \"{}\" : params);
          try {
            return JSON.parse(resultJson);
          } catch (_err) {
            return resultJson;
          }
        }

        const fleet = {};
        fleet.tabs = {};
        fleet.tabs.set_note = function (tabId, note) {
          return call(\"tabs.set-note\", JSON.stringify({ tab: tabId, note: note }));
        };
        return fleet.tabs.set_note(\"t3\", \"ship it\").ok;";
    let wasm = compile_qjs_m1_with(
        source,
        Options {
            names: Names::Declared(table),
        },
    )
    .unwrap_or_else(|e| panic!("compiling the wrapper: {e}"));
    let mut module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .unwrap_or_else(|e| panic!("load gate: {}", e.message()));

    let asked: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&asked);
    module
        .bind_import_typed("fleet", "call", move |args, memory| {
            let [Val::I32(op), Val::I32(op_len), Val::I32(p), Val::I32(p_len)] = args else {
                return Err(WasmError::Trap("fleet.call wants four i32"));
            };
            let text = |at: i32, len: i32| {
                String::from_utf8(memory[at as usize..(at + len) as usize].to_vec())
                    .expect("the guest hands over utf-8")
            };
            sink.borrow_mut()
                .push((text(*op, *op_len), text(*p, *p_len)));
            Ok(vec![Val::I32(0)])
        })
        .expect("bind fleet.call");
    let answer: &[u8] = br#"{"ok":true,"tab":"t3"}"#;
    module
        .bind_import_typed("fleet", "result_len", move |_args, _memory| {
            Ok(vec![Val::I32(answer.len() as i32)])
        })
        .expect("bind fleet.result_len");
    module
        .bind_import_typed("fleet", "result", move |args, memory| {
            let [Val::I32(dst), Val::I32(cap)] = args else {
                return Err(WasmError::Trap("fleet.result wants (i32, i32)"));
            };
            if (answer.len() as i32) > *cap {
                return Ok(vec![Val::I32(-1)]);
            }
            let at = *dst as usize;
            memory[at..at + answer.len()].copy_from_slice(answer);
            Ok(vec![Val::I32(answer.len() as i32)])
        })
        .expect("bind fleet.result");

    let mut instance = module.instantiate().expect("instantiate");
    let vals = instance
        .invoke_by_name("main", &[])
        .unwrap_or_else(|e| panic!("trap in the wrapper: {}", e.message()));
    assert_eq!(
        Value::returned(&vals),
        Ok(Value::Bool(true)),
        "the wrapper must read `ok` off the parsed answer"
    );
    // What the broker was actually sent -- the operation id and the params
    // JSON this engine wrote. The key is `tab`, which is the whole reason the
    // wrapper exists.
    assert_eq!(
        *asked.borrow(),
        vec![(
            "tabs.set-note".to_string(),
            r#"{"tab":"t3","note":"ship it"}"#.to_string()
        )]
    );
}
