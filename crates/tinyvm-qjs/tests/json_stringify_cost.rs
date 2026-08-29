//! What `JSON.stringify` costs per kind of content, pinned so it cannot creep.
//!
//! Measured through the downstream CLI at 904a22ee: ~700 steps per byte of
//! output for small objects.

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

const LONG: &str = r#"let t = ""; for (let i = 0; i < 100; i = i + 1) { t = t + "0123456789"; }"#;
const OBJECTS: &str = r#"let a = []; for (let i = 0; i < 50; i = i + 1) { a.push({name: "item" + i, count: i, ok: true}); }"#;

#[test]
fn a_plain_string_is_quoted_in_runs() {
    let build = steps(&format!("{LONG} return t.length;"));
    let quote = steps(&format!("{LONG} return JSON.stringify(t).length;"));
    let per_byte = (quote - build) / 1002;
    println!("JSON.stringify of a 1000-char string: {per_byte} steps per byte");
    assert!(per_byte < 50, "a plain string byte cost {per_byte} steps to quote; it was ~117");
}

#[test]
fn small_objects_have_a_known_price() {
    let build = steps(&format!("{OBJECTS} return a.length;"));
    let ser = steps(&format!("{OBJECTS} return JSON.stringify(a).length;"));
    let len = 50 * 40; // ~ {"name":"item12","count":12,"ok":true},
    let per_byte = (ser - build) / len;
    println!("JSON.stringify of 50 small objects: {} steps, ~{per_byte} per output byte", ser - build);
    assert!(per_byte < 400, "a small-object byte cost {per_byte} steps to serialize");
}

#[test]
fn quoting_keeps_every_escape_around_the_runs() {
    for (source, want) in [
        (r#"return JSON.stringify("plain");"#, r#""plain""#),
        (r#"return JSON.stringify("");"#, r#""""#),
        (r#"return JSON.stringify("a\"b\\c\nd");"#, r#""a\"b\\c\nd""#),
        (r#"return JSON.stringify("\"");"#, r#""\"""#),
        (r#"return JSON.stringify("\u0001x\ty");"#, r#""\u0001x\ty""#),
        (r#"return JSON.stringify("héllo wörld");"#, r#""héllo wörld""#),
        (r#"return JSON.stringify("end\\");"#, r#""end\\""#),
        (r#"return JSON.stringify({k: "v\"q"});"#, r#"{"k":"v\"q"}"#),
    ] {
        let wasm = compile_qjs_m1(source).expect("compiles");
        let module = WasmModule::from_bytes_with(&wasm, Limits::default()).expect("loads");
        let mut instance = module.instantiate().expect("instantiates");
        let vals = instance.invoke_by_name("main", &Value::args(&[])).expect("runs");
        let Value::String(ptr) = Value::returned(&vals).expect("value") else { panic!("{source}: not a string") };
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
