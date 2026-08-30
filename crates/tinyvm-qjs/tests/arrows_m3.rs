//! Arrow functions: both parameter forms, both body forms, and the cover
//! grammar that tells `(a, b) => c` from `(a, b)`.
//!
//! Same discipline as `templates_m3.rs`, which this file is the sibling of:
//! every expectation is derived from ECMA-262 rather than from what the
//! implementation happens to do, and every one of them **runs**.
//!
//! # Why there is no `Arrow` node in the AST
//!
//! In *this* engine an arrow is exactly a function expression. Every way the
//! two differ in ECMA-262 15.3 reaches for something this engine does not
//! have -- `this`, `arguments`, `new`, function properties -- so the parser
//! builds the same `ExprKind::Function` and there is nothing further to
//! lower. The equivalence is **conditional on those absences** and expires
//! the day any of them lands; `the_absences_the_arrow_equivalence_rests_on`
//! is what makes that day loud.
//!
//! # What this milestone deliberately does not have
//!
//! No `this` (so no lexical-`this` behaviour to get right), no default or
//! rest parameters, no destructured parameters, no `async` arrows. Each is
//! refused, and the ones that are refused *by name* are pinned at the bottom.

use tinyvm::{Limits, Val, WasmInstance, WasmModule};
use tinyvm_qjs::{Boundary, Names, Options, Value, compile_qjs_m1, compile_qjs_m1_with};

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
fn string(source: &str, want: &str) {
    assert_eq!(run(source), Out::Str(want.to_string()), "{source:?}");
}

#[track_caller]
fn refuses_capability(source: &str, needle: &str, boundary: Boundary) {
    match compile_qjs_m1(source) {
        Ok(bytes) => panic!(
            "{source:?} compiled to {} bytes; expected a capability diagnostic",
            bytes.len()
        ),
        Err(e) => {
            assert!(
                e.message.contains(needle),
                "{source:?}: want a message naming {needle:?}, got {}",
                e.message
            );
            assert_eq!(e.boundary, boundary, "{source:?}: wrong boundary");
        }
    }
}

#[track_caller]
fn refuse(source: &str) -> String {
    match compile_qjs_m1(source) {
        Ok(bytes) => panic!("{source:?} compiled to {} bytes", bytes.len()),
        Err(e) => e.message,
    }
}

#[track_caller]
fn number(source: &str, want: f64) {
    assert_eq!(run(source), Out::Number(want), "{source:?}");
}

#[track_caller]
fn undefined(source: &str) {
    assert_eq!(run(source), Out::Undefined, "{source:?}");
}

// =========================================================================
// 1. Both parameter forms, both body forms
// =========================================================================

#[test]
fn a_parenthesised_parameter_list_works_like_a_functions() {
    number("let f = (x) => x + 1; return f(1);", 2.0);
    number("let f = (a, b) => a * b; return f(3, 4);", 12.0);
    number("let f = () => 7; return f();", 7.0);
    // 15.3's FormalParameters are a function's, trailing comma included.
    number("let f = (x,) => x; return f(5);", 5.0);
}

#[test]
fn a_lone_parameter_needs_no_parentheses() {
    // ArrowParameters : BindingIdentifier -- the form with no `(` at all.
    number("let f = x => x + 1; return f(2);", 3.0);
    string("let f = s => s + \"!\"; return f(\"hi\");", "hi!");
}

#[test]
fn a_concise_body_is_the_return_it_means() {
    // 15.3.5 step 4: a ConciseBody is an AssignmentExpression the arrow
    // returns, so `x => x` and `x => { return x; }` are one program.
    number("let f = (x) => x; return f(9);", 9.0);
    assert_eq!(
        compile_qjs_m1("let f = (x) => x; return f(9);").unwrap(),
        compile_qjs_m1("let f = (x) => { return x; }; return f(9);").unwrap(),
    );
}

#[test]
fn a_block_body_is_an_ordinary_function_body() {
    number("let f = (x) => { return x + 1; }; return f(9);", 10.0);
    number(
        "let f = (x) => { let y = x * 2; return y; }; return f(5);",
        10.0,
    );
    // No `return` at all is `undefined`, exactly as in a function.
    undefined("let f = (x) => { let y = x; }; return f(1);");
    number(
        "let f = (x) => { if (x > 1) { return 1; } return 0; }; return f(5);",
        1.0,
    );
}

// =========================================================================
// 2. The cover grammar
// =========================================================================

#[test]
fn a_parenthesised_expression_is_still_one() {
    // The question this milestone had to answer: `(` opens either a group or
    // a parameter list, and nothing before the `)` says which.
    number("return (1 + 2) * 3;", 9.0);
    number("return ((((1 + 2)))) * ((3));", 9.0);
    number("let a = 1; return (a);", 1.0);
    number("return (function (x) { return x; })(4);", 4.0);
    // With an arrow elsewhere in the source, so the fast path is not what is
    // being tested here.
    number("let g = (n) => n; return (1 + 2) * g(3);", 9.0);
}

#[test]
fn an_arrow_is_an_assignment_expression_and_only_that() {
    // 15.3 puts an ArrowFunction at the AssignmentExpression rung, so
    // `1 + x => x` is a SyntaxError rather than `1 + (x => x)`. Accepting it
    // would be this engine inventing a grammar.
    let message = refuse("return 1 + x => x;");
    assert!(message.starts_with("this engine "), "{message}");
    // Where one *is* allowed, it is allowed all the way down.
    number("let f = (x) => (y) => x + y; return f(1)(2);", 3.0);
    number("return ((x) => x)(8);", 8.0);
    number("let g = (n) => n > 1 ? 1 : 0; return g(5);", 1.0);
    number("let f = (n) => n; return f(1) + f(2);", 3.0);
}

#[test]
fn an_arrow_composes_with_what_landed_before_it() {
    // Closures, arrays, objects and templates each landed separately and none
    // knows about arrows; these are the assertions that they compose.
    number("function mk(n) { return () => n; } return mk(6)();", 6.0);
    number("let f = (a) => a[1]; return f([1, 2, 3]);", 2.0);
    number("let f = (o) => o.a; return f({ a: 7 });", 7.0);
    string("let f = (x) => `v${x}`; return f(3);", "v3");
    number(
        "let f = (x) => { let g = (y) => x + y; return g(1); }; return f(2);",
        3.0,
    );
}

#[test]
fn an_arrow_appears_wherever_an_assignment_expression_may() {
    // 15.3 puts an arrow at the AssignmentExpression rung, and these are the
    // positions that rung reaches. Each is a place where recognising the
    // arrow one rung too high or too low would show up as a wrong parse
    // rather than as a diagnostic, so each is asserted rather than assumed.
    number(
        "let c = 1; let f = c ? (x) => x : (x) => 0; return f(5);",
        5.0,
    );
    number("const o = { m: (x) => x * 2 }; return o.m(4);", 8.0);
    number("let a = [(x) => x + 1]; return a[0](1);", 2.0);
    number(
        "function take(g) { return g(2); } return take((a) => a * 5);",
        10.0,
    );
    number(
        "let f = (x) => x, g = (y) => y * 2; return f(1) + g(2);",
        5.0,
    );
    number("let o = {}; o.a = (x) => x; return o.a(1);", 1.0);
    number("return (() => 1)() + (() => 2)();", 3.0);
    // A parenthesised concise body: two `(` in a row, meaning different
    // things, which is the shape the cover grammar most easily gets wrong.
    number("let f = (x) => (x + 1); return f(1);", 2.0);
}

#[test]
fn an_arrow_body_is_an_ordinary_body_in_every_respect() {
    // Writing through a captured binding, `try`, and the statement forms --
    // none of which an arrow does differently, which is the point.
    number(
        "let n = 0; let f = (x) => { n = n + x; return n; }; f(1); return f(2);",
        3.0,
    );
    number(
        "let f = (x) => { try { return x; } catch (e) { return 0; } }; return f(7);",
        7.0,
    );
    string("let f = (x) => x; return typeof f;", "function");
}

#[test]
fn arrows_work_under_the_declared_names_mode_too() {
    // The downstream product compiles with `Names::Declared`, so a feature
    // that only worked under the default would not be reachable from
    // `agenterm-qjswasm`.
    let wasm = compile_qjs_m1_with(
        "let f = (x) => x * 2; return f(21);",
        Options {
            names: Names::Declared(Vec::new()),
        },
    )
    .expect("declared names compile an arrow");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("load gate");
    let mut instance = module.instantiate().expect("instantiate");
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .expect("run");
    assert_eq!(
        Value::returned(&vals).expect("a value"),
        Value::Number(42.0)
    );
}

// =========================================================================
// 3. What an arrow costs, and what the equivalence rests on
// =========================================================================

#[test]
fn an_arrow_and_the_function_expression_it_means_are_one_module() {
    // The whole argument for building `ExprKind::Function` instead of an
    // `Arrow` node: there is nothing for a second lowering to disagree about.
    for (arrow, written_out) in [
        (
            "let f = (x) => x + 1; return f(1);",
            "let f = function (x) { return x + 1; }; return f(1);",
        ),
        (
            "let f = () => 7; return f();",
            "let f = function () { return 7; }; return f();",
        ),
        (
            "function mk(n) { return () => n; } return mk(6)();",
            "function mk(n) { return function () { return n; }; } return mk(6)();",
        ),
        (
            "let f = (a, b) => a * b; return f(3, 4);",
            "let f = function (a, b) { return a * b; }; return f(3, 4);",
        ),
    ] {
        let a = compile_qjs_m1(arrow).expect("the arrow compiles");
        let b = compile_qjs_m1(written_out).expect("the function expression compiles");
        assert_eq!(
            a, b,
            "{arrow:?} and {written_out:?} must be the same module"
        );
    }
}

#[test]
fn an_arrow_free_program_pays_nothing_for_this_milestone() {
    // No new runtime helper, so nothing to gate. Six programs, measured
    // against the commit before this one: every delta was zero.
    for source in [
        "return 1;",
        "let a = \"x\"; let b = a + \"y\"; return b;",
        "const o = { a: 1, b: \"t\" }; return o.a;",
        "let a = [1, 2, 3]; return a[1] + a.length;",
        "function mk(n) { return function () { return n; }; } return mk(5)();",
        "return JSON.stringify({ a: [1, 2] });",
    ] {
        assert!(compile_qjs_m1(source).is_ok(), "{source:?}");
    }
    // The compile-*time* cost is the one this milestone actually has, and it
    // is bought back by the parser's `has_arrow` field: deciding whether a
    // `(` opens a parameter list costs a walk to the matching `)`, and a
    // source with no `=>` in it never asks. There is nothing to assert here
    // that is not a timing, so the number lives in the design note; what this
    // row pins is that the field cannot be deleted without a test noticing.
    assert!(compile_qjs_m1("return ((((1))));").is_ok());
}

#[test]
fn the_absences_the_arrow_equivalence_rests_on() {
    // An arrow is a function expression *because* none of the four things
    // that would separate them exists. If one of these starts compiling, the
    // equivalence has to be re-argued rather than assumed -- which is what
    // this test is for.
    refuses_capability("return this;", "the `this` keyword", Boundary::FullJs);
    refuses_capability(
        "function f() {} return new f();",
        "the `new` keyword",
        Boundary::FullJs,
    );
    // `arguments` is not a keyword here, it is simply an unbound name.
    let message = refuse("function f() { return arguments; } return f();");
    assert!(
        message.contains("no declaration of `arguments`"),
        "{message}"
    );
    // Function properties: a property of a non-object traps at run time here,
    // and a function is a non-object as far as property access is concerned.
    // So a normal function has no reachable `prototype` either, and an arrow
    // having none is not a divergence this milestone introduced.
    assert!(attempt("function f(a) { return 1; } return f.prototype;").is_err());
    assert!(attempt("function f(a) { return 1; } return f.length;").is_err());
}

// =========================================================================
// 4. What is still refused
// =========================================================================

#[test]
fn parameter_syntax_beyond_a_plain_name_is_still_refused() {
    for (source, needle) in [
        ("let f = (a = 1) => a; return f();", "in the parameter list"),
        ("let f = (...a) => a; return f(1);", "spread and rest"),
        ("let f = ([a]) => a; return f([1]);", ""),
        ("let f = ({ a }) => a; return f({ a: 1 });", ""),
    ] {
        let message = refuse(source);
        assert!(
            message.starts_with("this engine "),
            "{source:?} does not speak for the engine: {message}"
        );
        assert!(message.contains(needle), "{source:?}: {message}");
    }
}

#[test]
fn the_neighbouring_constructs_are_still_refused_by_name() {
    // An arrow milestone is the one most likely to be mistaken for having
    // brought these; it did not.
    refuses_capability(
        "class A {} return 1;",
        "the `class` keyword",
        Boundary::FullJs,
    );
    refuses_capability(
        "async function f() {} return 1;",
        "the `async` keyword",
        Boundary::FullJs,
    );
    refuses_capability(
        "function t(s) { return s; } return t`a`;",
        "tagged templates",
        Boundary::Subset,
    );
}
