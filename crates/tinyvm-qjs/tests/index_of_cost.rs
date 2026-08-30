//! What `indexOf` / `includes` cost on a long haystack, pinned.
//!
//! A miss over 128 KiB measured 38.7 / 35.7 steps a character on
//! 2026-08-30, when a downstream lint scanned 13.9 MB of tracked text that
//! way. The position loop now skips a four-byte window that holds no copy
//! of the needle's first byte.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

fn steps(source: &str) -> u64 {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(
        &wasm,
        Limits {
            max_steps: 4_000_000_000,
            ..Limits::default()
        },
    )
    .expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    instance
        .invoke_by_name("main", &Value::args(&[]))
        .unwrap_or_else(|e| panic!("trap in {source:?}: {}", e.message()));
    instance.last_steps()
}

const BUILD: &str =
    r#"let s = "0123456789abcdef"; for (let i = 0; i < 13; i = i + 1) { s = s + s; }"#;

#[test]
fn a_miss_skips_clear_windows() {
    let base = steps(&format!("{BUILD} return s.length;"));
    let inc = steps(&format!("{BUILD} return s.includes(\"\\n<<<<<<<\");"));
    let idx = steps(&format!("{BUILD} return s.indexOf(\"zz\");"));
    let per_inc = (inc - base) as f64 / 131_072.0;
    let per_idx = (idx - base) as f64 / 131_072.0;
    println!("includes miss {per_inc:.1}/char, indexOf miss {per_idx:.1}/char on 128 KiB");
    assert!(
        per_inc < 10.0,
        "includes miss cost {per_inc:.1} steps a character; it was ~36"
    );
    assert!(
        per_idx < 10.0,
        "indexOf miss cost {per_idx:.1} steps a character; it was ~39"
    );
}

#[test]
fn split_skips_clear_windows_between_separators() {
    let base = steps(&format!("{BUILD} return s.length;"));
    let split = steps(&format!("{BUILD} return s.split(\"\\n\").length;"));
    let per = (split - base) as f64 / 131_072.0;
    println!("split on a 128 KiB string without the separator: {per:.1} steps per character");
    assert!(
        per < 35.0,
        "split cost {per:.1} steps a character between separators; it was ~73"
    );
    for (source, want) in [
        (r#"return "a,b,c".split(",").length;"#, 3.0),
        (r#"return "abcdefgh".split("h").length;"#, 2.0),
        (r#"return "abcdefghijk".split("ijk").length;"#, 2.0),
        (r#"return "aaaaaaab".split("ab").length;"#, 2.0),
        (
            r#"return "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".split("y").length;"#,
            1.0,
        ),
        (
            r#"return "line one\nline two\nline three".split("\n").length;"#,
            3.0,
        ),
        (r#"return "a--b--c--".split("--").length;"#, 4.0),
        (
            r#"let t = ""; for (let i = 0; i < 100; i = i + 1) { t = t + "0123456789\n"; } return t.split("\n").length;"#,
            101.0,
        ),
    ] {
        let wasm = compile_qjs_m1(source).expect("compiles");
        let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
        let mut instance = module.instantiate().expect("instantiates");
        let vals = instance
            .invoke_by_name("main", &Value::args(&[]))
            .expect("runs");
        assert_eq!(
            Value::returned(&vals).expect("value"),
            Value::Number(want),
            "{source}"
        );
    }
}

#[test]
fn hits_and_misses_stay_exact_around_the_windows() {
    for (source, want) in [
        (r#"return "abcdefgh".indexOf("h");"#, 7.0),
        (r#"return "abcdefgh".indexOf("a");"#, 0.0),
        (r#"return "abcdefgh".indexOf("de");"#, 3.0),
        (r#"return "abcdefgh".indexOf("gh");"#, 6.0),
        (r#"return "abcdefgh".indexOf("hi");"#, -1.0),
        (r#"return "abcdefghijk".indexOf("ijk");"#, 8.0),
        (r#"return "abcabcabd".indexOf("abd");"#, 6.0),
        (r#"return "aaaaaaab".indexOf("ab");"#, 6.0),
        (
            r#"return "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".indexOf("y");"#,
            -1.0,
        ),
        (r#"return "héllo wörld".indexOf("wö");"#, 6.0),
        (r#"return "abc".indexOf("");"#, 0.0),
        (r#"return "ab".indexOf("abc");"#, -1.0),
        (
            r#"let t = ""; for (let i = 0; i < 100; i = i + 1) { t = t + "0123456789"; } t = t + "needle"; return t.indexOf("needle");"#,
            1000.0,
        ),
        (
            r#"let t = ""; for (let i = 0; i < 100; i = i + 1) { t = t + "0123456789"; } return t.indexOf("9012") ;"#,
            9.0,
        ),
    ] {
        let wasm = compile_qjs_m1(source).expect("compiles");
        let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
        let mut instance = module.instantiate().expect("instantiates");
        let vals = instance
            .invoke_by_name("main", &Value::args(&[]))
            .expect("runs");
        assert_eq!(
            Value::returned(&vals).expect("value"),
            Value::Number(want),
            "{source}"
        );
    }
    for (source, want) in [
        (r#"return "abcdefgh".includes("h");"#, true),
        (r#"return "abcdefgh".includes("hi");"#, false),
        (r#"return "abcabcabd".includes("abd");"#, true),
        (r#"return "aaaaaaab".includes("ab");"#, true),
        (
            r#"return "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".includes("y");"#,
            false,
        ),
        (
            r#"let t = ""; for (let i = 0; i < 100; i = i + 1) { t = t + "0123456789"; } return t.includes("\n<<<<<<<");"#,
            false,
        ),
    ] {
        let wasm = compile_qjs_m1(source).expect("compiles");
        let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
        let mut instance = module.instantiate().expect("instantiates");
        let vals = instance
            .invoke_by_name("main", &Value::args(&[]))
            .expect("runs");
        assert_eq!(
            Value::returned(&vals).expect("value"),
            Value::Bool(want),
            "{source}"
        );
    }
}
