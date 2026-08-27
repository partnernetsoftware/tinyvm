//! Q1's measurement target. Lives here rather than under `tests/` because it
//! belongs to the research track, not to the crate's suite; `Cargo.toml`
//! points a `[[test]]` at it. Deleted with the rest of the track.
//!
//! Run one variant at a time -- `Cargo.toml`'s three features are mutually
//! exclusive -- and diff against the same command with no feature:
//!
//! ```sh
//! cargo test -p tinyvm-qjs --features method-callsite \
//!   --test method_measure -- --nocapture | grep '^SIZE'
//! cargo test -p tinyvm-qjs --test method_measure -- --nocapture | grep '^SIZE'
//! ```
//!
//! Size 口径, per `.claude/skills/decisive-experiment` §2.5.1:
//! **边界** the whole `.wasm` a program compiles to, mechanism and method
//! bodies and data included; **工具** `compile_qjs_m1().len()`, one tool for
//! the whole track, never divided across tools; **构建** guest wasm, so the
//! `no_std`/`panic=abort` axis does not apply and these numbers must never be
//! set beside a host binary's; **目标/执行** wasm32, and every program below
//! is also *run* by the conformance suite -- these are not measure-only
//! artifacts.
//!
//! That is L3. L1 (mechanism alone) and L2 (mechanism + bodies) are **未测定**
//! for now: separating them needs per-function sizes, and no number here may
//! be presented as either.

/// Programs that call no method at all. Criterion ②, a boolean gate: every
/// delta must be 0.
const METHOD_FREE: &[(&str, &str)] = &[
    ("empty", "return 1;"),
    ("strings", "let a = \"x\"; let b = a + \"y\"; return b;"),
    ("objects", "const o = { a: 1, b: \"t\" }; return o.a;"),
    ("arrays", "let a = [1, 2, 3]; return a[1] + a.length;"),
    ("closures", "function mk(n) { return function () { return n; }; } return mk(5)();"),
    ("json", "return JSON.stringify({ a: [1, 2] });"),
    ("strlen", "return \"ab\".length;"),
];

/// Criterion ⑤ (intercept) and the per-call-site slope. `trim1` -> `trim2` is
/// the cost of a *second call site of the same method*, which is variant C's
/// characteristic cost and is ~0 for a variant that puts the dispatch in the
/// value rather than at the site.
const WITH_METHODS: &[(&str, &str)] = &[
    ("trim1", "return \"  a  \".trim();"),
    ("trim2", "return \"  a  \".trim() + \"  b  \".trim();"),
    ("trim3", "return \"  a  \".trim() + \"  b  \".trim() + \"  c  \".trim();"),
    // A receiver that is not a String: the gate turns on and the prefab is
    // never reached, so this is what the gate's inexactness costs.
    ("objtrim", "const o = { trim: function () { return 1; } }; return o.trim();"),
    // Criterion ③: what a *second method in the set* costs a program that
    // uses only the first. If the set is all-or-nothing, `trim1` moves when a
    // method it never calls is added -- which is the linear growth the
    // decision tree judges negative.
    ("idx1", "return \"abc\".indexOf(\"b\");"),
    ("idx2", "return \"abc\".indexOf(\"b\") + \"de\".indexOf(\"e\");"),
    ("both", "return \"  a  \".trim().length + \"abc\".indexOf(\"b\");"),
];

#[test]
fn measure() {
    for (name, src) in METHOD_FREE.iter().chain(WITH_METHODS) {
        let bytes = tinyvm_qjs::compile_qjs_m1(src).expect(name).len();
        println!("SIZE {name} {bytes}");
    }
}
