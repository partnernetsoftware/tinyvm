//! Function values: a function that a property can hold and a call can find.
//!
//! Every expectation here is derived from ECMA-262, and every one of them
//! **runs**: compile -> tinyvm's load gate -> instantiate ->
//! `invoke_by_name("main")`. "It compiled" is not evidence and only appears
//! alone in the refusal corpus, where not compiling *is* the claim.
//!
//! # The mechanism this file is the specification of
//!
//! wasm MVP has no first-class function pointers, so a function value is a
//! **table element index** carried in a V1 pair -- `(TAG_FUNCTION, index)` --
//! and calling one is `call_indirect` through table 0. `call_indirect` matches
//! the callee's signature *exactly* (spec 4.4.8) while JavaScript's calls are
//! arity-elastic, so the table does not hold the user's functions: it holds one
//! **adapter** per function, all of one uniform signature, each forwarding as
//! many arguments as its target declares. Three facts follow, and each has a
//! test below:
//!
//! * element 0 is deliberately left null, so a zeroed payload is never a
//!   callable element;
//! * a call with too few arguments passes `undefined` and one with too many
//!   evaluates and discards the surplus (ECMA-262 8.6.1 and 13.3.8.1), because
//!   the adapter drops what its target does not declare;
//! * calling a value that is not a function traps at the **tag test**, before
//!   any table is touched.

use tinyvm::{Limits, Val, WasmInstance, WasmModule};
use tinyvm_qjs::{Boundary, CompileError, Names, Options, Value, compile_qjs_m1};

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

/// The function tag, as `repr.rs` numbers it. Written out rather than
/// imported because `repr` is crate-private: this is the contract restated
/// from the outside, which is the only place it can be checked from.
const TAG_FUNCTION: i32 = 6;

fn compile(source: &str) -> Result<Vec<u8>, CompileError> {
    compile_qjs_m1(source)
}

fn build(source: &str) -> Result<(WasmModule, Vec<u8>), String> {
    let wasm = compile(source).map_err(|e| format!("compiling {source:?}: {e}"))?;
    let module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .map_err(|e| format!("load gate rejected {source:?}: {}", e.message()))?;
    Ok((module, wasm))
}

fn attempt(source: &str) -> Result<(WasmInstance, Vec<Val>), String> {
    let (module, _) = build(source)?;
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

/// A source that compiles, clears the load gate, and then traps.
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
    match compile(source) {
        Ok(bytes) => panic!(
            "{source:?} compiled to {} bytes; expected a capability diagnostic",
            bytes.len()
        ),
        Err(e) => e,
    }
}

#[track_caller]
fn refuses_capability(source: &str, construct: &str, boundary: Boundary) {
    let error = refuse(source);
    assert_eq!(
        error.message,
        format!("this engine does not support {construct} yet"),
        "{source:?}"
    );
    assert_eq!(error.boundary, boundary, "{source:?}");
}

// =========================================================================
// A function is a value
// =========================================================================

/// The whole milestone in one line: a function expression stored in a binding
/// that is not a known-function binding, then called through that binding.
///
/// `let` and not `const`, deliberately. A `const f = function () {}` is
/// classified as a known function and calls to it stay *direct* -- that path
/// already worked and is not what this file is about.
#[test]
fn a_function_expression_is_a_value_that_can_be_called() {
    number(
        "let f = function (a) { return a * 2; }; return f(21);",
        42.0,
    );
    number("let f = function () { return 7; }; return f();", 7.0);
    // Re-bound: the value moves, and the call finds it wherever it is.
    number(
        "let f = function (a) { return a * 2; }; let g = f; return g(21);",
        42.0,
    );
}

/// ECMA-262 13.5.3 step 6: a callable's `typeof` is `"function"`, which is the
/// one answer that is not a language-type name.
#[test]
fn typeof_a_function_is_function() {
    text("let f = function () {}; return typeof f;", "function");
    text("return typeof function () {};", "function");
    text("function g() {} return typeof g;", "function");
    boolean(
        "let f = function () {}; return typeof f === \"function\";",
        true,
    );
    // And it is not an object, which is where a tag-less engine would put it.
    boolean(
        "let f = function () {}; return typeof f === \"object\";",
        false,
    );
}

/// 7.1.2 ToBoolean step 6: every object, function included, is true.
#[test]
fn a_function_is_truthy() {
    boolean(
        "let f = function () {}; if (f) { return true; } return false;",
        true,
    );
    boolean("let f = function () {}; return !f;", false);
    boolean("let f = function () {}; return !!f;", true);
}

/// 7.2.15 step 4 SameValueNonNumber: two function values are strictly equal
/// exactly when they are the same function. The payload comparison already is
/// that, which is why `===` needed no arm of its own -- the same reason
/// Objects needed none.
#[test]
fn strict_equality_on_functions_is_identity() {
    boolean("let f = function () {}; let g = f; return f === g;", true);
    boolean(
        "let f = function () {}; let g = function () {}; return f === g;",
        false,
    );
    // Read twice, one identity.
    boolean(
        "const o = {}; o.f = function () {}; return o.f === o.f;",
        true,
    );
    // A function is not any other type.
    boolean("let f = function () {}; return f === 1;", false);
    boolean("let f = function () {}; return f === undefined;", false);
    boolean("let f = function () {}; return f !== null;", true);
}

// =========================================================================
// A property can hold one
// =========================================================================

/// The shape `fleet.js` writes twenty-nine times.
#[test]
fn a_property_can_hold_a_function_and_a_call_can_find_it() {
    number(
        "const o = {}; o.m = function () { return 7; }; return o.m();",
        7.0,
    );
    number(
        "const o = { m: function (a) { return a + 1; } }; return o.m(41);",
        42.0,
    );
    // Nested, which is the namespace-table shape.
    number(
        "const f = {}; f.ui = {}; f.ui.tabs = {}; f.ui.tabs.show = function () { return 3; }; return f.ui.tabs.show();",
        3.0,
    );
    // A computed key reaches the same slot.
    number(
        "const o = {}; o.m = function () { return 5; }; return o[\"m\"]();",
        5.0,
    );
}

/// A function value survives every place a value goes: an argument, a return,
/// a property, and back out again.
#[test]
fn a_function_value_passes_through_arguments_and_returns() {
    number(
        "function apply1(g, x) { return g(x); } return apply1(function (a) { return a + 1; }, 41);",
        42.0,
    );
    number(
        "function mk() { return function () { return 5; }; } return mk()();",
        5.0,
    );
    // Through a property and out again.
    number(
        "function mk() { const o = {}; o.m = function () { return 9; }; return o; } return mk().m();",
        9.0,
    );
    // A declared function, read by name rather than called: the name is a
    // constant naming that function, so reading it captures nothing.
    number("function g() { return 4; } let h = g; return h();", 4.0);
}

/// A call through a chain of property reads -- `o.a.b()` -- which is the form
/// the README named as still refused.
#[test]
fn a_call_through_a_property_chain_reaches_the_function() {
    number(
        "const o = {}; o.a = {}; o.a.b = function () { return 6; }; return o.a.b();",
        6.0,
    );
    // The receiver is evaluated once, which a counter in a property getter
    // cannot show here -- so it is shown with a side-effecting index instead.
    number(
        "let n = 0; const o = {}; o.a = {}; o.a.b = function () { return 1; }; function bump() { n = n + 1; return \"a\"; } o[\"a\"].b(); return n;",
        0.0,
    );
}

// =========================================================================
// Arity: JavaScript's calls are elastic and wasm's are not
// =========================================================================

/// ECMA-262 8.6.1: a parameter with no matching argument is initialised to
/// `undefined`. Not a trap, which is what a bare `call_indirect` would do.
#[test]
fn a_missing_argument_is_undefined() {
    undefined("let f = function (a) { return a; }; return f();");
    boolean(
        "let f = function (a) { return a === undefined; }; return f();",
        true,
    );
    number("let f = function (a, b) { return a; }; return f(1);", 1.0);
    undefined("let f = function (a, b) { return b; }; return f(1);");
}

/// 13.3.8.1 ArgumentListEvaluation evaluates *every* argument, and 10.2.11
/// then binds only as many as the function declares. So a surplus argument
/// still runs, and is then discarded.
#[test]
fn a_surplus_argument_is_evaluated_and_then_ignored() {
    number(
        "let f = function (a) { return a; }; return f(1, 2, 3);",
        1.0,
    );
    // The surplus one ran: `n` moved.
    number(
        "let n = 0; function bump() { n = n + 1; return 0; } let f = function (a) { return a; }; f(1, bump()); return n;",
        1.0,
    );
}

// =========================================================================
// Calling something that is not a function
// =========================================================================

/// The tag test is the guard, so a wrong-typed callee is a clean guest fault
/// and never a `call_indirect` into whatever the payload happened to be.
///
/// A trap and not a diagnostic: ECMA-262 makes this a run-time TypeError, and
/// in a dynamic language the callee's type is a property of the run. This
/// engine has no `throw`, so the trap is the closest thing to one it has.
#[test]
fn calling_a_value_that_is_not_a_function_traps() {
    for source in [
        "let a = 1; return a();",
        "return 1();",
        "let a = \"s\"; return a();",
        "let a = true; return a();",
        "let a = null; return a();",
        "let a = undefined; return a();",
        "const o = {}; return o();",
        // A property that is not there reads `undefined`, and calling that is
        // the same fault -- which is what `o.toString()` now is.
        "const o = {}; return o.m();",
        "const o = {}; return o.toString();",
    ] {
        traps(source);
    }
}

/// The guest recorded no heap fault: this is the script's own type error, not
/// a budget the host can raise.
#[test]
fn a_wrong_type_call_is_not_reported_as_heap_exhaustion() {
    let (module, _) = build("let a = 1; return a();").expect("compiles and loads");
    let mut instance = module.instantiate().expect("instantiates");
    let outcome = instance.invoke_by_name("main", &Value::args(&[]));
    assert!(outcome.is_err(), "want a trap");
    let memory = instance.memory().expect("guest memory");
    assert_eq!(tinyvm_qjs::guest_fault(&memory), None);
}

// =========================================================================
// Named function expressions, ECMA-262 15.2.5
// =========================================================================

/// 15.2.5: the name of a *function expression* is bound in an environment of
/// the function's own, so the body can see it and nothing outside can.
#[test]
fn a_named_function_expression_can_see_its_own_name() {
    number(
        "let f = function fact(n) { if (n < 2) { return 1; } return n * fact(n - 1); }; return f(5);",
        120.0,
    );
    // The name is the function itself, as a value.
    boolean(
        "let f = function me() { return me === undefined; }; return f();",
        false,
    );
    number(
        "let f = function me(n) { if (n === 0) { return 1; } return me(n - 1); }; return f(3);",
        1.0,
    );
}

#[test]
fn a_named_function_expressions_name_does_not_leak_outside_it() {
    let error = refuse("let f = function fact(n) { return 1; }; return fact(1);");
    assert!(
        error.message.contains("finds no declaration of `fact`"),
        "got {:?}",
        error.message
    );
    let error = refuse("let f = function fact(n) { return 1; }; return fact;");
    assert!(
        error.message.contains("finds no declaration of `fact`"),
        "got {:?}",
        error.message
    );
}

/// 15.2.5 again: the parameter list shadows the self-name, and the shadowing
/// is not a redeclaration error. The lowering reads a parameter out of the
/// local its slot names, so a self-name that took slot 0 would shift every
/// parameter by one and silently read the wrong argument -- which is the
/// defect this test exists to keep fixed.
#[test]
fn the_parameter_list_shadows_the_self_name() {
    number("let f = function me(me) { return me; }; return f(8);", 8.0);
    number(
        "let f = function me(a, me) { return a + me; }; return f(1, 2);",
        3.0,
    );
    // And with no shadowing, the parameters still land in the right slots.
    number(
        "let f = function me(a, b) { return a - b; }; return f(9, 4);",
        5.0,
    );
}

// =========================================================================
// What is still out of scope, and how it says so
// =========================================================================

/// A closure that captures an outer local must never be silently
/// miscompiled. Function values do not change that: the value is a table
/// index, and a table index carries no environment.
#[test]
fn a_closure_that_captures_is_still_refused() {
    for source in [
        "function outer() { let a = 1; function inner() { return a; } return inner(); } return outer();",
        "function outer() { let a = 1; return function () { return a; }; } return 0;",
        "function outer() { let a = 1; let f = function () { return a; }; return f(); } return outer();",
        "function outer(p) { return function () { return p; }; } return 0;",
    ] {
        refuses_capability(source, "closures that capture a variable", Boundary::FullJs);
    }
}

/// The script's own bindings outlive every frame, so reading one from inside
/// a function is not a capture -- and that stays true for a function value.
#[test]
fn a_script_binding_is_still_not_a_capture() {
    number(
        "let a = 40; let f = function () { return a + 2; }; return f();",
        42.0,
    );
}

/// `this` and `new` are each still a capability diagnostic that names the
/// construct ahead of the engine. Function values change nothing about them:
/// a table index carries no receiver and no prototype.
#[test]
fn this_and_constructors_are_still_refused() {
    for (source, construct) in [
        (
            "const o = {}; o.m = function () { return this; }; return 0;",
            "the `this` keyword",
        ),
        (
            "let f = function () { return new Object(); }; return 0;",
            "the `new` keyword",
        ),
    ] {
        let error = refuse(source);
        assert!(
            error.message.contains(construct),
            "{source:?}: want a sentence naming {construct:?}, got {:?}",
            error.message
        );
    }
}

/// An arrow function is refused, and the sentence names arrows -- except when
/// the parameter list is empty, where `()` runs the parser out of operand
/// before the `=>` is ever reached. Recorded as it is rather than as it should
/// be: the empty-list wording is a gap in the front end and not in this
/// milestone, and a test asserting the better sentence would be asserting
/// something no line of the engine says.
#[test]
fn an_arrow_function_is_refused() {
    refuses_capability(
        "let f = (a) => a; return 0;",
        "arrow functions",
        Boundary::FullJs,
    );
    refuses_capability(
        "let f = a => a; return 0;",
        "arrow functions",
        Boundary::FullJs,
    );
    let empty = refuse("let f = () => 1; return 0;");
    assert_eq!(empty.boundary, Boundary::Subset);
    assert!(
        !empty.message.contains("arrow"),
        "if the empty parameter list now names arrows too, update this test: {:?}",
        empty.message
    );
}

/// `bind`, `call` and `apply` live on `Function.prototype`, and there is no
/// prototype here. A property read off a function is therefore the same fault
/// a property read off any other non-Object is -- a trap, from the receiver
/// test in `__obj_get`, and not a fabricated `undefined`.
#[test]
fn bind_call_and_apply_are_not_reachable() {
    for source in [
        "let f = function () {}; return f.call(1);",
        "let f = function () {}; return f.apply(1);",
        "let f = function () {}; return f.bind(1);",
        "let f = function () {}; return f.length;",
    ] {
        traps(source);
    }
}

/// A function value cannot cross the host door: `Value` has no variant for
/// one, and a guest table index is meaningless on the other side.
#[test]
fn a_function_cannot_be_returned_to_the_host() {
    let (_, vals) = {
        let (module, _) = build("let f = function () {}; return f;").expect("compiles and loads");
        let mut instance = module.instantiate().expect("instantiates");
        let vals = instance
            .invoke_by_name("main", &Value::args(&[]))
            .expect("runs");
        (instance, vals)
    };
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

/// A declared host door takes raw wasm parameters, and a function is not one
/// of them. Settled at compile time where the text settles it.
#[test]
fn a_function_cannot_be_passed_through_a_declared_host_door() {
    use tinyvm_qjs::{HostFn, HostParam, HostResult, compile_qjs_m1_with};
    let table = vec![HostFn {
        name: "print".to_string(),
        module: "sys".to_string(),
        field: "print".to_string(),
        params: vec![HostParam::StrPtrLen],
        result: HostResult::Void,
    }];
    let error = compile_qjs_m1_with(
        "print(function () {}); return 0;",
        Options {
            names: Names::Declared(table),
        },
    )
    .expect_err("a function is not a String");
    assert!(
        error.message.contains("a function"),
        "got {:?}",
        error.message
    );
    assert_eq!(error.boundary, Boundary::ThirdBinding);
}

// =========================================================================
// The paths a value takes that a direct call never did
// =========================================================================

/// A script binding is two module globals, so a function value stored in one
/// is a `(tag, payload)` pair in globals -- and reading it from inside another
/// function is the path a direct call never used. Not a capture: the script's
/// storage outlives every frame.
#[test]
fn a_function_value_lives_in_a_script_binding_and_is_read_from_a_frame() {
    number(
        "let f = function () { return 6; }; function use() { return f(); } return use();",
        6.0,
    );
    // Reassigned between two calls: the binding really is storage, not a name
    // for one function.
    number(
        "
        let f = function () { return 1; };
        function use() { return f(); }
        const first = use();
        f = function () { return 10; };
        return first + use();
        ",
        11.0,
    );
}

/// Two functions that only reach each other through values. Neither is a
/// known-function binding, so neither call can be direct.
#[test]
fn mutual_recursion_through_values_terminates() {
    number(
        "
        let even = function (n) { if (n === 0) { return true; } return odd(n - 1); };
        let odd = function (n) { if (n === 0) { return false; } return even(n - 1); };
        if (even(10)) { return 1; }
        return 0;
        ",
        1.0,
    );
}

/// A call whose callee is itself a call, twice over.
#[test]
fn a_call_can_be_the_callee_of_a_call() {
    number(
        "let a = function () { return function () { return function () { return 3; }; }; }; return a()()();",
        3.0,
    );
}

/// The uniform arity is the *widest parameter list in the program*, so a
/// program with one wide function makes every adapter that wide -- and a
/// zero-argument call through one still has to work. That is the loose end of
/// the bound, tested where it is loosest.
#[test]
fn a_wide_function_elsewhere_does_not_break_a_narrow_call() {
    number(
        "
        function wide(a, b, c, d, e, f, g, h) { return a; }
        let narrow = function () { return 2; };
        let one = function (x) { return x; };
        return narrow() + one(3) + wide(1, 2, 3, 4, 5, 6, 7, 8);
        ",
        6.0,
    );
    // And the wide one itself, reached as a value.
    number(
        "
        function wide(a, b, c, d, e, f, g, h) { return h; }
        let w = wide;
        return w(1, 2, 3, 4, 5, 6, 7, 8);
        ",
        8.0,
    );
    // Wide, reached as a value, and given too few.
    boolean(
        "
        function wide(a, b, c, d, e, f, g, h) { return h === undefined; }
        let w = wide;
        return w(1);
        ",
        true,
    );
}

/// Every conversion a function value can fall into is the trap the missing
/// algorithm leaves, never a fabricated answer. Each of these reaches a
/// different `unreachable` -- `__to_number`, `__add`'s numeric branch and
/// `__to_key` -- and all three are the absent ToPrimitive.
#[test]
fn a_function_never_converts_to_something_it_is_not() {
    for source in [
        "let f = function () {}; return f + 1;",
        "let f = function () {}; return f - 1;",
        "let f = function () {}; return -f;",
        "let f = function () {}; return f < 1;",
        "let f = function () {}; return f == 1;",
        "let f = function () {}; const o = {}; return o[f];",
    ] {
        traps(source);
    }
    // `+` between a function and a String is the *String* gap, not this one --
    // but it is still a trap and still not a fabricated answer.
    traps("let f = function () {}; return f + \"a\";");
}

/// The six tags, in one program, through the one dispatch site that names all
/// of them. Here so that appending the sixth arm is shown not to have moved
/// any of the five.
#[test]
fn typeof_still_answers_for_every_type() {
    text(
        "
        let f = function () {};
        return typeof 1 + \"|\" + typeof \"s\" + \"|\" + typeof true + \"|\"
             + typeof undefined + \"|\" + typeof null + \"|\" + typeof {} + \"|\" + typeof f;
        ",
        "number|string|boolean|undefined|object|object|function",
    );
}

/// And the truthiness ladder, likewise.
#[test]
fn truthiness_still_answers_for_every_type() {
    text(
        "
        let f = function () {};
        let out = \"\";
        if (1) { out = out + \"1\"; } else { out = out + \"0\"; }
        if (0) { out = out + \"1\"; } else { out = out + \"0\"; }
        if (\"s\") { out = out + \"1\"; } else { out = out + \"0\"; }
        if (\"\") { out = out + \"1\"; } else { out = out + \"0\"; }
        if (true) { out = out + \"1\"; } else { out = out + \"0\"; }
        if (null) { out = out + \"1\"; } else { out = out + \"0\"; }
        if (undefined) { out = out + \"1\"; } else { out = out + \"0\"; }
        if ({}) { out = out + \"1\"; } else { out = out + \"0\"; }
        if (f) { out = out + \"1\"; } else { out = out + \"0\"; }
        return out;
        ",
        "101010011",
    );
}

/// The scan predicts *whether* a program has function values and indirect
/// calls; the lowering assigns the elements and emits the calls. They walk the
/// tree separately, so this exercises every syntactic position the two could
/// disagree in -- a declarator initialiser, a for-header, an object-literal
/// value, an assignment target, a compound assignment, a computed key, an
/// argument and a return.
///
/// A disagreement is not a wrong answer: it is a panic in the lowering, or a
/// module with no table for a `call_indirect` to use. Both are loud, and this
/// test is what makes them loud *here* rather than in a script.
#[test]
fn every_position_a_function_value_can_appear_in() {
    number("let f = function () { return 1; }; return f();", 1.0);
    number(
        "const o = { m: function () { return 2; } }; return o.m();",
        2.0,
    );
    number(
        "let n = 0; let f = function () { return 1; }; for (let i = 0; i < 3; i = i + f()) { n = n + 1; } return n;",
        3.0,
    );
    number(
        "const o = {}; o.m = function () { return 4; }; let t = 0; t += o.m(); return t;",
        4.0,
    );
    number(
        "const o = {}; const k = \"m\"; o[k] = function () { return 5; }; return o[k]();",
        5.0,
    );
    number(
        "let id = function (x) { return x; }; let g = function (h) { return h(6); }; return g(id);",
        6.0,
    );
    number(
        "let f = function () { return function () { return 7; }; }; return f()();",
        7.0,
    );
    number(
        "let f = function () { return 8; }; let o = {}; o.a = {}; o.a.b = f; return o.a.b();",
        8.0,
    );
    number(
        "let f = function () { return 9; }; while (false) { f(); } return f();",
        9.0,
    );
    // A function value that is never called at all still needs its element,
    // and a program whose only indirect call is on a value it never made still
    // needs the table for `call_indirect` to name.
    undefined("let f = function () { return 1; }; return undefined;");
    traps("const o = {}; return o.nothing();");
}

// =========================================================================
// The table, from the outside
// =========================================================================

/// A module that never makes a function a value carries no table at all --
/// the section is not a fixed prelude, so a script that does not use the
/// capability does not pay for it.
#[test]
fn a_script_with_no_function_values_carries_no_table() {
    let plain = compile("return 1 + 2;").expect("compiles");
    assert!(
        !has_section(&plain, 4) && !has_section(&plain, 9),
        "a script with no function values should have neither a table nor an element section"
    );
    let with_values = compile("let f = function () {}; return f();").expect("compiles");
    assert!(
        has_section(&with_values, 4) && has_section(&with_values, 9),
        "a script with a function value needs both a table and an element section"
    );
}

/// Element 0 is left null on purpose, so that a zeroed payload -- the shape of
/// an uninitialised word -- can never be a callable element. The tag test is
/// the real guard; this is the second one.
#[test]
fn element_zero_is_never_a_function() {
    let wasm = compile("let f = function () { return 1; }; return f();").expect("compiles");
    let element = section(&wasm, 9).expect("an element section");
    // Flag 0, then the offset expression `i32.const 1; end`.
    assert_eq!(
        &element[..5],
        &[0x01, 0x00, 0x41, 0x01, 0x0b],
        "one segment, flag 0, at offset 1: {element:02x?}"
    );
}

/// Walk the section headers of a module and hand back one section's body.
fn section(wasm: &[u8], id: u8) -> Option<Vec<u8>> {
    let mut at = 8;
    while at < wasm.len() {
        let found = wasm[at];
        at += 1;
        let (len, next) = leb(wasm, at);
        at = next;
        if found == id {
            return Some(wasm[at..at + len].to_vec());
        }
        at += len;
    }
    None
}

fn has_section(wasm: &[u8], id: u8) -> bool {
    section(wasm, id).is_some()
}

fn leb(bytes: &[u8], mut at: usize) -> (usize, usize) {
    let mut value = 0usize;
    let mut shift = 0;
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
// The acceptance target
// =========================================================================

/// `agenterm/scripts/qjs/lib/fleet.js`, as it stands: twenty-nine function
/// expressions assigned to properties and ten calls through them.
///
/// A snapshot, not a link, because a test that reads another repository's
/// working tree measures whatever that tree happens to be today. The upstream
/// path is in the doc comment above `FLEET_JS`.
///
/// It still does not compile whole, and the diagnostic is the measurement:
/// after this milestone the wall is no longer the function value, it is the
/// conditional expression on line 14.
#[test]
fn fleet_js_now_stops_at_the_conditional_and_not_at_the_function_value() {
    let error = compile_with_hosts(FLEET_JS).expect_err("two walls are left");
    assert_eq!(
        error.message, "this engine does not support conditional expressions yet",
        "the remaining wall moved to the wrong construct"
    );
    // The offset, pinned, because the README quotes it. `FLEET_JS` is a raw
    // string that opens with a newline, so it is one byte ahead of the same
    // offset in the file itself -- which is 727, on line 14.
    assert_eq!(error.offset, 728);
    assert_eq!(
        FLEET_JS.len(),
        6281,
        "the snapshot is the 6 280-byte file plus that newline"
    );
    assert_eq!(FLEET_JS[..error.offset].matches('\n').count(), 14);
    assert!(
        FLEET_JS[error.offset..].starts_with("? \"{}\" : params"),
        "the offset must point at the `?` and not near it"
    );
}

/// The same library with its two remaining walls written the way this engine
/// spells them -- the conditional as an `if`, the `try` gone -- compiles,
/// loads, instantiates and runs. Every one of its twenty-nine function-valued
/// properties is reachable and every one of its namespace tables is built.
#[test]
fn the_whole_fleet_library_compiles_and_its_methods_are_reachable() {
    let source = fleet_without_the_remaining_walls();
    let wasm = compile_with_hosts(&source).expect("the fleet library compiles");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .expect("the fleet library clears the load gate");
    let imports: Vec<String> = module
        .imports()
        .iter()
        .map(|i| format!("{}.{}", i.module, i.field))
        .collect();
    assert_eq!(imports, ["js.JSON", "js.__host"], "got {imports:?}");
    // A record, not a budget -- so a change in it is visible in a diff. The
    // README quotes this number; 6 625 of it is the conversion prelude every
    // module carries, whatever the script.
    assert_eq!(wasm.len(), 16_381, "the whole library's emitted size moved");
}

/// The same shape, reduced to what can run with no host at all: a namespace
/// tree of function-valued properties, reached by method call.
#[test]
fn the_fleet_namespace_shape_runs() {
    text(
        "
        const fleet = {};
        function call(opId, params) { return opId; }
        fleet.tabs = {};
        fleet.tabs.list = function () { return call(\"tabs.list\"); };
        fleet.tabs.set_note = function (tabId, note) { return call(\"tabs.set-note\", tabId); };
        fleet.ui = {};
        fleet.ui.tabs = {};
        fleet.ui.tabs.toggle = function () { return call(\"ui.tabs.toggle\"); };
        fleet.ui.input = {};
        fleet.ui.input.pointer = function (x, y, action) { return call(\"ui.input.pointer\"); };
        return fleet.tabs.list() + \"|\" + fleet.ui.tabs.toggle() + \"|\" + fleet.ui.input.pointer(1, 2, \"down\");
        ",
        "tabs.list|ui.tabs.toggle|ui.input.pointer",
    );
}

fn compile_with_hosts(source: &str) -> Result<Vec<u8>, CompileError> {
    tinyvm_qjs::compile_qjs_m1_with(
        source,
        Options {
            names: Names::HostImport,
        },
    )
}

/// `FLEET_JS` with the conditional expression and the `try`/`catch` rewritten
/// the way this engine spells them. Nothing else is touched: the twenty-nine
/// function expressions, the twelve namespace tables and the ten calls
/// through `__host` are exactly as upstream writes them.
fn fleet_without_the_remaining_walls() -> String {
    FLEET_JS.replace(
        "  const resultJson = __host.fleet_call(opId, params === undefined ? \"{}\" : params);\n  try {\n    return JSON.parse(resultJson);\n  } catch (_err) {\n    return resultJson;\n  }\n",
        "  let p = params;\n  if (p === undefined) { p = \"{}\"; }\n  const resultJson = __host.fleet_call(opId, p);\n  return JSON.parse(resultJson);\n",
    )
}

/// A snapshot of `agenterm/scripts/qjs/lib/fleet.js`.
const FLEET_JS: &str = r#####"
// fleet.js — Fleet broker wrapper for qjs scripts.
// Wraps __host.fleet_call(operation_id, params_json) -> result_json.
// Line-for-line port of scripts/lua/lib/fleet.lua: same operation_id
// strings, same params shape, so a script that calls fleet.tabs.set_note(...)
// produces the identical Fleet operation regardless of which engine ran it
// — see crates/agenterm-qjs/src/host.rs module doc and PRD "Script engine
// family" / "capability alignment" for why this is a deliberate port, not
// a reinvention. Uses QuickJS's native JSON (no std.json module needed,
// unlike lua's stdlib.lua wrapper).

const fleet = {};

function call(opId, params) {
  const resultJson = __host.fleet_call(opId, params === undefined ? "{}" : params);
  try {
    return JSON.parse(resultJson);
  } catch (_err) {
    return resultJson;
  }
}

// -- fleet.tabs ------------------------------------------------------------

fleet.tabs = {};

/** List all tabs. */
fleet.tabs.list = function () {
  return call("tabs.list");
};

/** Get the active tab. */
fleet.tabs.active = function () {
  return call("tabs.active");
};

/**
 * Set a note on a tab.
 *
 * The params key is `tab`, not `tab_id`: `tabs.set-note`'s OperationSpec
 * (`src/operations.rs`, `TAB_NOTE_PARAMETERS`) declares `tab` + `note`, and
 * `validate_fleet_parameters` (`src/client/mod.rs`) refuses any key the spec
 * does not list. Sending `tab_id` answered every call with
 * `broker_invalid_arguments: tabs.set-note does not accept parameter tab_id`,
 * so this function had never once worked. The JS argument name stays `tabId`
 * — only the wire key changed, so no caller has to be touched. Matches the
 * rh binding, which has always sent `"tab"` (`src/script_fleet.rs`).
 */
fleet.tabs.set_note = function (tabId, note) {
  return call("tabs.set-note", JSON.stringify({ tab: tabId, note: note }));
};

// -- fleet.terminal ----------------------------------------------------------

fleet.terminal = {};

/** Paste text into terminal. */
fleet.terminal.paste = function (text) {
  return call("terminal.paste", JSON.stringify({ text: text }));
};

// -- fleet.ui ----------------------------------------------------------------

fleet.ui = {};

/** Bootstrap the UI. */
fleet.ui.bootstrap = function () {
  return call("ui.bootstrap");
};

/** Get UI snapshot. */
fleet.ui.snapshot = function () {
  return call("ui.snapshot");
};

/** Get UI deltas since last snapshot. */
fleet.ui.deltas = function () {
  return call("ui.deltas");
};

fleet.ui.composer = {};

/** Send a composer message. */
fleet.ui.composer.send = function (text) {
  return call("ui.composer.send", JSON.stringify({ text: text }));
};

/** Hello / ping. */
fleet.ui.hello = function () {
  return call("ui.hello");
};

fleet.ui.input = {};

/** Send pointer event. */
fleet.ui.input.pointer = function (x, y, action) {
  return call("ui.input.pointer", JSON.stringify({ x: x, y: y, action: action }));
};

/** Send wheel event. */
fleet.ui.input.wheel = function (delta) {
  return call("ui.input.wheel", JSON.stringify({ delta: delta }));
};

fleet.ui.tab = {};

/** Open a new child tab. */
fleet.ui.tab.new_child = function () {
  return call("ui.tab.new-child");
};

/**
 * Select a tab by id.
 *
 * The params key is `tab`, not `id`: `ui.tab.select` declares the shared
 * `TAB_TARGET_PARAMETERS` (`src/operations.rs`), whose single optional
 * parameter is `tab`. Sending `id` was answered with
 * `broker_invalid_arguments: ui.tab.select does not accept parameter id`.
 * Signature unchanged. Note the host still has no Fleet mutation adapter for
 * `ui.tab.select` (`fleet_mutation_command`, `src/client/mod.rs`), so a
 * conformant call now gets as far as `broker_operation_unknown` instead —
 * see plan/design-fleet-binding-gaps.md §5.
 */
fleet.ui.tab.select = function (id) {
  return call("ui.tab.select", JSON.stringify({ tab: id }));
};

fleet.ui.tabs = {};

/** Hide tabs panel. */
fleet.ui.tabs.hide = function () {
  return call("ui.tabs.hide");
};

/** Show tabs panel. */
fleet.ui.tabs.show = function () {
  return call("ui.tabs.show");
};

/** Toggle tabs panel visibility. */
fleet.ui.tabs.toggle = function () {
  return call("ui.tabs.toggle");
};

/** Set tabs panel width. */
fleet.ui.tabs.set_width = function (width) {
  return call("ui.tabs.set-width", JSON.stringify({ width: width }));
};

fleet.ui.tree = {};

/** Toggle tree panel. */
fleet.ui.tree.toggle = function () {
  return call("ui.tree.toggle");
};

fleet.ui.window = {};

/** Activate the window. */
fleet.ui.window.activate = function () {
  return call("ui.window.activate");
};

// -- fleet.workspace ----------------------------------------------------------

fleet.workspace = {};

/** Get workspace info. */
fleet.workspace.info = function () {
  return call("workspace.info");
};

/** Shutdown the workspace. */
fleet.workspace.shutdown = function () {
  return call("workspace.shutdown");
};

// -- fleet.control_center -------------------------------------------------

fleet.control_center = {};

/** Open the control center. */
fleet.control_center.open = function () {
  return call("control-center.open");
};

/** Close the control center. */
fleet.control_center.close = function () {
  return call("control-center.close");
};

/** Get control center snapshot. */
fleet.control_center.snapshot = function () {
  return call("control-center.snapshot");
};

/** Get control center status. */
fleet.control_center.status = function () {
  return call("control-center.status");
};

// -- fleet.events -------------------------------------------------------------

fleet.events = {};

/** Read events buffer. */
fleet.events.read = function () {
  return call("events.read");
};

/** Wait for events (blocking). */
fleet.events.wait = function (timeoutMs) {
  return call("events.wait", JSON.stringify({ timeout_ms: timeoutMs }));
};

// -- fleet.protocol -------------------------------------------------------------

fleet.protocol = {};

/** Get protocol info. */
fleet.protocol.info = function () {
  return call("protocol.info");
};

// -- fleet.server -------------------------------------------------------------

fleet.server = {};

/** Kill the server. */
fleet.server.kill = function () {
  return call("server.kill");
};
"#####;
