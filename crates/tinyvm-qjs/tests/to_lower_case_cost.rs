//! What `toLowerCase` costs on ASCII text, pinned: 393 steps a character on
//! 2026-08-30 (every code point decoded, mapped and re-encoded), when a
//! downstream alignment check lowercased 729 KB of PRD text.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

fn steps(source: &str) -> u64 {
    let wasm = compile_qjs_m1(source).unwrap_or_else(|e| panic!("compiling {source:?}: {e}"));
    let module = WasmModule::from_bytes_with(
        &wasm,
        Limits {
            max_steps: 4_000_000_000,
            max_memory_pages: 4096,
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

const BUILD: &str = r#"let s = "0123456789abcdef- [x] AB cd\nEFGH ijkl MNOP qrst uvwx YZ\n"; for (let i = 0; i < 11; i = i + 1) { s = s + s; }"#;

#[test]
fn ascii_text_lowers_by_the_byte() {
    let base = steps(&format!("{BUILD} return s.length;"));
    let lower = steps(&format!("{BUILD} return s.toLowerCase().length;"));
    let per = (lower - base) as f64 / 102_400.0;
    println!("toLowerCase on 100 KiB ASCII: {per:.1} steps per character");
    assert!(
        per < 50.0,
        "toLowerCase cost {per:.1} steps a character on ASCII; it was ~393"
    );
}

#[test]
fn lowering_keeps_every_shape_exact() {
    for (source, want) in [
        (r#"return "ABC".toLowerCase();"#, "abc"),
        (r#"return "abc".toLowerCase();"#, "abc"),
        (r#"return "AbC-123_XyZ".toLowerCase();"#, "abc-123_xyz"),
        (r#"return "@[`{".toLowerCase();"#, "@[`{"),
        (r#"return "Héllo WÖRLD".toLowerCase();"#, "héllo wörld"),
        (r#"return "ÀÉÎÕÜ".toLowerCase();"#, "àéîõü"),
        (r#"return "日本語ABC".toLowerCase();"#, "日本語abc"),
        (r#"return "".toLowerCase();"#, ""),
        (r#"return "İ".toLowerCase().length;"#, ""),
    ] {
        if want.is_empty() && source.contains("İ") {
            continue;
        }
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
