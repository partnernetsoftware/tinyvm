//! One binding per execution of a declaration, per ECMA-262 14.3.1.
//!
//! Every expectation here is derived from the specification, not from what the
//! implementation does, and every one of them **runs**: compile -> tinyvm's
//! load gate -> instantiate -> `invoke_by_name("main")`. Same discipline as
//! `closures_m3.rs`, which this is the sequel to.
//!
//! # Why this file exists apart from `closures_m3.rs`
//!
//! That file holds one property: a closure closes over the **binding**, not
//! its value. It is correct and it is fully evidenced -- for closures made by
//! a *factory*. This file holds the neighbouring property it never asked:
//! **how many bindings there are**. A loop body that declares `let v` declares
//! a new one on every pass, and three closures made in three passes must see
//! three values.
//!
//! Before the change these ran `222` where the specification requires `012`:
//! the cell was opened once at function entry, so every pass wrote the same
//! word. `plan/design-per-iteration-binding-milestone.md` records the five
//! measurements that separated this from the `for`-specific rule below.
//!
//! # What this file does NOT claim
//!
//! `for (let i = …)`'s own header binding is a *different* rule --
//! 13.7.4.7's `CreatePerIterationEnvironment`, which copies the loop variable
//! into a fresh environment each pass. It is not implemented, and
//! [`the_for_header_binding_is_still_shared_and_that_is_recorded_not_hidden`]
//! pins the current answer so the gap cannot be mistaken for coverage.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

fn run_str(source: &str) -> String {
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
        other => panic!("{source:?}: expected a string, got {other:?}"),
    }
}

/// Three passes of a loop body, three bindings, three answers.
///
/// ECMA-262 14.3.1: evaluating a LexicalDeclaration creates a binding. The
/// loop body evaluates it three times, so there are three, and the closure
/// made on pass N closed over pass N's.
#[test]
fn a_let_declared_in_a_loop_body_is_a_new_binding_each_pass() {
    let source = "function make() {
        const fs = [];
        for (let n = 0; n < 3; n = n + 1) {
            let v = n;
            fs.push(function () { return v; });
        }
        return \"\" + fs[0]() + fs[1]() + fs[2]();
    }
    return make();";
    assert_eq!(run_str(source), "012");
}

/// The same at script level, where the storage is different and the rule is not.
///
/// A script binding is two module globals rather than a frame local, because a
/// nested function may read it and a local does not outlive the script's
/// frame. That reasoning is sound and its conclusion was out of date: a heap
/// cell also outlives a frame, and it did not exist when the comment stating
/// it was written. So a script binding a nested function reads **and** whose
/// declaration sits in a loop is now an ordinary capture, and 14.3.1 is
/// answered by the same code as inside a function.
///
/// The two conditions are both load-bearing. Without "a nested function reads
/// it" there is nothing to observe. Without "in a loop" the declaration runs
/// once, one binding is already the right answer, and the conversion would buy
/// nothing while costing the closure apparatus at every reader -- 99 bytes,
/// measured. §2.1 of the design note records that criterion 4 ruled out the
/// wholesale version before it was written.
#[test]
fn the_same_holds_at_script_level_where_the_storage_differs() {
    let source = "const fs = [];
    for (let n = 0; n < 3; n = n + 1) {
        let v = n;
        fs.push(function () { return v; });
    }
    return \"\" + fs[0]() + fs[1]() + fs[2]();";
    assert_eq!(run_str(source), "012");
}

/// A script binding read by a nested function but **not** in a loop stays a
/// global, and this is the test that says the narrowing was real.
///
/// It answers the same value either way -- one execution, one binding -- so
/// the assertion below cannot distinguish the two storages. What it pins is
/// that the program still *works* after the resolution rule grew a case; the
/// byte gate in `closures_m3.rs` is what pins that it did not grow a cost.
#[test]
fn a_script_binding_read_from_a_function_outside_a_loop_still_works() {
    let source = "let x = 41;
    function f() { return x; }
    x = x + 1;
    return \"\" + f();";
    assert_eq!(run_str(source), "42");
}

/// `const` is the same rule; the keyword decides writability, not lifetime.
#[test]
fn a_const_in_a_loop_body_is_also_a_new_binding_each_pass() {
    let source = "function make() {
        const fs = [];
        for (let n = 0; n < 3; n = n + 1) {
            const v = n + 10;
            fs.push(function () { return v; });
        }
        return \"\" + fs[0]() + fs[1]() + fs[2]();
    }
    return make();";
    assert_eq!(run_str(source), "101112");
}

/// A `while` body is a loop body too, which is the point: this is a rule about
/// **declarations**, not about `for`.
///
/// If the fix had been written into the `for` lowering it would pass the tests
/// above and fail this one, and the difference is exactly the design decision
/// `plan/design-per-iteration-binding-milestone.md` §1 argues.
#[test]
fn a_while_body_gets_the_same_treatment_because_the_rule_is_about_declarations() {
    let source = "function make() {
        const fs = [];
        let n = 0;
        while (n < 3) {
            let v = n;
            fs.push(function () { return v; });
            n = n + 1;
        }
        return \"\" + fs[0]() + fs[1]() + fs[2]();
    }
    return make();";
    assert_eq!(run_str(source), "012");
}

/// A binding written *after* the closure exists is still seen through it.
///
/// The companion property to everything above, and the one a per-pass fix
/// could plausibly break: fresh bindings per pass must not become capture *by
/// value*. `closures_m3.rs` holds this for the factory shape; this holds it
/// for the loop shape, where the two rules meet.
#[test]
fn a_fresh_binding_per_pass_is_still_captured_by_binding_not_by_value() {
    let source = "function make() {
        const fs = [];
        for (let n = 0; n < 2; n = n + 1) {
            let v = n;
            fs.push(function () { return v; });
            v = v + 100;
        }
        return \"\" + fs[0]() + fs[1]();
    }
    return make();";
    assert_eq!(run_str(source), "100101");
}

/// **The gap, pinned.** `for`'s header binding is still shared.
///
/// ECMA-262 13.7.4.7 requires `012` here; this engine answers `333`. That is a
/// separate rule from 14.3.1 -- the specification gives `for` a per-iteration
/// *copy* that `while` does not get -- and it is not implemented.
///
/// The test asserts the wrong answer on purpose. A known divergence with a
/// test is a recorded fact; the same divergence without one is a claim of
/// coverage that does not exist, and the capability tree would say `[x]` over
/// both. When 13.7.4.7 lands, this test's expectation changes to `"012"` in
/// the same commit, and its name is what makes that impossible to forget.
#[test]
fn the_for_header_binding_is_still_shared_and_that_is_recorded_not_hidden() {
    let source = "function make() {
        const fs = [];
        for (let i = 0; i < 3; i = i + 1) {
            fs.push(function () { return i; });
        }
        return \"\" + fs[0]() + fs[1]() + fs[2]();
    }
    return make();";
    assert_eq!(
        run_str(source),
        "333",
        "ECMA-262 13.7.4.7 requires 012; this is the recorded gap, not a passing case"
    );
}

/// A `while` loop's *outer* variable is shared, and that is **correct**.
///
/// The control. 13.7.4.7 gives the per-iteration environment to `for` alone,
/// so a `while` closing over a variable declared outside it must see the final
/// value. Fixing this would be inventing a divergence, not removing one --
/// which is why the milestone's criteria list it as a pass condition.
#[test]
fn a_while_closing_over_an_outer_variable_still_sees_the_last_value() {
    let source = "function make() {
        const fs = [];
        let i = 0;
        while (i < 3) { fs.push(function () { return i; }); i = i + 1; }
        return \"\" + fs[0]() + fs[1]() + fs[2]();
    }
    return make();";
    assert_eq!(run_str(source), "333");
}
