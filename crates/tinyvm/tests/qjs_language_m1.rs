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
