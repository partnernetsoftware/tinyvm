//! Adversarial pressure on indirect dispatch: the one place this compiler
//! stops being able to check anything statically.
//!
//! A direct call names a function index the encoder wrote down. A call through
//! a *value* names an `i64` payload the program computed, wrapped to `i32` and
//! handed to `call_indirect`. Everything between the tag test and the table
//! bound is the engine's word, so this file spends its effort on the two
//! outcomes that would be **silent**:
//!
//! * a payload that is not a table index at all, and
//! * a payload that is a *valid* index for the wrong function.
//!
//! The second is the worse one and the harder to see, because a jump to a real
//! function returns a real value. So the mis-index probes never assert "it did
//! not crash": each one is a **fingerprint** -- either a weighted sum that no
//! permutation of the callees can reproduce, or a trace recorded by a host
//! import naming which function actually ran -- and one of them cross-checks
//! the emitted element section against the emitted `name` section, so a
//! mis-index is visible in the bytes as well as in the answer.
//!
//! The rule for a verdict, throughout: a **panic is always a bug**; a typed
//! compile refusal or a clean wasm trap is correct. Nothing here is allowed to
//! be "it returned something".
//!
//! Everything runs for real: compile -> tinyvm's load gate -> instantiate ->
//! `invoke_by_name("main")`.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use tinyvm::{Limits, Val, WasmError, WasmInstance, WasmModule};
use tinyvm_qjs::{
    GuestFault, HostFn, HostParam, HostResult, Names, Options, Value, compile_qjs_m1,
    compile_qjs_m1_with, guest_fault,
};

// =========================================================================
// Harness
// =========================================================================

/// The function tag, as `repr.rs` numbers it. Restated from outside the crate
/// on purpose: `repr` is crate-private, so this is the only place the contract
/// can be checked from, and a renumbering that forgot this file would show up
/// as a failure rather than as a silently different attack.
const TAG_FUNCTION: i32 = 6;

#[derive(Debug, Clone, PartialEq)]
enum Out {
    Undefined,
    Null,
    Number(f64),
    Bool(bool),
    Str(String),
}

fn build(source: &str) -> Vec<u8> {
    compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"))
}

/// Compile, clear the load gate, instantiate, and call `main` with a **raw**
/// argument list.
///
/// Raw rather than [`Value::args`], because half of this file's job is to hand
/// the guest a pair the public `Value` cannot spell -- there is no
/// `Value::Function`, which is exactly why an injected `(TAG_FUNCTION, n)` is
/// the sharpest probe available for what `call_indirect` does with a payload
/// nobody in the compiler produced.
fn attempt_raw(source: &str, args: &[Val]) -> Result<(WasmInstance, Vec<Val>), String> {
    let wasm = compile_qjs_m1(source).map_err(|e| format!("compiling {source:?}: {e}"))?;
    let module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .map_err(|e| format!("load gate rejected {source:?}: {}", e.message()))?;
    let mut instance = module
        .instantiate()
        .map_err(|e| format!("instantiating {source:?}: {}", e.message()))?;
    let vals = instance
        .invoke_by_name("main", args)
        .map_err(|e| format!("trap in {source:?}: {}", e.message()))?;
    Ok((instance, vals))
}

fn attempt(source: &str) -> Result<(WasmInstance, Vec<Val>), String> {
    attempt_raw(source, &Value::args(&[]))
}

#[track_caller]
fn run(source: &str) -> Out {
    let (instance, vals) = attempt(source).unwrap_or_else(|e| panic!("{e}"));
    decode(&instance, &vals, source)
}

#[track_caller]
fn decode(instance: &WasmInstance, vals: &[Val], source: &str) -> Out {
    match Value::returned(vals)
        .unwrap_or_else(|e| panic!("{source:?}: cannot read the result back: {e}"))
    {
        Value::Undefined => Out::Undefined,
        Value::Null => Out::Null,
        Value::Number(x) => Out::Number(x),
        Value::Bool(b) => Out::Bool(b),
        Value::String(ptr) => Out::Str(read_string(instance, ptr).expect("a string record")),
    }
}

fn read_string(instance: &WasmInstance, ptr: i32) -> Result<String, String> {
    let view = instance
        .memory()
        .map_err(|e| format!("no guest memory: {}", e.message()))?;
    let at = ptr as usize;
    let header = view
        .get(at..at + 4)
        .ok_or_else(|| format!("string header at {ptr} is out of bounds"))?;
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let body = view
        .get(at + 4..at + 4 + len)
        .ok_or_else(|| format!("string body at {ptr} (len {len}) is out of bounds"))?;
    String::from_utf8(body.to_vec()).map_err(|_| "string is not valid UTF-8".to_string())
}

#[track_caller]
fn number(source: &str, want: f64) {
    match run(source) {
        Out::Number(got) if want.is_nan() && got.is_nan() => {}
        Out::Number(got) if got.to_bits() == want.to_bits() => {}
        other => panic!("{source:?}: want Number({want}), got {other:?}"),
    }
}

#[track_caller]
fn boolean(source: &str, want: bool) {
    assert_eq!(run(source), Out::Bool(want), "{source:?}");
}

/// Compiles, clears the gate, and then traps. The message is checked too: a
/// trap for the *wrong* reason is not evidence of the right guard.
#[track_caller]
fn traps_with(source: &str, expect: &str) {
    match attempt(source) {
        Err(message) => assert!(
            message.contains(expect),
            "{source:?} trapped, but not with {expect:?}: {message}"
        ),
        Ok((_, vals)) => panic!("{source:?} produced {vals:?} instead of trapping"),
    }
}

#[track_caller]
fn traps(source: &str) {
    traps_with(source, "trap in");
}

/// A raw call that must trap, with the trap text.
#[track_caller]
fn raw_trap(source: &str, args: &[Val]) -> String {
    match attempt_raw(source, args) {
        Err(message) => message,
        Ok((_, vals)) => panic!("{source:?} with {args:?} produced {vals:?} instead of trapping"),
    }
}

// ---- a host that records which function ran ------------------------------

/// `mark("x")` is one declared host door with a String parameter. A function
/// that calls it leaves its own name in a log the test can read, so a claim
/// about *which* function a call reached is evidence and not inference.
fn mark_table() -> Vec<HostFn> {
    vec![HostFn {
        name: "mark".into(),
        module: "sys".into(),
        field: "mark".into(),
        params: vec![HostParam::StrPtrLen],
        result: HostResult::Void,
    }]
}

/// Run with the recording door bound. Returns what `main` did *and* the trace,
/// because the trace is the interesting half even when the call trapped.
fn traced(source: &str) -> (Result<Out, String>, Vec<String>) {
    let log: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let wasm = compile_qjs_m1_with(
        source,
        Options {
            names: Names::Declared(mark_table()),
        },
    )
    .unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let mut module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .unwrap_or_else(|e| panic!("load gate rejected {source:?}: {}", e.message()));
    if module
        .imports()
        .iter()
        .any(|i| i.module == "sys" && i.field == "mark")
    {
        let log = Rc::clone(&log);
        module
            .bind_import_typed("sys", "mark", move |args, memory| {
                let [Val::I32(ptr), Val::I32(len)] = args else {
                    return Err(WasmError::Trap("sys.mark wants (i32, i32)"));
                };
                let at = *ptr as usize;
                log.borrow_mut()
                    .push(String::from_utf8_lossy(&memory[at..at + *len as usize]).into_owned());
                Ok(Vec::new())
            })
            .expect("binding sys.mark");
    }
    let mut instance = module
        .instantiate()
        .unwrap_or_else(|e| panic!("instantiating {source:?}: {}", e.message()));
    let outcome = match instance.invoke_by_name("main", &Value::args(&[])) {
        Ok(vals) => Ok(decode(&instance, &vals, source)),
        Err(e) => Err(e.message().to_string()),
    };
    let trace = log.borrow().clone();
    (outcome, trace)
}

// ---- reading the emitted module back -------------------------------------

fn uleb(bytes: &[u8], at: &mut usize) -> u64 {
    let (mut value, mut shift) = (0u64, 0);
    loop {
        let byte = bytes[*at];
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    value
}

fn sections(wasm: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut at = 8;
    let mut out = Vec::new();
    while at < wasm.len() {
        let id = wasm[at];
        at += 1;
        let len = uleb(wasm, &mut at) as usize;
        out.push((id, wasm[at..at + len].to_vec()));
        at += len;
    }
    out
}

fn section(wasm: &[u8], id: u8) -> Option<Vec<u8>> {
    sections(wasm)
        .into_iter()
        .find(|(i, _)| *i == id)
        .map(|(_, b)| b)
}

/// Every `(element index, function index)` the element section installs.
fn table_elements(wasm: &[u8]) -> Vec<(u32, u32)> {
    let Some(body) = section(wasm, 9) else {
        return Vec::new();
    };
    let mut at = 0usize;
    let mut out = Vec::new();
    let segments = uleb(&body, &mut at);
    for _ in 0..segments {
        let flag = uleb(&body, &mut at);
        assert_eq!(flag, 0, "the compiler documents flag 0 (active, table 0)");
        assert_eq!(body[at], 0x41, "the offset expression must be i32.const");
        at += 1;
        let offset = uleb(&body, &mut at) as u32;
        assert_eq!(body[at], 0x0b, "the offset expression must end");
        at += 1;
        let count = uleb(&body, &mut at);
        for k in 0..count {
            let f = uleb(&body, &mut at) as u32;
            out.push((offset + k as u32, f));
        }
    }
    out
}

/// The declared minimum of table 0, which is what bounds every index.
fn table_min(wasm: &[u8]) -> Option<u32> {
    let body = section(wasm, 4)?;
    let mut at = 0usize;
    let count = uleb(&body, &mut at);
    assert_eq!(count, 1, "this compiler declares at most one table");
    assert_eq!(body[at], 0x70, "funcref");
    at += 1;
    let flags = uleb(&body, &mut at);
    let min = uleb(&body, &mut at) as u32;
    assert_eq!(flags, 0, "no declared maximum");
    Some(min)
}

/// The `name` custom section's function-name map. A custom section carries no
/// semantics, so this cannot be *authoritative* -- but the compiler writes one
/// name per emitted function from the same data the element section is built
/// from, so a disagreement between the two is exactly the mis-index this file
/// is hunting.
fn func_names(wasm: &[u8]) -> BTreeMap<u32, String> {
    let mut out = BTreeMap::new();
    for (id, body) in sections(wasm) {
        if id != 0 {
            continue;
        }
        let mut at = 0usize;
        let n = uleb(&body, &mut at) as usize;
        let section_name = String::from_utf8_lossy(&body[at..at + n]).into_owned();
        at += n;
        if section_name != "name" {
            continue;
        }
        while at < body.len() {
            let sub = body[at];
            at += 1;
            let len = uleb(&body, &mut at) as usize;
            let end = at + len;
            if sub == 1 {
                let entries = uleb(&body, &mut at);
                for _ in 0..entries {
                    let index = uleb(&body, &mut at) as u32;
                    let l = uleb(&body, &mut at) as usize;
                    out.insert(
                        index,
                        String::from_utf8_lossy(&body[at..at + l]).into_owned(),
                    );
                    at += l;
                }
            }
            at = end;
        }
    }
    out
}

// =========================================================================
// 1. The tag test: what a call does with something that is not a function
// =========================================================================

/// Every non-Function tag, at a call. The trap has to come from the tag test,
/// which means it happens for a Number whose payload *would* be a fine table
/// index just as surely as for a String whose payload is a heap pointer.
#[test]
fn every_other_tag_traps_at_the_call() {
    // Number, and deliberately the small integers that are also valid element
    // indices in the same module: 1 is `f`'s element, and it must not matter.
    traps("let f = function () { return 1; }; return (1)();");
    traps("let f = function () { return 1; }; return (0)();");
    traps("let f = function () { return 1; }; let n = 1; return n();");
    // String: the payload is a guest pointer, which is an i32 like an index is.
    traps("return \"x\"();");
    traps("let f = function () { return 1; }; let s = \"x\"; return s();");
    // Boolean: payload 1, the same bit pattern as element 1.
    traps("let f = function () { return 1; }; return true();");
    traps("let f = function () { return 1; }; return false();");
    // Null and Undefined: payload 0, the same bit pattern as the null element.
    traps("return null();");
    traps("return undefined();");
    // Object: the tag next door.
    traps("return ({})();");
    traps("const o = { a: 1 }; return o();");
}

/// A property that was never assigned reads `undefined` (that is the object
/// record's answer, not a fault) and *then* the call faults. Two different
/// mechanisms, and the diagnostic boundary is the second one.
#[test]
fn calling_a_property_that_was_never_assigned_traps() {
    traps("const o = {}; return o.nope();");
    traps("const o = { a: 1 }; return o.b();");
    traps("const o = {}; o.m = function () { return 1; }; return o.n();");
    // Read first, call second: the read itself is fine.
    assert_eq!(
        run("const o = {}; return typeof o.nope;"),
        Out::Str("undefined".into())
    );
    // A whole namespace chain where the last link is missing.
    traps("const f = {}; f.ui = {}; f.ui.tabs = {}; return f.ui.tabs.show();");
}

/// The callee of a call can be another call, and a call that answers something
/// uncallable has to fault at the second call and not the first.
#[test]
fn a_call_returning_a_non_function_traps_at_the_second_call() {
    traps("function g() { return 1; } return g()();");
    traps("function g() { return \"s\"; } return g()();");
    traps("function g() {} return g()();"); // returns undefined
    // And the working shape, so the trap above is about the value and not
    // about the nesting.
    number(
        "function g() { return function () { return 5; }; } return g()();",
        5.0,
    );
}

/// A wrong-type call is a type error, not a budget problem. The guest writes
/// its reason word before it gives up on the heap, so a fault with nothing
/// written is the guest saying "this was your script".
#[test]
fn a_wrong_tag_call_is_not_misreported_as_a_budget_fault() {
    let wasm = build("const o = { a: 1 }; return o.a();");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("gate");
    let mut instance = module.instantiate().expect("instantiate");
    let outcome = instance.invoke_by_name("main", &Value::args(&[]));
    assert!(outcome.is_err(), "expected a trap");
    let memory = instance.memory().expect("memory");
    assert_eq!(
        guest_fault(&memory),
        None,
        "a call on a non-function must not be recorded as a heap fault"
    );
}

// =========================================================================
// 2. The payload the tag promises
//
// These hand the guest a `(TAG_FUNCTION, n)` pair the compiler never
// produces. No script can build one -- `const_function` is the only producer
// and it only ever emits an index it just assigned -- and the public `Value`
// has no `Function` variant, so this is strictly below the supported door.
// That is the point: it is the only way to ask what stands between a bad
// payload and a jump.
// =========================================================================

/// A payload past the end of the table is a bounds trap, not a jump.
#[test]
fn a_function_payload_out_of_range_traps() {
    // The module has exactly one element (index 1); 2 and beyond are past it.
    let src = "let f = function () { return 1; }; return $0();";
    for bad in [2i64, 3, 99, 1_000_000, i64::from(i32::MAX)] {
        let message = raw_trap(src, &[Val::I32(TAG_FUNCTION), Val::I64(bad)]);
        assert!(
            message.contains("table element out of bounds"),
            "payload {bad} gave {message}"
        );
    }
}

/// Negative payloads are read as unsigned indices, so they land far past the
/// end rather than before the start. Worth pinning: an implementation that
/// sign-extended into a signed index would be reading backwards from the table.
#[test]
fn a_negative_function_payload_traps_rather_than_reading_backwards() {
    let src = "let f = function () { return 1; }; return $0();";
    for bad in [-1i64, -2, -1000, i64::from(i32::MIN)] {
        let message = raw_trap(src, &[Val::I32(TAG_FUNCTION), Val::I64(bad)]);
        assert!(
            message.contains("table element out of bounds"),
            "payload {bad} gave {message}"
        );
    }
}

/// Element 0 is the null one, and this is the behavioural half of that claim:
/// a payload of zero -- the bit pattern of `undefined`, of `null`, of `false`,
/// of a fresh local and of a zeroed word of memory -- reaches the table and
/// finds nothing there.
#[test]
fn a_zero_function_payload_finds_the_null_element() {
    let message = raw_trap(
        "let f = function () { return 1; }; return $0();",
        &[Val::I32(TAG_FUNCTION), Val::I64(0)],
    );
    assert!(
        message.contains("uninitialised table element"),
        "a zero payload gave {message}"
    );
    // And with several functions in the table, so element 0 is not merely
    // "the table is short".
    let message = raw_trap(
        "let a = function () { return 1; }; let b = function () { return 2; }; \
         let c = function () { return 3; }; a(); b(); c(); return $0();",
        &[Val::I32(TAG_FUNCTION), Val::I64(0)],
    );
    assert!(
        message.contains("uninitialised table element"),
        "a zero payload with a populated table gave {message}"
    );
}

/// No element index the compiler assigns is ever 0, in a module with many.
#[test]
fn no_element_index_is_ever_zero_and_none_repeats() {
    let mut source = String::from("const o = {};");
    for i in 0..64 {
        source.push_str(&format!("o.f{i} = function () {{ return {i}; }};"));
    }
    source.push_str("return 0;");
    let wasm = build(&source);
    let elements = table_elements(&wasm);
    assert_eq!(elements.len(), 64, "one element per function value");
    let indices: BTreeSet<u32> = elements.iter().map(|(e, _)| *e).collect();
    assert_eq!(indices.len(), 64, "no element index is used twice");
    assert!(!indices.contains(&0), "element 0 is left null");
    assert_eq!(*indices.iter().next().unwrap(), 1, "the first element is 1");
    let funcs: BTreeSet<u32> = elements.iter().map(|(_, f)| *f).collect();
    assert_eq!(funcs.len(), 64, "no two elements name one adapter");
    assert_eq!(
        table_min(&wasm),
        Some(65),
        "the table is exactly the elements plus the null one"
    );
}

/// **The truncation.** The payload is an `i64` and the index is an `i32`, so
/// `unbox_function`'s `i32.wrap_i64` discards the high half without looking at
/// it: `(TAG_FUNCTION, 2^32 + 1)` calls element **1**.
///
/// This is not reachable from any script -- the compiler's only producer of
/// the tag emits a small assigned index -- and it is not reachable through the
/// supported host door either, since `Value` has no `Function` variant. It is
/// recorded here because it is the one place where a payload that is *not* an
/// element index still reaches a real function, and because the guard that
/// makes it unreachable is a fact about the producers rather than a check at
/// the consumer.
#[test]
fn the_high_half_of_a_function_payload_is_discarded() {
    let src = "let f = function () { return 41; }; return $0();";
    // The honest index, for the baseline.
    let (instance, vals) =
        attempt_raw(src, &[Val::I32(TAG_FUNCTION), Val::I64(1)]).expect("element 1 is f");
    assert_eq!(decode(&instance, &vals, src), Out::Number(41.0));
    // The same low half, with rubbish above it.
    for high in [1u64 << 32, 0xdead_beef << 32, 0xffff_ffff << 32] {
        let payload = (high | 1) as i64;
        let (instance, vals) = attempt_raw(src, &[Val::I32(TAG_FUNCTION), Val::I64(payload)])
            .unwrap_or_else(|e| panic!("payload {payload:#x}: {e}"));
        assert_eq!(
            decode(&instance, &vals, src),
            Out::Number(41.0),
            "payload {payload:#x} reached element 1 with its high half discarded"
        );
    }
}

// =========================================================================
// 3. The table is not mis-indexed
//
// A jump to the wrong function returns a real number, so every probe here is
// built so that *any* permutation of the callees changes the answer.
// =========================================================================

/// Definition order, element order and call order are all different, and each
/// callee contributes a different power of ten. Any swap moves the total.
#[test]
fn element_order_is_assignment_order_and_the_answer_says_so() {
    // `a`, `b`, `c` are defined in that order; assigned to properties in the
    // order c, a, b (so the elements are c=1, a=2, b=3); and called in the
    // order a, b, c.
    number(
        "function a() { return 1; } function b() { return 2; } function c() { return 4; } \
         const o = {}; o.c = c; o.a = a; o.b = b; \
         return o.a() * 100 + o.b() * 10 + o.c();",
        124.0,
    );
    // The same three functions, elements assigned in yet another order.
    number(
        "function a() { return 1; } function b() { return 2; } function c() { return 4; } \
         const o = {}; o.b = b; o.c = c; o.a = a; \
         return o.a() * 100 + o.b() * 10 + o.c();",
        124.0,
    );
}

/// The bytes, cross-checked. Element `k` must name the adapter of the
/// function the assignment order says, which the `name` section spells out.
#[test]
fn the_element_section_names_the_adapter_the_assignment_order_asks_for() {
    let wasm = build(
        "function alpha() { return 1; } function beta() { return 2; } \
         function gamma() { return 3; } \
         const o = {}; o.g = gamma; o.a = alpha; o.b = beta; \
         o.z = function () { return 4; }; return 0;",
    );
    let names = func_names(&wasm);
    let mapped: Vec<(u32, String)> = table_elements(&wasm)
        .into_iter()
        .map(|(element, func)| {
            (
                element,
                names
                    .get(&func)
                    .unwrap_or_else(|| panic!("function {func} has no name"))
                    .clone(),
            )
        })
        .collect();
    assert_eq!(
        mapped
            .iter()
            .take(3)
            .map(|(e, n)| (*e, n.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (1, "<adapter of gamma>"),
            (2, "<adapter of alpha>"),
            (3, "<adapter of beta>"),
        ],
        "the three named functions take elements in assignment order, not definition order"
    );
    // The anonymous one is named after where it was written, which is the only
    // thing that tells two of them apart -- so the offset is not pinned here.
    let (element, name) = &mapped[3];
    assert_eq!(*element, 4);
    assert!(
        name.starts_with("<adapter of <anonymous@"),
        "element 4 should be the inline function expression, got {name}"
    );
    assert_eq!(mapped.len(), 4, "four function values, four elements");
}

/// Two hundred function values, so every element index crosses the one-byte
/// LEB128 boundary and the adapters' own function indices do too. The
/// fingerprint is a base-3 Horner fold over the callees in reverse order:
/// swapping any two of them changes it.
#[test]
fn two_hundred_function_values_each_dispatch_to_themselves() {
    let mut source = String::from("const o = {};");
    for i in 0..200 {
        source.push_str(&format!("o.f{i} = function () {{ return {i}; }};"));
    }
    source.push_str("let s = 0;");
    for i in (0..200).rev() {
        source.push_str(&format!("s = s * 3 + o.f{i}();"));
    }
    source.push_str("return s;");

    let mut want = 0f64;
    for i in (0..200).rev() {
        want = want * 3.0 + f64::from(i);
    }
    number(&source, want);

    // And three spot indices either side of the 127/128 boundary, read back
    // individually so a failure names which one moved.
    let mut probe = String::from("const o = {};");
    for i in 0..200 {
        probe.push_str(&format!("o.f{i} = function () {{ return {i}; }};"));
    }
    probe.push_str("return o.f0() * 1000000 + o.f127() * 1000 + o.f199();");
    number(&probe, 127_199.0);
}

/// The strongest form: the trace says which function *ran*, so a callee that
/// happened to return the same number could not pass.
#[test]
fn the_trace_names_the_function_that_actually_ran() {
    let letters = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"];
    let call_order = ["k", "a", "l", "c", "j", "b", "h", "d", "i", "e", "g", "f"];
    let mut source = String::from("const o = {};");
    for name in letters {
        source.push_str(&format!(
            "o.{name} = function () {{ mark(\"{name}\"); return 0; }};"
        ));
    }
    for name in call_order {
        source.push_str(&format!("o.{name}();"));
    }
    source.push_str("return 0;");
    let (outcome, trace) = traced(&source);
    assert_eq!(outcome, Ok(Out::Number(0.0)));
    assert_eq!(trace, call_order.to_vec());
}

/// The adapter forwards exactly what its target declares, and the uniform
/// arity -- set here by an eight-parameter function that is never called --
/// does not leak into the two-parameter one.
#[test]
fn an_adapter_forwards_its_own_arity_and_not_the_uniform_one() {
    number(
        "let wide = function (a, b, c, d, e, f, g, h) { return 0; }; \
         const o = {}; o.m = function (a, b) { return a * 100 + b * 10; }; \
         return o.m(1, 2, 3, 4, 5);",
        120.0,
    );
    // The other direction: a target as wide as the uniform arity, called
    // narrow, sees `undefined` in the tail (ECMA-262 8.6.1).
    boolean(
        "const o = {}; o.m = function (a, b, c) { return c === undefined; }; return o.m(1, 2);",
        true,
    );
    // A call site wider than any declaration is what sets the bound instead.
    number(
        "const o = {}; o.m = function () { return 7; }; return o.m(1, 2, 3, 4, 5, 6, 7, 8, 9, 10);",
        7.0,
    );
}

/// Surplus arguments at an indirect site are evaluated and then discarded, in
/// source order -- the trace is the evidence, and it also pins that the
/// callee's own body runs last.
#[test]
fn surplus_arguments_are_evaluated_in_order_then_dropped() {
    let (outcome, trace) = traced(
        "const o = {}; o.m = function () { mark(\"body\"); return 1; }; \
         let s1 = function () { mark(\"a1\"); return 1; }; \
         let s2 = function () { mark(\"a2\"); return 2; }; \
         return o.m(s1(), s2());",
    );
    assert_eq!(outcome, Ok(Out::Number(1.0)));
    assert_eq!(trace, vec!["a1", "a2", "body"]);
}

/// ECMA-262 13.3.6.1: the arguments are evaluated (EvaluateCall step 3) before
/// the callee is checked for callability (step 4). So a script whose arguments
/// have side effects sees them happen even though the call is about to fault.
/// The trace survives the trap, which is what makes this observable at all.
#[test]
fn arguments_run_before_the_tag_test_faults() {
    let (outcome, trace) = traced(
        "const o = {}; o.notfn = 5; \
         let bump = function () { mark(\"arg\"); return 1; }; \
         return o.notfn(bump());",
    );
    assert!(outcome.is_err(), "the call must fault: {outcome:?}");
    assert_eq!(trace, vec!["arg"], "the argument ran before the fault");
}

// =========================================================================
// 4. Lifetime and aliasing
// =========================================================================

/// A function value has no per-value state, so it outlives whatever held it.
#[test]
fn a_function_value_outlives_the_object_it_came_from() {
    number(
        "let g; { const o = { m: function () { return 9; } }; g = o.m; } return g();",
        9.0,
    );
    // The binding that held the object is overwritten with a Number.
    number(
        "let o = { m: function () { return 9; } }; let g = o.m; o = 1; return g();",
        9.0,
    );
    // Returned out of the function that built the object.
    number(
        "function make() { const o = { m: function () { return 9; } }; return o.m; } \
         let g = make(); return g();",
        9.0,
    );
}

/// One function under many keys is one element, and every key reaches it.
#[test]
fn one_function_under_many_keys_is_one_element() {
    let wasm =
        build("function k() { return 3; } const o = {}; o.a = k; o.b = k; o.c = k; return 0;");
    assert_eq!(
        table_elements(&wasm).len(),
        1,
        "three properties, one function, one element"
    );
    boolean(
        "function k() { return 3; } const o = {}; o.a = k; o.b = k; o.c = k; \
         return o.a === o.b && o.b === o.c;",
        true,
    );
    number(
        "function k() { return 3; } const o = {}; o.a = k; o.b = k; o.c = k; \
         return o.a() + o.b() + o.c();",
        9.0,
    );
}

/// Two objects, two function values, and no route from one slot to the other.
/// The weighted answer is what proves it: 11 and 22 cannot be swapped without
/// moving the total.
#[test]
fn two_slots_cannot_reach_each_others_functions() {
    number(
        "const p = {}; p.m = function () { return 11; }; \
         const q = {}; q.m = function () { return 22; }; \
         return p.m() * 100 + q.m();",
        1122.0,
    );
    boolean(
        "const p = {}; p.m = function () { return 11; }; \
         const q = {}; q.m = function () { return 22; }; return p.m === q.m;",
        false,
    );
    // Assigning one into the other moves the value and nothing else.
    number(
        "const p = {}; p.m = function () { return 11; }; \
         const q = {}; q.m = function () { return 22; }; \
         q.m = p.m; return p.m() * 100 + q.m();",
        1111.0,
    );
}

/// The payload is an index into **this module's** table, so the same index
/// means a different function in a different module. That is by construction
/// -- `repr`'s `host_decode` refuses the tag outward for exactly this reason
/// -- and this test states it as a fact rather than leaving it implied: a
/// module handed an index from elsewhere calls its own element, quietly.
#[test]
fn a_table_index_means_nothing_outside_its_own_module() {
    // Module A's element 1 answers 100. Module B's element 1 answers 1.
    let a = "let f = function () { return 100; }; let g = function () { return 200; }; \
             return f() + g();";
    let b = "let f = function () { return 1; }; let g = function () { return 2; }; return $0();";
    assert_eq!(run(a), Out::Number(300.0));
    for (index, want) in [(1i64, 1.0), (2, 2.0)] {
        let (instance, vals) = attempt_raw(b, &[Val::I32(TAG_FUNCTION), Val::I64(index)])
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            decode(&instance, &vals, b),
            Out::Number(want),
            "element {index} answers with B's own function, not A's"
        );
    }
    // And past B's own table it is a bounds trap rather than anything else.
    assert!(
        raw_trap(b, &[Val::I32(TAG_FUNCTION), Val::I64(3)]).contains("table element out of bounds")
    );
}

/// A persistent instance, called many times. The table is immutable -- nothing
/// this compiler emits ever writes an element -- so a function value found on
/// the fiftieth call is the same one the first call found.
#[test]
fn a_function_value_survives_many_calls_on_one_instance() {
    let source = "const o = {}; o.m = function (n) { return n * 2; }; \
                  o.k = function () { return o.m(21); }; return o.k();";
    let wasm = build(source);
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("gate");
    let mut instance = module.instantiate().expect("instantiate");
    for call in 0..50 {
        let vals = instance
            .invoke_by_name("main", &Value::args(&[]))
            .unwrap_or_else(|e| panic!("call {call}: {}", e.message()));
        assert_eq!(
            decode(&instance, &vals, source),
            Out::Number(42.0),
            "call {call}"
        );
    }
    assert_eq!(
        table_elements(&wasm).len(),
        2,
        "two functions, two elements"
    );
}

/// A trap through the table does not poison the instance: the next call runs.
#[test]
fn an_instance_still_works_after_an_indirect_call_trapped() {
    let source = "const o = {}; o.m = function () { return 5; }; \
                  if ($0 === 1) { return o.bad(); } return o.m();";
    let wasm = build(source);
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("gate");
    let mut instance = module.instantiate().expect("instantiate");
    let one = Val::I64(1f64.to_bits() as i64);
    let zero = Val::I64(0f64.to_bits() as i64);
    assert!(
        instance
            .invoke_by_name("main", &[Val::I32(1), one])
            .is_err(),
        "the missing property must fault"
    );
    let vals = instance
        .invoke_by_name("main", &[Val::I32(1), zero])
        .expect("the instance is still usable");
    assert_eq!(decode(&instance, &vals, source), Out::Number(5.0));
}

// =========================================================================
// 5. Mutation under a live call
// =========================================================================

/// A callee that overwrites the very property it was reached through. The old
/// function is already running; the new one is what the next lookup finds.
#[test]
fn a_function_can_replace_the_property_it_was_called_through() {
    // Replaces itself, then calls the replacement from inside itself.
    number(
        "const o = {}; o.f = function () { o.f = function () { return 2; }; return o.f(); }; \
         return o.f();",
        2.0,
    );
    // Replaces itself and returns; the caller's *second* call sees the new one.
    number(
        "const o = {}; o.m = function () { o.m = function () { return 20; }; return 1; }; \
         return o.m() + o.m();",
        21.0,
    );
    // The trace, so "the new one" is not inferred from the number alone.
    let (outcome, trace) = traced(
        "const o = {}; \
         o.m = function () { mark(\"old\"); o.m = function () { mark(\"new\"); return 0; }; return 0; }; \
         o.m(); o.m(); o.m(); return 0;",
    );
    assert_eq!(outcome, Ok(Out::Number(0.0)));
    assert_eq!(trace, vec!["old", "new", "new"]);
}

/// Reassignment inside a loop, so the dispatch target changes on every pass.
#[test]
fn reassigning_the_property_each_pass_changes_what_the_next_pass_calls() {
    number(
        "const o = {}; o.m = function () { return 1; }; let i = 0; let s = 0; \
         while (i < 3) { s = s + o.m(); o.m = function () { return 10; }; i = i + 1; } return s;",
        21.0,
    );
    let (outcome, trace) = traced(
        "const o = {}; o.m = function () { mark(\"one\"); return 0; }; \
         let i = 0; \
         while (i < 3) { o.m(); o.m = function () { mark(\"two\"); return 0; }; i = i + 1; } \
         return 0;",
    );
    assert_eq!(outcome, Ok(Out::Number(0.0)));
    assert_eq!(trace, vec!["one", "two", "two"]);
}

/// Mutual recursion through two properties, deep enough that a wrong element
/// would land on the wrong parity and answer wrongly rather than crash.
#[test]
fn mutual_recursion_through_two_properties_keeps_its_parity() {
    for (n, want) in [
        (0.0, 1.0),
        (1.0, 0.0),
        (10.0, 1.0),
        (11.0, 0.0),
        (100.0, 1.0),
    ] {
        number(
            &format!(
                "const o = {{}}; \
                 o.even = function (n) {{ if (n === 0) {{ return 1; }} return o.odd(n - 1); }}; \
                 o.odd = function (n) {{ if (n === 0) {{ return 0; }} return o.even(n - 1); }}; \
                 return o.even({n});"
            ),
            want,
        );
    }
}

/// Duplicate keys in one literal: 13.2.5.5 writes the same property twice, so
/// the second function is the one the slot holds -- but *both* took an element.
#[test]
fn a_duplicate_key_leaves_a_stranded_element_and_the_later_function_wins() {
    number(
        "const o = { m: function () { return 1; }, m: function () { return 2; } }; return o.m();",
        2.0,
    );
    let wasm = build(
        "const o = { m: function () { return 1; }, m: function () { return 2; } }; return 0;",
    );
    assert_eq!(
        table_elements(&wasm).len(),
        2,
        "the shadowed function still occupies an element, which nothing can reach"
    );
}

// =========================================================================
// 6. Depth, steps and heap: does it fault honestly?
// =========================================================================

/// A call through a value is **two** wasm frames -- the adapter and its target
/// -- so an indirect recursion reaches roughly half the depth a direct one
/// does. It is a `call depth` trap either way: the ceiling is tinyvm's and it
/// is loud, not a native stack overflow and not a wrong answer.
#[test]
fn indirect_recursion_hits_the_call_depth_ceiling_and_nothing_else() {
    let indirect = |n: u32| {
        format!(
            "let f = function (n) {{ if (n <= 0) {{ return 0; }} return 1 + f(n - 1); }}; \
             return f({n});"
        )
    };
    number(&indirect(254), 254.0);
    traps_with(&indirect(255), "call depth");
    traps_with(&indirect(100_000), "call depth");

    // The direct form gets about twice as far, which is the adapter frame
    // showing up as a measurement rather than as an assertion about the code.
    let direct = |n: u32| {
        format!(
            "function f(n) {{ if (n <= 0) {{ return 0; }} return 1 + f(n - 1); }} return f({n});"
        )
    };
    number(&direct(509), 509.0);
    traps_with(&direct(510), "call depth");

    // Through a property, which adds the `__obj_get` frame while it is live.
    let through_property = |n: u32| {
        format!(
            "const o = {{}}; o.f = function (n) {{ if (n <= 0) {{ return 0; }} return 1 + o.f(n - 1); }}; \
             return o.f({n});"
        )
    };
    number(&through_property(254), 254.0);
    traps_with(&through_property(255), "call depth");
}

/// The depth trap is a ceiling, not a heap problem, and the guest says so by
/// writing nothing in its fault word.
#[test]
fn a_depth_trap_is_not_reported_as_heap_exhaustion() {
    let source = "let f = function (n) { if (n <= 0) { return 0; } return 1 + f(n - 1); }; \
                  return f(5000);";
    let wasm = build(source);
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("gate");
    let mut instance = module.instantiate().expect("instantiate");
    assert!(instance.invoke_by_name("main", &Value::args(&[])).is_err());
    assert_eq!(guest_fault(&instance.memory().expect("memory")), None);
}

/// Twenty thousand calls through the table, in a loop. The point is that the
/// count is right at the end: an indirect call must not leak a frame, an
/// operand or a table slot per iteration.
#[test]
fn twenty_thousand_indirect_calls_leak_nothing_that_changes_the_answer() {
    number(
        "const o = {}; o.m = function (n) { return n + 1; }; \
         let i = 0; let s = 0; while (i < 20000) { s = o.m(s); i = i + 1; } return s;",
        20_000.0,
    );
    // The same loop where the callee also allocates, so the bump heap moves.
    number(
        "const o = {}; o.m = function (n) { return { v: n + 1 }; }; \
         let i = 0; let s = 0; while (i < 20000) { s = o.m(s).v; i = i + 1; } return s;",
        20_000.0,
    );
}

/// A callee that exhausts the heap faults as a *budget* problem and says so,
/// even though the call that reached it went through the table.
#[test]
fn heap_exhaustion_inside_an_indirect_callee_is_still_named_as_such() {
    let source = "const o = {}; o.m = function (n) { return { v: n }; }; \
                  let i = 0; while (i < 100000) { o.m(i); i = i + 1; } return 1;";
    let wasm = build(source);
    let tight = Limits {
        max_memory_pages: 1,
        ..Limits::default()
    };
    let module = WasmModule::from_bytes_with(&wasm, tight).expect("gate");
    let mut instance = module.instantiate().expect("instantiate");
    assert!(instance.invoke_by_name("main", &Value::args(&[])).is_err());
    assert_eq!(
        guest_fault(&instance.memory().expect("memory")),
        Some(GuestFault::HeapExhausted),
        "the budget, not the script"
    );
}

/// The step budget is reached honestly too, rather than silently truncating a
/// loop of indirect calls.
#[test]
fn a_loop_of_indirect_calls_stops_at_the_step_budget() {
    let source = "const o = {}; o.m = function (n) { return n + 1; }; \
                  let i = 0; while (i < 100000) { i = o.m(i); } return i;";
    let wasm = build(source);
    let few = Limits {
        max_steps: 200_000,
        ..Limits::default()
    };
    let module = WasmModule::from_bytes_with(&wasm, few).expect("gate");
    let mut instance = module.instantiate().expect("instantiate");
    let message = instance
        .invoke_by_name("main", &Value::args(&[]))
        .err()
        .map(|e| e.message().to_string())
        .expect("the budget must stop it");
    assert!(message.contains("step budget"), "got {message}");
}

// =========================================================================
// 7. A function value is not anything else
// =========================================================================

/// The tag stops every operation that would need a prototype, and it stops it
/// as a trap rather than as a fabricated answer.
#[test]
fn a_function_is_never_quietly_converted() {
    traps("let f = function () {}; return f + 1;");
    traps("let f = function () {}; return f * 2;");
    traps("let f = function () {}; return -f;");
    traps("let f = function () {}; return f < 1;");
    traps("let f = function () {}; return f == 1;");
    // A property *of* a function: there is no prototype, so no `length`, no
    // `name`, no `call`.
    traps("let f = function () {}; return f.length;");
    traps("let f = function () {}; return f.call;");
    traps("let f = function () {}; f.x = 1; return 0;");
    // A function *as* a key: ToPropertyKey needs ToPrimitive, which needs a
    // prototype.
    traps("let f = function () {}; const o = {}; return o[f];");
    traps("let f = function () {}; const o = {}; o[f] = 1; return 0;");
    // The two that do answer, because they need no conversion at all.
    boolean("let f = function () {}; return f == undefined;", false);
    boolean("let f = function () {}; return f == null;", false);
    boolean("let f = function () {}; let g = f; return f == g;", true);
}

/// A function cannot leave through the door: `host_decode` names the tag.
#[test]
fn a_function_value_cannot_be_returned_to_the_host() {
    let source = "let f = function () { return 1; }; return f;";
    let (_, vals) = attempt(source).expect("it runs");
    let error = Value::returned(&vals).expect_err("a function has no host variant");
    assert!(
        error.contains("index into this module's own table"),
        "the refusal must name why, got {error}"
    );
}

// =========================================================================
// 8. A divergence this attack found
// =========================================================================

/// **Known defect, deliberately failing.** ECMA-262 15.2.5 /
/// InstantiateOrdinaryFunctionExpression makes each *evaluation* of a
/// FunctionExpression a new function object, so `make() === make()` is `false`
/// in JavaScript. Here a function expression is one `FuncId`, which is one
/// element index however many times it is evaluated, so `===` answers `true`.
///
/// It is exactly the case `repr.rs`'s module header does not cover: it says
/// "one function gets one element index however many times it is **read**, and
/// two function expressions get two", and both halves are true -- the gap is a
/// third case, one function expression *evaluated* twice, which JavaScript
/// says is two functions and this engine says is one.
///
/// The gap is inert for `fleet.js` (all 29 of its function expressions are
/// evaluated exactly once, at the top level) and cannot be reached at all
/// without `===` on two function values, since there are no closures for the
/// two objects to differ in. Ignored rather than deleted so that the day the
/// element table stops being keyed on `FuncId` alone, this flips green.
#[test]
#[ignore = "known divergence: one function expression evaluated twice is one element, not two"]
fn two_evaluations_of_one_function_expression_are_two_functions() {
    boolean(
        "function make() { return function () { return 1; }; } return make() === make();",
        false,
    );
    boolean(
        "let a; let b; let i = 0; \
         while (i < 2) { const g = function () { return 1; }; \
         if (i === 0) { a = g; } else { b = g; } i = i + 1; } return a === b;",
        false,
    );
}

/// The half of the rule that does hold, kept green beside the failing one so a
/// fix cannot pass by making every function distinct from itself.
#[test]
fn two_function_expressions_are_two_functions_and_one_read_twice_is_one() {
    boolean(
        "let a = function () { return 1; }; let b = function () { return 1; }; return a === b;",
        false,
    );
    boolean("let f = function () {}; let g = f; return f === g;", true);
    boolean(
        "const o = {}; o.f = function () {}; return o.f === o.f;",
        true,
    );
    boolean(
        "function make() { return function () { return 1; }; } \
         let x = make(); return x === x;",
        true,
    );
}
