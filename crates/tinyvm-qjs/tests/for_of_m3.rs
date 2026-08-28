//! `for … of`, ECMA-262 13.7.5, over arrays.
//!
//! Every expectation is derived from the specification rather than from what
//! the implementation does, and every one of them **runs**: compile ->
//! tinyvm's load gate -> instantiate -> `invoke_by_name("main")`.
//!
//! # Why this feature and why now
//!
//! It is the largest gap the demand survey found: 64 of the 82 scripts in the
//! downstream `scripts/rh/` corpus use a `for … in`/`of` loop, against 46 for
//! template literals, which shipped first. The survey is in `prd/PRD.md`
//! under "语言路线图的依据", and its point is that the earlier roadmap was in
//! catalogue order rather than demand order.
//!
//! It was queued behind the per-iteration binding milestone rather than done
//! first, because a `for … of` binding is per-iteration too and doing it
//! first would have meant fixing that twice.
//!
//! # What this is not
//!
//! Not the iterator protocol. There is no `Symbol.iterator` in this engine, so
//! "iterable" here means "an array", and the two guards below say so out loud
//! rather than letting a Map or a generator quietly produce nothing.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

enum Out {
    Str(String),
    Num(f64),
    Threw(String),
}

fn run(source: &str) -> Out {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .unwrap_or_else(|e| panic!("load gate rejected {source:?}: {}", e.message()));
    let mut instance = module
        .instantiate()
        .unwrap_or_else(|e| panic!("instantiating {source:?}: {}", e.message()));
    let vals = match instance.invoke_by_name("main", &Value::args(&[])) {
        Ok(vals) => vals,
        Err(e) => return Out::Threw(e.message().to_owned()),
    };
    match Value::returned(&vals) {
        Ok(Value::String(ptr)) => {
            let view = instance.memory().expect("guest memory");
            let bytes: &[u8] = &view;
            let at = ptr as usize;
            let len = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4")) as usize;
            Out::Str(String::from_utf8(bytes[at + 4..at + 4 + len].to_vec()).expect("utf-8"))
        }
        Ok(Value::Number(x)) => Out::Num(x),
        Ok(other) => panic!("{source:?}: unexpected {other:?}"),
        Err(e) => Out::Threw(e.to_string()),
    }
}

fn text(source: &str) -> String {
    match run(source) {
        Out::Str(s) => s,
        Out::Num(x) => format!("{x}"),
        Out::Threw(e) => panic!("{source:?} threw unexpectedly: {e}"),
    }
}

fn threw(source: &str) -> String {
    match run(source) {
        Out::Threw(e) => e,
        Out::Str(s) => panic!("{source:?} returned {s:?} instead of throwing"),
        Out::Num(x) => panic!("{source:?} returned {x} instead of throwing"),
    }
}

/// The elements, in order, once each.
#[test]
fn it_visits_every_element_in_order() {
    assert_eq!(text("let s = 0; for (const x of [1,2,3]) { s = s + x; } return s;"), "6");
    assert_eq!(
        text("let s = \"\"; for (const x of [\"a\",\"b\",\"c\"]) { s = s + x; } return s;"),
        "abc"
    );
}

/// An empty array runs the body zero times, and that is a real answer rather
/// than the accidental one the missing guards would have produced for every
/// non-array.
#[test]
fn an_empty_array_runs_the_body_no_times() {
    assert_eq!(text("let n = 0; for (const x of []) { n = n + 1; } return n;"), "0");
}

/// `let`, `const` and `var` all bind the element.
#[test]
fn the_three_declaration_keywords_all_work() {
    assert_eq!(text("let s = 0; for (let x of [1,2]) { s = s + x; } return s;"), "3");
    assert_eq!(text("let s = 0; for (const x of [1,2]) { s = s + x; } return s;"), "3");
    assert_eq!(text("let s = 0; for (var x of [1,2]) { s = s + x; } return s;"), "3");
}

/// Each pass binds a **new** `x`, so a closure made on pass N sees pass N's
/// element.
///
/// ECMA-262 13.7.5.13 creates a fresh environment per iteration. Nothing in
/// this file implements that: the element declarator is emitted inside the
/// loop body, so `Lower::fresh_cell` from the per-iteration milestone gives it
/// a new cell each pass. The test is here because the property is `for … of`'s
/// to keep, whoever implements it.
#[test]
fn each_pass_binds_a_new_element_so_closures_do_not_share() {
    let source = "function make() {
        const fs = [];
        for (const x of [10,20,30]) { fs.push(function () { return x; }); }
        return \"\" + fs[0]() + fs[1]() + fs[2]();
    }
    return make();";
    assert_eq!(text(source), "102030");
}

/// The same at script level, where the binding's storage is a global rather
/// than a frame local.
#[test]
fn each_pass_binds_a_new_element_at_script_level_too() {
    let source = "const fs = [];
    for (const x of [10,20,30]) { fs.push(function () { return x; }); }
    return \"\" + fs[0]() + fs[1]() + fs[2]();";
    assert_eq!(text(source), "102030");
}

/// The length is read each pass, not cached, so a body that shortens the array
/// stops early.
///
/// This is what iterating an array actually does -- the Array iterator checks
/// the length on every `next` -- and it is the reason the fold puts
/// `S.length` in the loop test rather than in a temporary.
#[test]
fn the_length_is_read_each_pass_rather_than_cached() {
    let source = "const a = [1,2,3,4];
    let n = 0;
    for (const x of a) { n = n + 1; a.pop(); }
    return n;";
    // Passes: i=0 len 4 -> pop to 3; i=1 len 3 -> pop to 2; i=2 len 2 fails.
    assert_eq!(text(source), "2");
}

/// Nesting works, and the inner loop's synthetic bindings do not collide with
/// the outer's.
///
/// They cannot: each `for … of` declares its own pair in its own header scope,
/// the same way two `for (let i…)` loops each get their own `i`. The test
/// exists because "synthetic name" invites a global-counter implementation
/// that would have made this fail.
#[test]
fn nested_loops_do_not_collide() {
    let source = "let s = 0;
    for (const a of [1,2]) { for (const b of [10,20]) { s = s + a * b; } }
    return s;";
    assert_eq!(text(source), "90");
}

/// `of` is still not usable as a variable name, and that is recorded rather
/// than fixed here.
///
/// ECMA-262 makes `of` a *contextual* keyword -- `let of = 7` is legal
/// JavaScript. This engine's lexer keeps it in the contextual-keyword list
/// that turns an unlowerable phrase into a sentence naming it, and this change
/// deliberately leaves that list alone: `for (x of y)` with no declaration is
/// still unsupported and still needs that sentence, so removing `of` from the
/// list would trade one honest refusal for a worse one.
///
/// The divergence is small and real. It is written down here so that removing
/// it is a decision someone makes on purpose.
#[test]
fn of_is_not_yet_usable_as_a_variable_name() {
    assert!(
        compile_qjs_m1("let of = 7; return of;").is_err(),
        "`of` as an identifier is legal ECMA-262 and this engine refuses it"
    );
}

/// `for (x of y)` without a declaration keeps its old refusal.
///
/// Only the declaration form is folded. Assigning each element to an existing
/// binding is a different lowering -- the target can be any assignment target,
/// including a member expression -- and pretending otherwise would be the
/// silent-wrong-answer shape this whole feature is guarded against.
#[test]
fn the_declarationless_form_is_still_refused_by_name() {
    let error = compile_qjs_m1("const y = [1]; for (x of y) { }  return 1;")
        .expect_err("no declaration keyword, so this is not the folded form");
    let message = format!("{error}");
    assert!(
        message.contains("of"),
        "the refusal must name what it cannot lower, got {message}"
    );
}

/// A string is refused, loudly, rather than iterated by index.
///
/// Indexing a string would give UTF-16 code units where the specification
/// gives code points, so `for (const c of "ab")` would be wrong for exactly
/// the inputs people reach for it with -- an emoji, an accented letter written
/// as a combining pair. It throws instead.
///
/// The throw is read through a `catch`, and that is not incidental: an
/// **uncaught** throw leaves this engine as `unreachable executed`, which
/// carries no reason at all. Asserting on the uncaught form would have pinned
/// the trap rather than the message, and would have passed just as happily if
/// the guard had been deleted and `s[i]` trapped on its own.
#[test]
fn a_string_is_refused_with_a_reason_a_catch_can_read() {
    let source = "let why = \"none\";
    try { for (const c of \"abc\") { } } catch (e) { why = e; }
    return why;";
    let message = text(source);
    assert!(
        message.contains("code units"),
        "the refusal must say why a string is not iterated here, got {message:?}"
    );
}

/// Anything without a numeric `length` is refused too, which is the guard that
/// matters most.
///
/// Without it the loop would compare `undefined` and run **zero passes in
/// silence** -- a well-formed answer that is the wrong one, with no diagnostic
/// anywhere. That is the same failure shape the per-iteration milestone was
/// written about, and it is why this costs two `if`s.
#[test]
fn a_non_array_is_refused_rather_than_silently_iterated_zero_times() {
    for source in [
        "for (const x of ({a:1})) { } return 1;",
        "for (const x of 42) { } return 1;",
        "for (const x of undefined) { } return 1;",
    ] {
        let message = threw(source);
        assert!(
            !message.is_empty(),
            "{source:?} must throw rather than quietly do nothing"
        );
    }
}

/// The refusal is catchable, because it is a `throw` and not a trap.
#[test]
fn the_refusal_is_a_throw_a_catch_can_see() {
    let source = "let caught = \"no\";
    try { for (const x of 42) { } } catch (e) { caught = \"yes\"; }
    return caught;";
    assert_eq!(text(source), "yes");
}

/// A program with no `for … of` pays nothing for it.
///
/// The fold emits statements this engine already had, so there is no runtime
/// prelude to gate and nothing to switch on -- the zero-cost property comes
/// from the shape of the implementation rather than from a predicate somebody
/// has to keep exact. These are the same four programs `closures_m3.rs` uses
/// for the same purpose, and the same expected sizes.
#[test]
fn a_program_without_for_of_pays_nothing_for_it() {
    for (source, want) in [
        ("return 1;", 9_765),
        ("let o = {a:1}; o.b = 2; return o.a;", 9_886),
        (
            "function mk() { return function () { return 1; }; } let f = mk(); return f();",
            9_929,
        ),
    ] {
        let n = compile_qjs_m1(source).expect("compiles").len();
        assert_eq!(
            n, want,
            "{source:?} is {n} bytes; a program with no `for … of` must pay nothing for it"
        );
    }
}
