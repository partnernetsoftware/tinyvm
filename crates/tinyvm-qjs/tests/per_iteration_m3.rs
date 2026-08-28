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

/// `for`'s own header binding, ECMA-262 13.7.4.7.
///
/// A rule of its own, separate from 14.3.1 above: the loop variable is
/// declared once, so "a new binding per execution of a declaration" never
/// fires for it. The specification instead copies it into a fresh environment
/// each pass, which is why the closure from pass N sees N and the update
/// expression still counts up.
///
/// The copy is what distinguishes this from a declaration. A declaration
/// starts its binding from an initialiser; a loop variable carries its value
/// forward.
#[test]
fn the_for_header_binding_is_fresh_each_pass() {
    let source = "function make() {
        const fs = [];
        for (let i = 0; i < 3; i = i + 1) {
            fs.push(function () { return i; });
        }
        return \"\" + fs[0]() + fs[1]() + fs[2]();
    }
    return make();";
    assert_eq!(run_str(source), "012");
}

/// The same at script level, where the loop variable's cell pointer lives in a
/// global rather than a local.
#[test]
fn the_for_header_binding_is_fresh_each_pass_at_script_level() {
    let source = "const fs = [];
    for (let i = 0; i < 3; i = i + 1) {
        fs.push(function () { return i; });
    }
    return \"\" + fs[0]() + fs[1]() + fs[2]();";
    assert_eq!(run_str(source), "012");
}

/// A write to the loop variable *inside* the body reaches both the closure
/// made before it and the update after it.
///
/// The property a per-iteration copy is most likely to break: if the body's
/// binding were a detached duplicate rather than the one the loop carries
/// forward, this loop would never terminate or would skip passes.
///
/// The expected string is `3:135` and was written as `3:024` first, which is
/// worth recording because the mistake is the natural one. The closure is
/// pushed *before* `i = i + 1`, and the intuition is that it therefore froze
/// the earlier value. It did not: a closure captures the **binding**, the
/// body's write goes to that same binding, and the copy into the next pass's
/// binding happens after the body. So pass one's closure answers `1`, not `0`.
/// Both halves of this milestone have to hold at once for that -- fresh
/// bindings per pass, and still by binding rather than by value.
#[test]
fn a_write_to_the_loop_variable_in_the_body_reaches_its_closure_and_the_update() {
    let source = "function make() {
        const fs = [];
        for (let i = 0; i < 6; i = i + 1) {
            fs.push(function () { return i; });
            i = i + 1;
        }
        return \"\" + fs.length + \":\" + fs[0]() + fs[1]() + fs[2]();
    }
    return make();";
    assert_eq!(run_str(source), "3:135");
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

/// What per-iteration binding costs, written down.
///
/// The milestone's criterion 5 owed a slope, not an intercept: "today it is
/// small" says nothing about a design, while "each additional one costs N"
/// does. Three programs rather than two, because the first captured binding in
/// a loop drags in the whole closure apparatus -- a function value, an
/// environment, a capture slot -- and only the *second* one isolates what this
/// change added.
///
/// Criteria 3 and 4 are not measured here: they are the byte expectations
/// already standing in `closures_m3.rs`, which this milestone left untouched.
/// A program with no closure, and a program whose closures capture nothing
/// declared in a loop, compile to the same bytes they did before.
#[test]
fn what_a_per_iteration_binding_costs_is_written_down() {
    let size = |src: &str| compile_qjs_m1(src).expect("compiles").len();

    let none = size(
        "function mk() { const fs=[]; for (let i=0;i<3;i=i+1) { fs.push(1); } return fs; } \
         return mk().length;",
    );
    let one = size(
        "function mk() { const fs=[]; for (let i=0;i<3;i=i+1) { fs.push(function(){return i;}); } \
         return fs; } return mk().length;",
    );
    let two = size(
        "function mk() { const fs=[]; for (let i=0;i<3;i=i+1) { let v=i; \
         fs.push(function(){return i+v;}); } return fs; } return mk().length;",
    );

    let marginal = two - one;
    println!("loop capture: {} for the first, {marginal} per additional binding", one - none);
    assert_eq!(
        marginal, 83,
        "one more binding captured inside a loop body: its fresh cell at the \
         declarator, its copy before the update, and its environment slot"
    );
}
