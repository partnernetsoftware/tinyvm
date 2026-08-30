//! What `a + b` on strings costs, pinned so it cannot creep.
//!
//! Measured through the downstream CLI at 904a22ee: `s = s + "x"` on a
//! growing string ran to ~8 800 steps an append, a byte copied per loop
//! iteration through both operands. Scripts build their output that way.

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

/// A 1 000-character string built ten characters at a time, then one more
/// append of a single character: the difference is what appending to a
/// 1 000-character string costs.
fn append_cost() -> u64 {
    let build = r#"let t = ""; for (let i = 0; i < 100; i = i + 1) { t = t + "0123456789"; }"#;
    let base = steps(&format!("{build} return t.length;"));
    let plus = steps(&format!("{build} t = t + \"x\"; return t.length;"));
    plus - base
}

#[test]
fn appending_to_a_long_string_copies_it_by_the_word() {
    let cost = append_cost();
    println!("s + \"x\" with s at 1 000 characters: {cost} steps");
    assert!(
        cost < 3_000,
        "one append to a 1 000-character string cost {cost} steps; it was ~8 800"
    );
}

#[test]
fn concatenation_keeps_its_bytes_and_its_tail() {
    for (source, want) in [
        (r#"return "abc" + "defgh";"#, "abcdefgh"),
        (r#"return "" + "x";"#, "x"),
        (r#"return "12345" + "";"#, "12345"),
        (r#"return "1234" + "5678" + "9";"#, "123456789"),
        (
            r#"let t = ""; for (let i = 0; i < 7; i = i + 1) { t = t + "ab" + i; } return t;"#,
            "ab0ab1ab2ab3ab4ab5ab6",
        ),
        (r#"return "héllo" + " wörld";"#, "héllo wörld"),
    ] {
        let wasm = compile_qjs_m1(source).expect("compiles");
        let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
        let mut instance = module.instantiate().expect("instantiates");
        let vals = instance
            .invoke_by_name("main", &Value::args(&[]))
            .expect("runs");
        let Value::String(ptr) = Value::returned(&vals).expect("value") else {
            panic!("{source}: not a string")
        };
        assert_eq!(read_string(&instance, ptr), want, "{source}");
    }
}

fn read_string(instance: &tinyvm::WasmInstance, ptr: i32) -> String {
    let view = instance.memory().expect("guest memory");
    let bytes: &[u8] = &view;
    let at = ptr as usize;
    let len = u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize;
    String::from_utf8(bytes[at + 4..at + 4 + len].to_vec()).expect("valid UTF-8")
}
