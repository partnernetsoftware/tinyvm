//! `JSON.stringify(value, null, space)` -- ECMA-262 25.5.2 steps 5-8 and the
//! `indent` / `gap` threading of 25.5.2.5 / 25.5.2.6 -- checked against
//! serde_json's pretty printer, which writes exactly the spec's shape for
//! a spaces gap, and by hand where serde has no spelling (a String gap,
//! the ten-unit cut).
//!
//! The downstream `rh_compat.qjs::stringify_pretty` wrote compact JSON for
//! want of this and said so at line 96.

use serde_json::Value as Json;
use tinyvm::{Limits, Val, WasmInstance, WasmModule};
use tinyvm_qjs::{Value, compile_qjs_m1};

fn attempt(source: &str) -> Result<(WasmInstance, Vec<Val>), String> {
    let wasm = compile_qjs_m1(source).map_err(|e| format!("compiling {source:?}: {e}"))?;
    let module = WasmModule::from_bytes_with(&wasm, Limits::default())
        .map_err(|e| format!("load gate rejected {source:?}: {}", e.message()))?;
    let mut instance = module
        .instantiate()
        .map_err(|e| format!("instantiating {source:?}: {}", e.message()))?;
    let vals = instance
        .invoke_by_name("main", &Value::args(&[]))
        .map_err(|e| format!("trap in {source:?}: {}", e.message()))?;
    Ok((instance, vals))
}

#[track_caller]
fn text(source: &str) -> String {
    let (instance, vals) = attempt(source).unwrap_or_else(|e| panic!("{e}"));
    match Value::returned(&vals).expect("a value") {
        Value::String(ptr) => read_string(&instance, ptr),
        other => panic!("{source:?} answered {other:?}, not a String"),
    }
}

fn read_string(instance: &WasmInstance, ptr: i32) -> String {
    let view = instance.memory().expect("guest memory");
    let bytes: &[u8] = &view;
    let at = ptr as usize;
    let len = u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize;
    String::from_utf8(bytes[at + 4..at + 4 + len].to_vec()).expect("UTF-8")
}

/// 25.5.2.5 / 25.5.2.6 by hand over a serde value, with `indent` as the
/// gap: what a conforming `JSON.stringify(v, null, gap)` writes, byte for
/// byte -- `{}` and `[]` for empties, `": "` after a key, each member on
/// its own line, the closer on the outer indent. Written out rather than
/// borrowed from serde's pretty printer so the oracle is the spec's text
/// and not a library's reading of it (and because serde's is not on this
/// crate's dependency list).
fn pretty(value: &Json, indent: &str) -> String {
    fn go(value: &Json, indent: &str, depth: usize, out: &mut String) {
        let inner = indent.repeat(depth + 1);
        let outer = indent.repeat(depth);
        match value {
            Json::Object(map) if !map.is_empty() => {
                out.push('{');
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push('\n');
                    out.push_str(&inner);
                    out.push_str(&serde_json::to_string(k).expect("a key"));
                    out.push_str(": ");
                    go(v, indent, depth + 1, out);
                }
                out.push('\n');
                out.push_str(&outer);
                out.push('}');
            }
            Json::Array(items) if !items.is_empty() => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push('\n');
                    out.push_str(&inner);
                    go(v, indent, depth + 1, out);
                }
                out.push('\n');
                out.push_str(&outer);
                out.push(']');
            }
            other => out.push_str(&serde_json::to_string(other).expect("a scalar")),
        }
    }
    let mut out = String::new();
    go(value, indent, 0, &mut out);
    out
}

const DOC: &str = r#"{"a":1,"b":[1,2,{"c":null}],"d":{},"e":[],"f":"x","g":{"h":[[]]}}"#;

#[test]
fn a_number_space_indents_by_that_many_spaces() {
    let value: Json = serde_json::from_str(DOC).expect("the document is JSON");
    for n in 1..=10 {
        let got = text(&format!(
            "return JSON.stringify(JSON.parse('{DOC}'), null, {n});"
        ));
        assert_eq!(got, pretty(&value, &" ".repeat(n)), "space = {n}");
    }
    // Step 6: clamped to ten; below one is no gap at all.
    assert_eq!(
        text(&format!(
            "return JSON.stringify(JSON.parse('{DOC}'), null, 20);"
        )),
        pretty(&value, &" ".repeat(10))
    );
    for zero in ["0", "-3", "0.5", "0 / 0"] {
        assert_eq!(
            text(&format!(
                "return JSON.stringify(JSON.parse('{DOC}'), null, {zero});"
            )),
            DOC,
            "space = {zero}"
        );
    }
    // A fraction truncates (ToIntegerOrInfinity).
    assert_eq!(
        text(&format!(
            "return JSON.stringify(JSON.parse('{DOC}'), null, 2.9);"
        )),
        pretty(&value, "  ")
    );
}

#[test]
fn a_string_space_is_its_first_ten_code_units() {
    let value: Json = serde_json::from_str(DOC).expect("the document is JSON");
    assert_eq!(
        text(&format!(
            "return JSON.stringify(JSON.parse('{DOC}'), null, \"\\t\");"
        )),
        pretty(&value, "\t")
    );
    assert_eq!(
        text(&format!(
            "return JSON.stringify(JSON.parse('{DOC}'), null, \"--\");"
        )),
        pretty(&value, "--")
    );
    // Step 7: the first ten code units, and no more.
    assert_eq!(
        text(&format!(
            "return JSON.stringify(JSON.parse('{DOC}'), null, \"abcdefghijklmnop\");"
        )),
        pretty(&value, "abcdefghij")
    );
    assert_eq!(
        text(&format!(
            "return JSON.stringify(JSON.parse('{DOC}'), null, \"abcdefghij\");"
        )),
        pretty(&value, "abcdefghij")
    );
    // Multi-byte characters are one unit each; a pair is two, and a pair
    // that would be the eleventh unit is left out whole.
    assert_eq!(
        text(&format!(
            "return JSON.stringify(JSON.parse('{DOC}'), null, \"\\u{{e9}}\\u{{e9}}\");"
        )),
        pretty(&value, "éé")
    );
    assert_eq!(
        text(&format!(
            "return JSON.stringify(JSON.parse('{DOC}'), null, \"123456789\\u{{1f600}}x\");"
        )),
        pretty(&value, "123456789")
    );
    assert_eq!(
        text(&format!(
            "return JSON.stringify(JSON.parse('{DOC}'), null, \"12345678\\u{{1f600}}x\");"
        )),
        pretty(&value, "12345678\u{1f600}")
    );
    // An empty String is no gap; so is any other type (step 8).
    for none in ["\"\"", "true", "null", "undefined", "{}", "[]"] {
        assert_eq!(
            text(&format!(
                "return JSON.stringify(JSON.parse('{DOC}'), null, {none});"
            )),
            DOC,
            "space = {none}"
        );
    }
}

#[test]
fn scalars_and_empties_are_unchanged_by_a_gap() {
    for (source, want) in [
        ("return JSON.stringify(1, null, 2);", "1"),
        ("return JSON.stringify(\"a\", null, 2);", "\"a\""),
        ("return JSON.stringify(true, null, 2);", "true"),
        ("return JSON.stringify(null, null, 2);", "null"),
        ("return JSON.stringify({}, null, 2);", "{}"),
        ("return JSON.stringify([], null, 2);", "[]"),
        ("return JSON.stringify([[]], null, 2);", "[\n  []\n]"),
        (
            "return JSON.stringify({a: {}}, null, 2);",
            "{\n  \"a\": {}\n}",
        ),
        // An omitted property leaves no line behind.
        (
            "return JSON.stringify({a: undefined, b: 1}, null, 2);",
            "{\n  \"b\": 1\n}",
        ),
        ("return JSON.stringify({a: undefined}, null, 2);", "{}"),
        (
            "return JSON.stringify([undefined, 1], null, 1);",
            "[\n null,\n 1\n]",
        ),
    ] {
        assert_eq!(text(source), want, "{source}");
    }
    // `undefined` is still `undefined`, gap or not.
    let (_, vals) = attempt("return JSON.stringify(undefined, null, 2);").expect("runs");
    assert_eq!(Value::returned(&vals), Ok(Value::Undefined));
}

#[test]
fn a_generated_corpus_agrees_with_serde_at_every_gap() {
    // The same shape `json.rs` draws its round-trip corpus from, printed
    // pretty at a few gaps and read back through the engine. Keys are in
    // sorted order because serde's map is sorted and the engine's is
    // insertion order; `json.rs` pins the insertion order separately.
    let docs = [
        r#"[1,[2,[3,[4]]],{"a":{"b":{"c":[{}]}}}]"#,
        r#"{"n":-1500,"s":"a\"b\\c\n","t":true,"z":[null,false]}"#,
        r#"[[],[[]],[[[]]],{},{"":{}}]"#,
        r#"{"k":"caf\u00e9 \ud83d\ude00","u":[1,"2",[3]]}"#,
    ];
    for doc in docs {
        let value: Json = serde_json::from_str(doc).expect("the document is JSON");
        let src = doc.replace('\\', "\\\\");
        for n in [1, 2, 4] {
            let got = text(&format!(
                "return JSON.stringify(JSON.parse('{src}'), null, {n});"
            ));
            assert_eq!(got, pretty(&value, &" ".repeat(n)), "{doc} at {n}");
        }
        // And compact is exactly what it was.
        let compact = text(&format!("return JSON.stringify(JSON.parse('{src}'));"));
        assert_eq!(
            compact,
            serde_json::to_string(&value).expect("compact"),
            "{doc}"
        );
    }
}

#[test]
fn a_replacer_is_still_refused_by_name() {
    // Step 4 wants a callback or a property list; neither is priced yet, so
    // a replacer that is not `undefined` / `null` throws, and the message
    // says which argument -- the space is no longer in that sentence.
    for source in [
        "try { return JSON.stringify({a: 1}, x => x, 2); } catch (e) { return e; }",
        "try { return JSON.stringify({a: 1}, [\"a\"]); } catch (e) { return e; }",
        "try { return JSON.stringify({a: 1}, 1); } catch (e) { return e; }",
    ] {
        assert_eq!(
            text(source),
            "this engine does not support a JSON.stringify replacer yet",
            "{source}"
        );
    }
}
