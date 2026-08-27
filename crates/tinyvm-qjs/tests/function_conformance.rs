//! An ECMA-262 conformance corpus for **function values**: what it means for a
//! function to be a thing a binding can hold, a property can store, a call can
//! find, and an operator can be handed.
//!
//! Every expectation here was written from the specification *before* it was
//! run -- the clause is named at each one -- and every expectation that is
//! about behaviour **executes**: compile -> tinyvm's load gate -> instantiate
//! -> `invoke_by_name("main")`. "It compiled" is evidence of nothing and
//! appears alone only in the refusal corpus, where not compiling *is* the
//! claim.
//!
//! # How this differs from `function_values.rs`
//!
//! `function_values.rs` is the implementation's own suite: written next to the
//! code, it knows there is an adapter table and it tests the seams that table
//! has. This file was written from the other side. It does not care that a
//! function value is a table index; it cares that ECMA-262 says a function is
//! an ordinary object with [[Call]], and it asks the questions that follow from
//! that -- when two evaluations of one FunctionExpression are two objects, in
//! what order a call evaluates its callee and its arguments, whether a
//! namespace table's two methods can see each other, whether a declaration
//! hoists. The overlap between the two files is deliberate: an expectation only
//! one of them holds is an expectation with one witness.
//!
//! # The three kinds of row below
//!
//! * A plain assertion: ECMA-262 says X and this engine does X.
//! * `DIVERGENCE:` -- ECMA-262 says X, this engine does Y, and Y is what is
//!   asserted. The marker carries the answer real JavaScript gives, so the gap
//!   is a line somebody has to delete rather than a fact nobody wrote down.
//! * A refusal: the engine will not compile it, and the diagnostic names the
//!   *engine's* boundary. Those are the lock on the product's claims -- a
//!   capability that quietly starts working is a README that quietly goes
//!   stale, and a test asserting the refusal is what makes that a red build
//!   instead of a surprise.

use tinyvm::{Limits, Val, WasmInstance, WasmModule};
use tinyvm_qjs::{Boundary, CompileError, Value, compile_qjs_m1};

// =========================================================================
// Harness
// =========================================================================

/// What `main` returned, with a String's text already resolved -- a pointer
/// into a dropped instance's memory is unreadable.
#[derive(Debug, Clone, PartialEq)]
enum Out {
    Undefined,
    Null,
    Number(f64),
    Bool(bool),
    Str(String),
}

/// The function tag, as `repr.rs` numbers it. Restated from outside the crate
/// rather than imported, because `repr` is crate-private and a contract
/// checked only from the inside is not checked.
const TAG_FUNCTION: i32 = 6;

fn attempt(source: &str) -> Result<(WasmInstance, Vec<Val>), String> {
    let wasm = compile_qjs_m1(source).map_err(|e| format!("compiling {source:?}: {e}"))?;
    let module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .map_err(|e| format!("load gate rejected {source:?}: {}", e.message()))?;
    let mut instance = module
        .instantiate()
        .map_err(|e| format!("instantiating {source:?}: {}", e.message()))?;
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .map_err(|e| format!("trap in {source:?}: {}", e.message()))?;
    Ok((instance, vals))
}

#[track_caller]
fn run(source: &str) -> Out {
    let (instance, vals) = attempt(source).unwrap_or_else(|e| panic!("{e}"));
    let value = Value::returned(&vals)
        .unwrap_or_else(|e| panic!("{source:?}: cannot read the result back: {e}"));
    match value {
        Value::Undefined => Out::Undefined,
        Value::Null => Out::Null,
        Value::Number(x) => Out::Number(x),
        Value::Bool(b) => Out::Bool(b),
        Value::String(ptr) => Out::Str(read_string(&instance, ptr).expect("a string record")),
    }
}

/// A string record in guest memory: `[len: i32][utf8 bytes]`.
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

#[track_caller]
fn text(source: &str, want: &str) {
    assert_eq!(run(source), Out::Str(want.to_string()), "{source:?}");
}

#[track_caller]
fn undefined(source: &str) {
    assert_eq!(run(source), Out::Undefined, "{source:?}");
}

/// Compiles, clears the load gate, and then faults at run time. A thrown
/// TypeError has nowhere to go in this subset -- there is no `try` -- so a trap
/// is how one reaches the host.
#[track_caller]
fn traps(source: &str) {
    match attempt(source) {
        Err(message) => assert!(
            message.contains("trap in"),
            "{source:?} failed for the wrong reason: {message}"
        ),
        Ok((_, vals)) => panic!("{source:?} produced {vals:?} instead of trapping"),
    }
}

#[track_caller]
fn refuse(source: &str) -> CompileError {
    match compile_qjs_m1(source) {
        Ok(bytes) => panic!(
            "{source:?} compiled to {} bytes; expected a capability diagnostic",
            bytes.len()
        ),
        Err(e) => e,
    }
}

/// A refusal in the fixed wording `diag::unsupported` locks.
#[track_caller]
// Unused since arrows landed: they were this file's last capability refusal.
// Kept because the next construct this file has to refuse will want it, and
// four sibling test files define the same helper.
#[allow(dead_code, reason = "for the next construct this file has to refuse")]
fn refuses_capability(source: &str, construct: &str, boundary: Boundary) {
    let error = refuse(source);
    assert_eq!(
        error.message,
        format!("this engine does not support {construct} yet"),
        "{source:?}"
    );
    assert_eq!(error.boundary, boundary, "{source:?}");
}

/// A refusal whose exact sentence this corpus deliberately does not pin,
/// because the construct the engine names is not the construct in the source.
/// Asserts the part that is a product promise: the engine speaks about itself,
/// carries an offset, and never says "syntax error".
#[track_caller]
fn refuses_somehow(source: &str) -> CompileError {
    let error = refuse(source);
    assert!(
        error.message.starts_with("this engine"),
        "{source:?}: a diagnostic must speak for the engine, got {:?}",
        error.message
    );
    assert!(
        !error.message.to_lowercase().contains("syntax error"),
        "{source:?}: never a bare syntax error, got {:?}",
        error.message
    );
    error
}

// =========================================================================
// 15.2 Function Definitions -- a function is a value
// =========================================================================

/// 15.2.5 InstantiateOrdinaryFunctionExpression: evaluating a
/// FunctionExpression *produces a value*. 14.3.1 then lets an Initializer be
/// any AssignmentExpression, so a binding can hold one; 13.3.6.1 then calls it
/// through that binding.
#[test]
fn a_function_expression_initializes_a_binding_and_the_binding_is_callable() {
    number(
        "let f = function (a) { return a + 1; }; return f(41);",
        42.0,
    );
    number(
        "var f = function (a) { return a + 1; }; return f(41);",
        42.0,
    );
    number(
        "const f = function (a) { return a + 1; }; return f(41);",
        42.0,
    );
}

/// 13.15.2 AssignmentExpression with a MemberExpression target, then 13.3.2
/// property access to read it back. This is the shape the whole of
/// `fleet.js` is built from: a namespace table whose slots hold functions.
#[test]
fn a_function_expression_can_be_assigned_to_a_property_and_called_through_it() {
    number(
        "const o = {}; o.m = function (a) { return a * 3; }; return o.m(14);",
        42.0,
    );
    // 13.3.2.1: a computed key reaches the same slot, since 13.2.5.1 makes
    // every property key a String.
    number(
        "const o = {}; o[\"m\"] = function (a) { return a * 3; }; return o[\"m\"](14);",
        42.0,
    );
    number(
        "const o = {}; o.m = function (a) { return a * 3; }; return o[\"m\"](14);",
        42.0,
    );
}

/// 13.2.5 PropertyDefinitionEvaluation: `PropertyName : AssignmentExpression`
/// takes any AssignmentExpression, and a FunctionExpression is one. The
/// property does not have to be assigned after the fact.
#[test]
fn an_object_literal_can_carry_a_function_in_a_slot() {
    number(
        "const o = { m: function () { return 7; } }; return o.m();",
        7.0,
    );
    number(
        "const o = { a: function () { return 1; }, b: function () { return 2; } }; return o.a() * 10 + o.b();",
        12.0,
    );
    // 13.2.5.5 shorthand: `{ m }` is `{ m: m }`, so it carries the value the
    // binding holds -- which here is a function.
    number(
        "let m = function () { return 5; }; const o = { m }; return o.m();",
        5.0,
    );
}

/// 13.3.6.1 step 1-2: the callee is whatever expression stands before the
/// argument list, so a function value reached through several property reads
/// is as callable as one reached through a name. `fleet.ui.tab.select(...)`
/// is exactly this shape.
#[test]
fn a_call_reaches_a_function_through_a_chain_of_property_reads() {
    number(
        "const a = {}; a.b = {}; a.b.c = {}; a.b.c.d = function (n) { return n; }; return a.b.c.d(42);",
        42.0,
    );
    // And the object the chain ends in can be built by a call.
    number(
        "function mk() { const o = {}; o.m = function () { return 3; }; return o; } return mk().m();",
        3.0,
    );
}

/// 13.3.6.1: the callee expression may itself be a call, because what is
/// called is the *value* and not a name. `f()()` and `g(h)()`.
#[test]
fn the_result_of_a_call_is_itself_callable() {
    number(
        "function mk() { return function (n) { return n + 1; }; } return mk()(41);",
        42.0,
    );
    number(
        "function mk() { return function (n) { return n + 1; }; } let f = mk(); return f(41);",
        42.0,
    );
}

/// A function value is an ordinary argument (13.3.8.1 evaluates each
/// AssignmentExpression, whatever type it produces) and an ordinary return
/// value (14.10.1). Higher-order code is not a separate feature; it is what
/// falls out of a function being a value.
#[test]
fn a_function_value_can_be_passed_as_an_argument_and_returned() {
    number(
        "function apply1(g, x) { return g(x); } let dbl = function (n) { return n * 2; }; return apply1(dbl, 21);",
        42.0,
    );
    number(
        "function pick(g) { return g; } let one = function () { return 1; }; return pick(one)();",
        1.0,
    );
    // Through a whole round trip: binding -> argument -> return -> property ->
    // call.
    number(
        "function pick(g) { return g; } const o = {}; o.m = pick(function () { return 8; }); return o.m();",
        8.0,
    );
}

/// The `const`-bound function expression is worth its own row because the
/// compiler is allowed to recognise it as a known callee and keep the call
/// direct. That optimisation must not cost it its *value*-ness: 15.2.5 makes
/// no distinction, so the same binding must still be storable and callable
/// indirectly.
#[test]
fn a_binding_the_compiler_can_see_through_is_still_a_value() {
    number(
        "const f = function (n) { return n * 2; }; const o = {}; o.m = f; return f(3) + o.m(4);",
        14.0,
    );
    text("const f = function () {}; return typeof f;", "function");
    boolean(
        "const f = function () {}; const o = {}; o.m = f; return o.m === f;",
        true,
    );
}

// =========================================================================
// 10.2.11 / 13.3.8.1 -- arity is elastic, evaluation order is not
// =========================================================================

/// 10.2.11 FunctionDeclarationInstantiation: a parameter with no corresponding
/// argument is initialized to `undefined`. Not an error, not a missing
/// binding -- the name is there and it reads `undefined`.
#[test]
fn a_parameter_with_no_argument_is_undefined() {
    text(
        "let f = function (a, b) { return typeof b; }; return f(1);",
        "undefined",
    );
    undefined("let f = function (a, b) { return b; }; return f(1);");
    text(
        "let f = function (a) { return typeof a; }; return f();",
        "undefined",
    );
    // The `params === undefined` default-argument idiom `fleet.js` opens with,
    // spelled as this engine spells it.
    text(
        "let f = function (a) { if (a === undefined) { return \"{}\"; } return a; }; return f();",
        "{}",
    );
    text(
        "let f = function (a) { if (a === undefined) { return \"{}\"; } return a; }; return f(\"x\");",
        "x",
    );
}

/// 13.3.8.1 evaluates *every* AssignmentExpression in the list, and 10.2.11
/// binds only as many as the function declares. So a surplus argument is
/// evaluated -- its side effects happen -- and then has nowhere to go.
#[test]
fn a_surplus_argument_is_evaluated_and_then_dropped() {
    number(
        "let f = function (a) { return a; }; return f(1, 2, 3);",
        1.0,
    );
    text(
        "const t = {}; t.s = \"\"; function note(x) { t.s = t.s + x; return x; } \
         let f = function (a) { return a; }; let r = f(note(\"a\"), note(\"b\")); return t.s + r;",
        "aba",
    );
}

/// 13.3.8.1 ArgumentList: the list is evaluated **left to right**, and that is
/// observable through side effects even when every argument lands in a
/// parameter.
#[test]
fn arguments_are_evaluated_left_to_right() {
    text(
        "const t = {}; t.s = \"\"; function note(x) { t.s = t.s + x; return x; } \
         let f = function (a, b, c) { return t.s; }; return f(note(\"a\"), note(\"b\"), note(\"c\"));",
        "abc",
    );
    // Including the surplus ones, which are still part of the list.
    text(
        "const t = {}; t.s = \"\"; function note(x) { t.s = t.s + x; return x; } \
         let f = function (a) { return t.s; }; return f(note(\"a\"), note(\"b\"), note(\"c\"));",
        "abc",
    );
}

/// 13.3.6.1 steps 1-2 evaluate the MemberExpression and `GetValue` it *before*
/// EvaluateCall reaches ArgumentListEvaluation. So an argument that overwrites
/// the property the callee was read from does not change which function this
/// call reaches -- only the next call's.
#[test]
fn the_callee_is_read_before_the_arguments_are_evaluated() {
    number(
        "const o = {}; o.m = function () { return 1; }; \
         function swap() { o.m = function () { return 2; }; return 0; } \
         let r = o.m(swap()); return r * 10 + o.m();",
        12.0,
    );
}

/// 13.3.6.1 evaluates the MemberExpression once, so a receiver produced by a
/// call is computed once and not once per anything.
#[test]
fn a_receiver_expression_is_evaluated_exactly_once() {
    number(
        "const t = {}; t.n = 0; const o = {}; o.m = function (x) { return x; }; \
         function rec() { t.n = t.n + 1; return o; } return rec().m(5) + t.n;",
        6.0,
    );
}

/// A property read is a fresh read every time (13.3.2.1 GetValue), so the
/// function a call finds is the one in the slot *now*. That is what lets two
/// methods of one namespace table call each other regardless of the order the
/// slots were filled in.
#[test]
fn a_call_through_a_property_finds_whatever_the_slot_holds_now() {
    number(
        "const o = {}; o.m = function () { return 1; }; let a = o.m(); \
         o.m = function () { return 2; }; return a * 10 + o.m();",
        12.0,
    );
}

// =========================================================================
// 15.2.5 -- the name of a named function expression
// =========================================================================

/// 15.2.5 InstantiateOrdinaryFunctionExpression: for a *named* function
/// expression a new declarative Environment Record is created with the
/// function's own name bound in it, so the body can reach itself without any
/// outer binding at all.
#[test]
fn a_named_function_expression_can_reach_itself_by_name() {
    number(
        "let f = function fact(n) { if (n < 2) { return 1; } return n * fact(n - 1); }; return f(5);",
        120.0,
    );
    // Even when the binding it was stored in is then overwritten: the self
    // name lives in the function's own environment, not in the outer one.
    number(
        "let f = function fact(n) { if (n < 2) { return 1; } return n * fact(n - 1); }; \
         let g = f; f = function () { return 0; }; return g(5);",
        120.0,
    );
}

/// 15.2.5 step 4 initialises the self-name binding to *the object step 3 just
/// created*, so the name and whatever the expression was stored in are the
/// same object.
///
/// DIVERGENCE, and it is the one the per-evaluation function record cost.
/// That object is built in the enclosing frame and there is no closure
/// environment to carry it into the call, so the self name is initialised to
/// a fresh object on each call instead. Everything the binding exists for
/// still works -- it is a function, `typeof` says so, and recursion through
/// it runs -- and only identity tells the difference. The fix is a callee
/// slot in the frame, which is the same machinery `this` needs.
#[test]
fn a_named_function_expression_sees_a_function_but_not_its_own_object() {
    text(
        "let f = function me() { return typeof me; }; return f();",
        "function",
    );
    boolean(
        "let f = function me() { return me === undefined; }; return f();",
        false,
    );
    boolean(
        "let f = function me() { return me === f; }; return f();",
        false,
    );
}

/// The same clause, read the other way: that environment is created *for the
/// function*, so the name is not added to the enclosing scope.
#[test]
fn a_named_function_expressions_name_is_not_visible_outside_it() {
    let error = refuses_somehow("let f = function g() { return 1; }; return g();");
    assert!(
        error.message.contains('g'),
        "the diagnostic should name the binding it cannot find: {:?}",
        error.message
    );
    refuses_somehow("let f = function g() { return 1; }; return typeof g;");
}

/// 15.2.5 binds the self name with CreateImmutableBinding, so assigning to it
/// is not a way to rebind the function.
#[test]
fn the_self_name_of_a_function_expression_cannot_be_assigned() {
    // DIVERGENCE: ECMA-262 15.2.5 makes this binding immutable, so in strict
    // mode the assignment is a TypeError at run time and in sloppy mode it is
    // silently ignored and `f()` answers 1. This engine settles it at compile
    // time instead, which is a stricter answer than either -- and the only one
    // available to a compiler with no `throw`.
    let error = refuses_somehow("let f = function g() { g = 1; return 1; }; return f();");
    assert!(
        error.message.contains('g'),
        "the diagnostic should name the binding: {:?}",
        error.message
    );
}

// =========================================================================
// Recursion, mutual recursion, and the namespace table
// =========================================================================

/// A function stored in a property can call itself through that property:
/// the body reads the script binding the object is in (which outlives every
/// frame and so is not a capture), then reads the slot, then calls.
#[test]
fn recursion_through_a_property_terminates_with_the_right_answer() {
    number(
        "const o = {}; o.fact = function (n) { if (n < 2) { return 1; } return n * o.fact(n - 1); }; \
         return o.fact(5);",
        120.0,
    );
    number(
        "const o = {}; o.fib = function (n) { if (n < 2) { return n; } return o.fib(n - 1) + o.fib(n - 2); }; \
         return o.fib(10);",
        55.0,
    );
}

/// Because the slot is read at call time, mutual recursion through two
/// properties works even though the first function is created before the
/// second slot exists. This is the property that makes a namespace table like
/// `fleet.js` legal at all: `fleet.tabs.list` is written before
/// `fleet.terminal.paste` and either may call the other.
#[test]
fn mutual_recursion_through_two_properties_does_not_depend_on_definition_order() {
    boolean(
        "const o = {}; \
         o.even = function (n) { if (n === 0) { return true; } return o.odd(n - 1); }; \
         o.odd = function (n) { if (n === 0) { return false; } return o.even(n - 1); }; \
         return o.even(10);",
        true,
    );
    boolean(
        "const o = {}; \
         o.even = function (n) { if (n === 0) { return true; } return o.odd(n - 1); }; \
         o.odd = function (n) { if (n === 0) { return false; } return o.even(n - 1); }; \
         return o.even(7);",
        false,
    );
}

/// Two properties of one object hold two different functions. Obvious, and
/// exactly the thing a table-index representation could get wrong by handing
/// out one element for two functions -- which would make every method of every
/// `fleet` namespace the same method.
#[test]
fn two_functions_in_one_object_do_not_alias_each_other() {
    number(
        "const o = {}; o.a = function () { return 1; }; o.b = function () { return 2; }; \
         return o.a() * 10 + o.b();",
        12.0,
    );
    boolean(
        "const o = {}; o.a = function () { return 1; }; o.b = function () { return 2; }; return o.a === o.b;",
        false,
    );
    // Sixteen slots, each answering only for itself. A scan of the whole table
    // rather than a spot check, because "the last one wins" and "the first one
    // wins" both pass a two-slot test.
    number(
        "const o = {}; \
         o.a = function () { return 1; }; o.b = function () { return 2; }; \
         o.c = function () { return 4; }; o.d = function () { return 8; }; \
         o.e = function () { return 16; }; o.f = function () { return 32; }; \
         o.g = function () { return 64; }; o.h = function () { return 128; }; \
         return o.a() + o.b() + o.c() + o.d() + o.e() + o.f() + o.g() + o.h();",
        255.0,
    );
}

/// One call site, two different functions of two different arities reaching it
/// on two different calls. The call site cannot have specialised to either.
#[test]
fn one_call_site_dispatches_to_different_functions_on_different_calls() {
    number(
        "const o = {}; o.k = function () { return 1; }; \
         function callit(g) { return g(7); } \
         let one = callit(o.k); \
         o.k = function (n) { return n * 2; }; \
         return one + callit(o.k);",
        15.0,
    );
}

// =========================================================================
// 7.2.15 IsStrictlyEqual -- identity
// =========================================================================

/// 7.2.15 step 4: two values of type Object are strictly equal when they are
/// the *same* object. A function is an ordinary object, so `===` on functions
/// is identity and nothing else.
#[test]
fn strict_equality_on_a_function_value_is_identity() {
    boolean("let f = function () {}; return f === f;", true);
    boolean("let f = function () {}; let g = f; return f === g;", true);
    boolean(
        "let f = function () {}; let g = function () {}; return f === g;",
        false,
    );
    boolean("let f = function () {}; return f !== f;", false);
    // Through a property, and read twice.
    boolean(
        "const o = {}; let f = function () {}; o.m = f; return o.m === f;",
        true,
    );
    boolean(
        "const o = {}; o.m = function () {}; return o.m === o.m;",
        true,
    );
    // Chained assignment puts one function in two slots; they are one object.
    boolean(
        "const o = {}; o.a = o.b = function () {}; return o.a === o.b;",
        true,
    );
    // And a function is never equal to a value of another type.
    boolean("let f = function () {}; return f === undefined;", false);
    boolean("let f = function () {}; return f === null;", false);
    boolean("let f = function () {}; return f === 0;", false);
    boolean(
        "let f = function () {}; const o = {}; return f === o;",
        false,
    );
}

/// 15.2.5 is invoked on **each evaluation** of a FunctionExpression, so two
/// evaluations of one piece of source text are two distinct function objects.
#[test]
fn each_evaluation_of_a_function_expression_is_a_new_object() {
    // This was a DIVERGENCE when the corpus was written: a function value's
    // payload was the table element index, one per function expression in the
    // *source*, so every evaluation of that text yielded the same index and
    // `===` answered `true`. The payload is now the address of a record built
    // per evaluation, so both of these answer what 15.2.5 says.
    boolean(
        "function mk() { return function () { return 1; }; } let a = mk(); let b = mk(); return a === b;",
        false,
    );
    boolean(
        "const t = {}; t.first = undefined; t.same = false; let i = 0; \
         while (i < 2) { let g = function () { return 1; }; \
           if (i === 0) { t.first = g; } else { t.same = t.first === g; } i = i + 1; } \
         return t.same;",
        false,
    );
    // 10.2.11 asks the same of a *declaration*: it is instantiated when the
    // scope holding it is entered, so two calls of the enclosing function are
    // two objects, and reading the name twice inside one call is one.
    boolean(
        "function outer() { function inner() { return 1; } return inner; } \
         return outer() === outer();",
        false,
    );
    boolean(
        "function outer() { function inner() { return 1; } return inner === inner; } \
         return outer();",
        true,
    );
    // What is *not* divergent, and is the half that matters for a namespace
    // table: two distinct FunctionExpressions are always two distinct values,
    // even when their text is identical.
    boolean(
        "let a = function () { return 1; }; let b = function () { return 1; }; return a === b;",
        false,
    );
}

/// 7.2.14 IsLooselyEqual step 1: if both operands have the same type, the
/// answer is IsStrictlyEqual. Nothing is converted, so `==` between two
/// functions is the same identity question `===` asks.
#[test]
fn loose_equality_between_two_functions_is_the_same_question() {
    boolean("let f = function () {}; return f == f;", true);
    boolean(
        "let f = function () {}; let g = function () {}; return f == g;",
        false,
    );
    boolean("let f = function () {}; return f != f;", false);
}

// =========================================================================
// 13.5.3 typeof, 7.1.2 ToBoolean
// =========================================================================

/// 13.5.3 step 6: an object with a [[Call]] internal method answers
/// `"function"` -- the one `typeof` answer that is not the name of a language
/// type. It answers that wherever the function is reached from.
#[test]
fn typeof_a_function_is_function_wherever_it_is_reached_from() {
    text("let f = function () {}; return typeof f;", "function");
    text("return typeof function () {};", "function");
    text("function g() {} return typeof g;", "function");
    text(
        "const o = {}; o.m = function () {}; return typeof o.m;",
        "function",
    );
    text(
        "const o = { m: function () {} }; return typeof o.m;",
        "function",
    );
    text(
        "const a = {}; a.b = {}; a.b.c = function () {}; return typeof a.b.c;",
        "function",
    );
    text(
        "function mk() { return function () {}; } return typeof mk();",
        "function",
    );
    text(
        "function apply1(g) { return typeof g; } return apply1(function () {});",
        "function",
    );
    // 13.5.3 step 3: an *absent* property is `undefined`, not a function, and
    // reading one must not fabricate a slot that then answers "function".
    text(
        "const o = {}; o.m = function () {}; return typeof o.other;",
        "undefined",
    );
    // And what a function is not.
    boolean(
        "let f = function () {}; return typeof f === \"object\";",
        false,
    );
    boolean("const o = {}; return typeof o === \"function\";", false);
    text("let f = function () {}; return typeof typeof f;", "string");
}

/// 7.1.2 ToBoolean: every Object is `true`, and a function is an Object. There
/// is no empty-function special case, and the result flows through the
/// short-circuit operators (13.13.1, 13.14.1) unchanged.
#[test]
fn a_function_value_is_always_truthy() {
    boolean("let f = function () {}; return !!f;", true);
    boolean("let f = function () {}; return !f;", false);
    boolean("function g() {} return !!g;", true);
    boolean("const o = {}; o.m = function () {}; return !!o.m;", true);
    // 13.13.1: `&&` returns the right operand when the left is truthy.
    number("let f = function () {}; return f && 1;", 1.0);
    // 13.14.1: `||` returns the left operand when it is truthy -- so this
    // yields the function itself, and `typeof` is how that is observed.
    text(
        "let f = function () {}; return typeof (f || 1);",
        "function",
    );
    // 14.6.1: as an `if` condition.
    number(
        "let f = function () {}; if (f) { return 1; } return 0;",
        1.0,
    );
    number("const o = {}; if (o.missing) { return 1; } return 0;", 0.0);
}

// =========================================================================
// 13.3.6.1 -- calling something that is not a function
// =========================================================================

/// 13.3.6.1 step 4: if IsCallable(func) is false, throw a **TypeError**. There
/// is no `throw` in this subset, so the fault reaches the host as a trap --
/// but it must be a fault, never a fabricated answer and never a jump.
#[test]
fn calling_a_value_that_is_not_a_function_is_a_fault() {
    for source in [
        "let x = 1; return x();",
        "let x = \"s\"; return x();",
        "let x = true; return x();",
        "let x = null; return x();",
        "let x = undefined; return x();",
        "const o = {}; return o();",
        // The commonest shape of all: a method that is not there. 13.3.2.1
        // reads `undefined`, and 13.3.6.1 then refuses to call it.
        "const o = {}; return o.m();",
        "const o = {}; o.m = 1; return o.m();",
        "const o = {}; o.m = \"s\"; return o.m();",
        // A chain whose last link is missing.
        "const a = {}; a.b = {}; return a.b.c();",
        // A call whose callee is the result of a call that did not return a
        // function.
        "function mk() { return 1; } return mk()();",
    ] {
        traps(source);
    }
}

/// The tag is what makes the refusal safe. A payload of zero is the
/// representation's most reachable accident -- `undefined` and `null` both
/// carry one -- so calling one must fault on the *type*, not on whatever
/// element zero happens to be.
#[test]
fn a_zero_payload_is_not_a_callable_thing() {
    traps("let x = null; return x();");
    traps("let x = undefined; return x();");
    traps("let x = 0; return x();");
    traps("let x = false; return x();");
}

// =========================================================================
// 8.2 / 14.3 -- where a function name comes from, and when
// =========================================================================

/// A FunctionDeclaration is instantiated when its enclosing scope is entered
/// (10.2.11 / 8.2.4), so it is callable above its own text, and its binding
/// already holds the function value there too.
#[test]
fn a_function_declaration_is_available_above_its_own_text() {
    number("return f(); function f() { return 42; }", 42.0);
    number("let g = f; function f() { return 1; } return g();", 1.0);
    text("return typeof f; function f() {}", "function");
    number(
        "const o = {}; o.m = f; function f() { return 3; } return o.m();",
        3.0,
    );
}

/// 14.3.1: a `let` or `const` binding is in its temporal dead zone from the
/// top of the block until its Initializer runs, and touching it there is a
/// ReferenceError. A function *value* is no exception -- the value being a
/// function does not hoist the binding.
#[test]
fn a_let_bound_function_value_is_not_hoisted_out_of_its_dead_zone() {
    refuses_somehow("let g = f; let f = function () {}; return 0;");
    refuses_somehow("return f(); let f = function () { return 1; };");
    refuses_somehow("return typeof f; const f = function () {};");
}

/// DIVERGENCE: 8.2.4 makes a FunctionDeclaration's binding an ordinary mutable
/// var-scoped binding, so real JavaScript accepts `function f() {} f = 1;` and
/// afterwards `f` is `1`. This engine binds the name to the function
/// permanently and settles the assignment at compile time. The refusal names
/// the binding and the byte the function starts at, which is the information a
/// reader needs; what it costs is the shadowing idiom, where a script replaces
/// a declared helper with a wrapper.
#[test]
fn a_function_declarations_binding_cannot_be_reassigned() {
    for source in [
        "function f() { return 1; } f = 2; return f;",
        "function f() { return 1; } f = function () { return 2; }; return f();",
    ] {
        let error = refuses_somehow(source);
        assert!(
            error.message.contains("cannot assign to `f`"),
            "{source:?}: want a sentence about assigning to `f`, got {:?}",
            error.message
        );
    }
    // A function *expression* in a `let` has no such restriction, which is the
    // spelling that works today.
    number(
        "let f = function () { return 1; }; f = function () { return 2; }; return f();",
        2.0,
    );
}

/// 13.2.5.1 CoveredParenthesizedExpression around a FunctionExpression, then
/// 13.3.6.1: the immediately-invoked function expression, which is the one
/// place a function is a value and never a binding.
#[test]
fn an_immediately_invoked_function_expression_runs() {
    number("return (function () { return 8; })();", 8.0);
    number("return (function (a) { return a; })(42);", 42.0);
    number(
        "return (function fact(n) { if (n < 2) { return 1; } return n * fact(n - 1); })(5);",
        120.0,
    );
}

// =========================================================================
// The refusal corpus -- what a function value still is not
// =========================================================================

/// 13.3.7 `this`: a function value here carries no receiver, so `o.m()` calls
/// the function the slot holds and the function cannot see `o`. Refused by
/// name rather than answering `undefined`, because a `this` that silently
/// reads `undefined` turns a method into a wrong answer instead of a stop.
#[test]
fn this_is_refused_wherever_it_appears() {
    for source in [
        "const o = {}; o.m = function () { return this; }; return 0;",
        "const o = {}; o.m = function () { return this.x; }; return 0;",
        "function f() { return this; } return 0;",
        "return (function () { return this; })();",
    ] {
        let error = refuses_somehow(source);
        assert!(
            error.message.contains("`this`"),
            "{source:?}: want a sentence naming `this`, got {:?}",
            error.message
        );
    }
}

/// 15.3 ArrowFunction. In this engine it is exactly a FunctionExpression:
/// every way 15.3 separates the two -- no `this`, no `arguments`, no
/// `[[Construct]]`, no `prototype` -- reaches for something this engine does
/// not have. `tests/arrows_m3.rs` holds the milestone and pins those four
/// absences; what belongs *here* is the part this file is about, which is how
/// an arrow behaves as a function.
///
/// The empty parameter list used to be a recorded wart: `()` ran the parser
/// out of operand before the `=>` was reached, so `() => 1` was refused with
/// a sentence that did not name arrows. It parses now, and the wart is gone
/// with it.
#[test]
fn an_arrow_function_is_a_function_expression() {
    number("const o = {}; o.m = (a) => a; return o.m(3);", 3.0);
    number("let f = a => a * 2; return f(4);", 8.0);
    number("function apply1(g) { return g(1); } return apply1(x => x);", 1.0);
    number("const o = {}; o.m = () => 1; return o.m();", 1.0);
    // The claim that makes the paragraph above true rather than merely
    // plausible: the two spellings are one module.
    assert_eq!(
        compile_qjs_m1("let f = a => a * 2; return f(4);").unwrap(),
        compile_qjs_m1("let f = function (a) { return a * 2; }; return f(4);").unwrap(),
    );
}

/// 9.1.2.2 NewFunctionEnvironment gives a function a reference to the
/// environment it was created in; that is the closure. This engine builds no
/// environment, so a function that reads a *nested* binding is refused by
/// name. Reading a **script** binding is not a capture -- the script's
/// bindings outlive every frame -- and that exception is what makes a
/// namespace table's methods able to see the table.
/// A function value that captures **works** now, and this test used to assert
/// it was refused.
///
/// The three sources are unchanged: the same shapes, answering. The third is
/// the one worth keeping in a file about function *objects* -- a captured
/// binding reached through a property, which is a closure and a namespace
/// table at once.
#[test]
fn a_function_value_that_captures_a_binding_works() {
    number(
        "function outer() { let a = 1; return function () { return a; }; } return outer()();",
        1.0,
    );
    number(
        "function outer(p) { const o = {}; o.m = function () { return p; }; return o; } return outer(4).m();",
        4.0,
    );
    number(
        "function outer() { let a = 1; const o = {}; o.m = function () { return a; }; return o.m(); } return outer();",
        1.0,
    );
    // The script's own bindings, by contrast, are readable from any function.
    number(
        "let a = 40; let f = function () { return a + 2; }; return f();",
        42.0,
    );
    number(
        "const base = 40; const o = {}; o.m = function (n) { return base + n; }; return o.m(2);",
        42.0,
    );
}

/// 13.3.5 `new`: a function value here has no [[Construct]], because it has no
/// `prototype` property and there is no prototype chain for an instance to
/// have. Refused by name.
#[test]
fn a_function_value_is_not_a_constructor() {
    for source in [
        "let f = function () {}; return new f();",
        "function F() {} return new F();",
        "const o = {}; o.M = function () {}; return new o.M();",
    ] {
        let error = refuses_somehow(source);
        assert!(
            error.message.contains("`new`"),
            "{source:?}: want a sentence naming `new`, got {:?}",
            error.message
        );
    }
}

/// DIVERGENCE: 20.2.3 puts `call`, `apply`, `bind` and `toString` on
/// `Function.prototype`, and 20.2.4 gives every function instance own `length`
/// and `name` properties. Real JavaScript answers `f.length` with the
/// parameter count and `f.call(null)` with a call. This engine has no
/// prototype and functions have no own properties, so reading *any* property
/// off a function traps -- the same fault a property read off any other
/// non-Object is. A trap and not a fabricated `undefined`, deliberately:
/// `undefined` is the wrong answer by a right-looking route for exactly the
/// members a script reaches for by reflex.
#[test]
fn no_property_can_be_read_off_a_function() {
    for source in [
        "let f = function () {}; return f.call(1);",
        "let f = function () {}; return f.apply(1);",
        "let f = function () {}; return f.bind(1);",
        "let f = function (a, b) { return a; }; return f.length;",
        "let f = function g() {}; return f.name;",
        "let f = function () {}; return f.prototype;",
        "let f = function () {}; return f.toString();",
        "let f = function () {}; return f.constructor;",
        "let f = function () {}; return f.nonsense;",
        "let f = function () {}; return typeof f.length;",
        "const o = {}; o.m = function () {}; return o.m.length;",
    ] {
        traps(source);
    }
}

/// DIVERGENCE: 10.1 makes a function an ordinary object, so real JavaScript
/// accepts `f.x = 1` and reads it back. Here a function is a tag and an index
/// with no record behind it, so the write traps in the receiver test of
/// `__obj_set` -- and traps rather than silently discarding, since a silently
/// discarded write is a value that vanishes.
#[test]
fn no_property_can_be_written_onto_a_function() {
    traps("let f = function () {}; f.x = 1; return 0;");
    traps("let f = function () {}; f.x = function () {}; return 0;");
    traps("const o = {}; o.m = function () {}; o.m.tag = 1; return 0;");
}

/// DIVERGENCE: 7.1.1 ToPrimitive on a function calls the `toString` its
/// prototype carries, so real JavaScript answers every one of these rather
/// than faulting: `"" + f` is the function's source text, `f + 1` is that text
/// with a `1` on the end, `f < g` (7.2.13) compares two such texts, and
/// `f == 1` (7.2.14 step 12) converts and answers `false`. There is no
/// prototype here, so each of them traps instead. This is the same missing
/// algorithm that stops `"a" + 1`, seen from the function side -- and note
/// that `==` between two *functions* does not need it and does answer, which
/// is `loose_equality_between_two_functions_is_the_same_question` above.
#[test]
fn a_function_never_converts_to_a_primitive() {
    for source in [
        "let f = function () {}; return f + \"\";",
        "let f = function () {}; return \"\" + f;",
        "let f = function () {}; return f + 1;",
        "let f = function () {}; let g = function () {}; return f < g;",
        "let f = function () {}; return f == 1;",
        "let f = function () {}; return f == \"x\";",
    ] {
        traps(source);
    }
}

/// 7.1.4 ToNumber on an Object goes through ToPrimitive and then
/// StringToNumber, which for a function's source text is `NaN`. So real
/// JavaScript answers `f * 2` with `NaN` rather than an error.
#[test]
fn arithmetic_on_a_function_does_not_answer_nan() {
    // DIVERGENCE: ECMA-262 7.1.4 gives `NaN` for every one of these. This
    // engine traps, because ToPrimitive is the step it cannot take. Trapping
    // is the safer of the two wrong-looking answers -- `NaN` propagates
    // silently through arithmetic and surfaces far from its cause -- but it is
    // a divergence and not a design.
    for source in [
        "let f = function () {}; return f * 2;",
        "let f = function () {}; return f - 1;",
        "let f = function () {}; return -f;",
        "let f = function () {}; return f % 2;",
        "let f = function () {}; return +f;",
    ] {
        traps(source);
    }
}

/// DIVERGENCE: 13.2.5.1 / 7.1.19 ToPropertyKey converts a function to its
/// source text and uses that as a String key, so real JavaScript accepts
/// `o[f] = 1`. Here a function key traps in `__to_key`, for the same absent
/// ToPrimitive.
#[test]
fn a_function_is_not_a_property_key() {
    traps("const o = {}; let f = function () {}; o[f] = 1; return 0;");
    traps("const o = {}; let f = function () {}; return o[f];");
}

/// A function value is meaningful only inside the module that made it -- its
/// payload indexes *this* module's table -- so it cannot leave. The tag is
/// what the boundary refuses on, and it refuses by name rather than handing
/// the host an integer that looks like a number.
#[test]
fn a_function_value_cannot_cross_the_host_boundary() {
    let wasm = compile_qjs_m1("let f = function () {}; return f;").expect("compiles");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("clears the gate");
    let mut instance = module.instantiate().expect("instantiates");
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect("runs to completion");
    let [Val::I32(tag), Val::I64(_)] = vals.as_slice() else {
        panic!("want one V1 pair back, got {vals:?}");
    };
    assert_eq!(*tag, TAG_FUNCTION, "the tag a function value carries");
    let error = Value::returned(&vals).expect_err("a host cannot hold a function");
    assert!(
        error.contains("function"),
        "the refusal should name what it refused: {error:?}"
    );
}

// =========================================================================
// The shapes `fleet.js` is actually made of
// =========================================================================

/// The library's own idiom, end to end and running: one shared helper reached
/// through a script binding, a namespace table two levels deep, methods of
/// several arities, and a call that goes through the helper to a slot.
#[test]
fn the_fleet_idiom_runs_end_to_end() {
    let source = "\
        const fleet = {}; \
        const log = {}; log.last = \"\"; \
        function call(opId, params) { \
          if (params === undefined) { log.last = opId + \"|{}\"; } else { log.last = opId + \"|\" + params; } \
          return log.last; \
        } \
        fleet.tabs = {}; \
        fleet.tabs.list = function () { return call(\"tabs.list\"); }; \
        fleet.tabs.set_note = function (tab, note) { return call(\"tabs.set-note\", tab + \"/\" + note); }; \
        fleet.ui = {}; fleet.ui.tabs = {}; \
        fleet.ui.tabs.toggle = function () { return call(\"ui.tabs.toggle\"); }; \
        return fleet.tabs.list() + \" \" + fleet.tabs.set_note(\"7\", \"n\") + \" \" + fleet.ui.tabs.toggle();";
    text(source, "tabs.list|{} tabs.set-note|7/n ui.tabs.toggle|{}");
}

/// Every method of a namespace table is its own function, checked by calling
/// all of them rather than by trusting that two were enough.
#[test]
fn every_slot_of_a_two_level_namespace_answers_for_itself() {
    let source = "\
        const fleet = {}; \
        fleet.tabs = {}; fleet.ui = {}; fleet.ui.tabs = {}; fleet.ui.tree = {}; \
        fleet.tabs.list = function () { return \"a\"; }; \
        fleet.tabs.active = function () { return \"b\"; }; \
        fleet.ui.hello = function () { return \"c\"; }; \
        fleet.ui.tabs.hide = function () { return \"d\"; }; \
        fleet.ui.tabs.show = function () { return \"e\"; }; \
        fleet.ui.tree.toggle = function () { return \"f\"; }; \
        return fleet.tabs.list() + fleet.tabs.active() + fleet.ui.hello() \
             + fleet.ui.tabs.hide() + fleet.ui.tabs.show() + fleet.ui.tree.toggle();";
    text(source, "abcdef");
}

/// A method of one namespace calling a method of another through the shared
/// root binding -- the cross-namespace call `fleet.js` would make if one
/// wrapper wanted another. It works because the root is a script binding and
/// the slot is read at call time.
#[test]
fn one_namespace_method_can_call_another_through_the_root() {
    number(
        "const fleet = {}; fleet.a = {}; fleet.b = {}; \
         fleet.a.inner = function (n) { return n + 1; }; \
         fleet.b.outer = function (n) { return fleet.a.inner(n) * 2; }; \
         return fleet.b.outer(20);",
        42.0,
    );
}

// =========================================================================
// 12.10 ASI, where a function value ends a statement
// =========================================================================

/// 12.10 rule 1: an offending token on a new line gets a semicolon inserted
/// before it. A function expression is the statement ending that makes this
/// matter most, because the statement ends in `}` and the reader's eye reads
/// `}` as already terminal.
#[test]
fn a_statement_ending_in_a_function_expression_is_terminated_by_a_new_line() {
    number(
        "const o = {}; o.m = function (x) { return x; }\nfunction rec() { return o; }\nreturn rec().m(5);",
        5.0,
    );
    number(
        "let a = function () { return 1; }\nlet b = 2;\nreturn a() + b;",
        3.0,
    );
    number(
        "const o = {}\no.m = function () { return 7; }\nreturn o.m()",
        7.0,
    );
}

/// The same source without the line terminator has no ASI to save it, in this
/// engine or in any other -- 12.10 inserts a semicolon only before a line
/// terminator, a `}`, or end of input.
///
/// What is asserted here is only that it is refused and that the refusal
/// speaks for the engine. The *sentence* is deliberately not pinned, and the
/// assertion below records why: the engine reports the `function` keyword as
/// the construct it does not support, which is a construct it plainly does
/// support. That is a real defect in the diagnostic and not in this test --
/// see the note at the bottom of this file.
#[test]
fn without_a_line_terminator_the_same_source_is_refused() {
    let error = refuses_somehow(
        "const o = {}; o.m = function (x) { return x; } function rec() { return o; } return 0;",
    );
    // This used to read "this engine does not support the `function` keyword
    // yet" -- a keyword the engine has had since M1 -- and this test asserted
    // that wording so the fix would be a deliberate edit. It was fixed in
    // `Parser::semicolon`; the assertion is now the true sentence.
    assert_eq!(
        error.message,
        "this engine needs a `;` to end the statement, and found the `function` keyword \
         instead; ECMA-262 12.10 supplies one only across a line break",
        "{:?}",
        error.message
    );
}

// =========================================================================
// Adversarial: the shapes a table-index representation could get wrong
// =========================================================================

/// Many distinct functions in one program, each reached only through a value,
/// each answering only for itself. A count large enough that an off-by-one in
/// how elements are handed out would show up as a wrong answer rather than as
/// a coincidence.
#[test]
fn twenty_distinct_function_values_each_answer_for_themselves() {
    let mut source = String::from("const o = {}; ");
    for i in 0..20 {
        source.push_str(&format!("o.m{i} = function () {{ return {}; }}; ", i * i));
    }
    source.push_str("let total = 0; ");
    for i in 0..20 {
        source.push_str(&format!("total = total + o.m{i}(); "));
    }
    source.push_str("return total;");
    // 0 + 1 + 4 + ... + 361
    let want: f64 = (0..20).map(|i| (i * i) as f64).sum();
    number(&source, want);
}

/// A call site wider than every parameter list in the program, and a
/// parameter list wider than every call site, in one module. 10.2.11 and
/// 13.3.8.1 make both legal and say what each means; neither may disturb the
/// other.
#[test]
fn the_widest_call_and_the_widest_parameter_list_coexist() {
    // Ten arguments into a function that declares none.
    number(
        "let f = function () { return 1; }; return f(1, 2, 3, 4, 5, 6, 7, 8, 9, 10);",
        1.0,
    );
    // One argument into a function that declares eight.
    text(
        "let f = function (a, b, c, d, e, g, h, i) { return typeof i; }; return f(1);",
        "undefined",
    );
    // Both in one module, plus a narrow call that must not be widened.
    number(
        "let wide = function (a, b, c, d, e, g, h, i) { if (i === undefined) { return 8; } return i; }; \
         let narrow = function () { return 1; }; \
         return wide(1) + narrow(2, 3, 4, 5, 6, 7, 8, 9, 10, 11);",
        9.0,
    );
}

/// A function value handed along a chain of frames and only called at the far
/// end. Nothing along the way may consume, copy-flatten or re-tag it.
#[test]
fn a_function_value_survives_being_passed_through_several_frames() {
    number(
        "function one(g) { return two(g); } function two(g) { return three(g); } \
         function three(g) { return g(20) + 2; } \
         return one(function (n) { return n; });",
        22.0,
    );
    // And back out again, unchanged, checked by identity rather than by
    // behaviour -- two functions with the same body would pass a behaviour
    // check.
    boolean(
        "function one(g) { return two(g); } function two(g) { return g; } \
         let f = function () { return 1; }; return one(f) === f;",
        true,
    );
}

/// A binding reassigned to a different function on every iteration, with the
/// call inside the loop. The call site sees three different targets.
#[test]
fn a_loop_can_rebind_a_function_value_between_calls() {
    number(
        "let f = function () { return 1; }; let total = 0; let i = 0; \
         while (i < 3) { total = total + f(); \
           if (i === 0) { f = function () { return 10; }; } \
           if (i === 1) { f = function () { return 100; }; } \
           i = i + 1; } \
         return total;",
        111.0,
    );
}

/// A function value in an object that is itself a property value, built by a
/// literal rather than by assignment -- the nesting `fleet.js` writes as a
/// sequence of statements, written the other legal way.
#[test]
fn a_nested_object_literal_can_carry_functions_at_every_level() {
    number(
        "const fleet = { tabs: { list: function () { return 1; } }, \
                         ui: { tabs: { show: function () { return 2; }, hide: function () { return 4; } } } }; \
         return fleet.tabs.list() + fleet.ui.tabs.show() + fleet.ui.tabs.hide();",
        7.0,
    );
    text(
        "const fleet = { ui: { hello: function () { return \"hi\"; } } }; return typeof fleet.ui.hello;",
        "function",
    );
}

/// A function value used as the operand of every operator that must not
/// convert it, in one place, so a future arm added to a conversion ladder
/// cannot quietly start answering for functions.
#[test]
fn the_operators_that_do_not_convert_a_function_still_do_not() {
    // These three ask no conversion at all, and answer.
    text("let f = function () {}; return typeof f;", "function");
    boolean("let f = function () {}; return !f;", false);
    boolean("let f = function () {}; return f === f;", true);
    // Everything else that would need ToPrimitive still traps.
    for source in [
        "let f = function () {}; return f + f;",
        "let f = function () {}; return f * f;",
        "let f = function () {}; return f > f;",
        "let f = function () {}; return f <= f;",
        "let f = function () {}; let x = f; x++; return x;",
        "let f = function () {}; let x = f; x += 1; return x;",
    ] {
        traps(source);
    }
}

// =========================================================================
// A note this corpus leaves behind
// =========================================================================
//
// One diagnostic found by this file is worth a line of its own, because it is
// the failure mode `diag.rs` exists to prevent, pointing the other way.
//
// `o.m = function (x) { return x; } function rec() {}` -- two statements on
// one line with the `;` left out -- is refused with "this engine does not
// support the `function` keyword yet". The `function` keyword is a keyword
// this engine has supported since M1: the sentence tells the reader that a
// capability they are already using is absent. `diag.rs` argues that a
// sentence blaming the author would be a lie; a sentence disclaiming a
// capability the engine has is the same lie with the sign flipped, and it
// sends the reader to look for a workaround for function declarations instead
// of at the missing semicolon.
//
// `without_a_line_terminator_the_same_source_is_refused` asserted that wording
// so that fixing it would be a red build and a deliberate edit rather than a
// silent change to a sentence nobody had written down. It was a red build, and
// the edit is made: the position now says what it was looking for. The rest of
// the debt -- operand and header positions, where the same table is reached by
// callers that also can lower the token -- is still recorded, in
// `conformance_m2.rs`'s `a_misplaced_token_currently_claims_a_capability_the_
// engine_has`.

// =========================================================================
// Second pass: the questions the first pass suggested
// =========================================================================

/// 10.2.1 step 11 / 14.10: a function that returns nothing, and a function
/// that falls off its end, both complete with `undefined`. Through a value
/// that must still be a well-formed pair and not a half-written result.
#[test]
fn a_function_value_that_returns_nothing_answers_undefined() {
    text(
        "let f = function () { return; }; return typeof f();",
        "undefined",
    );
    text("let f = function () {}; return typeof f();", "undefined");
    undefined("const o = {}; o.m = function () {}; return o.m();");
    // And the `undefined` it produces is the same `undefined` `===` knows.
    boolean("let f = function () {}; return f() === undefined;", true);
    // A branch that returns and one that does not, in one function.
    text(
        "let f = function (n) { if (n > 0) { return 1; } }; return typeof f(0);",
        "undefined",
    );
    number(
        "let f = function (n) { if (n > 0) { return 1; } }; return f(1);",
        1.0,
    );
}

/// 15.2.5: the self-name binding lives in an environment *between* the
/// function and its enclosing scope, so inside the body it shadows an outer
/// binding of the same name -- and outside, that outer binding is untouched.
#[test]
fn the_self_name_shadows_an_outer_binding_of_the_same_name() {
    number(
        "let g = function () { return 1; }; \
         let f = function g(n) { if (n === 0) { return 5; } return g(0); }; \
         return f(1) + g();",
        6.0,
    );
}

/// 10.2.11: a parameter is a binding in the function's own scope, so it
/// shadows a script binding of the same name for the whole body -- including
/// when the value it shadows is a function and the value it holds is another.
#[test]
fn a_parameter_shadows_a_script_binding_of_the_same_name() {
    number(
        "let g = function () { return 1; }; \
         function use(g) { return g(); } \
         return use(function () { return 2; }) * 10 + g();",
        21.0,
    );
}

/// A function expression written directly in the argument list -- the callback
/// literal, which is the most common way a function value is ever created and
/// the one that never touches a binding at all.
#[test]
fn a_function_expression_can_be_written_directly_in_an_argument_list() {
    number(
        "function apply1(g, x) { return g(x); } return apply1(function (n) { return n * 2; }, 21);",
        42.0,
    );
    number(
        "function twice(g, x) { return g(g(x)); } return twice(function (n) { return n + 1; }, 40);",
        42.0,
    );
    text(
        "function kind(g) { return typeof g; } return kind(function () {});",
        "function",
    );
}

/// 13.15.2: an AssignmentExpression evaluates to the value assigned, so it can
/// stand where any value can -- including as the callee of a call.
#[test]
fn an_assignment_evaluates_to_the_function_it_assigned() {
    number("let f; return (f = function () { return 4; })();", 4.0);
    number(
        "const o = {}; return (o.m = function () { return 6; })() + o.m();",
        12.0,
    );
    boolean(
        "const o = {}; let f = function () {}; return (o.m = f) === f;",
        true,
    );
}

/// 7.1.19 ToPropertyKey turns a Number into its String, so `o[1]` and `o["1"]`
/// are one slot -- and that stays true when the value in the slot is a
/// function.
#[test]
fn a_function_can_live_under_a_numeric_key() {
    number(
        "const o = {}; o[1] = function () { return 9; }; return o[1]();",
        9.0,
    );
    number(
        "const o = {}; o[1] = function () { return 9; }; return o[\"1\"]();",
        9.0,
    );
    number(
        "const o = { 0: function () { return 3; } }; return o[0]();",
        3.0,
    );
    text(
        "const o = {}; o[2] = function () {}; return typeof o[2];",
        "function",
    );
}

/// A call in a `while` condition, driving the loop from a function value. The
/// condition is re-evaluated every iteration (14.7.3), so the call happens
/// every iteration and the value it answers with is put through ToBoolean.
#[test]
fn a_call_through_a_value_can_drive_a_loop() {
    number(
        "const s = {}; s.n = 0; let more = function () { s.n = s.n + 1; return s.n < 4; }; \
         let count = 0; while (more()) { count = count + 1; } return count * 10 + s.n;",
        34.0,
    );
}

/// 10.2.11 creates an `arguments` object for every non-arrow function, and a
/// script that reaches for it in this engine must be *stopped*, not quietly
/// resolved to something else. Under the default `Names::Unbound` there is no
/// global scope for it to come from, so the refusal names the missing
/// declaration.
#[test]
fn the_arguments_object_is_absent_and_says_so() {
    // DIVERGENCE: ECMA-262 10.2.11 step 19 binds `arguments` in every ordinary
    // function, so real JavaScript answers `f(1, 2).length` with 2. Here the
    // name resolves to nothing at all.
    let error = refuses_somehow("let f = function () { return arguments; }; return f(1);");
    assert!(
        error.message.contains("arguments"),
        "the diagnostic should name the binding it cannot find: {:?}",
        error.message
    );
    refuses_somehow("let f = function () { return arguments.length; }; return f(1);");
}

/// The target of a property assignment may be any MemberExpression, so the
/// object a function is stored into can itself be computed -- and 13.15.2
/// evaluates that target *before* the right-hand side.
#[test]
fn the_object_a_function_is_stored_into_can_be_computed() {
    number(
        "const t = {}; t.o = {}; function pick() { return t.o; } \
         pick().m = function () { return 5; }; return t.o.m();",
        5.0,
    );
}

/// Recursion through a value, deep enough that the frames are real. 10.2.1
/// puts no bound on depth; the host's stack does, and where that bound is is
/// the engine's business -- but well short of it the answer must simply be
/// right.
#[test]
fn recursion_through_a_value_is_correct_at_depth() {
    number(
        "const o = {}; o.down = function (n) { if (n === 0) { return 0; } return 1 + o.down(n - 1); }; \
         return o.down(200);",
        200.0,
    );
    number(
        "let sum = function sum(n) { if (n === 0) { return 0; } return n + sum(n - 1); }; return sum(100);",
        5050.0,
    );
}

/// A function value called through a chain whose links were built by other
/// calls -- objects allocated per call, functions stored into them, then
/// reached. The bump allocator moves under all of this and nothing may hold a
/// stale pointer.
#[test]
fn function_values_survive_objects_allocated_between_them() {
    number(
        "const keep = {}; \
         function stash(name) { const o = {}; o.tag = name; keep[name] = o; return o; } \
         stash(\"a\"); stash(\"b\"); stash(\"c\"); \
         keep.a.m = function () { return 1; }; keep.b.m = function () { return 2; }; \
         keep.c.m = function () { return 4; }; \
         return keep.a.m() + keep.b.m() + keep.c.m();",
        7.0,
    );
}

/// 10.2.11 is a property of *calls*, not of a particular calling mechanism, so
/// the elastic arity a call through a value has must also be what a call to a
/// statically known name has. Otherwise moving a function into a property --
/// exactly the refactor `fleet.js` is -- would change what its calls mean.
#[test]
fn a_direct_call_is_as_arity_elastic_as_a_call_through_a_value() {
    text(
        "function f(a) { return typeof a; } return f();",
        "undefined",
    );
    number("function f(a) { return a; } return f(1, 2);", 1.0);
    number("function f() { return 1; } return f(9);", 1.0);
    number("const f = function (a) { return a; }; return f(1, 2);", 1.0);
    // The same function, called both ways, with the same surplus.
    number(
        "function f(a) { return a; } const o = {}; o.m = f; return f(1, 9) + o.m(2, 9);",
        3.0,
    );
}

/// 14.2 / 14.6: a FunctionDeclaration in a Block is scoped to that Block.
/// Inside it, it is an ordinary function and an ordinary value.
#[test]
fn a_function_declared_in_a_block_is_usable_inside_that_block() {
    number(
        "if (1) { function f() { return 1; } return f(); } return 0;",
        1.0,
    );
    number("{ function f() { return 1; } return f(); }", 1.0);
    number(
        "if (1) { function f() { return 5; } const o = {}; o.m = f; return o.m(); } return 0;",
        5.0,
    );
}

/// The other half of 14.2, and a divergence worth naming precisely because
/// the two halves of the standard disagree with each other.
#[test]
fn a_function_declared_in_a_block_is_not_visible_outside_it() {
    // DIVERGENCE, and only against Annex B: ECMA-262 14.2 makes the binding
    // block-scoped, which is what this engine does -- but B.3.3.1 additionally
    // creates a *var*-scoped binding for a block-level FunctionDeclaration in
    // sloppy-mode code, so every browser answers `1` here. Under strict-mode
    // (and module) semantics the name is genuinely absent, so this engine's
    // answer is the standard's main-body answer and the refusal is honest.
    // Recorded rather than left implicit, because a script ported from a
    // browser will hit it.
    for source in [
        "if (1) { function f() { return 1; } } return f();",
        "{ function f() { return 1; } } return typeof f;",
    ] {
        let error = refuses_somehow(source);
        assert!(
            error.message.contains("finds no declaration of `f`"),
            "{source:?}: want the missing-binding sentence, got {:?}",
            error.message
        );
    }
}

/// 10.2.1 puts no bound on how deep calls may nest; the host does, through
/// `Limits::max_call_depth`. What matters for this corpus is not where the
/// bound is but what the fault *says*: runaway recursion through a function
/// value must arrive as a **resource ceiling an embedder can raise**, not as
/// a guest fault, and above all not as heap exhaustion -- the guest heap is
/// untouched by a call that never allocates, and telling an embedder to raise
/// `max_memory_pages` would be the misclassification `guest_fault` exists to
/// prevent.
#[test]
fn runaway_recursion_through_a_value_is_a_raisable_ceiling() {
    use tinyvm::{WasmCeiling, WasmFaultClass};
    use tinyvm_qjs::{GuestFault, guest_fault};

    let source = "const o = {}; o.down = function (n) { if (n === 0) { return 0; } return 1 + o.down(n - 1); }; \
                  return o.down(100000);";
    let wasm = compile_qjs_m1(source).expect("compiles");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("clears the gate");
    let mut instance = module.instantiate().expect("instantiates");
    let error = instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect_err("a recursion with no base case reached must not return");

    assert_eq!(
        error.class(),
        WasmFaultClass::ResourceCeiling,
        "runaway recursion is a budget, not a broken guest: {:?}",
        error.message()
    );
    assert_eq!(
        error.ceiling(),
        Some(WasmCeiling::CallDepth),
        "and the budget named is the one to raise: {:?}",
        error.message()
    );
    let recorded = guest_fault(&instance.memory().expect("guest memory"));
    assert_ne!(
        recorded,
        Some(GuestFault::HeapExhausted),
        "a call that never allocates must not be reported as an exhausted heap"
    );

    // Well short of the bound the answer is simply right, so the ceiling is a
    // ceiling and not a low wall in the way of ordinary recursion.
    number(
        "const o = {}; o.down = function (n) { if (n === 0) { return 0; } return 1 + o.down(n - 1); }; \
         return o.down(200);",
        200.0,
    );
}

/// 8.2.6 / 14.3.2: a `var` binding is created and initialized to `undefined`
/// when its scope is entered, and only *assigned* where its text is. So a
/// `var` holding a function expression reads `undefined` above its own line --
/// which is not a dead zone and not an error, and is the one hoisting shape a
/// reader is most likely to get wrong.
#[test]
fn a_var_holding_a_function_reads_undefined_above_its_own_line() {
    text("return typeof f; var f = function () {};", "undefined");
    undefined("var g = f; var f = function () {}; return g;");
    // And below its line it is the function.
    text("var f = function () {}; return typeof f;", "function");
    number("var f = function () { return 1; }; return f();", 1.0);
}

/// A function reached through a *parameter*'s property. The parameter is the
/// function's own binding, so there is no capture; what is being checked is
/// that a property read off an argument object finds the function the caller
/// put there.
#[test]
fn a_function_can_be_reached_through_a_property_of_an_argument() {
    number(
        "function run(o) { return o.m(); } const t = {}; t.m = function () { return 3; }; return run(t);",
        3.0,
    );
    number(
        "function run(o, n) { return o.m(n) + o.k(n); } const t = {}; \
         t.m = function (n) { return n; }; t.k = function (n) { return n * 2; }; return run(t, 7);",
        21.0,
    );
    // Two objects with the same key and different functions do not collide.
    number(
        "function run(o) { return o.m(); } const a = {}; const b = {}; \
         a.m = function () { return 1; }; b.m = function () { return 2; }; return run(a) * 10 + run(b);",
        12.0,
    );
}

/// A key held in a binding, so the slot a call goes through is not decidable
/// from the text -- the read is a real run-time lookup and the value it finds
/// is still callable.
#[test]
fn a_function_can_be_called_through_a_key_held_in_a_binding() {
    number(
        "const o = {}; o.m = function () { return 4; }; const k = \"m\"; return o[k]();",
        4.0,
    );
    number(
        "const o = {}; o.a = function () { return 1; }; o.b = function () { return 2; }; \
         let k = \"a\"; let first = o[k](); k = \"b\"; return first * 10 + o[k]();",
        12.0,
    );
}

/// A three-part `for` (14.7.4) around calls through values, with the function
/// itself changing between iterations.
#[test]
fn a_for_loop_can_call_through_values() {
    number(
        "const o = {}; o.step = function (n) { return n * 2; }; let total = 0; \
         for (let i = 1; i < 4; i = i + 1) { total = total + o.step(i); } return total;",
        12.0,
    );
    number(
        "let f = function () { return 1; }; let total = 0; \
         for (let i = 0; i < 3; i = i + 1) { total = total + f(); f = function () { return 10; }; } \
         return total;",
        21.0,
    );
}

/// A function value produced by a branch: the two arms of an `if` are two
/// FunctionExpressions, and which one the binding holds is a run-time fact.
/// (The engine has no `?:` yet, so this is how the choice is spelled.)
#[test]
fn a_branch_can_choose_which_function_a_binding_holds() {
    number(
        "function pick(flag) { if (flag) { return function () { return 1; }; } return function () { return 2; }; } \
         return pick(true)() * 10 + pick(false)();",
        12.0,
    );
    boolean(
        "function pick(flag) { if (flag) { return function () { return 1; }; } return function () { return 2; }; } \
         return pick(true) === pick(false);",
        false,
    );
}

// =========================================================================
// The product promise the whole refusal corpus rests on
// =========================================================================

/// Every refusal in the function-value area, swept in one place against the
/// four things `diag.rs` promises: the sentence speaks for the *engine*, it
/// never calls the script wrong, it carries a byte offset that is inside the
/// source, and it classifies itself for a caller with no room for a `String`.
///
/// A sweep and not a per-site check because the promise is about the whole
/// surface: one refusal that says "unexpected token" undoes the claim for all
/// of them, and it will be a refusal nobody thought to test individually.
#[test]
fn every_refusal_in_this_area_speaks_for_the_engine() {
    let sources = [
        "const o = {}; o.m = function () { return this; }; return 0;",
        "let f = function () { return this.x; }; return 0;",
        // The two capture rows left this list when closures landed; they are
        // assertions about answers now, in
        // `a_function_value_that_captures_a_binding_works`. The three arrow
        // rows left it the same way when arrows landed --
        // `an_arrow_function_is_a_function_expression` above, and the
        // milestone itself in `tests/arrows_m3.rs`. What is still refused
        // about an arrow is its *parameter* syntax, which is the same
        // refusal a function's parameters get and is already covered by the
        // rows below.
        "let f = function () {}; return new f();",
        "let f = function g() {}; return g();",
        "let g = f; let f = function () {}; return 0;",
        "function f() {} f = 1; return 0;",
        "let f = function () { return arguments; }; return f(1);",
        "if (1) { function f() { return 1; } } return f();",
        "const o = {}; o.m = function () {} function g() {} return 0;",
        "let f = function () { return yield 1; }; return 0;",
        "let f = async function () { return 1; }; return 0;",
        "let f = function* () { return 1; }; return 0;",
        "const o = { m() { return 1; } }; return o.m();",
        "const o = { get m() { return 1; } }; return o.m;",
    ];
    for source in sources {
        let error = refuse(source);
        assert!(
            error.message.starts_with("this engine"),
            "{source:?}: a diagnostic must speak for the engine, got {:?}",
            error.message
        );
        for forbidden in ["syntax error", "invalid", "illegal", "bad ", "you "] {
            assert!(
                !error.message.to_lowercase().contains(forbidden),
                "{source:?}: a diagnostic must not blame the author ({forbidden:?}): {:?}",
                error.message
            );
        }
        assert!(
            error.offset <= source.len(),
            "{source:?}: the offset {} is past the end of a {}-byte source",
            error.offset,
            source.len()
        );
        // `Boundary` is the machine-readable half; every one of these is a
        // real one, and `terse()` is what a fmt-free caller receives.
        assert!(
            !error.boundary.terse().is_empty(),
            "{source:?}: the boundary must carry a fmt-free summary"
        );
    }
}
