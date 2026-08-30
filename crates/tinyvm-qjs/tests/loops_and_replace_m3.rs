//! Two features that share nothing: `break`/`continue` (ECMA-262 14.9, 14.8)
//! and `replace`/`replaceAll` (22.1.3.19, .20).
//!
//! They are in one file because they were built in one pass -- one is loop
//! lowering, the other a string prefab, and neither touches the other's code.
//!
//! # Where they came from
//!
//! Rows five, six and seven of the second demand survey. `break` is in 17 of
//! 82 downstream scripts (56 uses) and `continue` in 15 (48). `.replace(` is
//! in only 14 -- but 142 uses, the widest gap in the survey between how many
//! scripts are blocked and how much it hurts the ones that are.
//!
//! # Why `replace` ships with `replaceAll`
//!
//! Every downstream `.replace(` looks like `.replace("\r\n", "\n")`:
//! normalising line endings, which means *every* occurrence. In JavaScript
//! that is `replaceAll`; `replace` with a string pattern changes only the
//! first. Shipping one without the other would hand somebody a silent wrong
//! answer whichever one it was.
//!
//! They share a Rust function and **not** their emitted bytes -- 525 for the
//! first and 515 more for the second, measured below. That was worth finding
//! out: the claim in the first draft was that the second would be nearly free,
//! which is what sharing source code feels like and is not what it costs.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

fn run(source: &str) -> String {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .unwrap_or_else(|e| panic!("load gate rejected {source:?}: {}", e.message()));
    let mut instance = module
        .instantiate()
        .unwrap_or_else(|e| panic!("instantiating {source:?}: {}", e.message()));
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .unwrap_or_else(|e| panic!("trap in {source:?}: {}", e.message()));
    match Value::returned(&vals).unwrap_or_else(|e| panic!("{source:?}: {e}")) {
        Value::String(ptr) => {
            let view = instance.memory().expect("guest memory");
            let bytes: &[u8] = &view;
            let at = ptr as usize;
            let len = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4")) as usize;
            String::from_utf8(bytes[at + 4..at + 4 + len].to_vec()).expect("utf-8")
        }
        Value::Number(x) => format!("{x}"),
        Value::Bool(b) => format!("{b}"),
        other => panic!("{source:?}: unexpected {other:?}"),
    }
}

// ---- break / continue ---------------------------------------------------

#[test]
fn break_leaves_the_loop() {
    assert_eq!(
        run(
            "let n = 0; for (let i = 0; i < 10; i = i + 1) { if (i === 3) { break; } n = n + 1; } return n;"
        ),
        "3"
    );
    assert_eq!(
        run("let n = 0; while (true) { n = n + 1; if (n > 4) { break; } } return n;"),
        "5"
    );
}

#[test]
fn continue_skips_the_rest_of_the_pass() {
    assert_eq!(
        run(
            "let n = 0; for (let i = 0; i < 6; i = i + 1) { if (i === 2) { continue; } n = n + i; } return n;"
        ),
        "13"
    );
}

/// The update still runs after a `continue` in a `for`.
///
/// The bug this is written against: branching to the loop's own label skips
/// the update and spins forever. `continue` must reach the *update*, which in
/// this lowering is what the loop label's top runs into.
#[test]
fn a_continue_in_a_for_still_runs_the_update() {
    assert_eq!(
        run(
            "let n = 0; for (let i = 0; i < 4; i = i + 1) { if (i === 1) { continue; } n = n + 1; } return n;"
        ),
        "3"
    );
}

/// Both work at any nesting depth, which is what the depth bookkeeping is for.
#[test]
fn they_work_through_nested_blocks_and_ifs() {
    let source = "let n = 0;
    for (const x of [1,2,3,4,5]) {
        if (x === 2) { continue; }
        if (x > 3) { if (x === 5) { break; } }
        n = n + x;
    }
    return n;";
    assert_eq!(run(source), "8");
}

/// The inner loop's `break` leaves the inner loop only.
#[test]
fn break_leaves_the_innermost_loop() {
    let source = "let n = 0;
    for (let i = 0; i < 3; i = i + 1) {
        for (let j = 0; j < 5; j = j + 1) { if (j === 2) { break; } n = n + 1; }
    }
    return n;";
    assert_eq!(run(source), "6");
}

/// Inside a `try`/`catch`, which is where 21 of the corpus's uses are.
#[test]
fn they_work_inside_a_try_catch() {
    let source = "let n = 0;
    for (let i = 0; i < 5; i = i + 1) {
        try { if (i === 3) { break; } n = n + 1; } catch (e) { n = 0; }
    }
    return n;";
    assert_eq!(run(source), "3");
}

/// Outside a loop, refused where the mistake is.
#[test]
fn a_break_outside_a_loop_is_refused() {
    let error = compile_qjs_m1("break; return 1;").expect_err("nothing to branch to");
    assert!(
        error.message.contains("outside any loop"),
        "{}",
        error.message
    );
}

// ---- replace / replaceAll -----------------------------------------------

/// `replace` changes the first occurrence; `replaceAll` changes every one.
///
/// The pair the corpus's habit depends on: it writes `.replace("\r\n", "\n")`
/// meaning all of them, and in JavaScript that is the second name.
#[test]
fn replace_is_first_only_and_replace_all_is_every_one() {
    assert_eq!(run("return \"a-b-c\".replace(\"-\", \"+\");"), "a+b-c");
    assert_eq!(run("return \"a-b-c\".replaceAll(\"-\", \"+\");"), "a+b+c");
}

/// The shape 142 downstream uses have.
#[test]
fn normalising_line_endings_is_what_this_is_for() {
    assert_eq!(
        run("return \"a\\r\\nb\\r\\nc\".replaceAll(\"\\r\\n\", \"\\n\");"),
        "a\nb\nc"
    );
}

/// A pattern that is not there leaves the string alone.
#[test]
fn an_absent_pattern_changes_nothing() {
    assert_eq!(run("return \"abc\".replace(\"z\", \"!\");"), "abc");
    assert_eq!(run("return \"abc\".replaceAll(\"z\", \"!\");"), "abc");
    assert_eq!(run("return \"\".replaceAll(\"z\", \"!\");"), "");
}

/// Replacements longer and shorter than the pattern, which is what the output
/// bound has to survive.
#[test]
fn the_replacement_may_be_longer_or_shorter_or_empty() {
    assert_eq!(run("return \"a.b\".replaceAll(\".\", \"::::\");"), "a::::b");
    assert_eq!(run("return \"aXXXb\".replaceAll(\"XXX\", \"-\");"), "a-b");
    assert_eq!(run("return \"a-b-c\".replaceAll(\"-\", \"\");"), "abc");
    assert_eq!(run("return \"aaa\".replaceAll(\"a\", \"bb\");"), "bbbbbb");
}

/// Adjacent and edge matches.
#[test]
fn adjacent_and_edge_matches_are_all_found() {
    assert_eq!(run("return \"--\".replaceAll(\"-\", \"+\");"), "++");
    assert_eq!(run("return \"-a\".replaceAll(\"-\", \"+\");"), "+a");
    assert_eq!(run("return \"a-\".replaceAll(\"-\", \"+\");"), "a+");
    assert_eq!(run("return \"---\".replace(\"--\", \"+\");"), "+-");
}

/// The empty pattern, which ECMA-262 matches at every position **including the
/// ends** -- and which, unlike `split("")`, is representable here because no
/// lone surrogate is involved.
#[test]
fn the_empty_pattern_matches_everywhere_including_the_ends() {
    assert_eq!(run("return \"abc\".replaceAll(\"\", \"-\");"), "-a-b-c-");
    assert_eq!(run("return \"abc\".replace(\"\", \"-\");"), "-abc");
    assert_eq!(run("return \"\".replaceAll(\"\", \"-\");"), "-");
}

/// Multi-byte characters, on both sides of the swap.
#[test]
fn multi_byte_patterns_and_replacements_work() {
    assert_eq!(run("return \"a→b→c\".replaceAll(\"→\", \"-\");"), "a-b-c");
    assert_eq!(run("return \"a-b\".replaceAll(\"-\", \"😀\");"), "a😀b");
    assert_eq!(run("return \"x😀y\".replace(\"😀\", \"!\");"), "x!y");
}

/// A program that uses neither pays for neither.
#[test]
fn a_program_using_neither_pays_for_neither() {
    for (source, want) in [
        ("return 1;", 10_007),
        ("let o = {a:1}; o.b = 2; return o.a;", 10_580), /* +23 on 2026-08-29: a program that reads a static property can reach `__obj_get` with a String receiver, and the arm that names the missing property is 23 bytes; see runtime.rs `FAULT_MISSING_STRING_METHOD` */
    ] {
        let n = compile_qjs_m1(source).expect("compiles").len();
        assert_eq!(n, want, "{source:?} is {n} bytes");
    }
}

/// What they cost, and what the second one adds over the first.
///
/// The marginal number is the interesting one, and it refuted the claim it was
/// written to check: `replaceAll` adds nearly as much as `replace` costs,
/// because `Reach` is a compile-time choice and each name emits its own copy.
/// Sharing a Rust function shares maintenance, not bytes.
#[test]
fn what_the_pair_costs_is_written_down() {
    let size = |src: &str| compile_qjs_m1(src).expect("compiles").len();
    let base = size("return \"a\";");
    let one = size("return \"a\".replace(\"a\", \"b\");") - base;
    let both = size("return \"a\".replace(\"a\", \"b\") + \"c\".replaceAll(\"c\", \"d\");") - base;
    println!(
        "replace: {one} bytes; adding replaceAll: {} more",
        both - one
    );
    assert!(one > 0 && one < 900, "replace is {one} bytes");
}

// ---- Number() -----------------------------------------------------------

/// `Number(x)` is ToNumber, which is what unary `+` already was.
///
/// The survey's last unbuilt row with real demand: `parse_int` in 7 downstream
/// scripts, 17 uses. Probing first showed the *conversion* had worked since it
/// landed -- `+x`, `x * 1` and `x - 0` all reach it -- and only the name was
/// missing. So this is a fold, not a function: no runtime code, no gate, no
/// binding.
#[test]
fn number_converts_the_way_unary_plus_does() {
    assert_eq!(run("return Number(\"42\");"), "42");
    assert_eq!(run("return Number(\"3.5\");"), "3.5");
    assert_eq!(run("return Number(\"-7\");"), "-7");
    assert_eq!(run("return Number(true);"), "1");
    assert_eq!(run("return Number(5);"), "5");
    assert_eq!(run("return Number(\"\");"), "0");
}

/// `Number()` with no argument is `+0`, not `NaN`.
///
/// ECMA-262 21.1.1.1 step 1. `Number(undefined)` is the `NaN` one, and the two
/// are easy to get backwards -- which is why both are here.
#[test]
fn number_of_nothing_is_zero_and_number_of_undefined_is_not() {
    assert_eq!(run("return Number();"), "0");
    assert_eq!(
        run("return Number(undefined) === Number(undefined);"),
        "false"
    );
}

/// It reads the way the corpus does: text out of an argument, into a count.
#[test]
fn it_reads_the_way_the_corpus_parses_arguments() {
    let source = "const args = [\"12\", \"7\", \"x\"];
    let total = 0;
    for (const a of args) {
        const n = Number(a);
        if (n === n) { total = total + n; }
    }
    return total;";
    assert_eq!(run(source), "19");
}

/// A script that declares its own `Number` gets its own.
///
/// The fold is a *default*, and a declaration is a deliberate act -- the same
/// precedence `JSON` follows. Without this the fold would be a reserved word
/// nobody declared.
#[test]
fn a_script_that_declares_number_gets_its_own() {
    let source = "function Number(x) { return 99; }
    return Number(\"42\");";
    assert_eq!(run(source), "99");
}

/// `parseInt` stays unbound, and the diagnostic says the name rather than
/// pretending.
///
/// It is a different function: prefix parsing with a radix, answering `42` for
/// `"42abc"` where `Number` answers `NaN`. The corpus's `parse_int` is strict,
/// so `Number` is the honest match and this waits for somebody to ask for
/// prefix parsing by name.
#[test]
fn parse_int_is_not_silently_number() {
    let error = compile_qjs_m1("return parseInt(\"42abc\");").expect_err("unbound");
    assert!(error.message.contains("parseInt"), "{}", error.message);
}

// ---- the fourth fault code ---------------------------------------------

/// A String property this engine does not have stops with a fault that names
/// the *kind* of thing that happened.
///
/// The decision it reports is unchanged and was already argued in
/// `runtime.rs`: `"ab".length` is the only property answered, and the rest
/// trap rather than becoming `undefined`, because `"ab".toUpperCase` is a real
/// function in ECMA-262 and `undefined` there is a wrong answer wearing a
/// right answer's clothes.
///
/// What changed is that the trap used to arrive as the bare `unreachable` a
/// genuine engine defect executes, so a host could not tell "this engine does
/// not have that" from "this engine is broken" -- the same confusion
/// `UncaughtThrow` was added to prevent for its own case.
#[test]
fn a_missing_string_property_reports_a_capability_boundary() {
    let wasm = compile_qjs_m1("return \"ab\".length + (\"cd\".nosuch ? 1 : 0);")
        .expect("it compiles: the receiver's type is a run-time fact");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let outcome = instance.invoke_by_name("main", &Value::args(&[]));
    assert!(outcome.is_err(), "it must stop");

    let memory = instance.memory().expect("guest memory");
    assert_eq!(
        tinyvm_qjs::guest_fault(&memory),
        Some(tinyvm_qjs::GuestFault::MissingStringMethod),
        "the fault word must say which kind this was -- and since 2026-08-29 \
         a missing String property is its own kind, one that names itself"
    );
}

/// The other three codes still mean what they meant.
///
/// A fourth code is only worth having if it is *distinct*, so the test that
/// matters is the one showing an ordinary uncaught throw did not start
/// reporting the new one.
#[test]
fn an_uncaught_throw_is_still_its_own_kind() {
    let wasm = compile_qjs_m1("throw \"x\"; return 1;").expect("compiles");
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    let _ = instance.invoke_by_name("main", &Value::args(&[]));
    let memory = instance.memory().expect("guest memory");
    assert_eq!(
        tinyvm_qjs::guest_fault(&memory),
        Some(tinyvm_qjs::GuestFault::UncaughtThrow)
    );
}
