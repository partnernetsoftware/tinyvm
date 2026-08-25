//! The M1 lowering, executed for real.
//!
//! Every test here goes the whole way: `compile_qjs_m1` produces bytes, the
//! bytes go through **tinyvm's load gate**, the module is instantiated, `main`
//! is called, and the value that comes back is compared against what ECMA-262
//! says the script means. "The parser accepted it" is not evidence and does not
//! appear in this file.
//!
//! The only public surface used is [`tinyvm_qjs::compile_qjs_m1`] and
//! [`tinyvm_qjs::Value`], so these tests see exactly what a host sees -- in
//! particular they see one JS value arrive as two wasm values, which is the
//! whole point of settling the representation before this milestone.

use tinyvm::{Limits, Val, ValueType, WasmError, WasmInstance, WasmModule};
use tinyvm_qjs::{
    Boundary, CompileError, Names, Options, Value, compile_qjs_m1, compile_qjs_m1_with,
};

// =========================================================================
// Harness
// =========================================================================

/// What `main` returned, with a String's text already resolved -- the pointer
/// alone is unreadable once the instance is gone.
#[derive(Debug, Clone, PartialEq)]
enum Out {
    Undefined,
    Null,
    Number(f64),
    Bool(bool),
    Str(String),
}

/// A host import: a name in the `js` module and what it answers with, given
/// the JS values the call site passed.
struct Host {
    field: &'static str,
    answer: fn(&[Value]) -> Value,
}

/// `expect` would work now that [`WasmError`] derives `Debug`; this stays
/// because a named stage reads better in a failure than `Trap("...")` does.
#[track_caller]
fn ok<T>(result: Result<T, WasmError>, what: &str) -> T {
    match result {
        Ok(value) => value,
        Err(e) => panic!("{what}: {}", e.message()),
    }
}

#[track_caller]
fn load(wasm: &[u8]) -> WasmModule {
    ok(
        WasmModule::from_bytes_with(wasm, Limits::default()),
        "load gate",
    )
}

fn compile(source: &str) -> Vec<u8> {
    compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"))
}

/// Compile, load through the gate, run `main(args)`, resolve the result.
fn call(source: &str, args: &[Value], hosts: &[Host]) -> Result<Out, String> {
    let wasm = compile_qjs_m1_with(
        source,
        Options {
            names: if hosts.is_empty() {
                Names::Unbound
            } else {
                Names::HostImport
            },
        },
    )
    .map_err(|e| format!("compiling {source:?}: {e}"))?;
    assert!(wasm.starts_with(b"\0asm"), "{source:?} must emit wasm");

    let mut module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .map_err(|e| format!("load gate rejected {source:?}: {}", e.message()))?;
    for host in hosts {
        let answer = host.answer;
        module
            .bind_import_typed("js", host.field, move |args, _memory| {
                // Arguments arrive as the same flat pairs `Value::args` writes.
                let given: Vec<Value> = args
                    .chunks(2)
                    .map(|pair| Value::returned(pair).expect("a V1 pair"))
                    .collect();
                Ok(Value::args(&[answer(&given)]))
            })
            .map_err(|e| format!("binding js.{}: {}", host.field, e.message()))?;
    }
    let mut instance = module
        .instantiate()
        .map_err(|e| format!("instantiating {source:?}: {}", e.message()))?;
    let vals = instance
        .invoke_by_name("main", &Value::args(args))
        .map_err(|e| format!("trap in {source:?}: {}", e.message()))?;
    let value = Value::returned(&vals)?;
    Ok(match value {
        Value::Undefined => Out::Undefined,
        Value::Null => Out::Null,
        Value::Number(x) => Out::Number(x),
        Value::Bool(b) => Out::Bool(b),
        Value::String(ptr) => Out::Str(read_string(&instance, ptr)?),
    })
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

fn run(source: &str) -> Out {
    call(source, &[], &[]).unwrap_or_else(|e| panic!("{e}"))
}

/// The common case: a script whose value is a Number.
#[track_caller]
fn number(source: &str, want: f64) {
    match run(source) {
        Out::Number(got) if got == want => {}
        // `-0 === 0` is true, so an equality check cannot tell them apart.
        other => panic!("{source:?}: want Number({want}), got {other:?}"),
    }
}

#[track_caller]
fn boolean(source: &str, want: bool) {
    assert_eq!(run(source), Out::Bool(want), "{source:?}");
}

#[track_caller]
fn text(source: &str, want: &str) {
    assert_eq!(run(source), Out::Str(want.to_string()), "{source:?}");
}

#[track_caller]
fn refuse(source: &str) -> CompileError {
    match compile_qjs_m1(source) {
        Ok(_) => panic!("{source:?}: compiled; expected a capability diagnostic"),
        Err(e) => e,
    }
}

// =========================================================================
// The representation reaches the ABI
// =========================================================================

/// The claim the whole milestone rests on: nothing is lowered as a bare `i32`
/// "for now". One JS argument is two wasm parameters and one JS result is two
/// wasm results, at the export boundary where a host can see it.
#[test]
fn a_js_value_is_two_wasm_values_at_every_boundary() {
    let wasm = compile("$0 + $1");
    let module = load(&wasm);
    // The i32 host ABI is exactly what V1 is not, so the portable-arity view
    // has to decline to describe `main`.
    assert!(
        module.export_i32_arity("main").is_none(),
        "a V1 entry point is not an i32-ABI function"
    );
    let instance = ok(module.instantiate(), "instantiate");
    let main = ok(instance.exported_function_handle("main"), "handle").expect("exports main");
    assert_eq!(
        main.parameter_count(),
        4,
        "two JS arguments, four wasm params"
    );
    assert_eq!(main.result_count(), 2, "one JS result, two wasm results");
    for (slot, want) in [(0, ValueType::I32), (1, ValueType::I64)] {
        assert_eq!(
            main.parameter_type(slot),
            Some(want),
            "parameter {slot} is not the V1 word it must be"
        );
        assert!(
            main.result_type(slot) == Some(want),
            "result {slot} is not the V1 word it must be"
        );
    }
}

/// An integer literal is a Number, not an `i32`: `1/2` is `0.5`.
#[test]
fn integer_literals_are_doubles() {
    number("1 / 2", 0.5);
    number("7 / 2", 3.5);
    number("1 / 0", f64::INFINITY);
    assert!(
        matches!(run("0 / 0"), Out::Number(x) if x.is_nan()),
        "0/0 is NaN, not a trap"
    );
}

#[test]
fn every_literal_type_survives_the_round_trip() {
    number("42", 42.0);
    boolean("true", true);
    boolean("false", false);
    assert_eq!(run("null"), Out::Null);
    assert_eq!(run("undefined"), Out::Undefined);
    text("\"hi\"", "hi");
}

#[test]
fn arguments_arrive_as_js_values_of_every_type() {
    assert_eq!(
        call("$0", &[Value::Number(1.5)], &[]).unwrap(),
        Out::Number(1.5)
    );
    assert_eq!(
        call("$0", &[Value::Bool(true)], &[]).unwrap(),
        Out::Bool(true)
    );
    assert_eq!(call("$0", &[Value::Null], &[]).unwrap(), Out::Null);
    assert_eq!(
        call("$0", &[Value::Undefined], &[]).unwrap(),
        Out::Undefined
    );
    // The script takes one past the highest `$N` it names, so `$1` alone
    // still means two -- and the one never named is whatever the host sent.
    assert_eq!(
        call("$1", &[Value::Number(1.0), Value::Bool(false)], &[]).unwrap(),
        Out::Bool(false)
    );
}

// =========================================================================
// Statements and declarations
// =========================================================================

/// A script with no `return` yields its *completion value* -- ECMA-262 14.1.1
/// over the `UpdateEmpty` in 14.6.7 and 14.7.1.1. An expression statement
/// produces one; a declaration, a block and an empty statement produce
/// nothing; and an `if`, `while` or `for` produces `undefined` before its body
/// gets a chance to produce anything.
#[test]
fn a_script_with_no_return_yields_its_completion_value() {
    assert_eq!(run("let x = 1;"), Out::Undefined);
    assert_eq!(run(";"), Out::Undefined);
    number("1 + 1;", 2.0);
    number("1; ;", 1.0);
    number("1; { }", 1.0);
    number("1; let x = 2;", 1.0);
    number("1; { 2; }", 2.0);
    // The `UpdateEmpty` cases, which are the ones a naive "last expression
    // wins" rule gets wrong.
    assert_eq!(run("1; if (false) { 2; }"), Out::Undefined);
    assert_eq!(run("1; while (false) { 2; }"), Out::Undefined);
    number("1; if (true) { 2; }", 2.0);
    // The last iteration's expression statement is the loop's value.
    number("let s = 10; for (let i = 0; i < 3; i++) { s + i; }", 12.0);
}

#[test]
fn declarations_become_storage_that_can_be_read_and_written() {
    number("let x = 1; return x;", 1.0);
    number("let x = 1; x = 2; return x;", 2.0);
    number("let x = 1; x += 4; return x;", 5.0);
    number("const k = 6; return k * 7;", 42.0);
    number("var v = 3; return v;", 3.0);
    // No initialiser is `undefined`, which is what tag 0 means.
    assert_eq!(run("let x; return x;"), Out::Undefined);
}

/// An assignment is an expression, and its value is the value assigned.
#[test]
fn assignment_has_a_value() {
    number("let x = 0; return x = 9;", 9.0);
    number("let x = 1; return x += 2;", 3.0);
}

#[test]
fn a_block_shares_the_enclosing_frame() {
    number("let x = 1; { let x = 2; } return x;", 1.0);
    number("let x = 1; { x = 2; } return x;", 2.0);
}

// =========================================================================
// Control flow
// =========================================================================

#[test]
fn if_else_runs_exactly_one_arm() {
    number("if (true) { return 1; } else { return 2; }", 1.0);
    number("if (false) { return 1; } else { return 2; }", 2.0);
    number("let x = 0; if (true) { x = 1; } return x;", 1.0);
    number("let x = 0; if (false) { x = 1; } return x;", 0.0);
    // The test goes through ToBoolean, not through a type check.
    number("if (0) { return 1; } else { return 2; }", 2.0);
    number("if (\"\") { return 1; } else { return 2; }", 2.0);
    number("if (\"a\") { return 1; } else { return 2; }", 1.0);
    number("if (null) { return 1; } else { return 2; }", 2.0);
}

/// Nested `if`s are where a wrong label depth shows up as a silently wrong
/// arm rather than as a validation failure.
#[test]
fn nested_if_else_picks_the_right_arm_at_every_depth() {
    let src = |a: &str, b: &str| {
        format!(
            "if ({a}) {{ if ({b}) {{ return 1; }} else {{ return 2; }} }} else {{ if ({b}) {{ return 3; }} else {{ return 4; }} }}"
        )
    };
    number(&src("true", "true"), 1.0);
    number(&src("true", "false"), 2.0);
    number(&src("false", "true"), 3.0);
    number(&src("false", "false"), 4.0);
}

#[test]
fn while_loops_and_terminates() {
    number("let i = 0; while (i < 5) { i = i + 1; } return i;", 5.0);
    number("let i = 0; while (false) { i = 1; } return i;", 0.0);
    number(
        "let n = 0; let i = 0; while (i < 4) { let j = 0; while (j < 3) { n = n + 1; j = j + 1; } i = i + 1; } return n;",
        12.0,
    );
}

#[test]
fn for_runs_init_test_update_in_that_order() {
    number(
        "let s = 0; for (let i = 0; i < 4; i = i + 1) { s = s + i; } return s;",
        6.0,
    );
    number(
        "let s = 0; for (let i = 0; i < 0; i = i + 1) { s = 1; } return s;",
        0.0,
    );
    number(
        "let s = 0; for (let i = 0; i < 3; i++) { s = s + 1; } return s;",
        3.0,
    );
    // A `for` with no test loops until a `return` gets out of it.
    number(
        "let i = 0; for (;;) { i = i + 1; if (i > 3) { return i; } }",
        4.0,
    );
}

#[test]
fn a_return_inside_a_loop_inside_an_if_leaves_the_function() {
    number(
        "let i = 0; while (true) { if (i > 2) { return i * 10; } i = i + 1; } return 0;",
        30.0,
    );
}

// =========================================================================
// Operators
// =========================================================================

#[test]
fn arithmetic_follows_ecma262() {
    number("1 + 2 * 3", 7.0);
    number("(1 + 2) * 3", 9.0);
    number("-3 + 5", 2.0);
    number("+true", 1.0);
    number("1 - true", 0.0);
    assert!(matches!(run("undefined + 1"), Out::Number(x) if x.is_nan()));
    number("null + 1", 1.0);
}

#[test]
fn comparison_and_equality_follow_ecma262() {
    boolean("1 < 2", true);
    boolean("2 <= 2", true);
    boolean("3 > 4", false);
    boolean("4 >= 4", true);
    boolean("1 == 1", true);
    boolean("1 === 1", true);
    boolean("1 !== 1", false);
    boolean("null == undefined", true);
    boolean("null === undefined", false);
    boolean("!true", false);
    boolean("!0", true);
    boolean("\"a\" === \"a\"", true);
}

/// `&&` and `||` yield an *operand*, and do not evaluate the right one unless
/// the left says so. Both halves are checked: the value, and the side effect
/// that must not happen.
#[test]
fn logical_operators_short_circuit_and_yield_an_operand() {
    number("1 && 2", 2.0);
    number("0 && 2", 0.0);
    number("1 || 2", 1.0);
    number("0 || 2", 2.0);
    assert_eq!(run("null || undefined"), Out::Undefined);
    number("let x = 0; false && (x = 1); return x;", 0.0);
    number("let x = 0; true || (x = 1); return x;", 0.0);
    number("let x = 0; true && (x = 1); return x;", 1.0);
}

#[test]
fn update_operators_differ_by_position() {
    number("let x = 1; return x++;", 1.0);
    number("let x = 1; x++; return x;", 2.0);
    number("let x = 1; return ++x;", 2.0);
    number("let x = 1; return x--;", 1.0);
    number("let x = 1; --x; return x;", 0.0);
    // ToNumeric first: the value of `x++` is a Number even when `x` was not.
    boolean("let x = true; x++; return x === 2;", true);
}

// =========================================================================
// Strings
// =========================================================================

#[test]
fn string_literals_live_in_the_data_section() {
    text("\"hello\"", "hello");
    text("\"\"", "");
    text("\"a\" + \"b\"", "ab");
    text("let s = \"x\"; s += \"y\"; return s;", "xy");
    boolean("\"ab\" === \"a\" + \"b\"", true);
    // Equal literals share one record, so a script with the same text twice
    // still says what it means.
    boolean("\"q\" === \"q\"", true);
}

#[test]
fn concatenation_allocates_and_the_result_is_readable() {
    text(
        "let s = \"\"; for (let i = 0; i < 3; i++) { s = s + \"ab\"; } return s;",
        "ababab",
    );
}

// =========================================================================
// Functions
// =========================================================================

#[test]
fn a_declared_function_can_be_called_with_arguments() {
    number(
        "function add(a, b) { return a + b; } return add(1, 2);",
        3.0,
    );
    number("function id(x) { return x; } return id(7);", 7.0);
    // Hoisted: the call may precede the declaration.
    number("return twice(4); function twice(x) { return x * 2; }", 8.0);
}

#[test]
fn a_function_with_no_return_yields_undefined() {
    assert_eq!(run("function f() { } return f();"), Out::Undefined);
    assert_eq!(run("function f() { return; } return f();"), Out::Undefined);
}

/// wasm calls are arity-exact and JavaScript calls are not, so the lowering
/// has to reconcile them -- and it must still evaluate an argument it drops.
#[test]
fn argument_count_mismatch_follows_javascript() {
    assert_eq!(
        run("function f(a, b) { return b; } return f(1);"),
        Out::Undefined
    );
    number("function f(a) { return a; } return f(1, 2);", 1.0);
    number(
        "let seen = 0; function f(a) { return a; } f(1, seen = 9); return seen;",
        9.0,
    );
}

#[test]
fn a_function_reads_the_scripts_bindings() {
    number("let g = 10; function f() { return g; } return f();", 10.0);
    number(
        "let g = 10; function f() { g = 20; return 0; } f(); return g;",
        20.0,
    );
}

#[test]
fn recursion_and_nested_calls_work() {
    number(
        "function fact(n) { if (n <= 1) { return 1; } return n * fact(n - 1); } return fact(5);",
        120.0,
    );
    number(
        "function a(x) { return b(x) + 1; } function b(x) { return x * 2; } return a(3);",
        7.0,
    );
}

/// A function body has its own frame: its parameters and locals are not the
/// script's, and a recursive call must not clobber the caller's.
#[test]
fn a_frame_is_per_call() {
    number(
        "function f(n) { let acc = 0; if (n > 0) { acc = f(n - 1); } return acc + n; } return f(4);",
        10.0,
    );
}

#[test]
fn an_immediately_invoked_function_expression_runs() {
    number("return (function (x) { return x + 1; })(41);", 42.0);
}

// =========================================================================
// Host imports
// =========================================================================

#[test]
fn a_host_name_is_an_import_that_takes_and_returns_js_values() {
    let hosts = [Host {
        field: "answer",
        answer: |_| Value::Number(40.0),
    }];
    assert_eq!(
        call("return answer() + 2;", &[], &hosts).unwrap(),
        Out::Number(42.0)
    );
    // A bare host name is the same call, as it is at M0.
    assert_eq!(
        call("return answer + 2;", &[], &hosts).unwrap(),
        Out::Number(42.0)
    );
}

/// M0's host door was zero-argument and `i32`-valued. This one carries JS
/// values in both directions, which is the whole reason the import signature
/// is written in pairs.
#[test]
fn a_host_call_passes_js_values_in_and_out() {
    let hosts = [Host {
        field: "pick",
        answer: |given| {
            assert_eq!(given.len(), 2, "the call site passed two arguments");
            match given[0] {
                Value::Bool(true) => given[1],
                _ => Value::Null,
            }
        },
    }];
    assert_eq!(
        call("return pick(true, \"yes\");", &[], &hosts).unwrap(),
        Out::Str("yes".to_string())
    );
    assert_eq!(
        call("return pick(false, 1);", &[], &hosts).unwrap(),
        Out::Null
    );
    // The arguments are ordinary expressions, evaluated at the call site.
    assert_eq!(
        call("let n = 1; return pick(n < 2, n + 1);", &[], &hosts).unwrap(),
        Out::Number(2.0)
    );
}

/// A wasm import has one signature. A name called at two arities has no single
/// import to be, and inventing one would silently drop or fabricate arguments.
#[test]
fn a_host_name_used_at_two_arities_is_refused() {
    let e = compile_qjs_m1_with(
        "g(1); return g(1, 2);",
        Options {
            names: Names::HostImport,
        },
    )
    .expect_err("two arities is not one import");
    assert_eq!(
        e.message,
        "this engine does not support calling the host name `g` with two different argument counts yet"
    );
    assert_eq!(e.boundary, Boundary::ThirdBinding);
}

// =========================================================================
// The boundary, named
// =========================================================================

/// Every refusal says what the *engine* cannot do, never that the script is
/// wrong. These are the ones the lowering itself raises.
///
/// The lowering used to raise one more -- a function in a value position --
/// and it does not any more: a function value is a table element index and a
/// call through one is `call_indirect`. What is left here is the capture, and
/// it is the one that matters: a captured binding needs an environment, and a
/// silently miscompiled closure is the failure this refusal exists to prevent.
#[test]
fn the_lowering_names_its_own_boundary() {
    let e = refuse("function o() { let a = 1; function i() { return a; } return i(); }");
    assert!(
        e.message.starts_with("this engine does not support "),
        "got {:?}",
        e.message
    );
}

#[test]
fn a_capture_is_refused_rather_than_miscompiled() {
    let e = refuse(
        "function outer() { let a = 1; function inner() { return a; } return inner(); } return outer();",
    );
    assert_eq!(
        e.message,
        "this engine does not support closures that capture a variable yet"
    );
    assert_eq!(e.boundary, Boundary::FullJs);
}

#[test]
fn every_refusal_speaks_for_the_engine() {
    for source in [
        "return 2 ** 3;",
        "function o() { let a = 1; function i() { return a; } return i(); }",
    ] {
        let e = refuse(source);
        assert!(
            e.message.starts_with("this engine "),
            "{source:?} gave {:?}",
            e.message
        );
    }
}

// =========================================================================
// The M0 path is untouched
// =========================================================================

/// The new entry point is a second door, not a replacement: the M0 one keeps
/// its `i32` in, `i32` out shape, which is what its callers are green on.
#[test]
fn compile_qjs_still_compiles_the_m0_way() {
    let wasm = tinyvm_qjs::compile_qjs("$0*2").expect("M0 still compiles");
    let module = load(&wasm);
    assert_eq!(
        module.export_i32_arity("main"),
        Some((1, 1)),
        "M0 is one i32 in, one i32 out"
    );
    let got = ok(tinyvm::eval_wasm(&wasm, &[], &[Val::I32(21)]), "eval_wasm");
    assert!(matches!(got.as_slice(), [Val::I32(42)]));
}

// =========================================================================
// The cases that catch a wrong label depth or a reused scratch local
// =========================================================================

/// Three levels of control flow, each kind inside each other kind, with the
/// answer depending on every one of them. A branch that targets the wrong
/// label here produces a *wrong number*, not a validation failure, which is
/// exactly the failure mode this test exists for.
#[test]
fn control_flow_nests_without_losing_a_branch_target() {
    number(
        "let total = 0;
         let i = 0;
         while (i < 4) {
             if (i > 1) {
                 for (let j = 0; j < i; j++) {
                     if (j == 1) { total = total + 100; } else { total = total + 1; }
                 }
             } else {
                 total = total + 10;
             }
             i = i + 1;
         }
         return total;",
        // i=0,1: +10 twice. i=2: j=0 (+1), j=1 (+100). i=3: j=0 (+1), j=1 (+100), j=2 (+1).
        223.0,
    );
}

/// A `return` from inside a loop that is itself inside a branch, in a
/// function, where the code after the branch must never run.
#[test]
fn a_return_leaves_every_enclosing_block() {
    number(
        "function find(n) {
             for (let i = 0; i < 10; i++) {
                 if (i * i > n) {
                     while (true) { return i; }
                 }
             }
             return -1;
         }
         return find(20);",
        5.0,
    );
    number(
        "function f() { while (true) { return 1; } } return f();",
        1.0,
    );
    number("function f() { for (;;) { return 2; } } return f();", 2.0);
}

/// Scratch locals are taken and given back, so a nested expression can reuse
/// one its parent has already released -- and must not reuse one it has not.
#[test]
fn nested_expressions_do_not_clobber_each_others_scratch() {
    number("(1 && 2) + (3 || 4)", 5.0);
    number("1 && (2 && (3 && 4))", 4.0);
    number("(0 || 1) && (0 || 2)", 2.0);
    number(
        "let a = 0; let b = 0; a = (b = 2) + 1; return a * 10 + b;",
        32.0,
    );
    number("let x = 1; return (x++) + (x++);", 3.0);
    // 2 + 2 + 3: the prefix yields the new value, the postfix the old.
    number("let x = 1; return (++x) + (x++) + x;", 7.0);
    number(
        "let x = 0; let y = 0; return ((x = 1) && (y = 2)) + x + y;",
        5.0,
    );
}

/// A logical operator in a loop's test, where the short circuit runs once per
/// iteration and its scratch local is reused every time round.
#[test]
fn a_short_circuit_in_a_loop_test_runs_every_iteration() {
    number(
        "let i = 0; let guard = true; while (guard && i < 3) { i = i + 1; } return i;",
        3.0,
    );
    number(
        "let i = 0; let hits = 0; while (i < 3 || false) { hits = hits + 1; i = i + 1; } return hits;",
        3.0,
    );
}

/// Calls as arguments to calls: the operand stack has to unwind exactly, and
/// two functions with the same signature must share one type-section entry
/// without sharing a body.
#[test]
fn calls_nest_as_arguments() {
    number(
        "function add(a, b) { return a + b; }
         function mul(a, b) { return a * b; }
         return add(mul(2, 3), add(1, mul(2, 2)));",
        11.0,
    );
    number(
        "function f(a, b, c) { return a * 100 + b * 10 + c; } return f(1, 2, 3);",
        123.0,
    );
}

/// A parameter is ordinary storage: writable, and private to the frame.
#[test]
fn a_parameter_can_be_reassigned_without_touching_the_caller() {
    number(
        "function f(x) { x = x + 1; return x; } let v = 1; return f(v) * 10 + v;",
        21.0,
    );
    number(
        "function g(a, b) { let t = a; a = b; b = t; return a * 10 + b; } return g(1, 2);",
        21.0,
    );
}

/// `$N` and declared bindings share one frame in the script, and must not
/// share an index.
#[test]
fn arguments_and_bindings_coexist_in_the_script() {
    assert_eq!(
        call(
            "let x = 10; let y = 20; return $0 + $1 + x + y;",
            &[Value::Number(1.0), Value::Number(2.0)],
            &[],
        )
        .unwrap(),
        Out::Number(33.0)
    );
}

/// String literals used only inside a nested function still reach the data
/// segment, and a concatenation crossing a call boundary still allocates.
#[test]
fn strings_cross_the_call_boundary() {
    text(
        "function greet(who) { return \"hello, \" + who; } return greet(\"world\");",
        "hello, world",
    );
    text(
        "function a() { return \"x\"; } function b() { return \"y\"; } return a() + b() + a();",
        "xyx",
    );
}

/// Two hundred iterations of concatenation: the bump allocator has to grow
/// linear memory rather than run off the first page.
#[test]
fn the_heap_grows_under_a_real_workload() {
    let out =
        run("let s = \"\"; for (let i = 0; i < 200; i++) { s = s + \"0123456789\"; } return s;");
    match out {
        Out::Str(text) => {
            assert_eq!(text.len(), 2000);
            assert!(text.starts_with("0123456789"));
            assert!(text.ends_with("0123456789"));
        }
        other => panic!("want a String, got {other:?}"),
    }
}

// =========================================================================
// The name section
// =========================================================================

/// A custom section is opaque to the engine, so the load gate accepting the
/// module is no evidence that the section is well formed. This decodes it.
#[test]
fn the_name_section_names_every_function() {
    let wasm = compile(
        "function outer() { return inner(); }
         function inner() { return \"x\"; }
         return outer();",
    );
    // It still clears the gate, and it still runs.
    assert_eq!(
        run(
            "function outer() { return inner(); } function inner() { return \"x\"; } return outer();"
        ),
        Out::Str("x".to_string())
    );

    let names = function_names(&wasm).expect("the module carries a name section");
    let named: Vec<&str> = names.iter().map(|(_, n)| n.as_str()).collect();
    for want in ["main", "outer", "inner", "__add", "__alloc", "__truthy"] {
        assert!(named.contains(&want), "no name for {want:?} in {named:?}");
    }
    // Indices are function indices and must be strictly increasing, which is
    // what makes the map readable by a tool that binary-searches it.
    assert!(
        names.windows(2).all(|w| w[0].0 < w[1].0),
        "the name map is not in index order: {names:?}"
    );

    // An anonymous function expression is named after where it was written,
    // because nothing else tells two of them apart.
    let anon = function_names(&compile("return (function () { return 1; })();")).unwrap();
    assert!(
        anon.iter().any(|(_, n)| n.starts_with("<anonymous@")),
        "{anon:?}"
    );
}

/// Walk the section table for the `name` custom section and decode its
/// function-name subsection. A deliberately independent reader: it shares no
/// code with the encoder, so a wrong length or a wrong LEB shows up here.
fn function_names(wasm: &[u8]) -> Option<Vec<(u32, String)>> {
    let mut at = 8; // past the magic and version
    while at < wasm.len() {
        let id = wasm[at];
        at += 1;
        let (size, used) = leb(wasm, at)?;
        at += used;
        let body = wasm.get(at..at + size as usize)?;
        at += size as usize;
        if id != 0 {
            continue;
        }
        let (len, used) = leb(body, 0)?;
        let section_name = std::str::from_utf8(body.get(used..used + len as usize)?).ok()?;
        if section_name != "name" {
            continue;
        }
        let mut sub = used + len as usize;
        while sub < body.len() {
            let kind = body[sub];
            sub += 1;
            let (size, used) = leb(body, sub)?;
            sub += used;
            let payload = body.get(sub..sub + size as usize)?;
            sub += size as usize;
            if kind != 1 {
                continue;
            }
            let (count, mut cursor) = leb(payload, 0)?;
            let mut out = Vec::new();
            for _ in 0..count {
                let (index, used) = leb(payload, cursor)?;
                cursor += used;
                let (len, used) = leb(payload, cursor)?;
                cursor += used;
                let text = std::str::from_utf8(payload.get(cursor..cursor + len as usize)?).ok()?;
                cursor += len as usize;
                out.push((index, text.to_string()));
            }
            return Some(out);
        }
    }
    None
}

/// An unsigned LEB128 at `at`: the value and how many bytes it took.
fn leb(bytes: &[u8], at: usize) -> Option<(u32, usize)> {
    let mut value = 0u32;
    let mut shift = 0;
    let mut used = 0;
    loop {
        let byte = *bytes.get(at + used)?;
        value |= u32::from(byte & 0x7f) << shift;
        used += 1;
        if byte & 0x80 == 0 {
            return Some((value, used));
        }
        shift += 7;
    }
}

// =========================================================================
// More of the boundary, and the runtime's own
// =========================================================================

/// `%` runs in both spellings, and `__rem` is one runtime function that the
/// compound form reaches through the same lowering as the infix one.
#[test]
fn the_remainder_operator_runs_in_both_spellings() {
    assert_eq!(run("return -5 % 3;"), Out::Number(-2.0));
    assert_eq!(run("let x = -5; x %= 3; return x;"), Out::Number(-2.0));
    // The compound form's value is the value assigned, as every other
    // compound assignment's is.
    assert_eq!(run("let x = -5; return (x %= 3);"), Out::Number(-2.0));
}

/// The three conversions are reachable from source and each one answers.
/// `convert.rs` holds the algorithms; `runtime.rs` calls them from `__add`,
/// `__to_number` and the relational four.
#[test]
fn each_conversion_is_reachable_from_source() {
    // ToString of a Number, StringToNumber, String relational comparison --
    // reached through `+`, through `-`, and through `<`.
    assert_eq!(
        call("return \"a\" + 1;", &[], &[]),
        Ok(Out::Str("a1".to_string()))
    );
    assert_eq!(call("return \"1\" - 1;", &[], &[]), Ok(Out::Number(0.0)));
    assert_eq!(call("return \"a\" < \"b\";", &[], &[]), Ok(Out::Bool(true)));
    // The conversion that is still missing is 7.1.1 ToPrimitive, which needs
    // a prototype. Its operand is an Object, and it traps.
    match call("const o = {}; return \"x\" + o;", &[], &[]) {
        Err(message) => assert!(
            message.contains("trap in"),
            "an Object operand failed for the wrong reason: {message}"
        ),
        Ok(value) => panic!("\"x\" + o produced {value:?} instead of trapping"),
    }
}

/// A function declared inside a function is an ordinary function: it reaches
/// its callee from any depth, because a direct call names an index and
/// captures nothing.
#[test]
fn a_function_may_be_declared_inside_a_function() {
    number(
        "function outer(n) {
             function double(x) { return x * 2; }
             return double(n) + double(1);
         }
         return outer(5);",
        12.0,
    );
}

/// A `for` whose `init` is an assignment rather than a declaration.
#[test]
fn for_accepts_an_expression_as_its_init() {
    number(
        "let i = 0; let s = 0; for (i = 0; i < 3; i++) { s = s + i; } return s;",
        3.0,
    );
    number("let i = 5; for (; i > 0; i--) { } return i;", 0.0);
}
