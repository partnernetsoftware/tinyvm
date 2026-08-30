//! Two ECMA-262 constructs and the machinery under them: the conditional
//! expression (13.14) and `try`/`catch`/`finally`/`throw` (14.15, 14.14).
//!
//! Written from the specification before it was run, in the shape
//! `objects_conformance.rs` established: every expectation executes -- compile
//! -> tinyvm's load gate -> instantiate -> `invoke_by_name("main")` -- and a
//! place where this engine and the standard genuinely part company is asserted
//! as it behaves and marked `DIVERGENCE:`.
//!
//! # Why the two are one file
//!
//! They arrived together and they share one mechanism question: both are
//! control flow whose value is a JavaScript value, and `repr`'s `BlockType`
//! has only `Empty`, so neither can be a block that yields. Both therefore go
//! through a scratch local, and the conditional is the simple half of the same
//! shape the `finally` join point needs.
//!
//! # The unwinding contract this file pins
//!
//! tinyvm's core does not implement the wasm exception-handling proposal --
//! `crates/tinyvm/src/wasm.rs`'s opcode decoder has no arm for `try` (0x06),
//! `catch` (0x07), `throw` (0x08) or `try_table` (0x1F) and ends at
//! `_other => return Err(WasmError::Decode("unsupported opcode 0x"))`
//! (line 2931), and its section table refuses the tag section id 13 at
//! `_ => return Err(WasmError::Decode("unsupported section id"))` (line
//! 4852). So there is no instruction to lower a handler onto and the compiler
//! encodes unwinding itself: a module-level "a throw is in flight" flag, the
//! thrown value in two more globals beside it, and a check after every call
//! that could throw. The two facts a consumer may rely on are asserted here:
//! **a program with no `throw` in it emits exactly the bytes it did before the
//! feature existed**, and a program that has one pays a fixed, small amount
//! per call site.

use tinyvm::{Limits, Val, WasmInstance, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

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
    /// An Object came back. The tag is all a host can read, which is the
    /// point of `repr::host_decode`'s named refusal.
    Object,
}

/// `repr.rs` numbers the Object tag 5. Restated rather than imported, as
/// `objects_conformance.rs` restates it: a contract checked only from inside
/// is not checked.
const TAG_OBJECT: i32 = 5;

/// `runtime.rs`'s `FAULT_WORD`: the first word of linear memory, where the
/// guest writes down why it is about to trap.
const FAULT_WORD: usize = 0;
/// `FAULT_NONE`.
const FAULT_NONE: i32 = 0;
/// The code an uncaught `throw` reaching the entry point writes. Restated
/// from outside for the same reason as the tag above; `emit.rs` defines it
/// as `FAULT_UNCAUGHT_THROW` and says there that it belongs beside
/// `FAULT_HEAP_EXHAUSTED` in `runtime.rs`.
const FAULT_UNCAUGHT_THROW: i32 = 2;

fn compile(source: &str) -> Vec<u8> {
    compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"))
}

fn instantiate(source: &str) -> WasmInstance {
    let wasm = compile(source);
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
    decode(&instance, &vals, source)
}

fn decode(instance: &WasmInstance, vals: &[Val], source: &str) -> Out {
    if let [Val::I32(TAG_OBJECT), _] = vals {
        return Out::Object;
    }
    let value = Value::returned(vals)
        .unwrap_or_else(|e| panic!("{source:?}: cannot read the result back: {e}"));
    match value {
        Value::Undefined => Out::Undefined,
        Value::Null => Out::Null,
        Value::Number(x) => Out::Number(x),
        Value::Bool(b) => Out::Bool(b),
        Value::String(ptr) => {
            let view = instance.memory().expect("guest memory");
            Out::Str(read_string_at(&view, ptr).expect("a string record"))
        }
    }
}

fn read_string_at(bytes: &[u8], ptr: i32) -> Result<String, String> {
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

/// Run a source that is expected to trap, and read the guest's own account of
/// why out of its linear memory.
#[track_caller]
fn trap_fault(source: &str) -> i32 {
    let mut instance = instantiate(source);
    let error = instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect_err(&format!("{source:?} was expected to trap"));
    assert!(
        error.message().contains("unreachable"),
        "{source:?} trapped with {:?}, which is not the guest's own `unreachable`",
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

/// The whole diagnostic sentence for a source this engine refuses.
#[track_caller]
fn refuse(source: &str) -> String {
    match compile_qjs_m1(source) {
        Ok(bytes) => panic!(
            "{source:?} compiled to {} bytes; expected a refusal",
            bytes.len()
        ),
        Err(e) => e.message,
    }
}

// =========================================================================
// 13.14 -- the conditional expression
// =========================================================================

/// The whole of it: a test, and exactly one of two operands.
#[test]
fn a_conditional_yields_the_branch_its_test_chose() {
    assert_eq!(run("return true ? 1 : 2;"), Out::Number(1.0));
    assert_eq!(run("return false ? 1 : 2;"), Out::Number(2.0));
    // Its value is an operand and not a Boolean, so any type comes out.
    assert_eq!(run("return true ? \"a\" : 2;"), Out::Str("a".into()));
    assert_eq!(run("return false ? 1 : null;"), Out::Null);
    assert_eq!(run("return false ? 1 : undefined;"), Out::Undefined);
    assert_eq!(run("return true ? {} : 1;"), Out::Object);
}

/// 13.14.1 step 2 is `ToBoolean(exprValue)` -- the same algorithm `if` runs,
/// and this engine has exactly one of it (`runtime.rs`'s `__truthy`).
#[test]
fn the_test_is_toboolean_and_not_a_boolean_check() {
    for (source, want) in [
        ("return 0 ? 1 : 2;", 2.0),
        ("return 1 ? 1 : 2;", 1.0),
        ("return \"\" ? 1 : 2;", 2.0),
        ("return \"x\" ? 1 : 2;", 1.0),
        ("return null ? 1 : 2;", 2.0),
        ("return undefined ? 1 : 2;", 2.0),
        // Every Object is truthy, and so is every function.
        ("const o = {}; return o ? 1 : 2;", 1.0),
        ("function f() {} const g = f; return g ? 1 : 2;", 1.0),
        // 7.1.2: NaN is falsy, and so is -0.
        ("return (0 / 0) ? 1 : 2;", 2.0),
        ("return (-0) ? 1 : 2;", 2.0),
    ] {
        assert_eq!(run(source), Out::Number(want), "{source:?}");
    }
}

/// 13.14.1: only the branch the test chose is *evaluated*. Proved by an
/// observable side effect, not by reading the emitted code -- a lowering that
/// evaluated both and then selected would pass every value assertion above.
#[test]
fn only_the_taken_branch_evaluates() {
    const COUNTER: &str = "let n = 0; function bump() { n = n + 1; return 0; } ";
    assert_eq!(
        run(&format!("{COUNTER} const v = true ? 0 : bump(); return n;")),
        Out::Number(0.0),
        "the else operand must not run when the test is truthy"
    );
    assert_eq!(
        run(&format!(
            "{COUNTER} const v = false ? bump() : 0; return n;"
        )),
        Out::Number(0.0),
        "the then operand must not run when the test is falsy"
    );
    // And the taken one does run, exactly once.
    assert_eq!(
        run(&format!("{COUNTER} const v = true ? bump() : 0; return n;")),
        Out::Number(1.0)
    );
    assert_eq!(
        run(&format!(
            "{COUNTER} const v = false ? 0 : bump(); return n;"
        )),
        Out::Number(1.0)
    );
}

/// `a ? b : c ? d : e` is `a ? b : (c ? d : e)`.
///
/// The case that tells the two groupings apart is the one where the *then*
/// branch is taken: read left-associatively, `true ? 1 : true ? 2 : 3` would
/// be `(true ? 1 : true) ? 2 : 3`, which is 2.
#[test]
fn the_conditional_is_right_associative() {
    assert_eq!(run("return true ? 1 : true ? 2 : 3;"), Out::Number(1.0));
    assert_eq!(run("return false ? 1 : true ? 2 : 3;"), Out::Number(2.0));
    assert_eq!(run("return false ? 1 : false ? 2 : 3;"), Out::Number(3.0));
    // Three deep, for the same reason.
    assert_eq!(
        run("return false ? 1 : false ? 2 : false ? 3 : 4;"),
        Out::Number(4.0)
    );
}

/// It is an *expression*, so it nests wherever one goes.
#[test]
fn a_conditional_goes_wherever_an_expression_goes() {
    assert_eq!(
        run("function f(x) { return x + 1; } return f(true ? 1 : 2);"),
        Out::Number(2.0),
        "as an argument"
    );
    assert_eq!(
        run("const o = { k: false ? 1 : 2 }; return o.k;"),
        Out::Number(2.0),
        "as a property value"
    );
    assert_eq!(
        run("return (true ? 1 : 2) + (false ? 10 : 20);"),
        Out::Number(21.0),
        "as an operand"
    );
    assert_eq!(
        run("let x = 0; x = true ? 5 : 6; return x;"),
        Out::Number(5.0),
        "as the right side of an assignment"
    );
    assert_eq!(
        run("if (true ? false : true) { return 1; } return 2;"),
        Out::Number(2.0),
        "as an `if` condition"
    );
    assert_eq!(
        run("return true ? (false ? 1 : 2) : 3;"),
        Out::Number(2.0),
        "nested inside its own then branch"
    );
    assert_eq!(
        run("const o = { m: function (c) { return c ? \"y\" : \"n\"; } }; return o.m(1);"),
        Out::Str("y".into()),
        "inside a function value"
    );
}

/// ECMA-262 puts the conditional between assignment and the short-circuit
/// operators: `ConditionalExpression : ShortCircuitExpression ? Assignment
/// Expression : AssignmentExpression`.
#[test]
fn the_conditional_binds_looser_than_or_and_tighter_than_assignment() {
    // `true || false ? 1 : 2` is `(true || false) ? 1 : 2`, a Number. Read the
    // other way it would be `true || (false ? 1 : 2)`, which is `true`.
    assert_eq!(
        run("return typeof (true || false ? 1 : 2);"),
        Out::Str("number".into())
    );
    assert_eq!(run("return true || false ? 1 : 2;"), Out::Number(1.0));
    assert_eq!(run("return false && true ? 1 : 2;"), Out::Number(2.0));
    // The middle operand is an AssignmentExpression, so an assignment fits
    // there without parentheses.
    assert_eq!(
        run("let x = 0; const v = true ? x = 5 : 0; return x;"),
        Out::Number(5.0)
    );
    // And so does the last one.
    assert_eq!(
        run("let x = 0; const v = false ? 0 : x = 7; return x;"),
        Out::Number(7.0)
    );
    // A whole conditional is an AssignmentExpression, so it is what an
    // argument and a declarator take.
    assert_eq!(run("const v = true ? 1 : 2; return v;"), Out::Number(1.0));
}

/// The `:` is not optional and the engine says what it was looking for.
#[test]
fn a_conditional_missing_its_colon_says_what_it_wanted() {
    let message = refuse("return true ? 1;");
    assert!(
        message.contains("`:`"),
        "{message:?} does not name the `:` it wanted"
    );
    assert!(
        message.starts_with("this engine"),
        "{message:?} does not speak for the engine"
    );
}

// =========================================================================
// 14.14 and 14.15 -- throw, try, catch, finally
// =========================================================================

#[test]
fn a_throw_is_caught_by_the_try_around_it() {
    assert_eq!(
        run("try { throw 1; } catch (e) { return e; } return 2;"),
        Out::Number(1.0)
    );
    assert_eq!(
        run("let out = 0; try { out = 1; } catch (e) { out = 2; } return out;"),
        Out::Number(1.0),
        "the catch clause runs only on a throw"
    );
}

/// 14.14.1: the thrown value is the value of the Expression, whatever type it
/// has. Nothing about it is a String and nothing about it is an Error object.
#[test]
fn a_thrown_value_is_any_javascript_value() {
    assert_eq!(
        run("try { throw \"boom\"; } catch (e) { return e; }"),
        Out::Str("boom".into())
    );
    assert_eq!(
        run("try { throw true; } catch (e) { return e; }"),
        Out::Bool(true)
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
        run("try { throw { a: 1 }; } catch (e) { return e.a; }"),
        Out::Number(1.0),
        "an Object survives the unwind with its identity"
    );
    assert_eq!(
        run("const o = {}; try { throw o; } catch (e) { return e === o ? 1 : 2; }"),
        Out::Number(1.0),
        "and it is the same object, not a copy"
    );
    assert_eq!(
        run("function f() {} const g = f; try { throw g; } catch (e) { return typeof e; }"),
        Out::Str("function".into())
    );
}

/// The value is an arbitrary expression, evaluated where the `throw` is.
#[test]
fn the_thrown_expression_is_evaluated_at_the_throw() {
    assert_eq!(
        run(
            "let n = 0; function bump() { n = n + 1; return n; } try { throw bump(); } catch (e) { return e; }"
        ),
        Out::Number(1.0)
    );
}

/// A `throw` that crosses a call boundary. Nothing in wasm carries it, so the
/// callee returns normally with the flag set and the caller's check after the
/// `call` is what turns that into a branch.
#[test]
fn a_throw_crosses_a_function_boundary() {
    assert_eq!(
        run("function bad() { throw 7; } try { bad(); } catch (e) { return e; }"),
        Out::Number(7.0)
    );
    assert_eq!(
        run(
            "function bad() { throw 7; } function mid() { bad(); return 1; } try { mid(); } catch (e) { return e; }"
        ),
        Out::Number(7.0),
        "and it keeps crossing"
    );
    assert_eq!(
        run(
            "function bad() { throw 7; } function mid() { bad(); return 1; } let after = 0; try { mid(); after = 1; } catch (e) { return after; }"
        ),
        Out::Number(0.0),
        "the statements after the throwing call do not run"
    );
    // Through a function *value*, which is `call_indirect` and a second site.
    assert_eq!(
        run("function bad() { throw 7; } const f = bad; try { f(); } catch (e) { return e; }"),
        Out::Number(7.0)
    );
    assert_eq!(
        run("const o = { m: function () { throw 9; } }; try { o.m(); } catch (e) { return e; }"),
        Out::Number(9.0)
    );
}

/// A callee may catch its own throw, and then nothing reaches the caller.
#[test]
fn a_function_catches_its_own_throw() {
    assert_eq!(
        run("function f() { try { throw 1; } catch (e) { return e + 10; } } return f();"),
        Out::Number(11.0)
    );
    assert_eq!(
        run(
            "function f() { try { throw 1; } catch (e) { return 5; } } let out = 0; try { out = f(); } catch (e) { out = 99; } return out;"
        ),
        Out::Number(5.0),
        "the outer try sees no throw at all"
    );
}

/// 14.15.3: the catch block is *not* inside its own try. A throw there is the
/// try statement's completion, and only an enclosing handler may take it.
#[test]
fn a_throw_inside_catch_is_not_caught_by_its_own_try() {
    assert_eq!(
        run("try { try { throw 1; } catch (e) { throw e + 1; } } catch (e) { return e; }"),
        Out::Number(2.0)
    );
    // And with no enclosing handler it leaves the script -- see the fault
    // test below for the shape of that.
    assert_eq!(
        trap_fault("try { throw 1; } catch (e) { throw 2; }"),
        FAULT_UNCAUGHT_THROW
    );
}

#[test]
fn catch_may_omit_its_binding() {
    // ECMA-262 14.15's optional CatchParameter.
    assert_eq!(
        run("let out = 0; try { throw 1; } catch { out = 5; } return out;"),
        Out::Number(5.0)
    );
}

/// The catch parameter is a binding of the catch block and nothing outside it.
#[test]
fn the_catch_parameter_is_its_own_binding() {
    assert_eq!(
        run("let e = 1; try { throw 2; } catch (e) { return e; }"),
        Out::Number(2.0),
        "it shadows an outer binding of the same name"
    );
    assert_eq!(
        run("let e = 1; try { throw 2; } catch (x) { } return e;"),
        Out::Number(1.0),
        "and the outer one is untouched"
    );
    assert_eq!(
        run("try { throw 1; } catch (e) { e = 5; return e; }"),
        Out::Number(5.0),
        "and it is writable, which ECMA-262 makes a `let`-like binding"
    );
}

// -- finally ----------------------------------------------------------------

#[test]
fn finally_runs_on_the_normal_path() {
    assert_eq!(
        run("let out = 0; try { out = 1; } finally { out = out + 10; } return out;"),
        Out::Number(11.0)
    );
    assert_eq!(
        run(
            "let out = 0; try { out = 1; } catch (e) { out = 2; } finally { out = out + 10; } return out;"
        ),
        Out::Number(11.0)
    );
}

#[test]
fn finally_runs_on_the_caught_path() {
    assert_eq!(
        run(
            "let out = 0; try { throw 1; } catch (e) { out = e; } finally { out = out + 10; } return out;"
        ),
        Out::Number(11.0)
    );
}

/// A `try`/`finally` with no `catch`: the finalizer runs and the throw keeps
/// going.
#[test]
fn finally_runs_on_the_throwing_path_and_the_throw_continues() {
    assert_eq!(
        run(
            "let out = 0; try { try { throw 1; } finally { out = 10; } } catch (e) { return out + e; }"
        ),
        Out::Number(11.0)
    );
    assert_eq!(
        run("let out = 0; try { try { throw 1; } finally { out = 10; } } catch (e) { return e; }"),
        Out::Number(1.0),
        "and the value that arrives is the one that was thrown"
    );
    // Even when a `catch` is there and rethrows.
    assert_eq!(
        run(
            "let out = 0; try { try { throw 1; } catch (e) { throw e + 1; } finally { out = 10; } } catch (e) { return out + e; }"
        ),
        Out::Number(12.0)
    );
}

/// 14.15.3 step 5: the finalizer runs even when the try block returns, and the
/// return still happens with the value it had.
#[test]
fn finally_runs_when_the_try_block_returns() {
    assert_eq!(
        run(
            "let out = 0; function f() { try { return 1; } finally { out = 10; } } const v = f(); return out + v;"
        ),
        Out::Number(11.0)
    );
    assert_eq!(
        run(
            "let out = 0; function f() { try { throw 1; } catch (e) { return 2; } finally { out = 10; } } const v = f(); return out + v;"
        ),
        Out::Number(12.0),
        "and when the catch block returns"
    );
    // The script's own `return` counts too.
    assert_eq!(
        run("let out = 0; try { return out + 1; } finally { out = 10; }"),
        Out::Number(1.0)
    );
}

/// 14.15.3: an abrupt completion of the finalizer *replaces* the pending one.
#[test]
fn an_abrupt_finally_replaces_what_was_pending() {
    assert_eq!(
        run("function f() { try { return 1; } finally { return 2; } } return f();"),
        Out::Number(2.0),
        "a return in finally overrides the try's return"
    );
    assert_eq!(
        run(
            "function f() { try { throw 1; } finally { return 2; } } let out = 0; try { out = f(); } catch (e) { out = 99; } return out;"
        ),
        Out::Number(2.0),
        "a return in finally swallows the pending throw"
    );
    assert_eq!(
        run("try { try { throw 1; } finally { throw 2; } } catch (e) { return e; }"),
        Out::Number(2.0),
        "a throw in finally replaces the pending throw"
    );
}

/// A pending throw survives a finalizer that throws and catches *inside* a
/// call.
///
/// The case the design has to defend against, and the reason the pending
/// value is parked in a local rather than left where the throw put it: the
/// inner `catch` clears the in-flight flag and the inner `throw` overwrites
/// the thrown value, and neither is the throw this finalizer is standing on
/// top of. Verified against JavaScriptCore.
#[test]
fn a_pending_throw_survives_a_finalizer_that_throws_and_catches() {
    assert_eq!(
        run(
            "let n = 0; function t() { throw 3; } function safe() { try { throw 9; } catch (q) { n = 1; } return 0; } try { try { t(); } finally { safe(); } } catch (e) { return e + n; }"
        ),
        Out::Number(4.0),
        "the value that arrives must be 3, not the 9 the finalizer's callee threw"
    );
}

/// Finalizers nested inside finalizers, each one overriding the last.
#[test]
fn a_finalizer_inside_a_finalizer_overrides_again() {
    assert_eq!(
        run(
            "function f() { try { return 1; } finally { try { return 2; } finally { return 3; } } } return f();"
        ),
        Out::Number(3.0)
    );
}

/// A throw reaches its handler from every expression position, because the
/// check sits at the call and not at the statement.
#[test]
fn a_throw_leaves_from_anywhere_an_expression_goes() {
    const T: &str = "function t() { throw \"T\"; } ";
    for tail in [
        "const v = !t();",
        "const v = +t();",
        "const v = typeof t();",
        "const v = t() + 1;",
        "const v = 1 + t();",
        "const v = t() || 1;",
        "const v = 1 && t();",
        "const v = { k: t() };",
        "const v = t() ? 1 : 2;",
        "const v = true ? t() : 2;",
        "const v = false ? 1 : t();",
        "if (t()) { }",
        "while (t()) { }",
        "let o = { n: 1 }; o.n += t();",
        "let o = { n: 1 }; o[t()] = 2;",
        "const v = (function () { return t(); })();",
    ] {
        let source = format!("{T} try {{ {tail} }} catch (e) {{ return e; }} return 0;");
        assert_eq!(run(&source), Out::Str("T".into()), "{tail}");
    }
}

/// Nested finalizers run innermost first, and a `return` walks all of them.
#[test]
fn nested_finalizers_all_run_in_order() {
    assert_eq!(
        run(
            "let log = \"\"; function f() { try { try { return 1; } finally { log = log + \"i\"; } } finally { log = log + \"o\"; } } const v = f(); return log;"
        ),
        Out::Str("io".into())
    );
    assert_eq!(
        run(
            "let log = \"\"; function f() { try { try { return 1; } finally { log = log + \"i\"; } } finally { log = log + \"o\"; } } return f();"
        ),
        Out::Number(1.0),
        "and the return value survives both"
    );
}

/// A `try` in a loop is entered afresh each pass.
#[test]
fn a_try_inside_a_loop_runs_every_pass() {
    assert_eq!(
        run(
            "let n = 0; let i = 0; while (i < 3) { try { throw 1; } catch (e) { n = n + e; } finally { n = n + 10; } i = i + 1; } return n;"
        ),
        Out::Number(33.0)
    );
}

// -- the uncaught throw ------------------------------------------------------

/// An uncaught `throw` reaching the entry point is a fault the host can tell
/// apart from a broken script, which is what `runtime.rs`'s `FAULT_WORD`
/// exists for. Without a code of its own it would be the same bare
/// `unreachable` a missing conversion executes.
#[test]
fn an_uncaught_throw_is_a_fault_the_host_can_name() {
    assert_eq!(trap_fault("throw 1;"), FAULT_UNCAUGHT_THROW);
    assert_eq!(
        trap_fault("function bad() { throw 1; } bad();"),
        FAULT_UNCAUGHT_THROW,
        "from inside a function too"
    );
    assert_eq!(
        trap_fault("function bad() { throw 1; } const f = bad; f();"),
        FAULT_UNCAUGHT_THROW,
        "and through a function value"
    );
    assert_eq!(
        trap_fault("let out = 0; try { throw 1; } finally { out = 1; }"),
        FAULT_UNCAUGHT_THROW,
        "a finalizer runs and then the throw still leaves the script"
    );
}

/// The word describes *this* call. The entry point clears it on the way in,
/// so a script that throws and catches leaves nothing behind.
#[test]
fn a_caught_throw_writes_no_fault() {
    let mut instance = instantiate("try { throw 1; } catch (e) { } return 0;");
    instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect("no trap");
    let view = instance.memory().expect("guest memory");
    let word = i32::from_le_bytes([
        view[FAULT_WORD],
        view[FAULT_WORD + 1],
        view[FAULT_WORD + 2],
        view[FAULT_WORD + 3],
    ]);
    assert_eq!(word, FAULT_NONE);
}

// -- what is refused, and how ------------------------------------------------

/// 14.15: a `try` has to have one of the two clauses.
#[test]
fn a_try_with_neither_clause_is_refused() {
    let message = refuse("try { }");
    assert!(
        message.contains("catch") && message.contains("finally"),
        "{message:?} does not say which clause it wanted"
    );
    assert!(message.starts_with("this engine"), "{message:?}");
}

/// ECMA-262 12.10: `throw` is a restricted production -- `throw [no
/// LineTerminator here] Expression` -- so a line break after it is not a
/// `throw undefined`, it is an error. The lexer's rule-3 pass inserts the
/// semicolon and the parser is what has to notice.
#[test]
fn a_line_break_after_throw_is_refused() {
    let message = refuse("throw\n1;");
    assert!(
        message.contains("line") || message.contains("12.10"),
        "{message:?} does not explain the line break"
    );
    // The same source on one line is fine.
    assert_eq!(trap_fault("throw 1;"), FAULT_UNCAUGHT_THROW);
}

/// The four keywords are IdentifierNames, so they name properties (13.2.5.1).
/// They stopped being reserved words the lexer refuses outright, so this is
/// the check that they did not become reserved words the *parser* refuses.
#[test]
fn the_new_keywords_are_still_property_names() {
    assert_eq!(
        run(
            "const o = { try: 1, catch: 2, finally: 3, throw: 4 }; return o.try + o.catch + o.finally + o.throw;"
        ),
        Out::Number(10.0)
    );
}

// =========================================================================
// What the unwinding costs
// =========================================================================

/// A program with no `throw` anywhere declares no unwinding global.
///
/// That is the whole design decision, checked where it is a fact about the
/// module rather than a byte count that another lane's work can move. Nothing
/// in such a program can set the in-flight flag -- a `throw` is the only
/// producer, and a trap is not a throw -- so the three globals are not
/// declared, no check is emitted, and the module is what it always was.
#[test]
fn a_program_that_cannot_throw_declares_no_unwinding_global() {
    // One global: the bump pointer. No bindings, no unwind channel.
    assert_eq!(globals_of("return 1;"), 1);
    // `function f(){}` binds a name in the script, and a script binding is
    // two globals whether or not it needs storage.
    assert_eq!(globals_of("function f() { return 1; } return f();"), 3);
    // Two script bindings are four more, and still no unwind channel.
    assert_eq!(globals_of("let a = 1; let b = 2; return a + b;"), 5);
    // A `try` with nothing that can throw inside it is the same: the clauses
    // still run, and there is still nothing to unwind.
    // Since 2026-08-29 a `try` declares the channel: it is where a TypeError
    // (a property read off undefined) can be caught, so three more globals
    // sit after `out` and the catch parameter.
    assert_eq!(
        globals_of(
            "let out = 0; try { out = 1; } catch (e) { out = 2; } finally { out = out + 10; } return out;"
        ),
        8,
        "`out` and the catch parameter are two script bindings, plus the unwind channel a `try` opens"
    );
    assert_eq!(
        run(
            "let out = 0; try { out = 1; } catch (e) { out = 2; } finally { out = out + 10; } return out;"
        ),
        Out::Number(11.0)
    );
    // One `throw`, anywhere, and the three appear -- once, however many
    // `throw`s there are.
    assert_eq!(globals_of("throw 1;"), 4);
    assert_eq!(
        globals_of("function t() { throw 1; } function u() { throw 2; } return 0;"),
        8,
        "two script bindings, and one unwind channel however many `throw`s there are"
    );
}

/// A property access on `undefined` is the TypeError ECMA-262 says it is,
/// and `catch` takes it -- as a String, since this engine has no `Error`
/// objects (2026-08-29; until then it was an `unreachable` the clause never
/// saw). A *real* trap -- calling a value that is not a function -- is still
/// not a throw, and `catch` still cannot see one; that is the honest shape
/// of what remains.
#[test]
fn a_property_read_off_undefined_is_a_throw_and_a_real_trap_still_is_not() {
    let mut caught = instantiate(
        "let out = 0; try { const u = undefined; out = u.a; } catch (e) { out = 5; } return out;",
    );
    let vals = caught
        .invoke_by_name("main", &Value::args(&[]))
        .expect("the TypeError is caught and the script completes");
    assert_eq!(Value::returned(&vals), Ok(Value::Number(5.0)));

    // Calling a non-function is ECMA-262's other TypeError, and since
    // 2026-08-30 it is caught too.
    let mut called =
        instantiate("let out = 0; try { const f = 1; f(); } catch (e) { out = 5; } return out;");
    let vals = called
        .invoke_by_name("main", &Value::args(&[]))
        .expect("the TypeError is caught and the script completes");
    assert_eq!(Value::returned(&vals), Ok(Value::Number(5.0)));
    // A real trap still is not a throw: a String method this engine does
    // not have stops with its own fault word, channel or no channel.
    // (`substring` was the example until it landed on 2026-08-31.)
    let mut trapped = instantiate(
        "let out = 0; try { const s = \"ab\"; out = s.padStart(1); } catch (e) { out = 5; } return out;",
    );
    trapped
        .invoke_by_name("main", &Value::args(&[]))
        .expect_err("a missing String method traps rather than throwing");
}

/// What a program that *can* throw pays on the path where it does not.
///
/// Two instructions -- `global.get` of the in-flight flag, and a `br_if` --
/// which is four bytes, at each call to a user function and each
/// `call_indirect`. Measured as a second difference, so nothing else in the
/// module has to hold still: the same program is compiled with one call site,
/// two, three and four, once without a `throw` in it and once with, and the
/// gap between the two grows by exactly the check.
#[test]
fn a_throwing_program_pays_four_bytes_per_call_site() {
    // The two variants declare the same functions and the same bindings, and
    // differ only in whether one of those functions throws -- so nothing but
    // the unwinding machinery moves an index or a section length between
    // them.
    for (plain, throwing) in [
        (
            "function f() { return 1; } function t() { return 1; } return ",
            "function f() { return 1; } function t() { throw 1; } return ",
        ),
        // The other site: a call through a value, which is `call_indirect`.
        (
            "function f() { return 1; } function t() { return 1; } const g = f; return ",
            "function f() { return 1; } function t() { throw 1; } const g = f; return ",
        ),
    ] {
        let call = if plain.contains("const g") {
            "g()"
        } else {
            "f()"
        };
        let gaps: Vec<usize> = (1..=4)
            .map(|calls| {
                let body = vec![call; calls].join(" + ");
                code_bytes(&format!("{throwing}{body};")) - code_bytes(&format!("{plain}{body};"))
            })
            .collect();
        let steps: Vec<usize> = gaps.windows(2).map(|w| w[1] - w[0]).collect();
        assert_eq!(
            steps,
            vec![4, 4, 4],
            "one {call} call site costs four bytes; the gaps were {gaps:?}"
        );
    }
}

// =========================================================================
// Reading the module back
// =========================================================================

/// How many globals the compiled module declares.
///
/// A ten-line walk of the section list rather than an accessor, because the
/// claim above is about the *emitted module* and an accessor would be this
/// crate agreeing with itself.
fn globals_of(source: &str) -> u32 {
    let wasm = compile(source);
    let mut at = 8; // the magic and the version
    while at < wasm.len() {
        let id = wasm[at];
        let (size, next) = leb(&wasm, at + 1);
        if id == 6 {
            return leb(&wasm, next).0 as u32;
        }
        at = next + size;
    }
    0
}

/// How many bytes of *function body* the compiled module holds: the code
/// section's entries with their own length prefixes removed.
///
/// The module's total length is the wrong ruler for a four-byte claim,
/// because a section that crosses a 128-byte boundary grows its own length
/// prefix by a byte and the measurement jitters by one. The payloads do not.
fn code_bytes(source: &str) -> usize {
    let wasm = compile(source);
    let mut at = 8;
    while at < wasm.len() {
        let id = wasm[at];
        let (size, next) = leb(&wasm, at + 1);
        if id == 10 {
            let (count, mut cursor) = leb(&wasm, next);
            let mut total = 0;
            for _ in 0..count {
                let (body, after) = leb(&wasm, cursor);
                total += body;
                cursor = after + body;
            }
            return total;
        }
        at = next + size;
    }
    panic!("{source:?} compiled to a module with no code section");
}

/// One unsigned LEB128, and where it ended.
fn leb(bytes: &[u8], mut at: usize) -> (usize, usize) {
    let (mut value, mut shift) = (0usize, 0u32);
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
