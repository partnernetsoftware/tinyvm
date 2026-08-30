//! A conformance corpus for the three constructs this milestone was for:
//! the conditional expression (ECMA-262 13.14), `try`/`catch`/`finally`/
//! `throw` (14.14, 14.15), and `JSON` (25.5).
//!
//! # Where the expectations come from
//!
//! Every row below was written from the specification text and then run.
//! Where this engine and ECMA-262 genuinely part company, the row asserts
//! **what this engine does today** and is marked `DIVERGENCE:` with the
//! answer JavaScript gives; a corpus that asserted the standard and failed
//! would be a wish list, and a corpus that quietly asserted the
//! implementation would be a mirror. Every `DIVERGENCE:` here was confirmed
//! against a second engine (`node`), not merely reasoned about.
//!
//! A second marker, `DEFECT:`, appears in the last section. It is not a
//! divergence in the *language* -- it is a diagnostic that says something
//! untrue about this engine, asserted as it reads today so that the untruth
//! is executable and a fix breaks a test rather than passing silently.
//!
//! # What this file is not
//!
//! It is not a second copy of `tests/conditional_and_try.rs`, which is the
//! lowering's own file, nor of `tests/json.rs`, which is the JSON set's. Rows
//! that merely restate those are left out. What is here is the spec's corners
//! that the mechanism's own tests had no reason to reach -- completion
//! values, evaluation order, early errors, scoping, precedence at the two
//! rungs on either side, and the nesting bound -- plus, in section D, the one
//! question a test written from inside the implementation cannot ask: **what
//! does a JavaScript source text actually get?**
//!
//! # Section D, and why `JSON` is measured from the outside
//!
//! `tests/json.rs` assembles its module by hand, on the stated grounds that
//! `src/emit.rs` was another lane's file and the wiring would be a hook the
//! integrator makes. Measured here at HEAD, that hook does not exist:
//! `src/emit.rs` contains no reference to `convert::build_json`, and so no
//! source text in any of the three [`Names`] modes reaches the JSON set. The
//! corpus in section D is therefore written as JavaScript, each row carrying
//! the answer ECMA-262 requires of it, and asserted to be **refused** today.
//! It is a lock in the honest direction: the day the hook lands, these rows
//! are the acceptance test, and until then nothing in the tree can claim
//! `JSON` is reachable while this file is green.

use tinyvm::{Limits, Val, WasmInstance, WasmModule};
use tinyvm_qjs::{
    CompileError, HostFn, HostParam, HostResult, Names, Options, Value, compile_qjs_m1,
    compile_qjs_m1_with,
};

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
    /// An Object or a function came back. The tag is all a host can read.
    Object,
    Function,
}

/// `repr.rs` numbers these 5 and 6. Restated from outside rather than
/// imported, as the neighbouring conformance files restate them: a contract
/// checked only from inside is not checked.
const TAG_OBJECT: i32 = 5;
const TAG_FUNCTION: i32 = 6;

/// `runtime.rs`'s `FAULT_WORD` -- the first word of linear memory, which the
/// bump pointer never hands out.
const FAULT_WORD: usize = 0;
/// `FAULT_NONE`, and the code an uncaught `throw` writes there.
const FAULT_NONE: i32 = 0;
const FAULT_UNCAUGHT_THROW: i32 = 2;

fn instantiate(source: &str) -> WasmInstance {
    let wasm =
        compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {}", e.message));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .unwrap_or_else(|e| panic!("load gate rejected {source:?}: {}", e.message()));
    module
        .instantiate()
        .unwrap_or_else(|e| panic!("instantiating {source:?}: {}", e.message()))
}

#[track_caller]
fn run(source: &str) -> Out {
    let mut instance = instantiate(source);
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .unwrap_or_else(|e| panic!("trap in {source:?}: {}", e.message()));
    match vals[..] {
        [Val::I32(TAG_OBJECT), _] => return Out::Object,
        [Val::I32(TAG_FUNCTION), _] => return Out::Function,
        _ => {}
    }
    let value = Value::returned(&vals)
        .unwrap_or_else(|e| panic!("{source:?}: cannot read the result back: {e}"));
    match value {
        Value::Undefined => Out::Undefined,
        Value::Null => Out::Null,
        Value::Number(x) => Out::Number(x),
        Value::Bool(b) => Out::Bool(b),
        Value::String(ptr) => {
            let view = instance.memory().expect("guest memory");
            let at = ptr as usize;
            let len =
                u32::from_le_bytes([view[at], view[at + 1], view[at + 2], view[at + 3]]) as usize;
            Out::Str(String::from_utf8(view[at + 4..at + 4 + len].to_vec()).expect("UTF-8"))
        }
    }
}

#[track_caller]
fn num(source: &str) -> f64 {
    match run(source) {
        Out::Number(x) => x,
        other => panic!("{source:?} answered {other:?}, not a Number"),
    }
}

#[track_caller]
fn text(source: &str) -> String {
    match run(source) {
        Out::Str(s) => s,
        other => panic!("{source:?} answered {other:?}, not a String"),
    }
}

/// Compile something expected to be refused, and answer the sentence.
#[track_caller]
fn refuse(source: &str) -> String {
    match compile_qjs_m1(source) {
        Ok(bytes) => panic!(
            "{source:?} compiled to {} bytes; a refusal was expected",
            bytes.len()
        ),
        Err(e) => e.message,
    }
}

/// Run something expected to trap, and read the guest's own account of why.
#[track_caller]
fn trap_fault(source: &str) -> i32 {
    let mut instance = instantiate(source);
    let error = instance
        .invoke_by_name("main", &Value::args(&[]))
        .unwrap_err();
    assert!(
        error.message().contains("unreachable"),
        "{source:?} trapped with {:?}, not the guest's own `unreachable`",
        error.message()
    );
    let view = instance.memory().expect("guest memory");
    i32::from_le_bytes([
        view[FAULT_WORD],
        view[FAULT_WORD + 1],
        view[FAULT_WORD + 2],
        view[FAULT_WORD + 3],
    ])
}

fn with_host_names(source: &str) -> Result<Vec<u8>, CompileError> {
    compile_qjs_m1_with(
        source,
        Options {
            names: Names::HostImport,
        },
    )
}

/// Every sentence this engine refuses with has to speak for the engine rather
/// than blame the script. Checked on every refusal this file makes.
#[track_caller]
fn speaks_for_the_engine(message: &str) {
    assert!(
        message.starts_with("this engine"),
        "{message:?} does not speak for the engine"
    );
    for blame in [
        "syntax error",
        "Syntax error",
        "SyntaxError",
        "invalid ",
        "illegal ",
    ] {
        assert!(
            !message.contains(blame),
            "{message:?} blames the script with {blame:?}"
        );
    }
}

// =========================================================================
// A -- 13.14, the conditional expression
// =========================================================================

/// 13.14.1: the value is one of the two operands, unconverted. Every type
/// this engine has passes through, and a reference passes through as itself.
#[test]
fn the_value_of_a_conditional_is_an_operand_and_not_a_boolean() {
    assert_eq!(run("return true ? 1 : 2;"), Out::Number(1.0));
    assert_eq!(run("return false ? 1 : 2;"), Out::Number(2.0));
    assert_eq!(run("return true ? \"a\" : \"b\";"), Out::Str("a".into()));
    assert_eq!(run("return false ? 1 : null;"), Out::Null);
    assert_eq!(run("return false ? 1 : undefined;"), Out::Undefined);
    assert_eq!(run("return true ? false : true;"), Out::Bool(false));
    assert_eq!(run("return true ? {} : 1;"), Out::Object);
    assert_eq!(run("return true ? function () {} : 1;"), Out::Function);

    // A Number keeps every bit, `-0` and NaN included -- the branch is a
    // value and not a copy through some narrower channel. `1 / -0` is the
    // only observation that tells `-0` from `0` (6.1.6.1).
    assert_eq!(num("return 1 / (true ? -0 : 1);"), f64::NEG_INFINITY);
    assert!(num("return false ? 1 : 0 / 0;").is_nan());

    // 7.2.15: a reference through a branch is the same reference.
    assert_eq!(
        run("const o = {}; return (true ? o : {}) === o;"),
        Out::Bool(true)
    );
    assert_eq!(
        run("const f = function () {}; return (false ? 1 : f) === f;"),
        Out::Bool(true)
    );
}

/// 13.14.1 step 2 is `ToBoolean(exprValue)` (7.1.2). Every falsy value the
/// table names is here except the two this engine has no type for -- BigInt's
/// `0n`, and the `document.all` a host object slot would carry.
///
/// The rows that matter are the ones a `!= 0` test would get wrong: `"0"` and
/// `" "` are **truthy** Strings whose numeric value is falsy, and every Object
/// is truthy however empty it is.
#[test]
fn the_test_is_toboolean_over_every_falsy_value_the_language_has() {
    let pick = "function pick(c) { return c ? \"y\" : \"n\"; } ";
    for (expr, want) in [
        // 7.1.2's falsy list, in the spec's own order.
        ("undefined", "n"),
        ("null", "n"),
        ("false", "n"),
        ("0", "n"),
        ("-0", "n"),
        ("0 / 0", "n"), // NaN
        ("\"\"", "n"),
        // and the near misses.
        ("true", "y"),
        ("1", "y"),
        ("-1", "y"),
        ("\"0\"", "y"),
        ("\" \"", "y"),
        ("\"false\"", "y"),
        ("{}", "y"),
        ("{ a: 1 }", "y"),
        ("function () {}", "y"),
        ("1 / 0", "y"), // Infinity
    ] {
        assert_eq!(
            text(&format!("{pick} return pick({expr});")),
            want,
            "ToBoolean({expr})"
        );
    }
}

/// 13.14.1: the three operands are evaluated left to right, and the test is
/// evaluated **once**, before either branch.
///
/// A lowering that evaluated the test twice -- once to branch and once to
/// yield -- would pass every value assertion above and fail this.
#[test]
fn the_operands_are_evaluated_left_to_right_and_the_test_exactly_once() {
    const LOG: &str = "let log = \"\"; \
         function a() { log = log + \"a\"; return true; } \
         function b() { log = log + \"b\"; return 1; } \
         function c() { log = log + \"c\"; return 2; } ";
    assert_eq!(
        text(&format!("{LOG} a() ? b() : c(); return log;")),
        "ab",
        "the test, then the then-branch, and nothing else"
    );
    assert_eq!(
        num(
            "let n = 0; function bump() { n = n + 1; return 1; } bump() ? bump() : bump(); return n;"
        ),
        2.0,
        "one evaluation of the test and one of a branch"
    );
}

/// 13.14: `ShortCircuitExpression ? AssignmentExpression : AssignmentExpression`
/// -- so the conditional binds **looser** than `||` and **tighter** than `=`,
/// and both branches take a whole AssignmentExpression, which is what makes
/// the form right-associative without a rule saying so.
#[test]
fn the_conditional_sits_between_short_circuit_and_assignment() {
    // Tighter than the arithmetic and relational rungs on its left.
    assert_eq!(text("return 1 + 1 ? \"t\" : \"f\";"), "t");
    assert_eq!(text("return 1 < 2 ? \"a\" : \"b\";"), "a");
    // Looser than `||`: read the other way `true || false ? 1 : 2` would be
    // `true || (false ? 1 : 2)`, whose value is the Boolean `true`.
    assert_eq!(text("return typeof (true || false ? 1 : 2);"), "number");
    // The else-branch swallows the whole of what follows it.
    assert_eq!(num("return false ? 1 : 2 + 3;"), 5.0);
    // Right-associative, both by chaining and by nesting in the middle.
    assert_eq!(num("return false ? 1 : true ? 2 : 3;"), 2.0);
    assert_eq!(num("return true ? 1 : 2 ? 3 : 4;"), 1.0);
    assert_eq!(num("return true ? false ? 1 : 2 : 3;"), 2.0);
    // A whole conditional is itself the test of another.
    assert_eq!(text("return (false ? 0 : 1) ? \"a\" : \"b\";"), "a");
}

/// It is an expression, so it goes where one goes -- and the places worth
/// naming are the ones `fleet.js` uses it in.
#[test]
fn a_conditional_goes_where_an_expression_goes() {
    // `fleet.js` line 14, reduced to what runs with no host.
    assert_eq!(
        text(
            "function call(p) { return p === undefined ? \"{}\" : p; } return call() + call(\"x\");"
        ),
        "{}x"
    );
    assert_eq!(
        num("const o = { a: 1, b: 2 }; return o[true ? \"a\" : \"b\"];"),
        1.0,
        "as a computed key"
    );
    assert_eq!(
        num("const o = { k: false ? 1 : 2 }; return o.k;"),
        2.0,
        "as a property value"
    );
    assert_eq!(
        num(
            "let i = 0; let n = 0; while (i < 3 ? true : false) { n = n + 1; i = i + 1; } return n;"
        ),
        3.0,
        "as a loop test"
    );
    assert_eq!(
        num("const f = true ? function () { return 1; } : function () { return 2; }; return f();"),
        1.0,
        "with function values as its branches"
    );
    assert_eq!(
        num("try { throw true ? 1 : 2; } catch (e) { return e; }"),
        1.0,
        "as the operand of `throw`"
    );
    assert_eq!(
        num("const a = 1; const b = 2; return (a < b ? a : b) + (a > b ? a : b);"),
        3.0,
        "twice in one expression"
    );
}

/// 13.14 is not a valid assignment target (13.15.1 requires a simple one), so
/// `(c ? a : b) = 1` is an early error.
#[test]
fn a_conditional_is_not_something_to_assign_to() {
    let message = refuse("let a = 1; let b = 2; return (true ? a : b) = 3;");
    speaks_for_the_engine(&message);
    assert!(
        message.contains("left of an assignment"),
        "{message:?} does not say what an assignment target has to be"
    );
}

/// 12.10: nothing in a conditional is a restricted production, so it may be
/// broken across lines freely -- but the `return` in front of it is, and one
/// line break there changes the program.
#[test]
fn a_line_break_inside_a_conditional_is_not_a_semicolon_but_one_after_return_is() {
    assert_eq!(run("return true\n? 1\n: 2;"), Out::Number(1.0));
    assert_eq!(run("return true ?\n1 :\n2;"), Out::Number(1.0));
    // 12.10.1: `return [no LineTerminator here] Expression`. The semicolon
    // goes in, `main` answers `undefined`, and the conditional that follows
    // is an expression statement nobody reads.
    assert_eq!(
        run("return\ntrue ? 1 : 2;"),
        Out::Undefined,
        "ASI turns this into `return; true ? 1 : 2;`"
    );
}

/// The compiler's frame budget is a diagnostic and not a process abort, and
/// the conditional's own recursion is inside it (`prd/PRD.md`: "nesting
/// bounded by a diagnostic, not an abort").
#[test]
fn a_conditional_nested_past_the_frame_budget_is_a_diagnostic() {
    let deep = |n: usize| format!("return {}1;", "1 ? 1 : ".repeat(n));
    assert_eq!(run(&deep(100)), Out::Number(1.0));
    let message = refuse(&deep(400));
    speaks_for_the_engine(&message);
    assert!(
        message.contains("nested") && message.contains("budget"),
        "{message:?} does not name the frame budget"
    );
}

// =========================================================================
// B -- 14.14 and 14.15, throw / try / catch / finally
// =========================================================================

/// 14.14.1: the thrown value is the Expression's value, of any type, and an
/// Object arrives as itself rather than as a copy.
#[test]
fn a_thrown_value_is_any_javascript_value() {
    assert_eq!(num("try { throw 1; } catch (e) { return e; }"), 1.0);
    assert_eq!(text("try { throw \"s\"; } catch (e) { return e; }"), "s");
    assert_eq!(
        run("try { throw false; } catch (e) { return e; }"),
        Out::Bool(false)
    );
    assert_eq!(
        run("try { throw null; } catch (e) { return e; }"),
        Out::Null
    );
    assert_eq!(
        run("try { throw undefined; } catch (e) { return e; }"),
        Out::Undefined
    );
    assert_eq!(
        run("const o = {}; try { throw o; } catch (e) { return e === o; }"),
        Out::Bool(true)
    );
    assert_eq!(
        text("const f = function () {}; try { throw f; } catch (e) { return typeof e; }"),
        "function"
    );
}

/// 14.15.3: the finalizer runs on all four ways out of the `try` statement.
#[test]
fn a_finalizer_runs_on_the_normal_the_caught_the_throwing_and_the_returning_path() {
    assert_eq!(
        num("let out = 0; try { out = 1; } finally { out = out + 10; } return out;"),
        11.0,
        "normal"
    );
    assert_eq!(
        num(
            "let out = 0; try { throw 1; } catch (e) { out = e; } finally { out = out + 10; } return out;"
        ),
        11.0,
        "caught"
    );
    assert_eq!(
        num(
            "let out = 0; try { try { throw 1; } finally { out = 10; } } catch (e) { return out + e; }"
        ),
        11.0,
        "throwing, and the throw continues past the finalizer"
    );
    assert_eq!(
        text(
            "let log = \"\"; function f() { try { return \"r\"; } finally { log = \"f\"; } } const v = f(); return v + log;"
        ),
        "rf",
        "returning, and the return keeps its value"
    );
}

/// 14.15.3 with a Finally: `B` is the finalizer's completion; **if `B` is a
/// normal completion, `B` is set to `F`**, the try/catch's own completion.
/// So a finalizer that finishes normally contributes nothing at all, not even
/// its value.
///
/// This was a DIVERGENCE when the corpus was written -- the engine let the
/// finalizer's value out, where JavaScript discards it -- and the integration
/// closed it: `Lower::try_finally` holds the pending completion across the
/// finalizer and puts it back. The third column, which was `node`'s answer
/// and the engine's refusal, is now both. The reach is exactly the script's
/// ECMA-262 completion value -- the value `main` answers with when the script
/// has no `return` -- because a `return` is an abrupt completion and takes the
/// other path, which was always right (see the next test).
#[test]
fn a_normally_completing_finalizer_should_not_replace_the_pending_value() {
    // (source, what ECMA-262 14.15.3 requires, what the finalizer's own last
    //  value would have been -- the answer before the fix)
    let rows: [(&str, f64, f64); 4] = [
        ("try { 1; } finally { 2; }", 1.0, 2.0),
        ("let n = 0; try { 9; } finally { n = 1; }", 9.0, 1.0),
        ("try { throw 1; } catch (e) { 5; } finally { 6; }", 5.0, 6.0),
        ("try { 4; } catch (e) { } finally { 8; }", 4.0, 8.0),
    ];
    for (source, ecma262, finalizers_own) in rows {
        assert_ne!(
            ecma262, finalizers_own,
            "a row where the two agree proves nothing"
        );
        assert_eq!(num(source), ecma262, "14.15.3 step 3: {source:?}");
    }
    // The finalizer still runs, which is what says the value was discarded
    // rather than the block skipped.
    assert_eq!(
        num("let n = 0; try { 9; } finally { n = 1; } return n;"),
        1.0
    );
    // And it is the *pending* value that survives, however deep the nesting.
    assert_eq!(num("try { try { 1; } finally { 2; } } finally { 3; }"), 1.0);
    // A finalizer with no value of its own is already right, which is what
    // says the defect is the *value* and not the finalizer.
    assert_eq!(num("try { 1; } finally { }"), 1.0);
    assert_eq!(num("try { 1; } catch (e) { } finally { }"), 1.0);
    // And UpdateEmpty(B, undefined) is right where the block is empty: an
    // empty completion becomes `undefined` rather than reaching further back.
    assert_eq!(run("1; try { } finally { }"), Out::Undefined);
    assert_eq!(run("try { throw 1; } catch (e) { }"), Out::Undefined);
    // Nor does a `catch` leak its value where the spec keeps it -- 14.15.3
    // without a Finally is `UpdateEmpty(F, undefined)` and this engine agrees.
    assert_eq!(num("try { 1; } catch (e) { 3; }"), 1.0);
    assert_eq!(num("try { throw 1; } catch (e) { 5; }"), 5.0);
}

/// 14.15.3: an **abrupt** finalizer is the one that replaces what was pending,
/// and this engine has it exactly right in all four combinations.
#[test]
fn an_abruptly_completing_finalizer_replaces_what_was_pending() {
    assert_eq!(
        num("function f() { try { return 1; } finally { return 2; } } return f();"),
        2.0,
        "return over return"
    );
    assert_eq!(
        num(
            "function f() { try { throw 1; } finally { return 2; } } try { return f(); } catch (e) { return 99; }"
        ),
        2.0,
        "return over a pending throw"
    );
    assert_eq!(
        num("try { try { throw 1; } finally { throw 2; } } catch (e) { return e; }"),
        2.0,
        "throw over a pending throw"
    );
    assert_eq!(
        num(
            "function f() { try { return 1; } finally { throw 2; } } try { f(); } catch (e) { return e; }"
        ),
        2.0,
        "throw over a pending return"
    );
    assert_eq!(
        num(
            "function f() { try { throw 1; } catch (e) { return \"c\"; } finally { throw 2; } } try { f(); } catch (e) { return e; }"
        ),
        2.0,
        "and over what the catch clause decided"
    );
}

/// 14.15.3: the Catch clause is evaluated *outside* the try block it belongs
/// to, so a throw in it is the try statement's own completion and only an
/// enclosing handler may take it.
#[test]
fn a_throw_inside_catch_leaves_its_own_try() {
    assert_eq!(
        num("try { try { throw 1; } catch (e) { throw e + 1; } } catch (e) { return e; }"),
        2.0
    );
    assert_eq!(
        num(
            "function f() { try { throw 1; } catch (e) { throw e; } } try { f(); } catch (q) { return q; }"
        ),
        1.0,
        "a re-throw crosses the call boundary like any other"
    );
    assert_eq!(
        trap_fault("try { throw 1; } catch (e) { throw 2; }"),
        FAULT_UNCAUGHT_THROW,
        "and with no enclosing handler it leaves the script"
    );
}

/// Nested `try` statements, and finalizers running innermost first as the
/// unwind passes each one.
#[test]
fn nested_handlers_run_from_the_inside_out() {
    assert_eq!(
        text(
            "let log = \"\"; function f() { try { try { throw 1; } finally { log = log + \"a\"; } } finally { log = log + \"b\"; } } try { f(); } catch (e) { log = log + \"c\"; } return log;"
        ),
        "abc"
    );
    assert_eq!(
        num(
            "function f() { try { try { return 1; } finally { return 2; } } finally { return 3; } } return f();"
        ),
        3.0,
        "and each abrupt finalizer overrides the last"
    );
    assert_eq!(
        num(
            "try { try { try { throw 1; } catch (e) { throw e + 1; } } catch (e) { throw e + 1; } } catch (e) { return e; }"
        ),
        3.0,
        "three deep"
    );
}

/// A throw crosses a call, however the call was written and however many
/// frames deep it started.
#[test]
fn a_throw_crosses_every_kind_of_call() {
    const T: &str = "function t() { throw \"T\"; } ";
    for call in [
        "t()",
        "(function () { return t(); })()",
        "({ m: function () { return t(); } }).m()",
    ] {
        assert_eq!(
            text(&format!("{T} try {{ {call}; }} catch (e) {{ return e; }}")),
            "T",
            "{call}"
        );
    }
    assert_eq!(
        text("function t() { throw \"T\"; } const f = t; try { f(); } catch (e) { return e; }"),
        "T",
        "through a function value, which is `call_indirect`"
    );
    assert_eq!(
        text(
            "function deep(n) { if (n === 0) { throw \"bottom\"; } return deep(n - 1); } try { deep(20); } catch (e) { return e; }"
        ),
        "bottom",
        "twenty frames of recursion"
    );
    // The statements after a throwing call do not run, and neither does the
    // rest of the expression the call was in.
    assert_eq!(
        num(
            "function t() { throw 1; } let n = 0; function s(x) { n = n + 1; return x; } try { s(t()); } catch (e) { return n; }"
        ),
        0.0,
        "the outer call never happens"
    );
    assert_eq!(
        text(
            "function t() { throw 1; } let s = \"\"; try { s = \"x\" + t(); } catch (e) { return s; }"
        ),
        "",
        "and the assignment the throw interrupted did not land"
    );
}

/// A throw in flight is a property of one unwind, not of the program: after
/// a caught one, the next call and the next `try` behave as if nothing had
/// happened.
#[test]
fn a_caught_throw_leaves_nothing_behind() {
    assert_eq!(
        num(
            "function t() { throw 1; } function g() { return 5; } try { t(); } catch (e) { } return g();"
        ),
        5.0,
        "a normal call after a caught throw returns normally"
    );
    assert_eq!(
        num(
            "function t() { throw 1; } try { t(); } catch (e) { } try { t(); } catch (e) { return 2; }"
        ),
        2.0,
        "and the second try still catches"
    );
    assert_eq!(
        num(
            "let n = 0; function t() { n = n + 1; throw n; } try { t(); } catch (e) { } try { t(); } catch (e) { return e; }"
        ),
        2.0,
        "with the second throw's own value, not the first's"
    );
    assert_eq!(
        num(
            "let n = 0; for (let i = 0; i < 3; i = i + 1) { try { throw i; } catch (e) { n = n + e; } } return n;"
        ),
        3.0,
        "and a handler inside a loop is entered afresh every pass"
    );
    // Nothing is written to the fault word when the throw was handled.
    let mut instance = instantiate("try { throw 1; } catch (e) { } return 0;");
    instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect("no trap");
    let view = instance.memory().expect("guest memory");
    assert_eq!(
        i32::from_le_bytes([view[0], view[1], view[2], view[3]]),
        FAULT_NONE
    );
}

/// An uncaught throw is **one** fault code whatever was thrown -- the host
/// learns that a throw escaped, and nothing about the value, because a
/// `(tag, payload)` pair does not fit in a fault word and inventing a channel
/// for it would be a second value representation.
#[test]
fn an_uncaught_throw_is_one_fault_code_for_every_thrown_type() {
    for thrown in ["1", "\"s\"", "true", "null", "undefined", "{}", "{ a: 1 }"] {
        assert_eq!(
            trap_fault(&format!("throw {thrown};")),
            FAULT_UNCAUGHT_THROW,
            "throw {thrown}"
        );
        assert_eq!(
            trap_fault(&format!("function t() {{ throw {thrown}; }} t();")),
            FAULT_UNCAUGHT_THROW,
            "from a callee: throw {thrown}"
        );
    }
    assert_eq!(
        trap_fault("let out = 0; try { throw 1; } finally { out = 1; }"),
        FAULT_UNCAUGHT_THROW,
        "a finalizer runs and the throw still leaves the script"
    );
}

/// 14.15: the Catch clause introduces a scope holding its parameter, and the
/// try block is a scope of its own. `var` still reaches the function.
#[test]
fn the_clauses_are_scopes_and_var_still_is_not() {
    assert_eq!(
        num("let e = 1; try { throw 2; } catch (e) { return e; }"),
        2.0,
        "the parameter shadows an outer binding"
    );
    assert_eq!(
        num("let e = 1; try { throw 2; } catch (x) { } return e;"),
        1.0,
        "and leaves it alone"
    );
    assert_eq!(
        num("try { throw 1; } catch (e) { e = 5; return e; }"),
        5.0,
        "and is writable, which 14.15.2 makes it"
    );
    assert_eq!(
        num("function f() { try { var v = 1; } catch (e) { } return v; } return f();"),
        1.0,
        "a `var` in the try block is the function's, per 14.15.3 and 8.6.1"
    );
    // 14.15's optional CatchParameter.
    assert_eq!(
        num("let out = 0; try { throw 1; } catch { out = 5; } return out;"),
        5.0
    );
    // The parameter is not visible after the statement -- and this engine has
    // no global scope for the name to be absent from, so the read is refused
    // rather than answered `undefined`. That refusal is the engine's
    // documented boundary (`README.md`, "typeof"), not this milestone's.
    let message = refuse("try { throw 1; } catch (e) { } return typeof e;");
    speaks_for_the_engine(&message);
    assert!(message.contains("no declaration of `e`"), "{message:?}");
}

/// 14.15.1's two early-error rules about the catch parameter, and this engine
/// gets each one backwards from the other's direction.
///
/// DIVERGENCE, twice over:
///
/// * `catch (e) { let e = 1; }` is a **SyntaxError** -- BoundNames of
///   CatchParameter may not occur in the LexicallyDeclaredNames of the Block.
///   This engine compiles it and lets the inner `let` shadow the parameter.
///   (`node`: "Identifier 'e' has already been declared".)
/// * `catch (e) { var e; }` is **legal** -- B.3.4 exempts a simple
///   BindingIdentifier from the VarDeclaredNames rule, and every web engine
///   implements it. This engine refuses it.
///
/// Neither costs `fleet.js` anything, and the refusing direction is the safe
/// one; both are written down because a corpus that only recorded the
/// permissive half would make this engine look stricter than it is.
#[test]
fn the_catch_parameters_two_early_error_rules_both_go_the_other_way() {
    assert_eq!(
        num("try { throw 1; } catch (e) { let e = 2; return e; }"),
        2.0,
        "DIVERGENCE: ECMA-262 14.15.1 makes this an early SyntaxError"
    );
    let message = refuse("try { } catch (e) { var e; }");
    speaks_for_the_engine(&message);
    assert!(
        message.contains("bind `e` twice"),
        "DIVERGENCE: B.3.4 allows this; the refusal at least says why: {message:?}"
    );
    // A differently named binding is fine in both directions, which is what
    // says the rows above are about the *name* and not about the clause.
    assert_eq!(
        num("try { throw 1; } catch (e) { let f = 2; return e + f; }"),
        3.0
    );
}

/// The grammar's own requirements, each refused with what was wanted.
#[test]
fn the_shape_of_a_try_statement_is_the_grammars() {
    for (source, wanted) in [
        ("try { }", "catch"),                       // 14.15 needs one clause
        ("throw;", "value after `throw`"),          // 14.14 needs an Expression
        ("try 1; catch (e) { }", "`{`"),            // the try body is a Block
        ("try { throw 1; } catch (1) { }", "name"), // a CatchParameter is a binding
    ] {
        let message = refuse(source);
        speaks_for_the_engine(&message);
        assert!(
            message.contains(wanted),
            "{source:?} answered {message:?}, which does not name {wanted:?}"
        );
    }
    // 12.10: `throw [no LineTerminator here] Expression` is restricted, so
    // the line break is the error and not a `throw undefined`.
    let message = refuse("throw\n1;");
    speaks_for_the_engine(&message);
    assert!(
        message.contains("line") || message.contains("12.10"),
        "{message:?} does not explain the line break"
    );
    // 13.2.5.1: the four keywords are still IdentifierNames, so they are
    // still property names.
    assert_eq!(
        num(
            "const o = { try: 1, catch: 2, finally: 3, throw: 4 }; return o.try + o.catch + o.finally + o.throw;"
        ),
        10.0
    );
}

/// `try` recurses in the parser like everything else, and the bound is a
/// diagnostic rather than an abort.
#[test]
fn a_try_nested_past_the_frame_budget_is_a_diagnostic() {
    let deep = |n: usize| {
        format!(
            "{}throw 1;{}",
            "try { ".repeat(n),
            " } catch (e) { }".repeat(n)
        )
    };
    assert_eq!(run(&deep(50)), Out::Undefined);
    let message = refuse(&deep(200));
    speaks_for_the_engine(&message);
    assert!(
        message.contains("nested") && message.contains("budget"),
        "{message:?} does not name the frame budget"
    );
}

// =========================================================================
// C -- the two constructs together, as `fleet.js` writes them
// =========================================================================

/// `fleet.js`'s `call()`, reduced to what runs with no host at all: the
/// conditional supplying a default argument, and the `try` turning a failed
/// parse into the raw text.
#[test]
fn the_shape_fleet_js_uses_them_in_runs() {
    const LIB: &str = "\
        function parse(s) { if (s === \"{}\") { return { ok: 1 }; } throw \"bad json\"; } \
        function host(op, p) { return op === \"good\" ? \"{}\" : \"nope\"; } \
        function call(opId, params) { \
            const resultJson = host(opId, params === undefined ? \"{}\" : params); \
            try { return parse(resultJson); } catch (_err) { return resultJson; } \
        } ";
    assert_eq!(
        num(&format!("{LIB} return call(\"good\").ok;")),
        1.0,
        "the parsed object comes back"
    );
    assert_eq!(
        text(&format!("{LIB} return call(\"bad\");")),
        "nope",
        "and a failed parse falls back to the raw text"
    );
    assert_eq!(
        text(&format!("{LIB} return call(\"bad\", \"{{\\\"a\\\":1}}\");")),
        "nope",
        "with an explicit params argument, which the conditional leaves alone"
    );
}

// =========================================================================
// D -- 25.5, JSON: what a source text actually gets
// =========================================================================

/// The corpus, as JavaScript, with the answer ECMA-262 25.5 requires of each
/// row. Nine of these shapes are what `agenterm/scripts/qjs/lib/fleet.js`
/// asks for; the rest are 25.5's own corners.
///
/// The second column was **not asserted** when this file was written, because
/// no row ran: `src/emit.rs` named `convert::build_json` nowhere, so the JSON
/// set existed and nothing in the language could reach it. The integration
/// joined the two and the column is now the assertion -- which is what the
/// corpus was written to become.
const JSON_CORPUS: &[(&str, Want)] = &[
    // -- round-tripping every value type (25.5.1, 25.5.2) --
    ("return JSON.stringify(1);", Want::Str("1")),
    // Written `1 / 2` and not `1.5`: a fractional *literal* is a boundary of
    // its own that this engine has not reached yet, and a row that stopped
    // there would be measuring the lexer instead of `JSON`.
    ("return JSON.stringify(1 / 2);", Want::Str("0.5")),
    // 6.1.6.1.20 step 2: the sign of a negative zero is lost at the printer,
    // which is exactly where the specification loses it.
    ("return JSON.stringify(-0);", Want::Str("0")),
    ("return JSON.stringify(true);", Want::Str("true")),
    ("return JSON.stringify(null);", Want::Str("null")),
    ("return JSON.stringify(\"s\");", Want::Str("\"s\"")),
    ("return JSON.stringify({});", Want::Str("{}")),
    (
        "return JSON.stringify({ a: 1, b: \"x\" });",
        Want::Str("{\"a\":1,\"b\":\"x\"}"),
    ),
    ("return JSON.parse(\"1\");", Want::Num(1.0)),
    ("return JSON.parse(\"true\");", Want::Bool(true)),
    ("return JSON.parse(\"null\");", Want::Null),
    ("return JSON.parse(\"\\\"s\\\"\");", Want::Str("s")),
    ("return JSON.parse(\"{}\");", Want::Object),
    (
        "return JSON.parse(JSON.stringify({ a: 1 })).a;",
        Want::Num(1.0),
    ),
    // -- what stringify leaves out (25.5.2.2 SerializeJSONProperty) --
    (
        "return typeof JSON.stringify(undefined);",
        Want::Str("undefined"),
    ),
    (
        "return typeof JSON.stringify(function () {});",
        Want::Str("undefined"),
    ),
    (
        "return JSON.stringify({ a: undefined, b: 1 });",
        Want::Str("{\"b\":1}"),
    ),
    (
        "return JSON.stringify({ a: function () {}, b: 1 });",
        Want::Str("{\"b\":1}"),
    ),
    // NaN is not JSON, and nor is an infinity: 25.5.2.2 step 10.
    ("return JSON.stringify(0 / 0);", Want::Str("null")),
    ("return JSON.stringify(1 / 0);", Want::Str("null")),
    // -- insertion order (10.1.11.1 OrdinaryOwnPropertyKeys) --
    (
        "return JSON.stringify({ b: 1, a: 2 });",
        Want::Str("{\"b\":1,\"a\":2}"),
    ),
    // -- escaping (25.5.2.2 QuoteJSONString) --
    (
        "return JSON.stringify(\"a\\\"b\");",
        Want::Str("\"a\\\"b\""),
    ),
    ("return JSON.stringify(\"a\\nb\");", Want::Str("\"a\\nb\"")),
    (
        "return JSON.stringify(\"a\\u0001b\");",
        Want::Str("\"a\\u0001b\""),
    ),
    // U+2028 verbatim. QuoteJSONString escapes its seven table characters,
    // everything below U+0020 and lone surrogates, and nothing else --
    // escaping the line separators is a habit from embedding JSON in JS
    // *source*, which is a different problem.
    (
        "return JSON.stringify(\"a\\u2028b\");",
        Want::Str("\"a\u{2028}b\""),
    ),
    // -- the parse grammar, and everything it excludes (25.5.1) --
    // This used to be the one row where this engine and ECMA-262 parted
    // company, and it parted at the value representation rather than at JSON:
    // 25.5.1 answers with an Array and there was none to answer with, so it
    // threw by name rather than approximating one. The Array milestone landed
    // the type; the row now asserts the spec's own answer, like every other.
    ("return JSON.parse(\"[1]\").length;", Want::Num(1.0)),
    ("return JSON.parse(\"[1,2,3]\")[1];", Want::Num(2.0)),
    ("return JSON.parse(\"[]\").length;", Want::Num(0.0)),
    (
        "try { JSON.parse(\"nope\"); } catch (e) { return 1; }",
        Want::Num(1.0),
    ),
    (
        "try { JSON.parse(\"\"); } catch (e) { return 1; }",
        Want::Num(1.0),
    ),
    (
        "try { JSON.parse(\"{'a':1}\"); } catch (e) { return 1; }",
        Want::Num(1.0),
    ),
    (
        "try { JSON.parse(\"{a:1}\"); } catch (e) { return 1; }",
        Want::Num(1.0),
    ),
    (
        "try { JSON.parse(\"[1,]\"); } catch (e) { return 1; }",
        Want::Num(1.0),
    ),
    (
        "try { JSON.parse(\"01\"); } catch (e) { return 1; }",
        Want::Num(1.0),
    ),
    (
        "try { JSON.parse(\"+1\"); } catch (e) { return 1; }",
        Want::Num(1.0),
    ),
    (
        "try { JSON.parse(\".5\"); } catch (e) { return 1; }",
        Want::Num(1.0),
    ),
    (
        "try { JSON.parse(\"NaN\"); } catch (e) { return 1; }",
        Want::Num(1.0),
    ),
    (
        "try { JSON.parse(\"undefined\"); } catch (e) { return 1; }",
        Want::Num(1.0),
    ),
    (
        "try { JSON.parse(\"1 2\"); } catch (e) { return 1; }",
        Want::Num(1.0),
    ),
    // -- the name itself --
    ("return typeof JSON;", Want::Str("object")),
    ("return typeof JSON.parse;", Want::Str("function")),
];

/// What a [`JSON_CORPUS`] row must answer. `Throws` is a value this engine
/// cannot represent reaching a `throw`, which is the only row that is not the
/// specification's own answer.
#[derive(Debug, Clone, Copy)]
/// `Throws` is kept with no row using it. It is the slot the next divergence
/// from 25.5 goes in, and the corpus test asserts the count is zero -- so the
/// variant existing is what makes that assertion able to fail.
#[allow(dead_code)]
enum Want {
    Num(f64),
    Bool(bool),
    Null,
    Str(&'static str),
    Object,
    Throws,
}

/// **Every row of [`JSON_CORPUS`] runs, and answers what 25.5 requires.**
///
/// This test used to be `no_source_text_reaches_this_engines_json`, and it
/// asserted the opposite: that every row was refused with "no declaration of
/// `JSON`". The set existed, `tests/json.rs` exercised it through a
/// hand-assembled module, and nothing in `src/emit.rs` joined it to the
/// language. `ast::Res::Json` is the join, and it is one name and no global
/// scope -- the scope walk still runs first, so a script's own declaration
/// shadows it outright.
#[test]
fn every_row_of_the_json_corpus_answers_what_ecma262_requires() {
    for (source, want) in JSON_CORPUS {
        match want {
            Want::Throws => {
                let mut instance = instantiate(source);
                let outcome = instance.invoke_by_name("main", &Value::args(&[]));
                assert!(outcome.is_err(), "{source:?} was expected to throw");
                let memory = instance.memory().expect("guest memory");
                assert_eq!(
                    tinyvm_qjs::guest_fault(&memory),
                    Some(tinyvm_qjs::GuestFault::UncaughtThrow),
                    "{source:?} trapped without recording a throw"
                );
            }
            Want::Num(x) => assert_eq!(run(source), Out::Number(*x), "{source:?}"),
            Want::Bool(b) => assert_eq!(run(source), Out::Bool(*b), "{source:?}"),
            Want::Null => assert_eq!(run(source), Out::Null, "{source:?}"),
            Want::Str(s) => assert_eq!(run(source), Out::Str((*s).to_string()), "{source:?}"),
            Want::Object => assert_eq!(run(source), Out::Object, "{source:?}"),
        }
    }
    assert_eq!(
        JSON_CORPUS.len(),
        41,
        "the corpus is a fixed list, so a row cannot be dropped to make this pass"
    );
    // **All forty-one are now ECMA-262's own answer.** There used to be one
    // exemption -- `JSON.parse("[1]")`, which threw because there was no Array
    // to answer with -- and this assertion existed so the exemption could not
    // grow quietly. It shrank instead, which is the outcome it was hoping for,
    // so it now says zero and will catch the next one being added.
    assert_eq!(
        JSON_CORPUS
            .iter()
            .filter(|(_, w)| matches!(w, Want::Throws))
            .count(),
        0,
        "a divergence from 25.5 was added; name it here and say why"
    );
}

/// The Array rows, on their own, because they are a **product** consequence
/// and not a JSON one.
///
/// This test used to be `a_json_array_is_refused_by_name_and_the_refusal_is_catchable`
/// and it asserted the opposite: that `JSON.parse("[1,2]")` threw "this engine
/// does not support JSON arrays yet", and that the `try`/`catch` `fleet.js`
/// wraps every broker answer in therefore handed a caller the **raw text**
/// where a value was expected. `tabs.list` was the named example.
///
/// That was the single largest thing standing between this engine and the
/// acceptance target's real traffic, and it is what the Array milestone was
/// for. The assertions below are the same shapes, answering correctly.
#[test]
fn a_json_array_parses_and_the_fleet_shape_gets_a_value() {
    // Anywhere in the text, not only at the top.
    assert_eq!(num("return JSON.parse(\"[1,2]\")[1];"), 2.0);
    assert_eq!(num("return JSON.parse(\"[]\").length;"), 0.0);
    assert_eq!(
        num("return JSON.parse(\"{\\\"tabs\\\":[]}\").tabs.length;"),
        0.0
    );
    assert_eq!(
        num("return JSON.parse(\"{\\\"a\\\":{\\\"b\\\":[1]}}\").a.b[0];"),
        1.0
    );

    // The shape `fleet.js` writes, on the answer `tabs.list` actually gives:
    // the `catch` is not taken, and the caller indexes a value.
    assert_eq!(
        text(
            "function call(s) { try { return JSON.parse(s); } catch (_err) { return s; } } \
             return call(\"[{\\\"id\\\":\\\"tab1\\\"}]\")[0].id;"
        ),
        "tab1"
    );

    // And the `catch` still catches what it is for: text that is not JSON.
    assert_eq!(
        text(
            "function call(s) { try { return JSON.parse(s); } catch (_err) { return s; } } \
             return call(\"[1,\");"
        ),
        "[1,"
    );

    // Round-trip, which is the other half of being usable.
    assert_eq!(
        text("return JSON.stringify(JSON.parse(\"[1,[2,{\\\"c\\\":3}]]\"));"),
        "[1,[2,{\"c\":3}]]"
    );
}

/// `JSON` is a name a script may take, which is the same fact from the other
/// side and worth its own row because "the compiler knows about JSON" is the
/// assumption a reader would otherwise make. It knows one name, the scope walk
/// runs before it, and there is no environment record anywhere.
#[test]
fn json_is_an_ordinary_name_and_a_script_may_take_it() {
    assert_eq!(
        text(
            "const JSON = { stringify: function (v) { return \"mine\"; } }; return JSON.stringify(1);"
        ),
        "mine",
        "a script's own `JSON` binding wins outright"
    );
    assert_eq!(text("const JSON = 1; return typeof JSON;"), "number");
    // Reading the engine's own twice is one object -- 25.5 makes `JSON` a
    // single ordinary object, and this engine's is a single record built once
    // per instance.
    assert_eq!(run("return JSON === JSON;"), Out::Bool(true));
    assert_eq!(run("const a = JSON; return a === JSON;"), Out::Bool(true));
    // Assigning to it without declaring it is refused, and the refusal says
    // whose name it is rather than claiming there is none.
    let message = refuse("JSON = 1; return 0;");
    speaks_for_the_engine(&message);
    assert!(message.contains("this engine's own binding"), "{message:?}");
    // Every *other* undeclared name is still refused the way it always was,
    // which is what says one name was bound and not a global scope opened.
    for name in ["Object", "Math", "Array", "console", "globalThis"] {
        let message = refuse(&format!("return {name}.x;"));
        assert!(
            message.contains(&format!("no declaration of `{name}`")),
            "{message:?}"
        );
    }
}

/// Under [`Names::HostImport`] a free name is a `js.*` import, and `JSON` used
/// to be one -- so `fleet.js` compiled and the `JSON` its nine call sites
/// reached was whatever the embedder put behind `js.JSON`, which no embedder
/// can answer with an object because `tinyvm_qjs::Value` has no Object
/// variant.
///
/// The import is gone. This is the row that keeps the acceptance claim
/// precise: the import list is the evidence that `fleet.js`'s `JSON.parse`
/// runs *this* engine's parser.
#[test]
fn json_is_this_engines_and_not_the_host_import_it_used_to_be() {
    let source = "\
        function call(opId, params) { \
            const resultJson = __host.fleet_call(opId, params === undefined ? \"{}\" : params); \
            try { return JSON.parse(resultJson); } catch (_err) { return resultJson; } \
        } \
        return call(\"tabs.list\");";
    let wasm = with_host_names(source).expect("the `fleet.js` call shape compiles");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("clears the gate");
    let imports: Vec<String> = module
        .imports()
        .iter()
        .map(|i| format!("{}.{}", i.module, i.field))
        .collect();
    assert_eq!(
        imports,
        ["js.__host"],
        "`JSON` must not be an import in any naming mode"
    );
    // A host name still is one, which is what says the default was narrowed
    // by exactly one name.
    assert!(
        refuse(source).contains("no declaration of `__host`"),
        "the default mode must not resolve a host name"
    );
    // And an embedder that *declares* a host function called `JSON` is being
    // explicit, so it wins: a declaration table is a deliberate act where
    // `HostImport`'s "any free name is an import" is a default.
    let declared = compile_qjs_m1_with(
        "return JSON(\"x\");",
        Options {
            names: Names::Declared(vec![HostFn {
                name: "JSON".into(),
                module: "sys".into(),
                field: "json".into(),
                params: vec![HostParam::StrPtrLen],
                result: HostResult::I32,
            }]),
        },
    )
    .expect("a declared `JSON` compiles");
    let module = WasmModule::from_bytes_with(&declared, Limits::default()).expect("clears");
    assert_eq!(
        module
            .imports()
            .iter()
            .map(|i| format!("{}.{}", i.module, i.field))
            .collect::<Vec<_>>(),
        ["sys.json"]
    );
}

/// A program that never names `JSON` carries none of it: not a function, not
/// an element, not a global, not a byte.
///
/// The gate is what makes the whole set affordable, and it is checked
/// structurally rather than against an absolute byte count another lane can
/// move: the same two programs, differing by one mention of the name.
#[test]
fn a_program_that_never_names_json_carries_none_of_it() {
    let without = compile_qjs_m1("return 1;").expect("compiles");
    let with = compile_qjs_m1("const j = JSON; return 1;").expect("compiles");
    // Section 4 is the table section and section 6 is the globals section.
    // Without the name there is neither a funcref table nor the unwind channel
    // `JSON.parse` raises through; with it, both.
    assert_eq!(section_len(&without, 4), None, "no table section");
    assert!(section_len(&with, 4).is_some(), "a table section");
    // One global is the bump pointer and two are the binding `j`; the three
    // unwind globals and `JSON`'s own pair are what the name added.
    assert_eq!(globals(&without), 1);
    assert_eq!(globals(&with), 1 + 2 + 3 + 2);
    assert!(
        with.len() > without.len() + 3_000,
        "the JSON set came to {} bytes, which is not a set",
        with.len() - without.len()
    );
}

/// The length of one wasm section, or `None` when the module has none.
fn section_len(bytes: &[u8], id: u8) -> Option<usize> {
    let mut at = 8;
    while at < bytes.len() {
        let this = bytes[at];
        at += 1;
        let (size, next) = uleb(bytes, at);
        if this == id {
            return Some(size);
        }
        at = next + size;
    }
    None
}

/// How many globals the module declares.
fn globals(bytes: &[u8]) -> usize {
    let mut at = 8;
    while at < bytes.len() {
        let id = bytes[at];
        at += 1;
        let (size, next) = uleb(bytes, at);
        if id == 6 {
            return uleb(bytes, next).0;
        }
        at = next + size;
    }
    0
}

fn uleb(bytes: &[u8], mut at: usize) -> (usize, usize) {
    let (mut value, mut shift) = (0usize, 0);
    loop {
        let byte = bytes[at];
        at += 1;
        value |= ((byte & 0x7f) as usize) << shift;
        if byte & 0x80 == 0 {
            return (value, at);
        }
        shift += 7;
    }
}

// =========================================================================
// E -- the diagnostics, and the claims they make about this engine
// =========================================================================

/// The rule the product sets for itself: a rejection names the engine's
/// boundary and never blames the script. The corollary, which is what this
/// section tests, is that the boundary it names has to be a real one --
/// telling a reader the engine lacks `throw` sends them hunting for a
/// workaround to a feature that shipped.
///
/// The position that was fixed: the end of a statement.
#[test]
fn the_end_of_a_statement_says_what_it_wanted_and_not_what_it_lacks() {
    let message = refuse("let a = 1 let b = 2;");
    speaks_for_the_engine(&message);
    assert!(
        message.contains("`;`") && message.contains("12.10"),
        "{message:?}"
    );
    assert!(
        !message.contains("does not support the `let`"),
        "the statement end is the position that was fixed: {message:?}"
    );
    // The conditional's own `:` is a second position that says what it wanted.
    let message = refuse("let x = 1 ? 2;");
    speaks_for_the_engine(&message);
    assert!(message.contains("`:`"), "{message:?}");
    // And so is a missing operand after one.
    let message = refuse("return true ? 1 : ;");
    speaks_for_the_engine(&message);
    assert!(message.contains("operand"), "{message:?}");
}

/// **CLOSED**, and the two features this milestone landed had each added one.
///
/// At an operand position and at a statement position, a refusal used to reach
/// for the offending token's capability phrase, so it disclaimed a capability
/// this engine demonstrably has. Each row below pairs the sentence it gives
/// now with a program that proves the capability it used to disclaim -- the
/// old lie is refuted by execution, not by argument.
///
/// The two the milestone added are the first two rows: before it, "this engine
/// does not support conditional expressions yet" and "this engine does not
/// support the `throw` keyword yet" were true sentences. Landing the features
/// turned the *feature* on and left the *sentence* alone, so the only place
/// either sentence appeared was a place where it was false.
///
/// `Parser::cannot_use` now says what the position wanted. See
/// `parse::unlowered_by_m1` for the short list of tokens whose phrase it still
/// trusts, and `conformance_m2.rs`'s
/// `a_misplaced_token_says_what_was_wanted_and_never_disclaims_what_the_engine_has`
/// for the same fix at the ten positions that corpus had recorded.
#[test]
fn no_diagnostic_here_disclaims_a_capability_this_engine_has() {
    // (a source refused, the sentence it gives now, a program that proves the
    //  capability it used to disclaim is present, what that program answers)
    let rows: [(&str, &str, &str, f64); 6] = [
        (
            "return ? 1 : 2;",
            "this engine needs an operand here, and found a `?` instead",
            "return 1 ? 2 : 3;",
            2.0,
        ),
        (
            "return true ? throw 1 : 2;",
            "this engine needs an operand here, and found the `throw` keyword instead",
            "try { throw 4; } catch (e) { return e; }",
            4.0,
        ),
        (
            "try { throw 1; } catch (e) { } catch (f) { }",
            "this engine needs an operand here, and found the `catch` keyword instead",
            "try { throw 5; } catch (e) { return e; }",
            5.0,
        ),
        (
            "try { throw 1; } catch (e) { } finally { } finally { }",
            "this engine needs an operand here, and found the `finally` keyword instead",
            "let n = 0; try { } finally { n = 6; } return n;",
            6.0,
        ),
        (
            "try { } catch (e) return 1;",
            "this engine needs a `{` to open the `catch` block, and found the `return` keyword instead",
            "return 7;",
            7.0,
        ),
        (
            "else { }",
            "this engine needs an operand here, and found the `else` keyword instead",
            "if (0) { return 1; } else { return 8; }",
            8.0,
        ),
    ];
    for (refused, sentence, proof, answer) in rows {
        let message = refuse(refused);
        speaks_for_the_engine(&message);
        assert_eq!(message, sentence, "{refused:?}");
        assert!(
            !message.contains("does not support"),
            "{refused:?} still disclaims something"
        );
        assert_eq!(
            num(proof),
            answer,
            "{proof:?} is what would have made the old sentence a false claim"
        );
    }
}

/// The narrower debt that is still open: a phrase that is **true** but names a
/// capability that would not have helped, which sends the reader after a
/// feature that is not the problem.
///
/// These are the tokens `parse::unlowered_by_m1` still trusts, standing where
/// their phrase is true of the engine and false of the position. `catch (e, f)`
/// is a SyntaxError in JavaScript too -- a CatchParameter is one binding, and
/// the comma operator has no place in a parameter list. Both refusals are
/// right; both reasons are not.
///
/// One row left this list when the rule narrowed: `catch ({ a })` claimed
/// "block statements", and blocks are something the engine has, so that row
/// was the *other* defect and is now an ordinary structural refusal.
#[test]
fn a_true_capability_phrase_can_still_be_the_wrong_answer() {
    for (source, phrase, why) in [
        (
            "try { throw 1; } catch (e, f) { }",
            "does not support the comma operator yet",
            "a CatchParameter is one binding; the comma operator would not make this legal",
        ),
        (
            "return true ? : 2;",
            "does not support labelled statements yet",
            "the `:` here is the conditional's own, and a missing middle operand is the fault",
        ),
    ] {
        let message = refuse(source);
        speaks_for_the_engine(&message);
        assert!(message.contains(phrase), "DEFECT row moved: {message:?}");
        assert!(!why.is_empty());
    }
    // The row that left: a destructuring catch parameter is refused for what
    // the position wanted, and block statements compile.
    let message = refuse("try { throw 1; } catch ({ a }) { }");
    speaks_for_the_engine(&message);
    assert_eq!(
        message,
        "this engine needs a name for the `catch` parameter, and found a `{` instead"
    );
    assert_eq!(num("{ let q = 1; } return 2;"), 2.0);
}

/// The phrases that are simply true, so that the two tests above are about
/// the *position* and not about capability phrases as such. Each of these
/// really is ahead of the engine.
#[test]
fn a_capability_phrase_is_right_where_the_capability_really_is_missing() {
    for (source, phrase) in [
        (
            "switch (1) { }",
            "does not support the `switch` keyword yet",
        ),
        ("return \"a\" ?? \"b\";", "nullish coalescing"),
        ("return 1 ** 2;", "exponentiation"),
        ("return 1 ** 2 ** 3;", "exponentiation"),
        ("lbl: { }", "labelled statements"),
        ("return (1, 2);", "comma operator"),
    ] {
        let message = refuse(source);
        speaks_for_the_engine(&message);
        assert!(message.contains(phrase), "{source:?} answered {message:?}");
    }
}
