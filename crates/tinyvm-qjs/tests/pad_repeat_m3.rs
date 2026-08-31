//! `padStart` / `padEnd` (ECMA-262 22.1.3.16/.17) and `repeat`
//! (22.1.3.19), landed 2026-08-31 in the fourth A13 batch:
//! rh_compat.qjs's `pad_left` prepends `"0"` in a loop -- one allocation
//! per turn -- because the engine had no `padStart`.

use tinyvm::{Limits, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1, guest_capability_name, guest_fault};

fn run(source: &str) -> Result<(String, u64), String> {
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
    let outcome = instance.invoke_by_name("main", &Value::args(&[]));
    let memory = instance.memory().expect("guest memory");
    let bytes: &[u8] = &memory;
    match outcome {
        Err(_) => Err(format!(
            "{:?} {:?}",
            guest_fault(&memory),
            guest_capability_name(&memory)
        )),
        Ok(vals) => {
            let Ok(Value::String(p)) = Value::returned(&vals) else {
                panic!("{source:?} did not answer a String");
            };
            let at = p as usize;
            let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            Ok((
                std::str::from_utf8(&bytes[at + 4..at + 4 + len])
                    .expect("UTF-8")
                    .to_string(),
                instance.last_steps(),
            ))
        }
    }
}

#[track_caller]
fn string(source: &str, want: &str) {
    assert_eq!(run(source).unwrap().0, want, "{source:?}");
}

#[test]
fn string_pad_fills_truncates_and_defaults() {
    string(r#"return "5".padStart(3, "0");"#, "005");
    string(r#"return "abc".padStart(9, "12");"#, "121212abc");
    string(r#"return "abc".padEnd(9, "12");"#, "abc121212");
    string(r#"return "5".padStart(4, "ab");"#, "aba5");
    string(r#"return "5".padEnd(4, "ab");"#, "5aba");
    // Nothing missing, a negative target, an empty filler: the receiver.
    string(r#"return "abc".padStart(2, "0");"#, "abc");
    string(r#"return "abc".padStart(3, "0");"#, "abc");
    string(r#"return "abc".padStart(-1);"#, "abc");
    string(r#"return "abc".padStart(5, "");"#, "abc");
    // The default filler is a space, written or not (step 4).
    string(r#"return "abc".padStart(5);"#, "  abc");
    string(r#"return "abc".padEnd(5);"#, "abc  ");
    string(r#"return "abc".padStart(5, undefined);"#, "  abc");
    // Positions are code units; the filler may be multi-byte.
    string(r#"return "x".padStart(3, "\u{e9}");"#, "\u{e9}\u{e9}x");
    string(r#"return "\u{1f600}".padEnd(4, "ab");"#, "\u{1f600}ab");
}

#[test]
fn repeat_copies_and_refuses_a_negative_count() {
    string(r#"return "ab".repeat(3);"#, "ababab");
    string(r#"return "ab".repeat(1);"#, "ab");
    string(r#"return "ab".repeat(0);"#, "");
    string(r#"return "".repeat(5);"#, "");
    string(r#"return "ab".repeat(2.9);"#, "abab");
    string(
        r#"return "\u{e9}\u{1f600}".repeat(2);"#,
        "\u{e9}\u{1f600}\u{e9}\u{1f600}",
    );
    let refusal = run(r#"return "ab".repeat(-1);"#).unwrap_err();
    assert!(
        refusal.contains("a negative String.repeat count"),
        "{refusal}"
    );
}

/// A pad cut mid-filler on a surrogate pair is `slice`'s own named
/// refusal: the missing half has no UTF-8, and this engine does not
/// fabricate one where a browser makes a lone surrogate.
#[test]
fn a_pad_that_splits_a_surrogate_pair_is_refused_by_name() {
    let refusal = run(r#"return "x".padStart(2, "\u{1f600}");"#).unwrap_err();
    assert!(
        refusal.contains("a slice boundary inside a surrogate pair"),
        "{refusal}"
    );
}

/// The demand's spelling: `pad_left` loops one `"0" + text` per missing
/// character; `padStart` is one allocation.
#[test]
fn pad_start_beats_the_hand_written_pad_left() {
    let by_hand = run(r#"
        function pad_left(value, width) {
          let text = "" + value;
          while (text.length < width) { text = "0" + text; }
          return text;
        }
        let s = "";
        for (let i = 0; i < 100; i = i + 1) { s = pad_left(7, 40); }
        return s;"#)
    .unwrap()
    .1;
    let by_engine = run(r#"let s = "";
        for (let i = 0; i < 100; i = i + 1) { s = "7".padStart(40, "0"); }
        return s;"#)
    .unwrap()
    .1;
    println!("100 pad_left by hand: {by_hand} steps; padStart: {by_engine} steps");
    assert!(
        by_engine * 2 < by_hand,
        "padStart should be at least 2x cheaper: {by_hand} vs {by_engine}"
    );
}

fn bytes(source: &str) -> usize {
    compile_qjs_m1(source)
        .unwrap_or_else(|e| panic!("compiling {source:?}: {e}"))
        .len()
}

#[test]
fn each_method_has_a_published_price() {
    let base = bytes("return \"6\" + 3;");
    let rows = [
        (r#"return "5".padStart(3, "0");"#, 1_635),
        (r#"return "5".padEnd(3, "0");"#, 1_633),
        (r#"return "5".padStart(3);"#, 1_666),
        (r#"return "ab".repeat(3);"#, 669),
    ];
    let got: Vec<usize> = rows
        .iter()
        .map(|(source, _)| bytes(source) - base)
        .collect();
    for ((source, _), n) in rows.iter().zip(&got) {
        println!("{source:?} costs {n} bytes over `return \"6\" + 3;`");
    }
    for ((source, want), n) in rows.iter().zip(&got) {
        assert_eq!(n, want, "{source:?} costs {n} bytes");
    }
    assert_eq!(bytes("return 1;"), 10_198, "the base program moved");
}
