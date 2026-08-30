//! What `.length` on a string costs, pinned so it cannot creep.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

fn steps(source: &str) -> u64 {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
    let mut instance = module.instantiate().expect("instantiates");
    instance
        .invoke_by_name("main", &Value::args(&[]))
        .unwrap_or_else(|e| panic!("trap in {source:?}: {}", e.message()));
    instance.last_steps()
}

const LONG: &str = r#"let t = ""; for (let i = 0; i < 600; i = i + 1) { t = t + "0123456789"; }"#;

#[test]
fn length_of_a_plain_string_is_cheap() {
    let base = steps(&format!("{LONG} return 1;"));
    let one = steps(&format!("{LONG} return t.length;"));
    let ten = steps(&format!(
        "{LONG} let n = 0; for (let k = 0; k < 10; k = k + 1) {{ n = n + t.length; }} return n;"
    ));
    let per_call = (ten - one) / 9;
    println!(
        ".length of a 6 000-char string: first {} steps, then {per_call} per call",
        one - base
    );
    assert!(
        per_call < 25_000,
        ".length on 6 000 plain characters cost {per_call} steps a call; it was ~180 000"
    );
}

#[test]
fn length_counts_utf16_units_across_word_boundaries() {
    for (source, want) in [
        (r#"return "".length;"#, 0.0),
        (r#"return "abcdefg".length;"#, 7.0),
        (r#"return "abcdefgh".length;"#, 8.0),
        (r#"return "abcdefghi".length;"#, 9.0),
        (r#"return "0123456789abcdef".length;"#, 16.0),
        (r#"return "héllo".length;"#, 5.0),
        (r#"return "abcdefghé".length;"#, 9.0),
        (r#"return "éabcdefgh".length;"#, 9.0),
        (r#"return "日本語テキスト".length;"#, 7.0),
        (r#"return "abcdefgh日本語abcdefghijklmnop".length;"#, 27.0),
        (r#"return "😀".length;"#, 2.0),
        (r#"return "abcdefg😀abcdefgh".length;"#, 17.0),
        (
            r#"let t = ""; for (let i = 0; i < 100; i = i + 1) { t = t + "0123456789"; } return t.length;"#,
            1000.0,
        ),
        (
            r#"let t = ""; for (let i = 0; i < 100; i = i + 1) { t = t + "012345678é"; } return t.length;"#,
            1000.0,
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
