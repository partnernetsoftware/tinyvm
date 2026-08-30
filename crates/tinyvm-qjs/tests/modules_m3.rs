//! `import * as ns from "…"` and `export`, ECMA-262 16.2.
//!
//! Every expectation runs: compile -> tinyvm's load gate -> instantiate ->
//! `invoke_by_name("main")`. Designed in
//! `plan/design-module-milestone.md`, whose criteria this file is.
//!
//! # What a module is here
//!
//! **Compile-time inclusion, not a link.** The imported source is parsed into
//! a scope of its own and its exports become an ordinary object bound to the
//! alias. One `.wasm` comes out, with one load gate and one set of `Limits`.
//! The design note records that this was first written down as a linking model
//! and why that was wrong -- the evidence being a `format!` in the downstream
//! test suite that had been doing exactly this by hand.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{CompileError, Options, Value, compile_qjs_m1, compile_qjs_m1_with_modules};

/// A resolver over an in-memory table, which is all the compiler ever sees.
fn table(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
    let owned: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |spec: &str| {
        owned
            .iter()
            .find(|(k, _)| k == spec)
            .map(|(_, v)| v.clone())
    }
}

fn build(source: &str, modules: &[(&str, &str)]) -> Result<Vec<u8>, CompileError> {
    compile_qjs_m1_with_modules(source, Options::default(), &table(modules))
}

fn run(source: &str, modules: &[(&str, &str)]) -> String {
    let wasm = build(source, modules).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .unwrap_or_else(|e| panic!("load gate rejected {source:?}: {}", e.message()));
    let mut instance = module
        .instantiate()
        .unwrap_or_else(|e| panic!("instantiating {source:?}: {}", e.message()));
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .unwrap_or_else(|e| panic!("trap in {source:?}: {}", e.message()));
    match Value::returned(&vals).unwrap_or_else(|e| panic!("{source:?}: {e}")) {
        Value::Number(x) => format!("{x}"),
        Value::String(ptr) => {
            let view = instance.memory().expect("guest memory");
            let bytes: &[u8] = &view;
            let at = ptr as usize;
            let len = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4")) as usize;
            String::from_utf8(bytes[at + 4..at + 4 + len].to_vec()).expect("utf-8")
        }
        other => panic!("{source:?}: unexpected {other:?}"),
    }
}

fn refuse(source: &str, modules: &[(&str, &str)]) -> String {
    build(source, modules)
        .expect_err("this must not compile")
        .message
}

/// An exported function is callable through the namespace.
#[test]
fn an_exported_function_is_reachable_through_the_alias() {
    assert_eq!(
        run(
            "import * as m from \"lib\"; return m.two() + 1;",
            &[("lib", "export function two() { return 2; }")],
        ),
        "3"
    );
}

/// So is an exported `const`, including one holding a function value.
#[test]
fn exported_bindings_of_every_declaration_kind_are_reachable() {
    let lib = "export const answer = 42;
               export const twice = function (n) { return n * 2; };
               export function name() { return \"lib\"; }";
    assert_eq!(
        run(
            "import * as m from \"lib\"; return m.name() + m.answer + m.twice(3);",
            &[("lib", lib)],
        ),
        "lib426"
    );
}

/// A module keeps its own helpers to itself.
///
/// **Criterion 3, and the point of the whole feature.** What this replaces is
/// `format!("{lib}\n{driver}")`, which tips every top-level name of the
/// library into the script's scope. If an unexported helper were reachable,
/// the namespace would be decoration over the same concatenation.
#[test]
fn an_unexported_name_is_not_visible_to_the_importer() {
    let lib = "function secret() { return 1; }
               export function open() { return secret(); }";
    let error = refuse(
        "import * as m from \"lib\"; return secret();",
        &[("lib", lib)],
    );
    assert!(
        error.contains("secret"),
        "an unexported helper must be undeclared in the importer, got {error}"
    );
    // ...while the module can still use it itself.
    assert_eq!(
        run(
            "import * as m from \"lib\"; return m.open();",
            &[("lib", lib)]
        ),
        "1"
    );
}

/// The module cannot see the importer either.
///
/// The leak in the other direction, and the one that would be easy to ship by
/// accident: the module's scope is rooted at the script scope rather than at
/// wherever the `import` was written, so an importer's local is not in scope
/// for it.
#[test]
fn the_importer_is_not_visible_to_the_module() {
    let lib = "export function peek() { return hidden; }";
    let error = refuse(
        "const hidden = 5; import * as m from \"lib\"; return m.peek();",
        &[("lib", lib)],
    );
    assert!(
        error.contains("hidden"),
        "a module must not read its importer's names, got {error}"
    );
}

/// A module may import another one.
#[test]
fn modules_compose() {
    assert_eq!(
        run(
            "import * as top from \"a\"; return top.value();",
            &[
                (
                    "a",
                    "import * as b from \"b\"; export function value() { return b.base() + 1; }"
                ),
                ("b", "export function base() { return 10; }"),
            ],
        ),
        "11"
    );
}

/// A cycle is named, not hung on.
///
/// **Criterion 4.** Recursion through a resolver has no natural bottom, so the
/// alternative to detecting this is a stack overflow -- which aborts the
/// process rather than returning an error, and is the worst failure a compiler
/// of untrusted source can have.
#[test]
fn a_cycle_is_refused_and_both_specifiers_are_named() {
    let error = refuse(
        "import * as a from \"a\"; return a.f();",
        &[
            (
                "a",
                "import * as b from \"b\"; export function f() { return b.g(); }",
            ),
            (
                "b",
                "import * as a from \"a\"; export function g() { return 1; }",
            ),
        ],
    );
    assert!(error.contains('a') && error.contains('b'), "{error}");
    assert!(
        error.contains("cycle") || error.contains("imports itself"),
        "the diagnostic must say what is wrong, got {error}"
    );
}

/// A module that imports itself is the same case, and the smallest one.
#[test]
fn a_self_import_is_refused() {
    let error = refuse(
        "import * as a from \"a\"; return 1;",
        &[(
            "a",
            "import * as a from \"a\"; export function f() { return 1; }",
        )],
    );
    assert!(error.contains("imports itself"), "{error}");
}

/// An unresolvable specifier names itself.
#[test]
fn an_unresolvable_specifier_is_named() {
    let error = refuse("import * as m from \"nope\"; return 1;", &[]);
    assert!(error.contains("nope"), "{error}");
}

/// A module that exports nothing is refused rather than yielding an empty
/// object.
///
/// An empty namespace is a silent wrong answer: every `m.thing()` off it would
/// fail somewhere later, pointing at the call rather than at the missing
/// `export`.
#[test]
fn a_module_with_no_exports_is_refused_where_the_mistake_is() {
    let error = refuse(
        "import * as m from \"lib\"; return m.f();",
        &[("lib", "function f() { return 1; }")],
    );
    assert!(error.contains("export"), "{error}");
}

/// `export` in the entry source is refused.
///
/// There is nobody to export to. Accepting it and doing nothing is the shape
/// of every silent-wrong-answer this engine has recorded.
#[test]
fn export_in_the_entry_source_is_refused() {
    let error = refuse("export function f() { return 1; } return f();", &[]);
    assert!(error.contains("entry source"), "{error}");
}

/// The forms outside v1 are refused by name, not accepted and ignored.
#[test]
fn the_unsupported_import_forms_name_themselves() {
    for source in [
        "import { a } from \"lib\"; return 1;",
        "import d from \"lib\"; return 1;",
    ] {
        let error = refuse(source, &[("lib", "export const a = 1;")]);
        assert!(
            error.contains("import"),
            "{source:?} must name what it cannot do, got {error}"
        );
    }
}

/// Without a resolver, an `import` says so rather than failing obscurely.
#[test]
fn a_build_without_a_resolver_says_that_is_what_happened() {
    let error = compile_qjs_m1("import * as m from \"lib\"; return 1;")
        .expect_err("no resolver was supplied")
        .message;
    assert!(error.contains("resolver"), "{error}");
}

/// **Criterion 2.** A program with no `import` is byte-identical.
///
/// The same programs and the same expected sizes as `closures_m3.rs`, so the
/// two gates cannot drift apart. Modules add a parser path and no emitted
/// code, which is why this is a property of the design rather than of a
/// predicate somebody maintains.
#[test]
fn a_program_without_imports_pays_nothing_for_them() {
    for (source, want) in [
        ("return 1;", 10_007),
        ("let o = {a:1}; o.b = 2; return o.a;", 10_365), /* +23 on 2026-08-29: a program that reads a static property can reach `__obj_get` with a String receiver, and the arm that names the missing property is 23 bytes; see runtime.rs `FAULT_MISSING_STRING_METHOD` */
        (
            "function mk() { return function () { return 1; }; } let f = mk(); return f();",
            10_510,
        ),
    ] {
        let n = compile_qjs_m1(source).expect("compiles").len();
        assert_eq!(n, want, "{source:?} is {n} bytes without any import");
        let m = build(source, &[("unused", "export const x = 1;")])
            .expect("compiles")
            .len();
        assert_eq!(
            m, want,
            "merely having a resolver available must not change a byte"
        );
    }
}
